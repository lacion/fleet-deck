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

# FLEETDECK_PORT: the demo's native http hooks are hardcoded to 4711 in
# demo/project/.claude/settings.json (per the hooks.json sketch rendered
# below), so this must stay 4711 unless settings.json is regenerated below
# with a different value too.
FLEETDECK_PORT="${FLEETDECK_PORT:-4711}"

# Scratch FLEETDECK_HOME for THIS run only. Never the user's real ~/.fleetdeck
# (that holds their actual fleet's SQLite state). Override with
# FLEETDECK_HOME_OVERRIDE if you want a specific location; default matches the
# repo's own .gitignore entry for `.fleetdeck-test/`.
SCRATCH_HOME="${FLEETDECK_HOME_OVERRIDE:-$FLEETDECK_ROOT/.fleetdeck-test}"

# Isolated tmux server for THIS run only, never the user's default server.
# The fleetd elected by the workers' SessionStart hook inherits this env and
# runs all its tmux calls as `tmux -L $FLEETDECK_TMUX_SOCKET`. Without it, a
# test-env daemon starting the default tmux server would bake FLEETDECK_*
# test values into that server's global env — poisoning every window (and
# production spawn) created there later.
export FLEETDECK_TMUX_SOCKET="fdaccept-$$"

# Kill the isolated tmux server (if anything ever spawned into it) with the
# run; the default server is never touched.
cleanup_tmux_server() {
  command -v tmux >/dev/null 2>&1 || return 0
  tmux -L "$FLEETDECK_TMUX_SOCKET" kill-server 2>/dev/null || true
}
trap cleanup_tmux_server EXIT

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

# ---------------------------------------------------------------- 1. reset
# Kill any REAL production fleetd (its pid lives in the user's real
# ~/.fleetdeck) so it isn't squatting on the port the demo's http hooks are
# hardcoded to. We do NOT touch its state directory.
REAL_HOME="${HOME:-/root}/.fleetdeck"
if [ -f "$REAL_HOME/fleetd.pid" ]; then
  REAL_PID="$(cat "$REAL_HOME/fleetd.pid" 2>/dev/null || true)"
  if [ -n "${REAL_PID:-}" ]; then
    echo "Stopping a possibly-running production fleetd (pid $REAL_PID) so the demo can bind :$FLEETDECK_PORT..."
    kill "$REAL_PID" 2>/dev/null || true
    sleep 0.5
  fi
fi

# Kill the SCRATCH daemon from a previous smoke run (its pid file lives in
# the scratch home, not the real one) BEFORE wiping — otherwise the old
# process keeps running with the deleted DB open and squats on the port with
# stale state.
if [ -f "$SCRATCH_HOME/fleetd.pid" ]; then
  SCRATCH_PID="$(cat "$SCRATCH_HOME/fleetd.pid" 2>/dev/null || true)"
  if [ -n "${SCRATCH_PID:-}" ]; then
    echo "Stopping previous smoke-run fleetd (pid $SCRATCH_PID)..."
    kill "$SCRATCH_PID" 2>/dev/null || true
    sleep 0.5
  fi
fi

# Wipe (only) the scratch FLEETDECK_HOME for this run.
rm -rf "$SCRATCH_HOME"
mkdir -p "$SCRATCH_HOME"

# A fleetd whose pid file was destroyed (e.g. by a previous run's wipe) can
# still be squatting on the port. If whatever listens there answers /health
# like a fleetd, kill it by port; a non-fleetd listener is left alone.
if curl -s -m 1 "http://127.0.0.1:$FLEETDECK_PORT/health" 2>/dev/null | grep -q '"ok"'; then
  echo "Killing orphaned fleetd on :$FLEETDECK_PORT (pid file was lost)..."
  fuser -k "$FLEETDECK_PORT/tcp" 2>/dev/null || true
  sleep 0.5
fi

# Final guard: the port must be free now; refuse to run against a foreign
# server (hooks are hardcoded to this port and results would be garbage).
if curl -s -m 1 "http://127.0.0.1:$FLEETDECK_PORT/health" > /dev/null 2>&1; then
  echo "ABORT: something is still listening on :$FLEETDECK_PORT after reset."
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
# SessionStart must run as a command hook with an absolute path to
# fleet-sessionstart.mjs (computed here, so this works regardless of where
# the repo is checked out) -- everything else is a native "http" hook per
# the hooks.json sketch below.
cat > "$PROJECT_DIR/.claude/settings.json" <<EOF
{
  "hooks": {
    "SessionStart": [
      { "hooks": [{ "type": "command", "command": "node $SESSIONSTART_SCRIPT", "timeout": 15 }] }
    ],
    "UserPromptSubmit": [
      { "hooks": [{ "type": "http", "url": "http://127.0.0.1:$FLEETDECK_PORT/hook/UserPromptSubmit", "timeout": 3 }] }
    ],
    "PostToolUse": [
      { "matcher": "Edit|Write|MultiEdit|NotebookEdit|Bash", "hooks": [{ "type": "http", "url": "http://127.0.0.1:$FLEETDECK_PORT/hook/PostToolUse", "timeout": 3 }] }
    ],
    "PreToolUse": [
      { "matcher": "AskUserQuestion", "hooks": [{ "type": "http", "url": "http://127.0.0.1:$FLEETDECK_PORT/hook/AskUserQuestion", "timeout": 65 }] }
    ],
    "PermissionRequest": [
      { "hooks": [{ "type": "http", "url": "http://127.0.0.1:$FLEETDECK_PORT/hook/PermissionRequest", "timeout": 65 }] }
    ],
    "Elicitation": [
      { "hooks": [{ "type": "http", "url": "http://127.0.0.1:$FLEETDECK_PORT/hook/Elicitation", "timeout": 65 }] }
    ],
    "Notification": [
      { "hooks": [{ "type": "http", "url": "http://127.0.0.1:$FLEETDECK_PORT/hook/Notification", "timeout": 3, "async": true }] }
    ],
    "Stop": [
      { "hooks": [{ "type": "http", "url": "http://127.0.0.1:$FLEETDECK_PORT/hook/Stop", "timeout": 5 }] }
    ],
    "SessionEnd": [
      { "hooks": [{ "type": "http", "url": "http://127.0.0.1:$FLEETDECK_PORT/hook/SessionEnd", "timeout": 3, "async": true }] }
    ],
    "FileChanged": [
      { "hooks": [{ "type": "http", "url": "http://127.0.0.1:$FLEETDECK_PORT/hook/FileChanged", "timeout": 3, "async": true }] }
    ]
  }
}
EOF

