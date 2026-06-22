//! slay — a modern pkill.
//!
//! Terminate processes by name, PID, or port. The headline trick is that a
//! target beginning with `:` is treated as a port — `slay :8080` finds and
//! kills whatever is listening there — while everything else behaves like a
//! friendlier, safer `pkill`.
//!
//! Linux-only (reads everything from `/proc`); the only dependency is `libc`
//! for the `kill(2)` syscall.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::MetadataExt;
use std::process;

const VERSION: &str = env!("CARGO_PKG_VERSION");

// ─── tiny ANSI helpers ──────────────────────────────────────────────────────
fn use_color() -> bool {
    // honour NO_COLOR and only colour a real terminal
    std::env::var_os("NO_COLOR").is_none() && unsafe { libc::isatty(1) == 1 }
}

struct Paint {
    on: bool,
}
impl Paint {
    fn wrap(&self, code: &str, s: &str) -> String {
        if self.on {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }
    fn bold(&self, s: &str) -> String { self.wrap("1", s) }
    fn dim(&self, s: &str) -> String { self.wrap("2", s) }
    fn red(&self, s: &str) -> String { self.wrap("31", s) }
    fn green(&self, s: &str) -> String { self.wrap("32", s) }
    fn yellow(&self, s: &str) -> String { self.wrap("33", s) }
    fn cyan(&self, s: &str) -> String { self.wrap("36", s) }
    fn magenta(&self, s: &str) -> String { self.wrap("35", s) }
}

// ─── target kinds ───────────────────────────────────────────────────────────
enum Target {
    Port(u16),
    Pid(i32),
    Name(String),
}

#[derive(Clone)]
struct Proc {
    pid: i32,
    user: String,
    name: String,    // /proc/<pid>/comm (truncated to 15 chars by the kernel)
    cmdline: String, // full argv, space-joined
    port: Option<u16>, // set when matched via a port target
}

// ─── option set ─────────────────────────────────────────────────────────────
struct Opts {
    signal: i32,
    full: bool,
    exact: bool,
    ignore_case: bool,
    yes: bool,
    user: Option<String>,
    columns: Vec<Col>,
    targets: Vec<String>,
    list: bool,
    help: bool,
    version: bool,
}

fn print_help() {
    let p = Paint { on: use_color() };
    let b = |s: &str| p.bold(s);
    println!(
        "{name} — a better pkill. Find and kill processes, then get out of your way.

{usage}
  slay <target>...          Kill matching processes (shows them, then asks)
  slay -l <target>...       List matching processes instead of killing
  slay -l                   List every process

A {tgt} is matched by name, PID, or port — whichever it looks like:
  slay firefox              by name      (substring of the process name)
  slay 4123                 by PID       (an exact process id)
  slay :8080                by port      (whoever is listening on it)

Mix them freely:  slay firefox 4123 :8080

{opts}
  -s, --signal <SIG>   Signal to send (name or number); default TERM
  -9                   Shorthand for --signal KILL
  -f, --full           Match the name against the full command line
  -x, --exact          Require an exact name match
  -i, --ignore-case    Case-insensitive name matching
  -u, --user <USER>    Only match processes owned by USER
  -c, --column <COL>   Add a column (repeatable, or comma-separated):
                         port, ppid, mem, threads, state, nice, time, age
  -y, --yes            Don't ask for confirmation
  -l, --list           List matches instead of killing (a built-in pgrep)
  -h, --help           Show this help
  -V, --version        Print version

The PORT column appears automatically when you target a port, or add it
anywhere with `-c port`.

{ex}
  slay node            # kill every process named like node (asks first)
  slay -9 -y firefox   # force-kill firefox, no questions
  slay -l vite -f      # list every process whose cmdline mentions vite
  slay -l -c mem -c age node   # list node procs with memory and age columns
  slay :5432           # something on Postgres' port? kill it",
        name = b("slay"),
        usage = b("USAGE"),
        tgt = b("target"),
        opts = b("OPTIONS"),
        ex = b("EXAMPLES"),
    );
}

fn parse_args() -> Result<Opts, String> {
    let mut o = Opts {
        signal: libc::SIGTERM,
        full: false,
        exact: false,
        ignore_case: false,
        yes: false,
        user: None,
        columns: Vec::new(),
        targets: Vec::new(),
        list: false,
        help: false,
        version: false,
    };
    let mut args = std::env::args().skip(1).peekable();
    let mut only_positional = false;
    while let Some(a) = args.next() {
        if only_positional {
            o.targets.push(a);
            continue;
        }
        match a.as_str() {
            "--" => only_positional = true,
            "-h" | "--help" => o.help = true,
            "-V" | "--version" => o.version = true,
            "-f" | "--full" => o.full = true,
            "-x" | "--exact" => o.exact = true,
            "-i" | "--ignore-case" => o.ignore_case = true,
            "-y" | "--yes" => o.yes = true,
            "-l" | "--list" => o.list = true,
            "-9" => o.signal = libc::SIGKILL,
            "-s" | "--signal" => {
                let v = args.next().ok_or("--signal needs a value")?;
                o.signal = parse_signal(&v)?;
            }
            "-u" | "--user" => {
                o.user = Some(args.next().ok_or("--user needs a value")?);
            }
            "-c" | "--column" => {
                let v = args.next().ok_or("--column needs a value")?;
                for part in v.split(',').filter(|x| !x.is_empty()) {
                    o.columns.push(parse_col(part)?);
                }
            }
            s if s.starts_with("--signal=") => {
                o.signal = parse_signal(&s["--signal=".len()..])?;
            }
            s if s.starts_with("--user=") => {
                o.user = Some(s["--user=".len()..].to_string());
            }
            s if s.starts_with("--column=") => {
                for part in s["--column=".len()..].split(',').filter(|x| !x.is_empty()) {
                    o.columns.push(parse_col(part)?);
                }
            }
            // attached short form: -cmem or -cmem,threads
            s if s.starts_with("-c") && s.len() > 2 && !s.starts_with("--") => {
                for part in s[2..].split(',').filter(|x| !x.is_empty()) {
                    o.columns.push(parse_col(part)?);
                }
            }
            // bundled short flags like -9y, -fx
            s if s.starts_with('-') && s.len() > 1 && !s.starts_with("--") => {
                for ch in s[1..].chars() {
                    match ch {
                        'f' => o.full = true,
                        'x' => o.exact = true,
                        'i' => o.ignore_case = true,
                        'y' => o.yes = true,
                        'l' => o.list = true,
                        '9' => o.signal = libc::SIGKILL,
                        'h' => o.help = true,
                        other => return Err(format!("unknown flag -{other}")),
                    }
                }
            }
            // reject unknown long flags instead of treating them as targets
            s if s.starts_with("--") => return Err(format!("unknown flag {s}")),
            _ => o.targets.push(a),
        }
    }
    Ok(o)
}

fn parse_signal(s: &str) -> Result<i32, String> {
    let up = s.trim().to_uppercase();
    let up = up.strip_prefix("SIG").unwrap_or(&up);
    if let Ok(n) = up.parse::<i32>() {
        return Ok(n);
    }
    let sig = match up {
        "HUP" => libc::SIGHUP,
        "INT" => libc::SIGINT,
        "QUIT" => libc::SIGQUIT,
        "ABRT" => libc::SIGABRT,
        "KILL" => libc::SIGKILL,
        "USR1" => libc::SIGUSR1,
        "USR2" => libc::SIGUSR2,
        "TERM" => libc::SIGTERM,
        "STOP" => libc::SIGSTOP,
        "CONT" => libc::SIGCONT,
        _ => return Err(format!("unknown signal: {s}")),
    };
    Ok(sig)
}

fn classify(t: &str) -> Target {
    if let Some(rest) = t.strip_prefix(':') {
        // allow :8080 or :8080/tcp
        let num = rest.split('/').next().unwrap_or(rest);
        if let Ok(p) = num.parse::<u16>() {
            return Target::Port(p);
        }
    }
    if let Ok(pid) = t.parse::<i32>() {
        return Target::Pid(pid);
    }
    Target::Name(t.to_string())
}

// ─── /proc enumeration ──────────────────────────────────────────────────────
fn passwd_map() -> HashMap<u32, String> {
    let mut m = HashMap::new();
    if let Ok(txt) = fs::read_to_string("/etc/passwd") {
        for line in txt.lines() {
            let mut f = line.split(':');
            let name = f.next().unwrap_or("");
            let _ = f.next(); // password
            if let Some(uid) = f.next().and_then(|u| u.parse::<u32>().ok()) {
                m.entry(uid).or_insert_with(|| name.to_string());
            }
        }
    }
    m
}

fn read_procs(users: &HashMap<u32, String>) -> Vec<Proc> {
    let mut out = Vec::new();
    let entries = match fs::read_dir("/proc") {
        Ok(e) => e,
        Err(_) => return out,
    };
    for ent in entries.flatten() {
        let fname = ent.file_name();
        let name_str = match fname.to_str() {
            Some(s) => s,
            None => continue,
        };
        let pid: i32 = match name_str.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let base = format!("/proc/{pid}");
        let uid = match fs::metadata(&base) {
            Ok(md) => md.uid(),
            Err(_) => continue, // process vanished
        };
        let comm = fs::read_to_string(format!("{base}/comm"))
            .unwrap_or_default()
            .trim_end()
            .to_string();
        let cmdline = read_cmdline(&base, &comm);
        out.push(Proc {
            pid,
            user: users.get(&uid).cloned().unwrap_or_else(|| uid.to_string()),
            name: comm,
            cmdline,
            port: None,
        });
    }
    out
}

fn read_cmdline(base: &str, comm: &str) -> String {
    match fs::read(format!("{base}/cmdline")) {
        Ok(bytes) if !bytes.is_empty() => {
            let s: Vec<&[u8]> = bytes.split(|b| *b == 0).filter(|p| !p.is_empty()).collect();
            String::from_utf8_lossy(&s.join(&b' ')).to_string()
        }
        // kernel threads have an empty cmdline; show the comm in brackets
        _ => format!("[{comm}]"),
    }
}

// ─── port → pid resolution ──────────────────────────────────────────────────
/// Socket inodes bound to `port`, gathered from all four /proc/net tables.
fn inodes_for_port(port: u16) -> HashSet<u64> {
    let mut inodes = HashSet::new();
    // tcp listeners are state 0A; for udp we accept any bound socket.
    for (path, listen_only) in [
        ("/proc/net/tcp", true),
        ("/proc/net/tcp6", true),
        ("/proc/net/udp", false),
        ("/proc/net/udp6", false),
    ] {
        let txt = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => continue,
        };
        for line in txt.lines().skip(1) {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 10 {
                continue;
            }
            let local = f[1];
            let state = f[3];
            let inode = f[9];
            if listen_only && state != "0A" {
                continue;
            }
            let port_hex = match local.rsplit(':').next() {
                Some(p) => p,
                None => continue,
            };
            if let Ok(p) = u16::from_str_radix(port_hex, 16) {
                if p == port {
                    if let Ok(ino) = inode.parse::<u64>() {
                        inodes.insert(ino);
                    }
                }
            }
        }
    }
    inodes
}

