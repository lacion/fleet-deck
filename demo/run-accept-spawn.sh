#!/bin/bash
# demo/run-accept-spawn.sh — Fleet Deck v1.2 dynamic-fleet live spawn acceptance.
#
# NEVER AUTO-RUN: this gate starts a real interactive Claude session and spends
# billed Claude usage. Only the human acceptance orchestrator should run it.
# Gate: v1.2 — dynamic fleet (board-spawned sessions over the tmux adapter).

set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FLEETDECK_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
PROJECT_DIR="$SCRIPT_DIR/project"
SESSIONSTART_SCRIPT="$FLEETDECK_ROOT/scripts/fleet-sessionstart.mjs"
WATCH_SCRIPT="$FLEETDECK_ROOT/scripts/fleet-watch.mjs"
FLEETDECK_PORT=4711
SCRATCH_HOME="$FLEETDECK_ROOT/.fleetdeck-test"
BASE="http://127.0.0.1:$FLEETDECK_PORT"
TMUX_SESSION="fleetdeck-$FLEETDECK_PORT"
WINDOW_PREFIX="fd$FLEETDECK_PORT-"
OUTPUT_FILE="$PROJECT_DIR/spawn-accept-done.txt"
DAEMON_LOG="$SCRATCH_HOME/fleetd.log"

PASS=0
FAIL=0
DAEMON_PID=""
SPAWN_ID=""
SESSION_ID=""
CALLSIGN=""
TMUX_WINDOW=""
CLEANUP_DONE=0

ok() {
  echo "PASS: $1"
  PASS=$((PASS + 1))
}

bad() {
  echo "FAIL: $1${2:+ -- $2}"
  FAIL=$((FAIL + 1))
}

scoped_session_exists() {
  tmux list-sessions -F '#{session_name}' -f "#{==:#{session_name},$TMUX_SESSION}" 2>/dev/null |
    grep -Fxq "$TMUX_SESSION"
}

scoped_window_exists() {
  [ -n "$TMUX_WINDOW" ] || return 1
  case "$TMUX_WINDOW" in
    "$WINDOW_PREFIX"*) ;;
    *) return 1 ;;
  esac
  tmux list-windows -t "$TMUX_SESSION" -F '#{window_name}' \
    -f "#{m:${WINDOW_PREFIX}*,#{window_name}}" 2>/dev/null |
    grep -Fxq "$TMUX_WINDOW"
}

cleanup_resources() {
  [ "$CLEANUP_DONE" -eq 0 ] || return 0
  CLEANUP_DONE=1

  rm -f "$OUTPUT_FILE"

  if [ -n "$DAEMON_PID" ] && kill -0 "$DAEMON_PID" 2>/dev/null; then
    kill "$DAEMON_PID" 2>/dev/null || true
    for _ in $(seq 1 20); do
      kill -0 "$DAEMON_PID" 2>/dev/null || break
      sleep 0.25
    done
  fi

  # This direct tmux operation is cleanup only. All inspection is server-side
  # filtered to the one Fleet Deck session; unrelated sessions are never listed.
  if command -v tmux >/dev/null 2>&1 && scoped_session_exists; then
    tmux kill-session -t "$TMUX_SESSION" 2>/dev/null || true
  fi
}

trap cleanup_resources EXIT

echo "== Fleet Deck v1.2 live spawn acceptance =="

# --------------------------------------------------------------- reset
# Match the established demo reset discipline: stop known daemon pidfiles
# first, then clear an orphan listener only when Fleet Deck's health endpoint
# proves the process on the port is Fleet Deck.
REAL_HOME="${HOME:-/root}/.fleetdeck"
if [ -f "$REAL_HOME/fleetd.pid" ]; then
  kill "$(cat "$REAL_HOME/fleetd.pid" 2>/dev/null)" 2>/dev/null || true
fi
if [ -f "$SCRATCH_HOME/fleetd.pid" ]; then
  kill "$(cat "$SCRATCH_HOME/fleetd.pid" 2>/dev/null)" 2>/dev/null || true
fi
sleep 0.5
if curl -s -m 1 "$BASE/health" 2>/dev/null | grep -q '"ok"'; then
  fuser -k "$FLEETDECK_PORT/tcp" 2>/dev/null || true
  sleep 0.5
fi

rm -rf "$SCRATCH_HOME"
mkdir -p "$SCRATCH_HOME" "$PROJECT_DIR/.claude"
rm -f "$OUTPUT_FILE"

