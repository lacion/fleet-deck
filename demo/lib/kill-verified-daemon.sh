#!/bin/bash
# demo/lib/kill-verified-daemon.sh — identity-bound daemon stop for the
# acceptance/demo scripts.
#
# Source this file; it defines stop_pidfile_daemon. Callers must set
# FLEETDECK_ROOT (the demo scripts already do).
#
# Why this exists (BUG-008): the demo reset used to run
# `kill "$(cat <home>/fleetd.pid)"` on whatever bytes the pidfile held. A
# legacy plain-PID pidfile that survives a crash/reboot can name a PID the OS
# has since recycled for an unrelated process, and the acceptance gate would
# SIGTERM that stranger mid-work. Production never does this: takeover.mjs
# signals a daemon only after the pidfile PID matches AND the live process
# still carries a fleetd /proc shape (verifyDaemonPid). This helper applies
# that same gate to the demo reset, and additionally cross-checks the pidfile
# PID against /health.pid when the pidfile records a port — the strict-record
# half of the production contract. A legacy plain-PID record carries no port,
# so it cannot be positively identified and is treated as unowned.
#
# stop_pidfile_daemon <home> [waitSeconds]
#   0 — a verified fleetd was SIGTERMed, or nothing live was recorded.
#   1 — the pidfile holds an UNVERIFIED but LIVE pid (recycled or shapeless):
#       nothing was signalled; the caller must abort rather than fall through
#       to a port-wide kill.
stop_pidfile_daemon() {
  local home="$1" wait_s="${2:-5}"
  local pidfile="$home/fleetd.pid"
  [ -f "$pidfile" ] || return 0

  # One node process does the whole gate and prints a shell-safe verdict line:
  #   "signal <pid>"  — identity proven (strict record, /health confirms the
  #                     pid when a port is recorded, fleetd /proc shape)
  #   "live <pid>"    — pid is alive but identity could NOT be proven
  #   "dead"          — pid recorded but no longer running: nothing to do
  #   "none"          — no/empty/unparseable pidfile
  local verdict
  verdict=$(node --input-type=module - "$FLEETDECK_ROOT" "$pidfile" <<'EOF'
import { readFileSync } from 'node:fs';
import path from 'node:path';

const [repoRoot, pidfile] = process.argv.slice(2);
const takeover = await import(path.join(repoRoot, 'scripts/fleetd/takeover.mjs'));

let text = '';
try { text = readFileSync(pidfile, 'utf8'); } catch { console.log('none'); process.exit(0); }
const record = takeover.pidRecord(text);
if (!record) { console.log('none'); process.exit(0); }

const home = path.dirname(pidfile);
const live = (pid) => { try { process.kill(pid, 0); return true; } catch { return false; } };

if (record.port == null) {
  // Legacy plain-PID record: no port to cross-check against /health, so the
  // pid can never be positively identified as ours (BUG-008). Unowned.
  console.log(live(record.pid) ? `live ${record.pid}` : 'dead');
  process.exit(0);
}

let healthPid = null;
try {
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(), 1000);
  const res = await fetch(`http://127.0.0.1:${record.port}/health`, { signal: ctl.signal });
  clearTimeout(timer);
  healthPid = (await res.json())?.pid ?? null;
} catch { /* no listener there — pidfile is stale */ }

const proven = healthPid === record.pid && takeover.verifyDaemonPid(record.pid, home);
if (proven) console.log(`signal ${record.pid}`);
else console.log(live(record.pid) ? `live ${record.pid}` : 'dead');
EOF
  ) || verdict="none"

  case "$verdict" in
    "signal "*)
      local pid="${verdict#signal }"
      kill "$pid" 2>/dev/null || true
      local i steps=$(( wait_s * 10 ))
      for (( i = 0; i < steps; i++ )); do
        kill -0 "$pid" 2>/dev/null || return 0
        sleep 0.1
      done
      # Wedged daemon that ignored SIGTERM. No SIGKILL escalation — same
      # contract as production terminateDaemon: a live stale daemon is safer
      # than a force-killed one.
      return 0
      ;;
    "live "*)
      echo "reset: $pidfile names pid ${verdict#live } but its fleetd identity cannot be proven — leaving the live process alone" >&2
      return 1
      ;;
    *)
      return 0
      ;;
  esac
}