/// PIDs holding any of the given socket inodes (scans /proc/<pid>/fd).
fn pids_for_inodes(inodes: &HashSet<u64>) -> HashSet<i32> {
    let mut pids = HashSet::new();
    if inodes.is_empty() {
        return pids;
    }
    let entries = match fs::read_dir("/proc") {
        Ok(e) => e,
        Err(_) => return pids,
    };
    for ent in entries.flatten() {
        let pid: i32 = match ent.file_name().to_str().and_then(|s| s.parse().ok()) {
            Some(p) => p,
            None => continue,
        };
        let fd_dir = format!("/proc/{pid}/fd");
        let fds = match fs::read_dir(&fd_dir) {
            Ok(f) => f,
            Err(_) => continue, // not ours / gone
        };
        for fd in fds.flatten() {
            if let Ok(link) = fs::read_link(fd.path()) {
                if let Some(ino) = link
                    .to_str()
                    .and_then(|s| s.strip_prefix("socket:["))
                    .and_then(|s| s.strip_suffix(']'))
                    .and_then(|s| s.parse::<u64>().ok())
                {
                    if inodes.contains(&ino) {
                        pids.insert(pid);
                        break;
                    }
                }
            }
        }
    }
    pids
}

// ─── matching ───────────────────────────────────────────────────────────────
fn name_matches(p: &Proc, pat: &str, o: &Opts) -> bool {
    let hay = if o.full { &p.cmdline } else { &p.name };
    let (mut hay, mut pat) = (hay.to_string(), pat.to_string());
    if o.ignore_case {
        hay = hay.to_lowercase();
        pat = pat.to_lowercase();
    }
    if o.exact {
        // exact compares against the process name regardless of -f
        let name = if o.ignore_case { p.name.to_lowercase() } else { p.name.clone() };
        name == pat
    } else {
        hay.contains(&pat)
    }
}