# Regenerate the proven local demo hook wiring. The Stop command hook keeps
# the v1.1 asyncRewake fields verbatim from hooks/hooks.json. This known
# baseline is intentionally left in place after the test: cleanup uses no git
# command and therefore cannot overwrite unrelated working-tree changes.
cat > "$PROJECT_DIR/.claude/settings.json" <<EOF
{
  "hooks": {
    "SessionStart": [
      { "hooks": [{ "type": "command", "command": "node $SESSIONSTART_SCRIPT", "timeout": 15 }] }
    ],
    "UserPromptSubmit": [
      { "hooks": [{ "type": "http", "url": "$BASE/hook/UserPromptSubmit", "timeout": 3 }] }
    ],
    "PostToolUse": [
      { "matcher": "Edit|Write|MultiEdit|NotebookEdit|Bash", "hooks": [{ "type": "http", "url": "$BASE/hook/PostToolUse", "timeout": 3 }] }
    ],
    "PreToolUse": [
      { "matcher": "AskUserQuestion", "hooks": [{ "type": "http", "url": "$BASE/hook/AskUserQuestion", "timeout": 65 }] }
    ],
    "PermissionRequest": [
      { "hooks": [{ "type": "http", "url": "$BASE/hook/PermissionRequest", "timeout": 65 }] }
    ],
    "Elicitation": [
      { "hooks": [{ "type": "http", "url": "$BASE/hook/Elicitation", "timeout": 65 }] }
    ],
    "Notification": [
      { "hooks": [{ "type": "http", "url": "$BASE/hook/Notification", "timeout": 3, "async": true }] }
    ],
    "Stop": [
      { "hooks": [
        { "type": "http", "url": "$BASE/hook/Stop", "timeout": 5 },
        {
          "type": "command",
          "command": "node $WATCH_SCRIPT",
          "asyncRewake": true,
          "rewakeMessage": "[FLEETDECK] Fleet board mail for you:",
          "rewakeSummary": "Fleet Deck: board mail delivered",
          "timeout": 7230
        }
      ] }
    ],
    "SessionEnd": [
      { "hooks": [{ "type": "http", "url": "$BASE/hook/SessionEnd", "timeout": 3, "async": true }] }
    ],
    "FileChanged": [
      { "hooks": [{ "type": "http", "url": "$BASE/hook/FileChanged", "timeout": 3, "async": true }] }
    ]
  }
}
EOF

# --------------------------------------------------------------- gate 1
DAEMON_READY=""
TMUX_READY=""
if command -v tmux >/dev/null 2>&1; then
  TMUX_READY=yes
fi

if ! curl -s -m 1 "$BASE/health" >/dev/null 2>&1; then
  env -u FLEETDECK_SPAWN_CMD \
    FLEETDECK_HOME="$SCRATCH_HOME" FLEETDECK_PORT="$FLEETDECK_PORT" \
    node "$FLEETDECK_ROOT/scripts/fleetd/fleetd.mjs" > "$DAEMON_LOG" 2>&1 &
  DAEMON_PID=$!
  for _ in $(seq 1 40); do
    HEALTH=$(curl -s -m 1 "$BASE/health" 2>/dev/null || true)
    if HEALTH_JSON="$HEALTH" node -e '
      try {
        const h = JSON.parse(process.env.HEALTH_JSON || "{}");
        process.exit(h.ok === true && h.spawn?.available === true ? 0 : 1);
      } catch { process.exit(1); }
    '; then
      DAEMON_READY=yes
      break
    fi
    kill -0 "$DAEMON_PID" 2>/dev/null || break
    sleep 0.25
  done
fi

if [ -n "$TMUX_READY" ] && [ -n "$DAEMON_READY" ]; then
  ok "daemon started directly with real tmux"
else
  REASON="daemon did not report spawn.available=true"
  [ -n "$TMUX_READY" ] || REASON="tmux is not available"
  bad "daemon started directly with real tmux" "$REASON"
fi

