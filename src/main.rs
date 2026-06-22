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
    dry_run: bool,
    user: Option<String>,
    targets: Vec<String>,
    list: bool,
    help: bool,
    version: bool,
}

fn print_help() {
    let p = Paint { on: use_color() };
    let b = |s: &str| p.bold(s);
    println!(
        "{name} — a modern pkill. Terminate processes by name, PID, or port.

{usage}
  slay <target>...          Kill matching processes (asks first)
  slay :8080                Kill whatever is listening on port 8080
  slay node                 Kill processes whose name contains \"node\"
  slay 4123                 Kill PID 4123
  slay -l                   List everything listening on a TCP port

You can mix targets:  slay :3000 node 4123

{opts}
  -s, --signal <SIG>   Signal to send (name or number); default TERM
  -9                   Shorthand for --signal KILL
  -f, --full           Match against the full command line, not just the name
  -x, --exact          Require an exact name match
  -i, --ignore-case    Case-insensitive name matching
  -u, --user <USER>    Only match processes owned by USER
  -y, --yes            Don't ask for confirmation
  -n, --dry-run        Show what would be killed, but kill nothing
  -l, --list           List all listening TCP ports
  -h, --help           Show this help
  -V, --version        Print version

{ex}
  slay :5432           # who's squatting on Postgres' port? kill it
  slay -9 -y node      # force-kill every node process, no questions
  slay -f vite -n      # preview every process whose cmdline mentions vite",
        name = b("slay"),
        usage = b("USAGE"),
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
        dry_run: false,
        user: None,
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
            "-n" | "--dry-run" => o.dry_run = true,
            "-l" | "--list" => o.list = true,
            "-9" => o.signal = libc::SIGKILL,
            "-s" | "--signal" => {
                let v = args.next().ok_or("--signal needs a value")?;
                o.signal = parse_signal(&v)?;
            }
            "-u" | "--user" => {
                o.user = Some(args.next().ok_or("--user needs a value")?);
            }
            s if s.starts_with("--signal=") => {
                o.signal = parse_signal(&s["--signal=".len()..])?;
            }
            s if s.starts_with("--user=") => {
                o.user = Some(s["--user=".len()..].to_string());
            }
            // bundled short flags like -9y, -fx, -ny
            s if s.starts_with('-') && s.len() > 1 && !s.starts_with("--") => {
                for ch in s[1..].chars() {
                    match ch {
                        'f' => o.full = true,
                        'x' => o.exact = true,
                        'i' => o.ignore_case = true,
                        'y' => o.yes = true,
                        'n' => o.dry_run = true,
                        'l' => o.list = true,
                        '9' => o.signal = libc::SIGKILL,
                        'h' => o.help = true,
                        other => return Err(format!("unknown flag -{other}")),
                    }
                }
            }
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

// ─── rendering ──────────────────────────────────────────────────────────────
fn print_table(procs: &[Proc], p: &Paint) {
    let pid_w = procs.iter().map(|x| x.pid.to_string().len()).max().unwrap_or(3).max(3);
    let user_w = procs.iter().map(|x| x.user.len()).max().unwrap_or(4).max(4);
    let port_w = procs
        .iter()
        .map(|x| x.port.map(|v| v.to_string().len()).unwrap_or(1))
        .max()
        .unwrap_or(4)
        .max(4);
    let name_w = procs.iter().map(|x| x.name.len()).max().unwrap_or(7).max(7);

    println!(
        "  {}  {}  {}  {}  {}",
        p.bold(&format!("{:>pid_w$}", "PID")),
        p.bold(&format!("{:<user_w$}", "USER")),
        p.bold(&format!("{:>port_w$}", "PORT")),
        p.bold(&format!("{:<name_w$}", "PROCESS")),
        p.bold("COMMAND"),
    );
    for x in procs {
        let port = x.port.map(|v| v.to_string()).unwrap_or_else(|| "-".into());
        println!(
            "  {}  {}  {}  {}  {}",
            p.dim(&format!("{:>pid_w$}", x.pid)),
            p.dim(&format!("{:<user_w$}", x.user)),
            p.cyan(&format!("{:>port_w$}", port)),
            p.magenta(&format!("{:<name_w$}", x.name)),
            p.dim(&x.cmdline),
        );
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

// ─── listing ────────────────────────────────────────────────────────────────
fn list_ports(procs: &[Proc], p: &Paint) {
    // Build inode→port for all TCP listeners, then map to pids.
    let mut inode_port: HashMap<u64, u16> = HashMap::new();
    for path in ["/proc/net/tcp", "/proc/net/tcp6"] {
        if let Ok(txt) = fs::read_to_string(path) {
            for line in txt.lines().skip(1) {
                let f: Vec<&str> = line.split_whitespace().collect();
                if f.len() < 10 || f[3] != "0A" {
                    continue;
                }
                if let (Some(ph), Ok(ino)) = (f[1].rsplit(':').next(), f[9].parse::<u64>()) {
                    if let Ok(port) = u16::from_str_radix(ph, 16) {
                        inode_port.insert(ino, port);
                    }
                }
            }
        }
    }
    let by_pid: HashMap<i32, &Proc> = procs.iter().map(|x| (x.pid, x)).collect();
    let mut rows: Vec<Proc> = Vec::new();
    if let Ok(entries) = fs::read_dir("/proc") {
        for ent in entries.flatten() {
            let pid: i32 = match ent.file_name().to_str().and_then(|s| s.parse().ok()) {
                Some(v) => v,
                None => continue,
            };
            let fds = match fs::read_dir(format!("/proc/{pid}/fd")) {
                Ok(f) => f,
                Err(_) => continue,
            };
            for fd in fds.flatten() {
                if let Ok(link) = fs::read_link(fd.path()) {
                    if let Some(port) = link
                        .to_str()
                        .and_then(|s| s.strip_prefix("socket:["))
                        .and_then(|s| s.strip_suffix(']'))
                        .and_then(|s| s.parse::<u64>().ok())
                        .and_then(|ino| inode_port.get(&ino).copied())
                    {
                        if let Some(pr) = by_pid.get(&pid) {
                            let mut c = (*pr).clone();
                            c.port = Some(port);
                            rows.push(c);
                        }
                    }
                }
            }
        }
    }
    rows.sort_by_key(|r| (r.port.unwrap_or(0), r.pid));
    rows.dedup_by_key(|r| (r.port.unwrap_or(0), r.pid));
    if rows.is_empty() {
        println!("{} Nothing is listening on a TCP port.", p.green("✓"));
        return;
    }
    print_table(&rows, p);
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

    if opts.list {
        list_ports(&procs, &p);
        return;
    }

    if opts.targets.is_empty() {
        eprintln!("slay: no targets. Give a name, PID, or :port — or `slay -l` to list ports.");
        eprintln!("Try `slay --help`.");
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
    println!(
        "{} {} {} matched:\n",
        p.yellow("●"),
        p.bold(&list.len().to_string()),
        word
    );
    print_table(&list, &p);
    println!();

    if opts.dry_run {
        println!(
            "{} dry run — would send {} to {} {}.",
            p.dim("·"),
            p.bold(&signal_name(opts.signal)),
            list.len(),
            word
        );
        return;
    }

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