// ─── columns ────────────────────────────────────────────────────────────────
#[derive(Clone, Copy, PartialEq)]
enum Col {
    Pid,
    User,
    Port,
    Name,
    Cmd,
    Ppid,
    Mem,
    Threads,
    State,
    Nice,
    Time,
    Age,
}

impl Col {
    fn header(self) -> &'static str {
        match self {
            Col::Pid => "PID",
            Col::User => "USER",
            Col::Port => "PORT",
            Col::Name => "PROCESS",
            Col::Cmd => "COMMAND",
            Col::Ppid => "PPID",
            Col::Mem => "MEM",
            Col::Threads => "THREADS",
            Col::State => "STATE",
            Col::Nice => "NICE",
            Col::Time => "TIME",
            Col::Age => "AGE",
        }
    }
    fn right(self) -> bool {
        matches!(self, Col::Pid | Col::Port | Col::Ppid | Col::Mem | Col::Threads | Col::Nice | Col::Time | Col::Age)
    }
    fn needs_stat(self) -> bool {
        matches!(self, Col::Ppid | Col::Mem | Col::Threads | Col::State | Col::Nice | Col::Time | Col::Age)
    }
}

fn parse_col(s: &str) -> Result<Col, String> {
    Ok(match s.trim().to_lowercase().as_str() {
        "port" | "ports" => Col::Port,
        "ppid" => Col::Ppid,
        "mem" | "rss" | "memory" => Col::Mem,
        "threads" | "thr" | "nlwp" => Col::Threads,
        "state" | "st" => Col::State,
        "nice" | "ni" => Col::Nice,
        "time" | "cpu" | "cputime" => Col::Time,
        "age" | "etime" | "elapsed" | "start" => Col::Age,
        other => {
            return Err(format!(
                "unknown column `{other}` (try: port, ppid, mem, threads, state, nice, time, age)"
            ))
        }
    })
}

