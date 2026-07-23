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
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FLEETDECK_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
PROJECT_DIR="$SCRIPT_DIR/project"
DEMO_LOGS="$SCRIPT_DIR/demo-logs"
SESSIONSTART_SCRIPT="$FLEETDECK_ROOT/scripts/fleet-sessionstart.mjs"
FLEET_HOOK_SCRIPT="$FLEETDECK_ROOT/scripts/fleet-hook.mjs"

# An isolated non-production port. Use a smoke-specific override so an ambient
# FLEETDECK_PORT from the current session can never redirect this run to :4711.
FLEETDECK_PORT="${FLEETDECK_SMOKE_PORT:-24711}"

# Assigned from mktemp after the cleanup trap is armed. An arbitrary override
# is intentionally unsupported: cleanup recursively deletes this directory, so
# it must be a unique path created by this run, never a caller-provided target.
SCRATCH_HOME=''

# Isolated tmux server for THIS run only, never the user's default server.
# The fleetd elected by the workers' SessionStart hook inherits this env and
# runs all its tmux calls as `tmux -L $FLEETDECK_TMUX_SOCKET`. Without it, a
# test-env daemon starting the default tmux server would bake FLEETDECK_*
# test values into that server's global env — poisoning every window (and
# production spawn) created there later.
export FLEETDECK_TMUX_SOCKET="fdaccept-$$"

