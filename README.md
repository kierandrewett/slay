# slay

> A modern `pkill`. Terminate processes by name, PID, or **port**.

`pkill` is great until you're staring at `EADDRINUSE` and have to go fishing for
whatever's squatting on a port. `slay` folds that in: any target starting with
`:` is a port, so `slay :8080` finds and kills whatever's listening there — and
everything else works like a friendlier, safer `pkill`.

```
$ slay :3000
● 1 process matched:

      PID  USER     PORT  PROCESS  COMMAND
   48213  kieran    3000  node     node /home/kieran/dev/app/server.js

Send SIGTERM to it? [y/N] y
  ✓ node (pid 48213)
```

## Why it's nicer than `pkill`

- **Kill by port.** `slay :8080` — the thing `pkill` can't do.
- **One verb, three targets.** Names, PIDs, and ports, mixed freely:
  `slay :3000 vite 4123`.
- **Shows you first, asks before killing.** No more `pkill -f` regret. Skip the
  prompt with `-y`; preview with `-n`.
- **Safe in scripts.** Refuses to kill without `-y` when there's no terminal.
- **Tiny & fast.** A single Rust binary, one dependency (`libc`), reads
  everything straight from `/proc`.

## Install

With Cargo:

```bash
cargo install --git https://github.com/kierandrewett/slay
```

Or from a clone:

```bash
git clone https://github.com/kierandrewett/slay
cargo install --path slay
```

Linux only (it reads `/proc`).

## Usage

```
slay <target>...          Kill matching processes (asks first)
slay :8080                Kill whatever is listening on port 8080
slay node                 Kill processes whose name contains "node"
slay 4123                 Kill PID 4123
slay -l                   List everything listening on a TCP port
```

A target is classified automatically:

| You type    | Means                                            |
| ----------- | ------------------------------------------------ |
| `:8080`     | Port — kill whoever listens on it (TCP or UDP)   |
| `4123`      | PID — an exact process id                          |
| `vite`      | Name — substring match on the process name        |

### Options

| Flag                  | What it does                                          |
| --------------------- | ---------------------------------------------------- |
| `-s, --signal <SIG>`  | Signal to send (name or number); default `TERM`      |
| `-9`                  | Shorthand for `--signal KILL`                        |
| `-f, --full`          | Match against the full command line, not just name   |
| `-x, --exact`         | Require an exact name match                           |
| `-i, --ignore-case`   | Case-insensitive name matching                       |
| `-u, --user <USER>`   | Only match processes owned by USER                   |
| `-y, --yes`           | Don't ask for confirmation                           |
| `-n, --dry-run`       | Show what would be killed, kill nothing              |
| `-l, --list`          | List all listening TCP ports                          |
| `-h, --help`          | Show help                                             |
| `-V, --version`       | Print version                                         |

Short flags bundle: `slay -9y node` force-kills every `node` with no prompt.

### Examples

```bash
slay :5432           # who's squatting on Postgres' port? kill it
slay -9 -y node      # force-kill every node process, no questions
slay -f vite -n      # preview every process whose cmdline mentions vite
slay :3000 :3001     # clear out a couple of dev ports at once
```

When a kill reports `permission denied`, the process belongs to another user —
re-run with `sudo`.

## License

MIT