/// Fields lifted from /proc/<pid>/stat for the optional columns.
struct Stat {
    ppid: i32,
    state: String,
    cpu_ticks: u64,   // utime + stime
    nice: i64,
    threads: i64,
    start_ticks: u64, // since boot
    rss_pages: i64,
}

fn read_stat(pid: i32) -> Option<Stat> {
    let s = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // comm (field 2) is parenthesised and may contain spaces/parens; everything
    // after the final ')' is whitespace-separated starting at field 3 (state).
    let close = s.rfind(')')?;
    let f: Vec<&str> = s[close + 1..].split_whitespace().collect();
    let g = |i: usize| f.get(i);
    Some(Stat {
        state: g(0)?.to_string(),
        ppid: g(1)?.parse().ok()?,
        cpu_ticks: g(11)?.parse::<u64>().ok()? + g(12)?.parse::<u64>().ok()?,
        nice: g(16)?.parse().ok()?,
        threads: g(17)?.parse().ok()?,
        start_ticks: g(19)?.parse().ok()?,
        rss_pages: g(21)?.parse().ok()?,
    })
}

struct Sys {
    page_size: u64,
    clk_tck: u64,
    uptime: f64,
}

fn sysinfo() -> Sys {
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(4096) as u64;
    let clk_tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as u64;
    let uptime = fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|s| s.split_whitespace().next().and_then(|n| n.parse::<f64>().ok()))
        .unwrap_or(0.0);
    Sys { page_size, clk_tck, uptime }
}

fn human_bytes(b: u64) -> String {
    const U: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b}B")
    } else if v >= 10.0 {
        format!("{:.0}{}", v, U[i])
    } else {
        format!("{:.1}{}", v, U[i])
    }
}

