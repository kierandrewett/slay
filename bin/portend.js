#!/usr/bin/env node
"use strict";

/**
 * portend — foretell what's lurking on a port, and end it.
 *
 * Zero-dependency CLI. Works on Linux & macOS (uses `lsof`).
 */

const { execFileSync } = require("node:child_process");
const readline = require("node:readline");

// ─── tiny ansi helpers ──────────────────────────────────────────────────────
const tty = process.stdout.isTTY;
const paint = (code) => (s) => (tty ? `\x1b[${code}m${s}\x1b[0m` : String(s));
const c = {
  bold: paint("1"),
  dim: paint("2"),
  red: paint("31"),
  green: paint("32"),
  yellow: paint("33"),
  blue: paint("34"),
  magenta: paint("35"),
  cyan: paint("36"),
};

const VERSION = "1.0.0";

function usage() {
  return `${c.bold("portend")} — foretell what's lurking on a port, and end it.

${c.bold("USAGE")}
  portend <port>            Show what's on a port, then offer to kill it
  portend <port> -k         Kill it without asking (SIGTERM)
  portend <port> -9         Kill it forcefully (SIGKILL), no prompt
  portend                   List every listening port
  portend -l                List every listening port

${c.bold("OPTIONS")}
  -k, --kill     Terminate the process(es) without confirmation (SIGTERM)
  -9, --force    Force-kill without confirmation (SIGKILL)
  -l, --list     List all listening ports
  -h, --help     Show this help
  -v, --version  Print version

${c.bold("EXAMPLES")}
  portend 3000             Who's hogging my dev server port?
  portend 8080 -9          Nuke it from orbit
  portend                  What am I even running right now?`;
}

// ─── argument parsing ───────────────────────────────────────────────────────
function parseArgs(argv) {
  const opts = { kill: false, force: false, list: false, help: false, version: false, port: null };
  for (const a of argv) {
    if (a === "-h" || a === "--help") opts.help = true;
    else if (a === "-v" || a === "--version") opts.version = true;
    else if (a === "-k" || a === "--kill") opts.kill = true;
    else if (a === "-9" || a === "--force") opts.force = true;
    else if (a === "-l" || a === "--list") opts.list = true;
    else if (/^\d+$/.test(a)) opts.port = Number(a);
    else {
      console.error(c.red(`Unknown argument: ${a}`));
      console.error(`Try ${c.bold("portend --help")}.`);
      process.exit(2);
    }
  }
  return opts;
}

// ─── lookups via lsof ───────────────────────────────────────────────────────
function lsof(args) {
  try {
    return execFileSync("lsof", args, { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] });
  } catch (err) {
    // lsof exits non-zero when nothing matches — that's not an error for us.
    if (err.status === 1 && !err.stdout) return "";
    if (err.code === "ENOENT") {
      console.error(c.red("portend needs `lsof`, which wasn't found on your PATH."));
      process.exit(127);
    }
    return err.stdout || "";
  }
}

// Parse lsof -F machine-readable output into per-process records.
function parseLsofF(out) {
  const procs = new Map();
  let cur = null;
  for (const line of out.split("\n")) {
    if (!line) continue;
    const tag = line[0];
    const val = line.slice(1);
    if (tag === "p") {
      cur = { pid: Number(val), command: "", user: "", ports: new Set() };
      procs.set(val, cur);
    } else if (!cur) {
      continue;
    } else if (tag === "c") {
      cur.command = val;
    } else if (tag === "L") {
      cur.user = val;
    } else if (tag === "n") {
      // name field, e.g. *:3000, 127.0.0.1:8080, [::1]:5432
      const m = val.match(/:(\d+)(?:->.*)?$/);
      if (m && !val.includes("->")) cur.ports.add(Number(m[1]));
    }
  }
  return [...procs.values()];
}

function processesOnPort(port) {
  const out = lsof(["-nP", "-FpcLn", `-iTCP:${port}`, "-sTCP:LISTEN"]);
  let procs = parseLsofF(out);
  // Also catch UDP listeners on the same port.
  const udp = parseLsofF(lsof(["-nP", "-FpcLn", `-iUDP:${port}`]));
  const seen = new Set(procs.map((p) => p.pid));
  for (const p of udp) if (!seen.has(p.pid)) procs.push(p);
  return procs;
}