# --------------------------------------------------------------- gate 2
SPAWN_HTTP=000
SPAWN_RESPONSE='{}'
if [ -n "$DAEMON_READY" ]; then
  SPAWN_BODY=$(PROJECT_DIR="$PROJECT_DIR" node -e '
    process.stdout.write(JSON.stringify({
      cwd: process.env.PROJECT_DIR,
      permission_mode: "default",
      prompt: "Say READY and end your turn."
    }));
  ')
  SPAWN_HTTP=$(curl -sS -m 10 -o "$SCRATCH_HOME/spawn-response.json" -w '%{http_code}' \
    -X POST "$BASE/api/spawn" -H 'content-type: application/json' -d "$SPAWN_BODY" 2>/dev/null || true)
  SPAWN_RESPONSE=$(cat "$SCRATCH_HOME/spawn-response.json" 2>/dev/null || echo '{}')
  SPAWN_FIELDS=$(SPAWN_JSON="$SPAWN_RESPONSE" node -e '
    try {
      const r = JSON.parse(process.env.SPAWN_JSON || "{}");
      if (!r.spawn_id || !r.session_id || !r.callsign) process.exit(1);
      process.stdout.write([r.spawn_id, r.session_id, r.callsign, r.tmux?.session || "", r.tmux?.window || ""].join("\t"));
    } catch { process.exit(1); }
  ' 2>/dev/null || true)
  if [ -n "$SPAWN_FIELDS" ]; then
    IFS=$'\t' read -r SPAWN_ID SESSION_ID CALLSIGN SPAWN_TMUX_SESSION TMUX_WINDOW <<EOF
$SPAWN_FIELDS
EOF
  fi
fi

if [ "$SPAWN_HTTP" = 200 ] && [ -n "$SPAWN_ID" ] && [ -n "$SESSION_ID" ] && [ -n "$CALLSIGN" ]; then
  ok "spawn accepted"
else
  bad "spawn accepted" "HTTP $SPAWN_HTTP or missing spawn_id/session_id/callsign"
fi

# --------------------------------------------------------------- gate 3
JOINED=""
if [ -n "$SESSION_ID" ]; then
  for _ in $(seq 1 30); do
    STATE=$(curl -s -m 2 "$BASE/state" 2>/dev/null || true)
    if STATE_JSON="$STATE" EXPECTED_SID="$SESSION_ID" node -e '
      try {
        const s = JSON.parse(process.env.STATE_JSON || "{}");
        const card = (s.sessions || []).find(x => x.session_id === process.env.EXPECTED_SID);
        process.exit(card?.source === "hooks" && card?.spawn?.status === "live" ? 0 : 1);
      } catch { process.exit(1); }
    '; then
      JOINED=yes
      break
    fi
    sleep 1
  done
fi

if [ -n "$JOINED" ]; then
  ok "spawned session joined the fleet"
else
  bad "spawned session joined the fleet" "no hooks-linked/live card within 30s"
fi

# --------------------------------------------------------------- gate 4
WATCHER_ARMED=""
if [ -n "$SESSION_ID" ]; then
  WATCH_PID_FILE="$SCRATCH_HOME/watch-$SESSION_ID.pid"
  for _ in $(seq 1 60); do
    STATE=$(curl -s -m 2 "$BASE/state" 2>/dev/null || true)
    if [ -f "$WATCH_PID_FILE" ] && STATE_JSON="$STATE" EXPECTED_SID="$SESSION_ID" node -e '
      try {
        const s = JSON.parse(process.env.STATE_JSON || "{}");
        const card = (s.sessions || []).find(x => x.session_id === process.env.EXPECTED_SID);
        process.exit(card?.col === "idle" ? 0 : 1);
      } catch { process.exit(1); }
    '; then
      WATCHER_ARMED=yes
      break
    fi
    sleep 1
  done
fi

if [ -n "$WATCHER_ARMED" ]; then
  ok "watcher armed"
else
  bad "watcher armed" "idle card and watch-<sid>.pid did not both appear within 60s"
fi

# --------------------------------------------------------------- gate 5
ROUTED=""
COMMAND_HTTP=000
if [ -n "$CALLSIGN" ]; then
  COMMAND_TEXT='assign auto:fleetdeck Create a file named spawn-accept-done.txt containing exactly SPAWNED AND WORKING. Then confirm briefly.'
  COMMAND_BODY=$(COMMAND_TEXT="$COMMAND_TEXT" node -e \
    'process.stdout.write(JSON.stringify({text: process.env.COMMAND_TEXT}));')
  COMMAND_HTTP=$(curl -sS -m 10 -o "$SCRATCH_HOME/command-response.json" -w '%{http_code}' \
    -X POST "$BASE/command" -H 'content-type: application/json' -d "$COMMAND_BODY" 2>/dev/null || true)
  COMMAND_RESPONSE=$(cat "$SCRATCH_HOME/command-response.json" 2>/dev/null || echo '{}')
  if COMMAND_JSON="$COMMAND_RESPONSE" EXPECTED_CALLSIGN="$CALLSIGN" EXPECTED_SID="$SESSION_ID" node -e '
    try {
      const r = JSON.parse(process.env.COMMAND_JSON || "{}");
      process.exit(r.ok === true && r.assigned_to?.callsign === process.env.EXPECTED_CALLSIGN &&
        r.assigned_to?.session_id === process.env.EXPECTED_SID ? 0 : 1);
    } catch { process.exit(1); }
  '; then
    ROUTED=yes
  fi
fi

if [ "$COMMAND_HTTP" = 200 ] && [ -n "$ROUTED" ]; then
  ok "assignment routed"
else
  bad "assignment routed" "HTTP $COMMAND_HTTP or assigned_to did not match $CALLSIGN"
fi

# --------------------------------------------------------------- gate 6
PERMISSION_APPROVED=""
QID=""
if [ -n "$SESSION_ID" ]; then
  for _ in $(seq 1 60); do
    STATE=$(curl -s -m 2 "$BASE/state" 2>/dev/null || true)
    QID=$(STATE_JSON="$STATE" EXPECTED_SID="$SESSION_ID" node -e '
      try {
        const s = JSON.parse(process.env.STATE_JSON || "{}");
        const q = (s.questions || []).find(x =>
          x.session_id === process.env.EXPECTED_SID && x.kind === "permission" && x.status === "pending");
        process.stdout.write(q ? String(q.id) : "");
      } catch {}
    ' 2>/dev/null)
    if [ -n "$QID" ]; then
      ANSWER_HTTP=$(curl -sS -m 10 -o "$SCRATCH_HOME/answer-response.json" -w '%{http_code}' \
        -X POST "$BASE/api/questions/$QID/answer" -H 'content-type: application/json' \
        -d '{"behavior":"allow"}' 2>/dev/null || true)
      if [ "$ANSWER_HTTP" = 200 ]; then
        PERMISSION_APPROVED=yes
      fi
      break
    fi
    sleep 1
  done
fi

if [ -n "$PERMISSION_APPROVED" ]; then
  ok "permission approved from board"
else
  bad "permission approved from board" "no pending permission approved within 60s"
fi

# --------------------------------------------------------------- gate 7
TASK_COMPLETE=""
for _ in $(seq 1 60); do
  if OUTPUT_FILE="$OUTPUT_FILE" node -e '
    const fs = require("fs");
    try {
      process.exit(fs.readFileSync(process.env.OUTPUT_FILE, "utf8") === "SPAWNED AND WORKING" ? 0 : 1);
    } catch { process.exit(1); }
  '; then
    TASK_COMPLETE=yes
    break
  fi
  sleep 1
done

if [ -n "$TASK_COMPLETE" ]; then
  ok "assigned task completed"
else
  bad "assigned task completed" "exact output file did not appear within 60s"
fi

# --------------------------------------------------------------- gate 8
NONFORCE_HTTP=000
FORCE_HTTP=000
WINDOW_GONE=""
if [ -n "$SPAWN_ID" ]; then
  NONFORCE_HTTP=$(curl -sS -m 10 -o "$SCRATCH_HOME/kill-nonforce-response.json" -w '%{http_code}' \
    -X POST "$BASE/api/spawn/$SPAWN_ID/kill" -H 'content-type: application/json' -d '{}' 2>/dev/null || true)
  FORCE_HTTP=$(curl -sS -m 10 -o "$SCRATCH_HOME/kill-force-response.json" -w '%{http_code}' \
    -X POST "$BASE/api/spawn/$SPAWN_ID/kill" -H 'content-type: application/json' -d '{"force":true}' 2>/dev/null || true)

  if [ "${SPAWN_TMUX_SESSION:-}" = "$TMUX_SESSION" ]; then
    for _ in $(seq 1 20); do
      if ! scoped_window_exists; then
        WINDOW_GONE=yes
        break
      fi
      sleep 0.25
    done
  fi
fi

if [ "$NONFORCE_HTTP" = 409 ] && [ "$FORCE_HTTP" = 200 ] && [ -n "$WINDOW_GONE" ]; then
  ok "kill semantics"
else
  bad "kill semantics" "non-force=$NONFORCE_HTTP force=$FORCE_HTTP or scoped window still present"
fi

# --------------------------------------------------------------- gate 9
cleanup_resources
CLEANUP_OK=yes
[ ! -e "$OUTPUT_FILE" ] || CLEANUP_OK=""
if [ -n "$DAEMON_PID" ] && kill -0 "$DAEMON_PID" 2>/dev/null; then
  CLEANUP_OK=""
fi
if command -v tmux >/dev/null 2>&1 && scoped_session_exists; then
  CLEANUP_OK=""
fi

if [ -n "$CLEANUP_OK" ]; then
  ok "cleanup"
else
  bad "cleanup" "output, daemon, or fleetdeck-4711 tmux session remains"
fi

echo
echo "RESULT: $PASS pass, $FAIL fail"
[ "$FAIL" -eq 0 ]