# Everything the smoke starts is isolated and torn down on success, failure, or
# interruption. The user's daemon, tmux server, database, and project files are
# never cleanup targets.
PA=''
PB=''
SMOKE_STARTED=0
stop_worker() {
  local pgid="$1"
  [ -n "$pgid" ] || return 0
  kill -TERM -- "-$pgid" 2>/dev/null || true
  for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
    kill -0 -- "-$pgid" 2>/dev/null || break
    sleep 0.1
  done
  kill -KILL -- "-$pgid" 2>/dev/null || true
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
  cp "$PROJECT_DIR/.seed/util.js" "$PROJECT_DIR/util.js" 2>/dev/null || true
  cp "$PROJECT_DIR/.seed/app.js" "$PROJECT_DIR/app.js" 2>/dev/null || true
  rm -f "$PROJECT_DIR/test.js" "$PROJECT_DIR/.claude/settings.json"
  rmdir "$PROJECT_DIR/.claude" 2>/dev/null || true
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
echo "PROJECT_DIR           = $PROJECT_DIR"
echo "SCRATCH_HOME          = $SCRATCH_HOME"
echo "FLEETDECK_PORT        = $FLEETDECK_PORT"
echo "FLEETDECK_TMUX_SOCKET = $FLEETDECK_TMUX_SOCKET"
echo

for required in timeout setsid; do
  if ! command -v "$required" >/dev/null 2>&1; then
    echo "ABORT: smoke requires $required on PATH"
    exit 1
  fi
done

# ---------------------------------------------------------------- 1. reset
# Final guard: never kill an unknown listener by port. The selected isolated
# port must already be free after the scratch-owned pid cleanup above.
if curl -s -m 1 "http://127.0.0.1:$FLEETDECK_PORT/health" > /dev/null 2>&1; then
  echo "ABORT: something is already listening on isolated port :$FLEETDECK_PORT."
  exit 1
fi

# Reset seed files; test.js must never be committed -- the workers create it.
cp "$PROJECT_DIR/.seed/util.js" "$PROJECT_DIR/util.js"
cp "$PROJECT_DIR/.seed/app.js" "$PROJECT_DIR/app.js"
rm -f "$PROJECT_DIR/test.js"

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

cd "$PROJECT_DIR"
SMOKE_STARTED=1

env "${CLAUDE_ENV_SCRUB[@]}" \
  FLEETDECK_HOME="$SCRATCH_HOME" FLEETDECK_PORT="$FLEETDECK_PORT" \
  FLEETDECK_TMUX_SOCKET="$FLEETDECK_TMUX_SOCKET" FLEETDECK_AGENTS_CMD=false \
  setsid timeout 300 claude -p "Add an exported function slugify(s) to util.js (lowercase, trim, spaces to dashes, strip punctuation). Add assert-based tests for it in test.js (create or extend). Verify each edge case one at a time with separate 'node -e' commands: spaces, capitals, punctuation, empty string. Then run node test.js. Preserve any existing exports. Work step by step, one small change per edit." \
  --session-id "$SA" --max-turns 24 --dangerously-skip-permissions \
  --output-format json > "$DEMO_LOGS/worker-a.json" 2> "$DEMO_LOGS/worker-a.err" &
PA=$!
echo "T+0 session A launched sid=$SA"

sleep 15

env "${CLAUDE_ENV_SCRUB[@]}" \
  FLEETDECK_HOME="$SCRATCH_HOME" FLEETDECK_PORT="$FLEETDECK_PORT" \
  FLEETDECK_TMUX_SOCKET="$FLEETDECK_TMUX_SOCKET" FLEETDECK_AGENTS_CMD=false \
  setsid timeout 300 claude -p "Add an exported function titleCase(s) to util.js (capitalize each word). Add assert-based tests for it in test.js (create or extend). Verify edge cases one at a time with separate 'node -e' commands: single word, multiple words, empty string. Then run node test.js. IMPORTANT: preserve any existing exports and tests you find. Work step by step, one small change per edit." \
  --session-id "$SB" --max-turns 24 --dangerously-skip-permissions \
  --output-format json > "$DEMO_LOGS/worker-b.json" 2> "$DEMO_LOGS/worker-b.err" &
PB=$!
echo "T+15 session B launched sid=$SB"

sleep 14
TOKEN="$(cat "$SCRATCH_HOME/token" 2>/dev/null || true)"
if [ -z "$TOKEN" ]; then
  echo "FAIL: smoke daemon did not mint its bearer token"
  exit 1
fi
if curl -fsS -X POST "http://127.0.0.1:$FLEETDECK_PORT/mail" \
  -H 'content-type: application/json' -H "authorization: Bearer $TOKEN" \
  -d '{"to":"all","from":"luis","text":"Fleet check-in: another agent is editing this repo right now. End your final summary with a line FLEET-NOTE: listing files you touched."}'; then
  echo " | T+29 mail sent"
else
  echo "FAIL: authenticated smoke mail was refused"
  exit 1
fi

sleep 12
echo "T+41 (board screenshot skipped -- Phase 1 board is the ported spike board, no shot.mjs yet)"

wait "$PA"; RC_A=$?; echo "session A done rc=$RC_A"; PA=''
wait "$PB"; RC_B=$?; echo "session B done rc=$RC_B"; PB=''

curl -fsS "http://127.0.0.1:$FLEETDECK_PORT/state" \
  -H "authorization: Bearer $TOKEN" > "$DEMO_LOGS/final-state.json"
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
function pass(label) { console.log('PASS: ' + label); }
function fail(label, detail) {
  failures += 1;
  console.log('FAIL: ' + label + (detail ? ' -- ' + detail : ''));
}

let state = null;
try {
  state = JSON.parse(readFileSync(demoLogs + '/final-state.json', 'utf8'));
} catch (e) {
  fail('load final-state.json', e.message);
  process.exit(1);
}

for (const [label, rc, file] of [
  ['A', rcA, 'worker-a.json'],
  ['B', rcB, 'worker-b.json'],
]) {
  let result = null;
  try { result = JSON.parse(readFileSync(demoLogs + '/' + file, 'utf8')); }
  catch (e) { fail('worker ' + label + ' emitted a structured result', e.message); }
  const acceptedStatus = rc === 0 || rc === 124;
  if (!acceptedStatus) fail('worker ' + label + ' process status', 'rc=' + rc);
  else if (!result || result.is_error !== false || result.subtype !== 'success') {
    fail('worker ' + label + ' completed successfully', 'rc=' + rc + ' result=' + JSON.stringify(result));
  } else {
    pass('worker ' + label + ' produced a successful result' + (rc === 124 ? ' before the authored timeout' : ''));
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
// NOTE: this whole block lives inside a bash double-quoted string -- never
// use a literal double-quote character anywhere in it.
const tickerText = (state.ticker || []).map(t => t.msg).join('\n');
const csA = (byId[sidA] || {}).callsign, csB = (byId[sidB] || {}).callsign;
const boundaryA = csA && tickerText.includes(csA + ' got fleet mail at the turn boundary');
const boundaryB = csB && tickerText.includes(csB + ' got fleet mail at the turn boundary');
let fleetNote = false;
for (const f of ['worker-a.json', 'worker-b.json']) {
  const p = demoLogs + '/' + f;
  if (existsSync(p) && /FLEET-NOTE/.test(readFileSync(p, 'utf8'))) fleetNote = true;
}
if (boundaryA && boundaryB) pass('mail delivered at Stop boundary to both sessions' + (fleetNote ? ' (and FLEET-NOTE compliance seen)' : ''));
else fail('mail delivered at Stop boundary to both sessions', 'A=' + boundaryA + ' B=' + boundaryB);

// 4. both tombstoned offline at the end
const offlineA = byId[sidA] && byId[sidA].col === 'offline' && !!byId[sidA].endedAt;
const offlineB = byId[sidB] && byId[sidB].col === 'offline' && !!byId[sidB].endedAt;
if (offlineA && offlineB) pass('both tombstoned offline at the end');
else fail('both tombstoned offline at the end', 'A col=' + (byId[sidA] || {}).col + ' B col=' + (byId[sidB] || {}).col);

if (failures) process.exit(1);
"