fn human_dur(secs: u64) -> String {
    let (d, h, m, s) = (secs / 86400, (secs % 86400) / 3600, (secs % 3600) / 60, secs % 60);
    if d > 0 {
        format!("{d}d{h:02}h")
    } else if h > 0 {
        format!("{h}h{m:02}m")
    } else if m > 0 {
        format!("{m}m{s:02}s")
    } else {
        format!("{s}s")
    }
}

fn cell(col: Col, pr: &Proc, st: &Option<Stat>, sys: &Sys) -> String {
    match col {
        Col::Pid => pr.pid.to_string(),
        Col::User => pr.user.clone(),
        Col::Port => pr.port.map(|v| v.to_string()).unwrap_or_else(|| "-".into()),
        Col::Name => pr.name.clone(),
        Col::Cmd => pr.cmdline.clone(),
        _ => match st {
            None => "?".into(),
            Some(s) => match col {
                Col::Ppid => s.ppid.to_string(),
                Col::State => s.state.clone(),
                Col::Nice => s.nice.to_string(),
                Col::Threads => s.threads.to_string(),
                Col::Mem => human_bytes(s.rss_pages.max(0) as u64 * sys.page_size),
                Col::Time => human_dur(s.cpu_ticks / sys.clk_tck),
                Col::Age => {
                    let start_s = s.start_ticks as f64 / sys.clk_tck as f64;
                    human_dur((sys.uptime - start_s).max(0.0) as u64)
                }
                _ => unreachable!(),
            },
        },
    }
}

/// Listening ports per displayed PID, as a comma-joined string. Built from the
/// /proc/net tables crossed with each process's open socket inodes. Only called
/// when the PORT column is actually shown, since it scans every listed pid's fds.
fn listening_port_map(procs: &[Proc]) -> HashMap<i32, String> {
    // Only TCP listeners — the "what is this serving" sense. (Bound UDP client
    // sockets would flood the column with ephemeral ports.) A port the user
    // explicitly targeted is merged in below regardless of protocol.
    let mut inode_port: HashMap<u64, u16> = HashMap::new();
    for path in ["/proc/net/tcp", "/proc/net/tcp6"] {
        if let Ok(txt) = fs::read_to_string(path) {
            for line in txt.lines().skip(1) {
                let f: Vec<&str> = line.split_whitespace().collect();
                if f.len() < 10 || f[3] != "0A" {
                    continue;
                }
                if let (Some(ph), Ok(ino)) = (f[1].rsplit(':').next(), f[9].parse::<u64>()) {
                    if let Ok(p) = u16::from_str_radix(ph, 16) {
                        inode_port.insert(ino, p);
                    }
                }
            }
        }
    }
    let mut ports: HashMap<i32, Vec<u16>> = HashMap::new();
    for pr in procs {
        if let Some(p) = pr.port {
            ports.entry(pr.pid).or_default().push(p); // an explicitly targeted port
        }
        if inode_port.is_empty() {
            continue;
        }
        if let Ok(fds) = fs::read_dir(format!("/proc/{}/fd", pr.pid)) {
            for fd in fds.flatten() {
                if let Ok(link) = fs::read_link(fd.path()) {
                    if let Some(p) = link
                        .to_str()
                        .and_then(|s| s.strip_prefix("socket:["))
                        .and_then(|s| s.strip_suffix(']'))
                        .and_then(|s| s.parse::<u64>().ok())
                        .and_then(|ino| inode_port.get(&ino).copied())
                    {
                        ports.entry(pr.pid).or_default().push(p);
                    }
                }
            }
        }
    }
    ports
        .into_iter()
        .map(|(pid, mut v)| {
            v.sort_unstable();
            v.dedup();
            // cap the width: show a few, then "+N" for the rest
            let s = if v.len() > 4 {
                let head = v[..4].iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",");
                format!("{head},+{}", v.len() - 4)
            } else {
                v.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",")
            };
            (pid, s)
        })
        .collect()
}

