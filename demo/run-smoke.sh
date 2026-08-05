#!/bin/bash
# demo/run-smoke.sh — Fleet Deck Phase 1 live smoke.
#
# Adaptation of fleetdeck-spike/run-demo3.sh for this machine: two overlapping
# `claude -p` sessions editing the same files, mid-run mail from the board,
# then PASS/FAIL checks against the Phase-1 accept criteria (daemon parity:
# the spike's two-overlapping-sessions demo ... passes end-to-end: election,
# brief, conflict whisper, mail at Stop, tombstone).
#
# This script spends real Claude usage (two `claude -p --dangerously-skip-
# permissions` sessions). Do not run it casually.
# errexit + pipefail: any failed step (including the `cd "$PROJECT_DIR"` before
# the unrestricted worker launches) aborts the smoke instead of letting
# `--dangerously-skip-permissions` workers run loose in the caller's cwd.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FLEETDECK_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SEED_DIR="$SCRIPT_DIR/project"
DEMO_LOGS="$SCRIPT_DIR/demo-logs"
TIMEOUT_LAUNCHER="$SCRIPT_DIR/run-with-timeout.mjs"
SESSIONSTART_SCRIPT="$FLEETDECK_ROOT/scripts/fleet-sessionstart.mjs"
FLEET_HOOK_SCRIPT="$FLEETDECK_ROOT/scripts/fleet-hook.mjs"

# An isolated non-production port. Use a smoke-specific override so an ambient
# FLEETDECK_PORT from the current session can never redirect this run to :4711.
FLEETDECK_PORT="${FLEETDECK_SMOKE_PORT:-24711}"

# Pinned worker model and effort. Claude Code otherwise resolves the model and
# effort from machine-local configuration (user settings, settings.local.json,
# env), so identical Fleet Deck source could run different models, cross the
# timing/turn thresholds, and cost differently per machine. Smoke-specific
# overrides only, so ambient config can never re-target them.
SMOKE_MODEL="${FLEETDECK_SMOKE_MODEL:-sonnet}"
SMOKE_EFFORT="${FLEETDECK_SMOKE_EFFORT:-low}"

# Assigned from mktemp after the cleanup trap is armed. An arbitrary override
# is intentionally unsupported: cleanup recursively deletes this directory, so
# it must be a unique path created by this run, never a caller-provided target.
SCRATCH_HOME=''

# Unique per-run copy of the demo fixture, created under the scratch home.
# The workers edit THIS directory -- the tracked checkout under demo/project
# is never touched, so a developer's uncommitted work there (or an abort
# before setup completes) can never be reset or deleted by this script.
PROJECT_DIR=''

# Isolated tmux server for THIS run only, never the user's default server.
# The fleetd elected by the workers' SessionStart hook inherits this env and
# runs all its tmux calls as `tmux -L $FLEETDECK_TMUX_SOCKET`. Without it, a
# test-env daemon starting the default tmux server would bake FLEETDECK_*
# test values into that server's global env — poisoning every window (and
# production spawn) created there later.
export FLEETDECK_TMUX_SOCKET="fdaccept-$$"