function allListeners() {
  const out = lsof(["-nP", "-FpcLn", "-iTCP", "-sTCP:LISTEN"]);
  const procs = parseLsofF(out).filter((p) => p.ports.size > 0);
  // Flatten to (port, proc) rows.
  const rows = [];
  for (const p of procs) for (const port of p.ports) rows.push({ port, ...p });
  rows.sort((a, b) => a.port - b.port);
  return rows;
}

function fullCommand(pid) {
  try {
    const out = execFileSync("ps", ["-p", String(pid), "-o", "args="], { encoding: "utf8" }).trim();
    return out || null;
  } catch {
    return null;
  }
}

// ─── rendering ──────────────────────────────────────────────────────────────
function describe(port, procs) {
  if (procs.length === 0) {
    console.log(`${c.green("✓")} Nothing is listening on port ${c.bold(port)}. ${c.dim("All clear.")}`);
    return;
  }
  const word = procs.length === 1 ? "process" : "processes";
  console.log(`${c.yellow("●")} Port ${c.bold(c.cyan(port))} is held by ${c.bold(procs.length)} ${word}:\n`);
  for (const p of procs) {
    const cmd = fullCommand(p.pid) || p.command;
    console.log(`  ${c.bold(c.magenta(p.command))} ${c.dim("(pid " + p.pid + ", user " + p.user + ")")}`);
    console.log(`    ${c.dim(cmd)}\n`);
  }
}

function ask(question) {
  const rl = readline.createInterface({ input: process.stdin, output: process.stdout });
  return new Promise((resolve) => rl.question(question, (a) => { rl.close(); resolve(a); }));
}

function kill(procs, signal) {
  const sigName = signal === "SIGKILL" ? "SIGKILL (-9)" : "SIGTERM";
  let ok = 0;
  for (const p of procs) {
    try {
      process.kill(p.pid, signal);
      console.log(`  ${c.green("✓")} Sent ${sigName} to ${c.bold(p.command)} ${c.dim("(pid " + p.pid + ")")}`);
      ok++;
    } catch (err) {
      const why = err.code === "EPERM" ? "permission denied — try with sudo" : err.message;
      console.log(`  ${c.red("✗")} Could not kill pid ${p.pid}: ${why}`);
    }
  }
  return ok;
}

// ─── main ───────────────────────────────────────────────────────────────────
async function main() {
  const opts = parseArgs(process.argv.slice(2));

  if (opts.help) { console.log(usage()); return; }
  if (opts.version) { console.log(`portend ${VERSION}`); return; }

  if (opts.list || opts.port === null) {
    const rows = allListeners();
    if (rows.length === 0) {
      console.log(`${c.green("✓")} Nothing is listening for TCP connections right now.`);
      return;
    }
    console.log(c.bold(`PORT     PID      USER         COMMAND`));
    for (const r of rows) {
      const port = String(r.port).padEnd(8);
      const pid = String(r.pid).padEnd(8);
      const user = String(r.user).padEnd(12);
      console.log(`${c.cyan(port)} ${c.dim(pid)} ${c.dim(user)} ${c.magenta(r.command)}`);
    }
    return;
  }

  const port = opts.port;
  const procs = processesOnPort(port);
  describe(port, procs);
  if (procs.length === 0) return;

  if (opts.force || opts.kill) {
    console.log();
    kill(procs, opts.force ? "SIGKILL" : "SIGTERM");
    return;
  }

  if (!process.stdin.isTTY) {
    console.log(c.dim(`\nRe-run with ${c.bold("-k")} to terminate or ${c.bold("-9")} to force-kill.`));
    return;
  }

  const answer = (await ask(`\n${c.yellow("Kill")} ${procs.length === 1 ? "it" : "them"}? [${c.bold("y")}/${c.bold("N")}/${c.bold("9")} for force] `)).trim().toLowerCase();
  if (answer === "9") { console.log(); kill(procs, "SIGKILL"); }
  else if (answer === "y" || answer === "yes") { console.log(); kill(procs, "SIGTERM"); }
  else console.log(c.dim("Left it alone."));
}

main().catch((err) => {
  console.error(c.red(`portend: ${err.message}`));
  process.exit(1);
});