// ─── rendering ──────────────────────────────────────────────────────────────
fn print_table(procs: &[Proc], extra: &[Col], p: &Paint) {
    // PID USER [PORT] PROCESS  <extra…>  COMMAND  — COMMAND stays last (free width).
    // PORT shows when targeted by port, or asked for explicitly with `-c port`.
    let want_port = extra.contains(&Col::Port) || procs.iter().any(|x| x.port.is_some());
    let mut cols = vec![Col::Pid, Col::User];
    if want_port {
        cols.push(Col::Port);
    }
    cols.push(Col::Name);
    for &c in extra {
        if !cols.contains(&c) {
            cols.push(c);
        }
    }
    cols.push(Col::Cmd);

    let sys = sysinfo();
    let need_stat = cols.iter().any(|c| c.needs_stat());
    let stats: Vec<Option<Stat>> = procs
        .iter()
        .map(|pr| if need_stat { read_stat(pr.pid) } else { None })
        .collect();
    let port_map = if want_port { listening_port_map(procs) } else { HashMap::new() };

    // precompute every cell so we can size columns
    let rows: Vec<Vec<String>> = procs
        .iter()
        .zip(&stats)
        .map(|(pr, st)| {
            cols.iter()
                .map(|&c| match c {
                    Col::Port => port_map
                        .get(&pr.pid)
                        .filter(|s| !s.is_empty())
                        .cloned()
                        .unwrap_or_else(|| "-".into()),
                    _ => cell(c, pr, st, &sys),
                })
                .collect()
        })
        .collect();

    let widths: Vec<usize> = cols
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let w = rows.iter().map(|r| r[i].len()).max().unwrap_or(0);
            w.max(c.header().len())
        })
        .collect();

    let pad = |s: &str, w: usize, right: bool| {
        if right { format!("{s:>w$}") } else { format!("{s:<w$}") }
    };

    // header
    let mut line = String::from("  ");
    for (i, c) in cols.iter().enumerate() {
        let last = i == cols.len() - 1;
        let w = if last { 0 } else { widths[i] };
        line += &p.bold(&pad(c.header(), w, c.right()));
        if !last {
            line += "  ";
        }
    }
    println!("{line}");

    for (ri, r) in rows.iter().enumerate() {
        let mut line = String::from("  ");
        for (i, c) in cols.iter().enumerate() {
            let last = i == cols.len() - 1;
            let w = if last { 0 } else { widths[i] };
            let text = pad(&r[i], w, c.right());
            let painted = match c {
                Col::Name => p.magenta(&text),
                Col::Port => p.cyan(&text),
                _ => p.dim(&text),
            };
            line += &painted;
            if !last {
                line += "  ";
            }
        }
        let _ = ri;
        println!("{line}");
    }
}

fn signal_name(sig: i32) -> String {
    let n = match sig {
        libc::SIGTERM => "TERM",
        libc::SIGKILL => "KILL",
        libc::SIGHUP => "HUP",
        libc::SIGINT => "INT",
        libc::SIGQUIT => "QUIT",
        libc::SIGSTOP => "STOP",
        libc::SIGCONT => "CONT",
        libc::SIGUSR1 => "USR1",
        libc::SIGUSR2 => "USR2",
        _ => return format!("signal {sig}"),
    };
    format!("SIG{n}")
}

// ─── kill ───────────────────────────────────────────────────────────────────
fn confirm(prompt: &str) -> bool {
    print!("{prompt}");
    let _ = io::stdout().flush();
    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
}

