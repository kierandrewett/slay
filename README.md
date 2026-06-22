# slay

> A better `pkill`. Find and kill processes, then get out of your way.

`pkill` works, but it's fiddly: you can never remember whether you need `-f`,
it kills silently with no idea what you actually hit, and it can't target a
port. `slay` fixes the ergonomics — it **shows you what matched and asks before
killing**, takes a name, a PID, *or* a port as a target, and ships a built-in
`pgrep` so the same command can just list.

```
$ slay node
● 2 processes matched:

      PID  USER     PORT  PROCESS  COMMAND
    48213  kieran      -  node     node /home/kieran/dev/app/server.js
    48999  kieran      -  node     node /home/kieran/dev/api/worker.js

Send SIGTERM to all 2? [y/N] y
  ✓ node (pid 48213)
  ✓ node (pid 48999)
```

## What makes it better than `pkill`

- **Shows you first, asks before killing.** No more silent `pkill -f` regret.
  Skip the prompt with `-y`.
- **List or kill with one command.** `slay -l node` lists matches (a built-in
  `pgrep`); drop the `-l` and it kills them.
- **Targets are smart.** Pass a name, a PID, or a port — `slay` figures out
  which from how it looks, and you can mix them.
- **Safe in scripts.** Refuses to kill without `-y` when there's no terminal.
- **Tiny & fast.** A single Rust binary, one dependency (`libc`), reading
  straight from `/proc`.

## Targets

A target is matched by name, PID, or port — whichever it looks like:

| You type    | Matched as | Means                                            |
| ----------- | ---------- | ------------------------------------------------ |
| `firefox`   | name       | substring of the process name (`-f` for cmdline) |
| `4123`      | PID        | an exact process id                              |
| `:8080`     | port       | whoever is listening on it (TCP or UDP)          |

```bash
slay firefox 4123 :8080   # mix freely
```

The port target is the bit `pkill` can't do — handy for the `EADDRINUSE`
dance — but it's just one of the three ways to name what you want gone.

## Install

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
slay <target>...          Kill matching processes (shows them, then asks)
slay -l <target>...       List matching processes instead of killing
slay -l                   List every process
slay --ports              Show everything currently listening on a TCP port
```

### Options

| Flag                  | What it does                                          |
| --------------------- | ---------------------------------------------------- |
| `-s, --signal <SIG>`  | Signal to send (name or number); default `TERM`      |
| `-9`                  | Shorthand for `--signal KILL`                        |
| `-f, --full`          | Match the name against the full command line         |
| `-x, --exact`         | Require an exact name match                           |
| `-i, --ignore-case`   | Case-insensitive name matching                       |
| `-u, --user <USER>`   | Only match processes owned by USER                   |
| `-y, --yes`           | Don't ask for confirmation                           |
| `-l, --list`          | List matches instead of killing (a built-in pgrep)   |
| `--ports`             | Show everything listening on a TCP port              |
| `-h, --help`          | Show help                                             |
| `-V, --version`       | Print version                                         |

Short flags bundle: `slay -9y firefox` force-kills with no prompt.

### Examples

```bash
slay node            # kill every process named like node (asks first)
slay -9 -y firefox   # force-kill firefox, no questions
slay -l vite -f      # list every process whose cmdline mentions vite
slay :5432           # something on Postgres' port? kill it
slay --ports         # what am I even listening on right now?
```

When a kill reports `permission denied`, the process belongs to another user —
re-run with `sudo`.

## License

MIT