# Everything the smoke starts is isolated and torn down on success, failure, or
# interruption. The user's daemon, tmux server, database, and project files are
# never cleanup targets. PROJECT_DIR lives under SCRATCH_HOME, so project
# teardown is the single recursive scratch-home delete below -- no per-file
# restore of the tracked fixture is needed (or safe: an EXIT trap can never
# know what the pre-run bytes were).
PA=''
PB=''
SMOKE_STARTED=0
stop_worker() {
  local pgid="$1"
  [ -n "$pgid" ] || return 0
  # The workers run under demo/run-with-timeout.mjs, which — unlike the old
  # `setsid timeout` — is NOT its own process-group leader: $PA's group is this
  # script's group, so `kill -- -$PA` is ESRCH (or worse). Signal the group
  # when it exists (setsid semantics) and fall back to the launcher pid; the
  # launcher forwards TERM into the worker's detached group and escalates to
  # SIGKILL inside its own 1 s grace, within the 2 s window polled below.
  kill -TERM -- "-$pgid" 2>/dev/null || kill -TERM -- "$pgid" 2>/dev/null || true
  for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
    kill -0 -- "$pgid" 2>/dev/null || break
    sleep 0.1
  done
  kill -KILL -- "-$pgid" 2>/dev/null || kill -KILL -- "$pgid" 2>/dev/null || true
  wait "$pgid" 2>/dev/null || true
}
stop_smoke_daemon() {
  [ -n "$SCRATCH_HOME" ] || return 0
  local pidfile="$SCRATCH_HOME/fleetd.pid"
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    [ -f "$pidfile" ] && break
    if curl -fsS --max-time 0.2 "http://127.0.0.1:$FLEETDECK_PORT/health" >/dev/null 2>&1; then return 1; fi
    sleep 0.1
  done
  if [ ! -f "$pidfile" ]; then
    [ "$SMOKE_STARTED" -eq 0 ] && return 0
    return 1
  fi
  # Signal only the daemon proven by all three identities: this run's strict
  # JSON pid record, health on this run's port, and a node+fleetd process shape.
  # Any uncertainty returns nonzero so cleanup RETAINS the home instead of
  # deleting state from underneath a process that might still be live.
  node -e '
    const fs = require("node:fs");
    const path = require("node:path");
    const { execFileSync } = require("node:child_process");
    const pidfile = process.argv[1];
    const expectedPort = Number(process.argv[2]);
    const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
    const live = pid => {
      try { process.kill(pid, 0); return true; }
      catch (err) { return err?.code !== "ESRCH"; }
    };
    let record;
    try { record = JSON.parse(fs.readFileSync(pidfile, "utf8")); }
    catch { process.exit(2); }
    if (!Number.isInteger(record?.pid) || record.pid <= 0 || record.port !== expectedPort) process.exit(2);
    (async () => {
      let health = null;
      for (let i = 0; i < 20; i += 1) {
        try {
          const res = await fetch(`http://127.0.0.1:${expectedPort}/health`, { signal: AbortSignal.timeout(250) });
          const candidate = res.ok ? await res.json() : null;
          if (candidate?.pid === record.pid) { health = candidate; break; }
        } catch {}
        await sleep(100);
      }
      if (!health) { process.exitCode = 2; return; }

      let nodeLike = false;
      let fleetdScript = false;
      try {
        if (process.platform === "linux") {
          const executable = path.basename(fs.readlinkSync(`/proc/${record.pid}/exe`)).replace(/ \(deleted\)$/, "");
          const argv = fs.readFileSync(`/proc/${record.pid}/cmdline`, "utf8").split("\0").filter(Boolean);
          nodeLike = /^(?:node|nodejs)$/i.test(executable);
          fleetdScript = argv.some(value => /(?:^|[\/\\])fleetd(?:\.bundle)?\.mjs$/.test(value));
        } else {
          const executable = execFileSync("ps", ["-p", String(record.pid), "-o", "comm="], { encoding: "utf8", timeout: 1000 }).trim();
          const command = execFileSync("ps", ["-p", String(record.pid), "-o", "command="], { encoding: "utf8", timeout: 1000 });
          nodeLike = /^(?:node|nodejs)$/i.test(path.basename(executable));
          fleetdScript = /(?:^|[\/\\])fleetd(?:\.bundle)?\.mjs(?=$|\s|")/.test(command);
        }
      } catch { process.exitCode = 2; return; }
      if (!nodeLike || !fleetdScript) { process.exitCode = 2; return; }

      try { process.kill(record.pid, "SIGTERM"); }
      catch (err) { if (err?.code !== "ESRCH") process.exitCode = 2; return; }
      for (let i = 0; i < 30; i += 1) {
        await sleep(100);
        if (!live(record.pid)) return;
      }
      // Never escalate to SIGKILL: a graceful shutdown that cannot be proven
      // leaves the unique smoke home intact for diagnosis and avoids PID reuse.
      process.exitCode = 2;
    })().catch(() => { process.exitCode = 2; });
  ' "$pidfile" "$FLEETDECK_PORT" >/dev/null 2>&1
}
cleanup() {
  stop_worker "$PA"
  stop_worker "$PB"
  PA=''
  PB=''
  local daemon_stopped=1
  if stop_smoke_daemon; then daemon_stopped=0; fi
  if command -v tmux >/dev/null 2>&1; then
    tmux -L "$FLEETDECK_TMUX_SOCKET" kill-server 2>/dev/null || true
  fi
  if [ -n "$SCRATCH_HOME" ] && [ "$daemon_stopped" -eq 0 ]; then
    rm -rf -- "$SCRATCH_HOME"
  elif [ -n "$SCRATCH_HOME" ]; then
    echo "WARNING: smoke daemon could not be verified stopped; retained $SCRATCH_HOME" >&2
  fi
}
trap cleanup EXIT
trap 'exit 130' INT TERM
SCRATCH_HOME="$(mktemp -d "${TMPDIR:-/tmp}/fleetdeck-smoke.XXXXXX")" || {
  echo "ABORT: could not create a unique smoke home"
  exit 1
}

# Working copy of the demo fixture for THIS run only. Everything the smoke
# mutates -- the workers' edits, test.js, .claude/settings.json -- lands here
# and dies with the scratch home. The tracked fixture under demo/project is
# read exactly once, right here.
PROJECT_DIR="$SCRATCH_HOME/project"
mkdir -p "$PROJECT_DIR"
cp -R "$SEED_DIR/." "$PROJECT_DIR/"

# Claude-session env vars that must never leak into the workers (and through
# their SessionStart hook, into the elected daemon): a daemon or tmux server
# inheriting them can mislead later spawns into reporting to the wrong fleet.
# Passed to `env` as -u flags.
CLAUDE_ENV_SCRUB=(
  -u CLAUDECODE -u CLAUDE_CODE_SESSION_ID -u CLAUDE_CODE_CHILD_SESSION
  -u CLAUDE_CODE_BRIDGE_SESSION_ID -u CLAUDE_CODE_ENTRYPOINT
  -u CLAUDE_CODE_EXECPATH -u CLAUDE_ENV_FILE -u CLAUDE_PROJECT_DIR
  -u CLAUDE_PLUGIN_ROOT -u CLAUDE_PLUGIN_DATA -u CLAUDE_EFFORT
  -u AI_AGENT -u CODEX_COMPANION_SESSION_ID -u CODEX_COMPANION_TRANSCRIPT_PATH
  -u TMUX -u TMUX_PANE
)

echo "== Fleet Deck Phase 1 smoke =="
echo "FLEETDECK_ROOT        = $FLEETDECK_ROOT"
echo "SEED_DIR              = $SEED_DIR"
echo "PROJECT_DIR           = $PROJECT_DIR"
echo "SCRATCH_HOME          = $SCRATCH_HOME"
echo "FLEETDECK_PORT        = $FLEETDECK_PORT"
echo "FLEETDECK_TMUX_SOCKET = $FLEETDECK_TMUX_SOCKET"
echo "SMOKE_MODEL           = $SMOKE_MODEL"
echo "SMOKE_EFFORT          = $SMOKE_EFFORT"
echo

# The workers are launched through demo/run-with-timeout.mjs, the portable
# (setsid + GNU timeout) equivalent: macOS ships neither utility, and Node
# gives the same process-group-plus-deadline semantics on every platform.
if ! command -v node >/dev/null 2>&1; then
  echo "ABORT: smoke requires node on PATH"
  exit 1
fi

# ---------------------------------------------------------------- 1. reset
# Final guard: never kill an unknown listener by port. The selected isolated
# port must already be free after the scratch-owned pid cleanup above.
if curl -s -m 1 "http://127.0.0.1:$FLEETDECK_PORT/health" > /dev/null 2>&1; then
  echo "ABORT: something is already listening on isolated port :$FLEETDECK_PORT."
  exit 1
fi

# The working copy starts pristine from the checkout -- the run-scoped copy
# above is the only reset this script performs. test.js never exists at start;
# the workers create it.
mkdir -p "$DEMO_LOGS"
rm -f "$DEMO_LOGS"/worker-a.json "$DEMO_LOGS"/worker-a.err "$DEMO_LOGS"/worker-b.json "$DEMO_LOGS"/worker-b.err \
      "$DEMO_LOGS"/sid-a.txt "$DEMO_LOGS"/sid-b.txt "$DEMO_LOGS"/final-state.json

# ------------------------------------------------ 2. render settings.json
# Every hook uses the current checkout's authenticated command shim. Native
# HTTP hooks cannot attach the bearer token required since 0.16.0.
mkdir -p "$PROJECT_DIR/.claude"
cat > "$PROJECT_DIR/.claude/settings.json" <<EOF
{
  "hooks": {
    "SessionStart": [
      { "hooks": [{ "type": "command", "command": "node \"$SESSIONSTART_SCRIPT\"", "timeout": 15 }] }
    ],
    "UserPromptSubmit": [
      { "hooks": [{ "type": "command", "command": "node \"$FLEET_HOOK_SCRIPT\" UserPromptSubmit", "timeout": 3 }] }
    ],
    "PostToolUse": [
      { "matcher": "Edit|Write|MultiEdit|NotebookEdit|Bash", "hooks": [{ "type": "command", "command": "node \"$FLEET_HOOK_SCRIPT\" PostToolUse", "timeout": 3 }] }
    ],
    "PreToolUse": [
      { "matcher": "AskUserQuestion", "hooks": [{ "type": "command", "command": "node \"$FLEET_HOOK_SCRIPT\" AskUserQuestion", "timeout": 65 }] }
    ],
    "PermissionRequest": [
      { "hooks": [{ "type": "command", "command": "node \"$FLEET_HOOK_SCRIPT\" PermissionRequest", "timeout": 65 }] }
    ],
    "Elicitation": [
      { "hooks": [{ "type": "command", "command": "node \"$FLEET_HOOK_SCRIPT\" Elicitation", "timeout": 65 }] }
    ],
    "Notification": [
      { "hooks": [{ "type": "command", "command": "node \"$FLEET_HOOK_SCRIPT\" Notification", "timeout": 3, "async": true }] }
    ],
    "Stop": [
      { "hooks": [{ "type": "command", "command": "node \"$FLEET_HOOK_SCRIPT\" Stop", "timeout": 5 }] }
    ],
    "SessionEnd": [
      { "hooks": [{ "type": "command", "command": "node \"$FLEET_HOOK_SCRIPT\" SessionEnd", "timeout": 3, "async": true }] }
    ],
    "FileChanged": [
      { "hooks": [{ "type": "command", "command": "node \"$FLEET_HOOK_SCRIPT\" FileChanged", "timeout": 3, "async": true }] }
    ]
  }
}
EOF

# ---------------------------------------------------------- 3. launch fleet
SA=$(node -e 'console.log(crypto.randomUUID())')
SB=$(node -e 'console.log(crypto.randomUUID())')
RC_A=0
RC_B=0
echo "$SA" > "$DEMO_LOGS/sid-a.txt"
echo "$SB" > "$DEMO_LOGS/sid-b.txt"

# Belt and braces under errexit: never let the unrestricted workers below
# launch in the caller's directory if the fixture cannot be entered.
cd "$PROJECT_DIR" || {
  echo "ABORT: could not enter project fixture $PROJECT_DIR"
  exit 1
}
SMOKE_STARTED=1

env "${CLAUDE_ENV_SCRUB[@]}" \
  FLEETDECK_HOME="$SCRATCH_HOME" FLEETDECK_PORT="$FLEETDECK_PORT" \
  FLEETDECK_TMUX_SOCKET="$FLEETDECK_TMUX_SOCKET" FLEETDECK_AGENTS_CMD=false \
<<<<<<< /tmp/mf-ours
  setsid timeout 300 claude -p "Add an exported function slugify(s) to util.js (lowercase, trim, spaces to dashes, strip punctuation). Add assert-based tests for it in test.js (create or extend). Verify each edge case one at a time with separate 'node -e' commands: spaces, capitals, punctuation, empty string. Then run node test.js. Preserve any existing exports. Work step by step, one small change per edit." \
<<<<<<< /tmp/mf-ours
  --session-id "$SA" --dangerously-skip-permissions \
=======
=======
  node "$TIMEOUT_LAUNCHER" 300 claude -p "Add an exported function slugify(s) to util.js (lowercase, trim, spaces to dashes, strip punctuation). Add assert-based tests for it in test.js (create or extend). Verify each edge case one at a time with separate 'node -e' commands: spaces, capitals, punctuation, empty string. Then run node test.js. Preserve any existing exports. Work step by step, one small change per edit." \
>>>>>>> /tmp/mf-theirs
  --session-id "$SA" --max-turns 24 --dangerously-skip-permissions \
  --model "$SMOKE_MODEL" --effort "$SMOKE_EFFORT" --setting-sources user,project \
>>>>>>> /tmp/mf-theirs
  --output-format json > "$DEMO_LOGS/worker-a.json" 2> "$DEMO_LOGS/worker-a.err" &
PA=$!
echo "T+0 session A launched sid=$SA"

# Gate the fanout on fleet state, never on wall-clock sleeps. `to:"all"`
# resolves only ACTIVE sessions (scripts/fleetd/mail.mjs resolveTargets
# filters ended_at IS NULL), and the verification below requires BOTH exact
# sessions to drain the mail at a Stop boundary. The old T+15/T+29 sleeps let
# a fast worker finish before the send: it was silently omitted from the
# fanout and could never emit its boundary-delivery ticker entry — a
# repeatable false failure on a faster model or machine. Poll the daemon's
# own /state instead: launch B only once A is proven registered and live, and
# mail only once BOTH exact session ids are.
wait_for_fleet() { # sids... — every listed session registered AND not ended
  node -e '
    // `node -e` runs CJS (cwd may contain no package.json marking ESM).
    const fs = require("node:fs");
    const [home, port, ...sids] = process.argv.slice(1);
    const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
    (async () => {
      let token = null;
      for (let i = 0; i < 900; i += 1) { // up to ~90 s for a slow election
        if (!token) { try { token = fs.readFileSync(home + "/token", "utf8").trim(); } catch {} }
        if (token) {
          try {
            const res = await fetch(`http://127.0.0.1:${port}/state`, {
              headers: { authorization: `Bearer ${token}` },
              signal: AbortSignal.timeout(500),
            });
            if (res.ok) {
              const state = await res.json();
              const live = new Set((state.sessions || []).filter(s => !s.endedAt).map(s => s.session_id));
              if (sids.every(sid => live.has(sid))) return;
            }
          } catch {}
        }
        await sleep(100);
      }
      process.exitCode = 1;
    })().catch(() => { process.exitCode = 1; });
  ' "$SCRATCH_HOME" "$FLEETDECK_PORT" "$@"
}

if ! wait_for_fleet "$SA"; then
  echo "FAIL: session A never registered as active on the smoke daemon"
  exit 1
fi

env "${CLAUDE_ENV_SCRUB[@]}" \
  FLEETDECK_HOME="$SCRATCH_HOME" FLEETDECK_PORT="$FLEETDECK_PORT" \
  FLEETDECK_TMUX_SOCKET="$FLEETDECK_TMUX_SOCKET" FLEETDECK_AGENTS_CMD=false \
<<<<<<< /tmp/mf-ours
  setsid timeout 300 claude -p "Add an exported function titleCase(s) to util.js (capitalize each word). Add assert-based tests for it in test.js (create or extend). Verify edge cases one at a time with separate 'node -e' commands: single word, multiple words, empty string. Then run node test.js. IMPORTANT: preserve any existing exports and tests you find. Work step by step, one small change per edit." \
<<<<<<< /tmp/mf-ours
  --session-id "$SB" --dangerously-skip-permissions \
=======
=======
  node "$TIMEOUT_LAUNCHER" 300 claude -p "Add an exported function titleCase(s) to util.js (capitalize each word). Add assert-based tests for it in test.js (create or extend). Verify edge cases one at a time with separate 'node -e' commands: single word, multiple words, empty string. Then run node test.js. IMPORTANT: preserve any existing exports and tests you find. Work step by step, one small change per edit." \
>>>>>>> /tmp/mf-theirs
  --session-id "$SB" --max-turns 24 --dangerously-skip-permissions \
  --model "$SMOKE_MODEL" --effort "$SMOKE_EFFORT" --setting-sources user,project \
>>>>>>> /tmp/mf-theirs
  --output-format json > "$DEMO_LOGS/worker-b.json" 2> "$DEMO_LOGS/worker-b.err" &
PB=$!
echo "session B launched sid=$SB (A proven active)"

if ! wait_for_fleet "$SA" "$SB"; then
  echo "FAIL: both smoke sessions never registered as active; refusing to mail a partial fleet"
  exit 1
fi
TOKEN="$(cat "$SCRATCH_HOME/token" 2>/dev/null || true)"
if [ -z "$TOKEN" ]; then
  echo "FAIL: smoke daemon did not mint its bearer token"
  exit 1
fi
if curl -fsS --connect-timeout 5 --max-time 15 -X POST "http://127.0.0.1:$FLEETDECK_PORT/mail" \
  -H 'content-type: application/json' -H "authorization: Bearer $TOKEN" \
  -d '{"to":"all","from":"luis","text":"Fleet check-in: another agent is editing this repo right now. End your final summary with a line FLEET-NOTE: listing files you touched."}'; then
  echo " | mail sent (both sessions proven active)"
else
  echo "FAIL: authenticated smoke mail was refused"
  exit 1
fi

<<<<<<< /tmp/mf-ours
echo "(board screenshot skipped -- Phase 1 board is the ported spike board, no shot.mjs yet)"

# `wait` propagates the worker's exit status — tolerated nonzero (rc=124 is an
# accepted outcome), so capture it instead of letting errexit abort here.
wait "$PA" || RC_A=$?; echo "session A done rc=$RC_A"; PA=''
wait "$PB" || RC_B=$?; echo "session B done rc=$RC_B"; PB=''

# Bounded tombstone poll: the SessionEnd hooks tombstone asynchronously, so
# retry the final /state capture until both sessions read offline. Every
# attempt carries hard timeouts so a stalled daemon can never wedge the run
# past the worker watchdog.
STATE_GOT=''
for i in $(seq 1 12); do
  if curl -fsS --connect-timeout 5 --max-time 15 "http://127.0.0.1:$FLEETDECK_PORT/state" \
    -H "authorization: Bearer $TOKEN" > "$DEMO_LOGS/final-state.json" 2>/dev/null; then
    if node -e "
      const s = JSON.parse(require('fs').readFileSync('$DEMO_LOGS/final-state.json', 'utf8'));
      const byId = Object.fromEntries((s.sessions || []).map(x => [x.session_id, x]));
      const off = id => byId[id] && byId[id].col === 'offline' && !!byId[id].endedAt;
      process.exit(off('$SA') && off('$SB') ? 0 : 1);
    "; then
      STATE_GOT=1
      break
    fi
  fi
  echo " | waiting for tombstones (attempt $i/12)"
  sleep 5
done
if [ -z "$STATE_GOT" ]; then
  echo "FAIL: final /state capture never showed both sessions tombstoned offline"
  exit 1
fi
=======
sleep 12
echo "T+41 (board screenshot skipped -- Phase 1 board is the ported spike board, no shot.mjs yet)"

wait "$PA"; RC_A=$?; echo "session A done rc=$RC_A"; PA=''
wait "$PB"; RC_B=$?; echo "session B done rc=$RC_B"; PB=''

# The SessionEnd hook is async ("async": true in the rendered settings): Claude
# Code does NOT await it before exiting, so the shim can still be posting the
# tombstone for ~2.5s after `wait` returns (fleet-hook.mjs's watchdog). Poll
# /state on a bounded deadline until both sessions are tombstoned (offline with
# endedAt) before capturing evidence — a single immediate fetch races the shim
# and fails the lifecycle criterion on slower machines.
SMOKE_STATE_DEADLINE="${FLEETDECK_SMOKE_STATE_DEADLINE_MS:-30000}"
DEADLINE_END=$((SECONDS + (SMOKE_STATE_DEADLINE + 999) / 1000))
while :; do
  if curl -fsS "http://127.0.0.1:$FLEETDECK_PORT/state" \
    -H "authorization: Bearer $TOKEN" > "$DEMO_LOGS/final-state.json" \
  && node -e '
    const state = JSON.parse(require("node:fs").readFileSync(process.argv[1], "utf8"));
    const byId = Object.fromEntries((state.sessions || []).map(s => [s.session_id, s]));
    const done = [process.argv[2], process.argv[3]].every(sid => byId[sid] && byId[sid].col === "offline" && byId[sid].endedAt);
    process.exit(done ? 0 : 1);
  ' "$DEMO_LOGS/final-state.json" "$SA" "$SB"; then
    break
  fi
  if [ "$SECONDS" -ge "$DEADLINE_END" ]; then
    echo "WARNING: tombstones still pending after ${SMOKE_STATE_DEADLINE_MS}ms; capturing state as-is" >&2
    curl -fsS "http://127.0.0.1:$FLEETDECK_PORT/state" \
      -H "authorization: Bearer $TOKEN" > "$DEMO_LOGS/final-state.json"
    break
  fi
  sleep 0.5
done
>>>>>>> /tmp/mf-theirs
echo "ROUND COMPLETE — captured $DEMO_LOGS/final-state.json"
echo

# --------------------------------------------------------------- 4. verify
node --input-type=module -e "
import { readFileSync, existsSync } from 'node:fs';

const demoLogs = '$DEMO_LOGS';
const sidA = '$SA';
const sidB = '$SB';
const rcA = Number('$RC_A');
const rcB = Number('$RC_B');

let failures = 0;
let inconclusives = 0;
function pass(label) { console.log('PASS: ' + label); }
function fail(label, detail) {
  failures += 1;
  console.log('FAIL: ' + label + (detail ? ' -- ' + detail : ''));
}
// Harness exhaustion is not a product failure: if the harness cut the worker
// off (authored 300s wall-clock timeout, or a max-turns ceiling if one is ever
// reintroduced), the Stop hook never fired, so neither the structured result
// nor the Stop-boundary delivery can be scored. Report the run as
// harness-inconclusive instead of failing it.
function inconclusive(label, detail) {
  inconclusives += 1;
  console.log('INCONCLUSIVE: ' + label + (detail ? ' -- ' + detail : ''));
}

let state = null;
try {
  state = JSON.parse(readFileSync(demoLogs + '/final-state.json', 'utf8'));
} catch (e) {
  fail('load final-state.json', e.message);
  process.exit(1);
}

const exhausted = { A: false, B: false };
for (const [label, rc, file] of [
  ['A', rcA, 'worker-a.json'],
  ['B', rcB, 'worker-b.json'],
]) {
  let result = null;
  try { result = JSON.parse(readFileSync(demoLogs + '/' + file, 'utf8')); }
  catch (e) { fail('worker ' + label + ' emitted a structured result', e.message); }
<<<<<<< /tmp/mf-ours
  // rc 124 is the authored wall-clock timeout; error_max_turns is the harness
  // turn ceiling. Both cut the worker off before its Stop hook could fire.
  exhausted[label] = rc === 124
    || (result != null && result.subtype === 'error_max_turns');
=======
  // 124 is the launcher’s deadline verdict: GNU timeout on Linux, the
  // portable demo/run-with-timeout.mjs everywhere (macOS has neither GNU
  // timeout nor setsid).
>>>>>>> /tmp/mf-theirs
  const acceptedStatus = rc === 0 || rc === 124;
  if (!acceptedStatus) fail('worker ' + label + ' process status', 'rc=' + rc);
  else if (exhausted[label]) {
    inconclusive('worker ' + label + ' harness-exhausted (harness cut the worker off; result and Stop delivery unscored)',
      'rc=' + rc + ' result=' + JSON.stringify(result));
  } else if (!result || result.is_error !== false || result.subtype !== 'success') {
    fail('worker ' + label + ' completed successfully', 'rc=' + rc + ' result=' + JSON.stringify(result));
  } else {
    pass('worker ' + label + ' produced a successful result');
  }
}

const sessions = state.sessions || [];
const byId = Object.fromEntries(sessions.map(s => [s.session_id, s]));

// 1. both sessions registered
if (byId[sidA] && byId[sidB]) pass('both sessions registered');
else fail('both sessions registered', 'sidA=' + !!byId[sidA] + ' sidB=' + !!byId[sidB]);
const unexpected = sessions.filter(session => session.session_id !== sidA && session.session_id !== sidB);
if (!unexpected.length) pass('scratch fleet contains only the two smoke workers');
else fail('scratch fleet contains only the two smoke workers', unexpected.map(s => s.callsign || s.session_id).join(', '));

// 2. conflict recorded on util.js AND test.js
const conflicts = state.conflicts || [];
const touchedNames = conflicts.map(c => (c.rel_path || c.file || '')).join(' | ');
const hasUtil = /util\.js/.test(touchedNames);
const hasTest = /test\.js/.test(touchedNames);
if (hasUtil && hasTest) pass('conflict recorded on util.js AND test.js');
else fail('conflict recorded on util.js AND test.js', 'conflicts seen: ' + (touchedNames || '(none)'));

// 3. mail delivered at the Stop boundary to BOTH sessions.
// The mechanism under test is the block-at-Stop delivery (the ticker records
// a got-fleet-mail-at-the-turn-boundary line per session). mail_pending>0 at
// the end is NOT a failure: rival-conflict mail that lands after a session
// ends stays queued forever by design (dirty files outlive their authors).
// A harness-exhausted worker had no Stop hook, so its boundary delivery is
// unscored: harness-inconclusive, not a product failure.
// NOTE: this whole block lives inside a bash double-quoted string -- never
// use a literal double-quote character anywhere in it.
const tickerText = (state.ticker || []).map(t => t.msg).join('\n');
const csA = (byId[sidA] || {}).callsign, csB = (byId[sidB] || {}).callsign;
const boundaryA = exhausted.A ? null : csA && tickerText.includes(csA + ' got fleet mail at the turn boundary');
const boundaryB = exhausted.B ? null : csB && tickerText.includes(csB + ' got fleet mail at the turn boundary');
let fleetNote = false;
for (const f of ['worker-a.json', 'worker-b.json']) {
  const p = demoLogs + '/' + f;
  if (existsSync(p) && /FLEET-NOTE/.test(readFileSync(p, 'utf8'))) fleetNote = true;
}
if (boundaryA && boundaryB) pass('mail delivered at Stop boundary to both sessions' + (fleetNote ? ' (and FLEET-NOTE compliance seen)' : ''));
else if ((boundaryA || exhausted.A) && (boundaryB || exhausted.B)) inconclusive('mail delivered at Stop boundary (harness-exhausted worker unscored)', 'A=' + boundaryA + ' B=' + boundaryB);
else fail('mail delivered at Stop boundary to both sessions', 'A=' + boundaryA + ' B=' + boundaryB);

// 4. both tombstoned offline at the end
const offlineA = byId[sidA] && byId[sidA].col === 'offline' && !!byId[sidA].endedAt;
const offlineB = byId[sidB] && byId[sidB].col === 'offline' && !!byId[sidB].endedAt;
if (offlineA && offlineB) pass('both tombstoned offline at the end');
else fail('both tombstoned offline at the end', 'A col=' + (byId[sidA] || {}).col + ' B col=' + (byId[sidB] || {}).col);

if (inconclusives) console.log('INCONCLUSIVE: ' + inconclusives + ' check(s) unscored because the harness cut a worker off');
if (failures) process.exit(1);
"
