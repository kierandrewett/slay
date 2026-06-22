# portend

> Foretell what's lurking on a port, and end it.

A tiny, zero-dependency CLI for the eternal `EADDRINUSE` dance. Point it at a
port to see exactly what's holding it — command, PID, user, full invocation —
then kill it right there. No more `lsof -i :3000 | awk ... | xargs kill`.

```
$ portend 3000
● Port 3000 is held by 1 process:

  node (pid 48213, user kieran)
    node /home/kieran/dev/app/server.js

Kill it? [y/N/9 for force] y

  ✓ Sent SIGTERM to node (pid 48213)
```

## Install

```bash
npm install -g portend
```

Or run it straight from the repo:

```bash
git clone https://github.com/kierandrewett/portend.git
npm install -g ./portend
```

Requires Node 16+ and `lsof` (preinstalled on macOS and most Linux distros).

## Usage

```
portend <port>            Show what's on a port, then offer to kill it
portend <port> -k         Kill it without asking (SIGTERM)
portend <port> -9         Force-kill without asking (SIGKILL)
portend                   List every listening port
portend -l                List every listening port
```

### Options

| Flag             | What it does                                  |
| ---------------- | --------------------------------------------- |
| `-k`, `--kill`   | Terminate the process(es) without confirming  |
| `-9`, `--force`  | Force-kill (SIGKILL) without confirming        |
| `-l`, `--list`   | List all listening ports                       |
| `-h`, `--help`   | Show help                                       |
| `-v`, `--version`| Print version                                  |

### Examples

```bash
portend 3000      # who's hogging my dev server port?
portend 8080 -9   # nuke it from orbit
portend           # what am I even running right now?
```

When a kill fails with `permission denied`, re-run with `sudo` — the process
belongs to another user.

## Why "portend"?

A portend foretells what's coming. This one foretells what's *already there*,
sitting on your port — and then puts an `end` to it.

## License

MIT