# ---------------------------------------------------------- 3. launch fleet
SA=$(node -e 'console.log(crypto.randomUUID())')
SB=$(node -e 'console.log(crypto.randomUUID())')
echo "$SA" > "$DEMO_LOGS/sid-a.txt"
echo "$SB" > "$DEMO_LOGS/sid-b.txt"

cd "$PROJECT_DIR"

env "${CLAUDE_ENV_SCRUB[@]}" \
  FLEETDECK_HOME="$SCRATCH_HOME" FLEETDECK_PORT="$FLEETDECK_PORT" \
  FLEETDECK_TMUX_SOCKET="$FLEETDECK_TMUX_SOCKET" \
  timeout 300 claude -p "Add an exported function slugify(s) to util.js (lowercase, trim, spaces to dashes, strip punctuation). Add assert-based tests for it in test.js (create or extend). Verify each edge case one at a time with separate 'node -e' commands: spaces, capitals, punctuation, empty string. Then run node test.js. Preserve any existing exports. Work step by step, one small change per edit." \
  --session-id "$SA" --max-turns 24 --dangerously-skip-permissions \
  --output-format json > "$DEMO_LOGS/worker-a.json" 2> "$DEMO_LOGS/worker-a.err" &
PA=$!
echo "T+0 session A launched sid=$SA"

sleep 15

env "${CLAUDE_ENV_SCRUB[@]}" \
  FLEETDECK_HOME="$SCRATCH_HOME" FLEETDECK_PORT="$FLEETDECK_PORT" \
  FLEETDECK_TMUX_SOCKET="$FLEETDECK_TMUX_SOCKET" \
  timeout 300 claude -p "Add an exported function titleCase(s) to util.js (capitalize each word). Add assert-based tests for it in test.js (create or extend). Verify edge cases one at a time with separate 'node -e' commands: single word, multiple words, empty string. Then run node test.js. IMPORTANT: preserve any existing exports and tests you find. Work step by step, one small change per edit." \
  --session-id "$SB" --max-turns 24 --dangerously-skip-permissions \
  --output-format json > "$DEMO_LOGS/worker-b.json" 2> "$DEMO_LOGS/worker-b.err" &
PB=$!
echo "T+15 session B launched sid=$SB"

sleep 14
curl -s -X POST "http://127.0.0.1:$FLEETDECK_PORT/mail" -H 'content-type: application/json' \
  -d '{"to":"all","from":"luis","text":"Fleet check-in: another agent is editing this repo right now. End your final summary with a line FLEET-NOTE: listing files you touched."}' \
  && echo " | T+29 mail sent"

sleep 12
echo "T+41 (board screenshot skipped -- Phase 1 board is the ported spike board, no shot.mjs yet)"

wait "$PA"; echo "session A done rc=$?"
wait "$PB"; echo "session B done rc=$?"

curl -s "http://127.0.0.1:$FLEETDECK_PORT/state" > "$DEMO_LOGS/final-state.json"
echo "ROUND COMPLETE — captured $DEMO_LOGS/final-state.json"
echo

# --------------------------------------------------------------- 4. verify
node --input-type=module -e "
import { readFileSync, existsSync } from 'node:fs';

const demoLogs = '$DEMO_LOGS';
const sidA = '$SA';
const sidB = '$SB';

function pass(label) { console.log('PASS: ' + label); }
function fail(label, detail) { console.log('FAIL: ' + label + (detail ? ' -- ' + detail : '')); }

let state = null;
try {
  state = JSON.parse(readFileSync(demoLogs + '/final-state.json', 'utf8'));
} catch (e) {
  fail('load final-state.json', e.message);
  process.exit(1);
}

const sessions = state.sessions || [];
const byId = Object.fromEntries(sessions.map(s => [s.session_id, s]));

// 1. both sessions registered
if (byId[sidA] && byId[sidB]) pass('both sessions registered');
else fail('both sessions registered', 'sidA=' + !!byId[sidA] + ' sidB=' + !!byId[sidB]);

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
"
