#!/usr/bin/env node
// Spike only (D8). The TypeScript Agent SDK starts this file as its
// `pathToClaudeCodeExecutable`. It starts the real `claude` CLI, passes stdin and
// stdout through unchanged, and appends every line to the log file in
// $FD_TEE_LOG as a fixture record (`spawn`, `in`, `out`, `raw`, `exit`).
import { spawn } from "node:child_process";
import { appendFileSync } from "node:fs";
import { createInterface } from "node:readline";

const log = process.env.FD_TEE_LOG;
const real = process.env.FD_REAL_CLAUDE || "claude";
const args = process.argv.slice(2);

function record(rec) {
  if (log) appendFileSync(log, JSON.stringify(rec) + "\n");
}

function parsed(dir, line) {
  if (line.trim() === "") return;
  try {
    record({ dir, frame: JSON.parse(line) });
  } catch {
    record({ dir: "raw", line });
  }
}

record({ dir: "spawn", args });
const child = spawn(real, args, { stdio: ["pipe", "pipe", "inherit"] });

createInterface({ input: process.stdin }).on("line", (line) => {
  parsed("in", line);
  child.stdin.write(line + "\n");
});
process.stdin.on("end", () => child.stdin.end());

createInterface({ input: child.stdout }).on("line", (line) => {
  parsed("out", line);
  process.stdout.write(line + "\n");
});

for (const sig of ["SIGTERM", "SIGINT"]) {
  process.on(sig, () => child.kill(sig));
}
child.on("exit", (code, signal) => {
  record({ dir: "exit", code: signal ? null : code });
  process.exit(code ?? 1);
});