fn main() {
    // Rust ignores SIGPIPE by default, which turns `slay -l | head` into a
    // panic. Restore the default so we exit quietly like any other CLI.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL); }

    let opts = match parse_args() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("slay: {e}");
            eprintln!("Try `slay --help`.");
            process::exit(2);
        }
    };
    let p = Paint { on: use_color() };

    if opts.help {
        print_help();
        return;
    }
    if opts.version {
        println!("slay {VERSION}");
        return;
    }

    let users = passwd_map();
    let procs = read_procs(&users);

    // `slay -l` with no targets is a plain process listing.
    if opts.list && opts.targets.is_empty() {
        let mut all = procs.clone();
        all.sort_by_key(|x| x.pid);
        print_table(&all, &opts.columns, &p);
        return;
    }

    if opts.targets.is_empty() {
        eprintln!("slay: no targets. Give a name, PID, or :port.");
        eprintln!("Try `slay -l` to browse, or `slay --help`.");
        process::exit(2);
    }

    // Resolve every target to a set of matched processes (deduped by pid).
    let me = process::id() as i32;
    let mut matched: HashMap<i32, Proc> = HashMap::new();
    let mut unresolved: Vec<String> = Vec::new();

    for t in &opts.targets {
        match classify(t) {
            Target::Port(port) => {
                let pids = pids_for_inodes(&inodes_for_port(port));
                let mut hit = false;
                for pr in &procs {
                    if pids.contains(&pr.pid) {
                        let e = matched.entry(pr.pid).or_insert_with(|| pr.clone());
                        e.port = Some(port);
                        hit = true;
                    }
                }
                if !hit {
                    unresolved.push(format!(":{port} (nothing listening)"));
                }
            }
            Target::Pid(pid) => {
                match procs.iter().find(|pr| pr.pid == pid) {
                    Some(pr) => { matched.entry(pid).or_insert_with(|| pr.clone()); }
                    None => unresolved.push(format!("{pid} (no such process)")),
                }
            }
            Target::Name(pat) => {
                let mut hit = false;
                for pr in &procs {
                    if name_matches(pr, &pat, &opts) {
                        matched.entry(pr.pid).or_insert_with(|| pr.clone());
                        hit = true;
                    }
                }
                if !hit {
                    unresolved.push(format!("{pat} (no match)"));
                }
            }
        }
    }

    // never target ourselves
    matched.remove(&me);

    // user filter
    if let Some(u) = &opts.user {
        matched.retain(|_, pr| &pr.user == u);
    }

    for u in &unresolved {
        eprintln!("{} {}", p.yellow("•"), p.dim(u));
    }

    if matched.is_empty() {
        if unresolved.is_empty() {
            println!("{} Nothing matched.", p.green("✓"));
        }
        process::exit(1);
    }

    let mut list: Vec<Proc> = matched.into_values().collect();
    list.sort_by_key(|x| x.pid);

    let word = if list.len() == 1 { "process" } else { "processes" };

    // -l: just show the matches and stop (a built-in pgrep).
    if opts.list {
        println!(
            "{} {} {}:\n",
            p.yellow("●"),
            p.bold(&list.len().to_string()),
            word
        );
        print_table(&list, &opts.columns, &p);
        return;
    }

    println!(
        "{} {} {} matched:\n",
        p.yellow("●"),
        p.bold(&list.len().to_string()),
        word
    );
    print_table(&list, &opts.columns, &p);
    println!();

    // Confirm unless -y. In a non-interactive shell, require -y for safety.
    if !opts.yes {
        if unsafe { libc::isatty(0) } != 1 {
            eprintln!(
                "{} refusing to kill {} {} without confirmation. Re-run with {}.",
                p.red("✗"),
                list.len(),
                word,
                p.bold("-y")
            );
            process::exit(1);
        }
        let prompt = format!(
            "{} {} {} {}? [{}/{}] ",
            p.yellow("Send"),
            p.bold(&signal_name(opts.signal)),
            p.yellow("to"),
            if list.len() == 1 { "it".into() } else { format!("all {}", list.len()) },
            p.bold("y"),
            p.bold("N")
        );
        if !confirm(&prompt) {
            println!("{}", p.dim("Left them alone."));
            return;
        }
    }

    let mut killed = 0;
    let mut failed = 0;
    for pr in &list {
        let rc = unsafe { libc::kill(pr.pid, opts.signal) };
        if rc == 0 {
            println!(
                "  {} {} {}",
                p.green("✓"),
                p.bold(&pr.name),
                p.dim(&format!("(pid {})", pr.pid))
            );
            killed += 1;
        } else {
            let err = io::Error::last_os_error();
            let why = match err.raw_os_error() {
                Some(libc::EPERM) => "permission denied — try sudo".to_string(),
                Some(libc::ESRCH) => "already gone".to_string(),
                _ => err.to_string(),
            };
            println!(
                "  {} pid {}: {}",
                p.red("✗"),
                pr.pid,
                why
            );
            failed += 1;
        }
    }

    let _ = killed;
    if failed > 0 {
        process::exit(1);
    }
}
