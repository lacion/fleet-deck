#!/bin/bash
# demo/run-accept-plan.sh — Fleet Deck v1.3 plan-library live acceptance.
#
# NEVER AUTO-RUN: this gate starts two real interactive Claude sessions and
# spends billed Claude usage. Only the human acceptance orchestrator should run it.
# Gate: v1.3 — unsupervised spawns + plan library.

set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FLEETDECK_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
PROJECT_DIR="$SCRIPT_DIR/project"
SEED_UTIL="$PROJECT_DIR/.seed/util.js"
UTIL_FILE="$PROJECT_DIR/util.js"
TEST_FILE="$PROJECT_DIR/test.js"
SESSIONSTART_SCRIPT="$FLEETDECK_ROOT/scripts/fleet-sessionstart.mjs"
WATCH_SCRIPT="$FLEETDECK_ROOT/scripts/fleet-watch.mjs"
FLEETDECK_PORT=4711
SCRATCH_HOME="$FLEETDECK_ROOT/.fleetdeck-test"
BASE="http://127.0.0.1:$FLEETDECK_PORT"
TMUX_SESSION="fleetdeck-$FLEETDECK_PORT"
WINDOW_PREFIX="fd$FLEETDECK_PORT-"
DAEMON_LOG="$SCRATCH_HOME/fleetd.log"
PLAN_FILE="$SCRATCH_HOME/plan.md"
EXECUTOR_SAMPLES="$SCRATCH_HOME/executor-state-samples.jsonl"

PASS=0
FAIL=0
DAEMON_PID=""
PLANNER_SPAWN_ID=""
PLANNER_SESSION_ID=""
PLANNER_CALLSIGN=""
PLANNER_WINDOW=""
EXECUTOR_SPAWN_ID=""
EXECUTOR_SESSION_ID=""
EXECUTOR_CALLSIGN=""
EXECUTOR_WINDOW=""
PLAN_ID=""
QID=""
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
  local window="${1:-}"
  [ -n "$window" ] || return 1
  case "$window" in
    "$WINDOW_PREFIX"*) ;;
    *) return 1 ;;
  esac
  tmux list-windows -t "$TMUX_SESSION" -F '#{window_name}' \
    -f "#{m:${WINDOW_PREFIX}*,#{window_name}}" 2>/dev/null |
    grep -Fxq "$window"
}

force_kill_spawn() {
  local spawn_id="${1:-}"
  local response_file="${2:-}"
  [ -n "$spawn_id" ] || return 0
  local force_body
  force_body=$(node -e 'process.stdout.write(JSON.stringify({force: true}));')
  curl -sS -m 10 -o "$response_file" -w '%{http_code}' \
    -X POST "$BASE/api/spawn/$spawn_id/kill" \
    -H 'content-type: application/json' -d "$force_body" 2>/dev/null || true
}

cleanup_resources() {
  [ "$CLEANUP_DONE" -eq 0 ] || return 0
  CLEANUP_DONE=1

  # Prefer the name-verified API while fleetd is alive. Both IDs came from
  # POST /api/spawn responses whose tmux session/window scopes were checked.
  if [ -n "$DAEMON_PID" ] && kill -0 "$DAEMON_PID" 2>/dev/null; then
    force_kill_spawn "$PLANNER_SPAWN_ID" "$SCRATCH_HOME/cleanup-planner.json" >/dev/null
    force_kill_spawn "$EXECUTOR_SPAWN_ID" "$SCRATCH_HOME/cleanup-executor.json" >/dev/null
  fi

  # Direct tmux cleanup is restricted to the one Fleet Deck acceptance session.
  if command -v tmux >/dev/null 2>&1 && scoped_session_exists; then
    tmux kill-session -t "$TMUX_SESSION" 2>/dev/null || true
  fi

  if [ -f "$SEED_UTIL" ]; then
    cp "$SEED_UTIL" "$UTIL_FILE" 2>/dev/null || true
  fi
  rm -f "$TEST_FILE"

  if [ -n "$DAEMON_PID" ] && kill -0 "$DAEMON_PID" 2>/dev/null; then
    kill "$DAEMON_PID" 2>/dev/null || true
    for _ in $(seq 1 20); do
      kill -0 "$DAEMON_PID" 2>/dev/null || break
      sleep 0.25
    done
  fi
}

trap cleanup_resources EXIT ERR
trap 'cleanup_resources; exit 130' INT

echo "== Fleet Deck v1.3 live plan acceptance =="

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

# Reset only the acceptance session; never enumerate or touch unrelated tmux
# sessions or windows.
if command -v tmux >/dev/null 2>&1 && scoped_session_exists; then
  tmux kill-session -t "$TMUX_SESSION" 2>/dev/null || true
fi

rm -rf "$SCRATCH_HOME"
mkdir -p "$SCRATCH_HOME" "$PROJECT_DIR/.claude"
cp "$SEED_UTIL" "$UTIL_FILE"
rm -f "$TEST_FILE"

# Regenerate the proven local demo hook wiring. The Stop command hook keeps
# the v1.1 asyncRewake fields verbatim from hooks/hooks.json.
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

if [ -n "$TMUX_READY" ]; then
  env -u FLEETDECK_SPAWN_CMD \
    FLEETDECK_HOME="$SCRATCH_HOME" FLEETDECK_PORT="$FLEETDECK_PORT" \
    node "$FLEETDECK_ROOT/scripts/fleetd/fleetd.mjs" > "$DAEMON_LOG" 2>&1 &
  DAEMON_PID=$!
  for _ in $(seq 1 40); do
    STATE=$(curl -s -m 1 "$BASE/state" 2>/dev/null || true)
    if STATE_JSON="$STATE" node -e '
      try {
        const s = JSON.parse(process.env.STATE_JSON || "{}");
        process.exit(s.spawn?.available === true ? 0 : 1);
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
  ok "reset complete & daemon ready with real tmux"
else
  REASON="/state did not report spawn.available=true"
  [ -n "$TMUX_READY" ] || REASON="tmux is not available"
  bad "reset complete & daemon ready with real tmux" "$REASON"
fi

# --------------------------------------------------------------- gate 2
PLANNER_HTTP=000
PLANNER_JOINED=""
if [ -n "$DAEMON_READY" ]; then
  PLANNER_PROMPT='Plan (do not implement): add a function slugToTitle(slug) to util.js that converts kebab-case slugs to Title Case, with 2-3 assert tests in test.js. Keep the plan short, then present it.'
  PLANNER_BODY=$(PROJECT_DIR="$PROJECT_DIR" PLANNER_PROMPT="$PLANNER_PROMPT" node -e '
    process.stdout.write(JSON.stringify({
      cwd: process.env.PROJECT_DIR,
      permission_mode: "plan",
      prompt: process.env.PLANNER_PROMPT
    }));
  ')
  PLANNER_HTTP=$(curl -sS -m 10 -o "$SCRATCH_HOME/planner-spawn.json" -w '%{http_code}' \
    -X POST "$BASE/api/spawn" -H 'content-type: application/json' \
    -d "$PLANNER_BODY" 2>/dev/null || true)
  PLANNER_RESPONSE=$(PLANNER_JSON_FILE="$SCRATCH_HOME/planner-spawn.json" node -e '
    const fs = require("fs");
    try { process.stdout.write(fs.readFileSync(process.env.PLANNER_JSON_FILE, "utf8")); }
    catch { process.stdout.write("{}"); }
  ')
  PLANNER_FIELDS=$(PLANNER_JSON="$PLANNER_RESPONSE" node -e '
    try {
      const r = JSON.parse(process.env.PLANNER_JSON || "{}");
      if (!r.spawn_id || !r.session_id || !r.callsign || !r.tmux?.session || !r.tmux?.window) process.exit(1);
      process.stdout.write([r.spawn_id, r.session_id, r.callsign, r.tmux.session, r.tmux.window].join("\t"));
    } catch { process.exit(1); }
  ' 2>/dev/null || true)
  if [ -n "$PLANNER_FIELDS" ]; then
    IFS=$'\t' read -r CANDIDATE_SPAWN CANDIDATE_SID CANDIDATE_CALLSIGN CANDIDATE_TMUX CANDIDATE_WINDOW <<EOF
$PLANNER_FIELDS
EOF
    case "$CANDIDATE_WINDOW" in
      "$WINDOW_PREFIX"*)
        if [ "$CANDIDATE_TMUX" = "$TMUX_SESSION" ]; then
          PLANNER_SPAWN_ID="$CANDIDATE_SPAWN"
          PLANNER_SESSION_ID="$CANDIDATE_SID"
          PLANNER_CALLSIGN="$CANDIDATE_CALLSIGN"
          PLANNER_WINDOW="$CANDIDATE_WINDOW"
        fi
        ;;
    esac
  fi
fi

if [ "$PLANNER_HTTP" = 200 ] && [ -n "$PLANNER_SESSION_ID" ]; then
  for _ in $(seq 1 30); do
    STATE=$(curl -s -m 2 "$BASE/state" 2>/dev/null || true)
    if STATE_JSON="$STATE" EXPECTED_SID="$PLANNER_SESSION_ID" node -e '
      try {
        const s = JSON.parse(process.env.STATE_JSON || "{}");
        const card = (s.sessions || []).find(x => x.session_id === process.env.EXPECTED_SID);
        process.exit(card?.source === "hooks" && card?.spawn?.status === "live" ? 0 : 1);
      } catch { process.exit(1); }
    '; then
      PLANNER_JOINED=yes
      break
    fi
    sleep 1
  done
fi

if [ -n "$PLANNER_JOINED" ]; then
  ok "planner spawned"
else
  bad "planner spawned" "HTTP $PLANNER_HTTP or no hooks/live card within 30s"
fi

# --------------------------------------------------------------- gate 3
PLAN_VISIBLE=""
if [ -n "$PLANNER_SESSION_ID" ]; then
  for _ in $(seq 1 120); do
    STATE=$(curl -s -m 2 "$BASE/state" 2>/dev/null || true)
    CAPTURE_FIELDS=$(STATE_JSON="$STATE" EXPECTED_SID="$PLANNER_SESSION_ID" node -e '
      try {
        const s = JSON.parse(process.env.STATE_JSON || "{}");
        const q = (s.questions || []).find(x =>
          x.session_id === process.env.EXPECTED_SID && x.kind === "permission" &&
          x.status === "pending" && x.payload?.tool_name === "ExitPlanMode");
        if (!q) process.exit(1);
        const plan = (s.plans || []).find(x =>
          x.session_id === process.env.EXPECTED_SID && x.plan_id === q.plan_id &&
          x.status === "proposed" && typeof x.plan_md === "string" && x.plan_md.trim());
        if (!plan) process.exit(1);
        process.stdout.write([String(q.id), String(plan.plan_id)].join("\t"));
      } catch { process.exit(1); }
    ' 2>/dev/null || true)
    if [ -n "$CAPTURE_FIELDS" ]; then
      IFS=$'\t' read -r QID PLAN_ID <<EOF
$CAPTURE_FIELDS
EOF
      PLAN_VISIBLE=yes
      break
    fi
    sleep 1
  done
fi

if [ -n "$PLAN_VISIBLE" ] && [ -n "$QID" ] && [ -n "$PLAN_ID" ]; then
  ok "plan captured on board"
else
  bad "plan captured on board" "no matching pending ExitPlanMode question and proposed plan within 120s"
fi

# --------------------------------------------------------------- gate 4
CAPTURE_HTTP=000
PLAN_CAPTURED=""
if [ -n "$QID" ]; then
  CAPTURE_BODY=$(node -e 'process.stdout.write(JSON.stringify({behavior: "capture"}));')
  CAPTURE_HTTP=$(curl -sS -m 10 -o "$SCRATCH_HOME/capture-answer.json" -w '%{http_code}' \
    -X POST "$BASE/api/questions/$QID/answer" -H 'content-type: application/json' \
    -d "$CAPTURE_BODY" 2>/dev/null || true)
fi
if [ "$CAPTURE_HTTP" = 200 ] && [ -n "$PLAN_ID" ]; then
  for _ in $(seq 1 30); do
    STATE=$(curl -s -m 2 "$BASE/state" 2>/dev/null || true)
    if STATE_JSON="$STATE" EXPECTED_PLAN_ID="$PLAN_ID" node -e '
      try {
        const s = JSON.parse(process.env.STATE_JSON || "{}");
        const plan = (s.plans || []).find(x => String(x.plan_id) === process.env.EXPECTED_PLAN_ID);
        process.exit(plan?.status === "captured" ? 0 : 1);
      } catch { process.exit(1); }
    '; then
      PLAN_CAPTURED=yes
      break
    fi
    sleep 1
  done
fi

if [ -n "$PLAN_CAPTURED" ]; then
  ok "plan captured & planner released"
else
  bad "plan captured & planner released" "HTTP $CAPTURE_HTTP or plan did not become captured within 30s"
fi

# --------------------------------------------------------------- gate 5
PLANNER_RELEASED=""
if [ -n "$PLANNER_SESSION_ID" ]; then
  for _ in $(seq 1 60); do
    STATE=$(curl -s -m 2 "$BASE/state" 2>/dev/null || true)
    if STATE_JSON="$STATE" EXPECTED_SID="$PLANNER_SESSION_ID" node -e '
      try {
        const s = JSON.parse(process.env.STATE_JSON || "{}");
        const card = (s.sessions || []).find(x => x.session_id === process.env.EXPECTED_SID);
        process.exit(card && (card.col === "idle" || card.col === "offline") ? 0 : 1);
      } catch { process.exit(1); }
    '; then
      PLANNER_RELEASED=yes
      break
    fi
    sleep 1
  done
fi

if [ -n "$PLANNER_RELEASED" ]; then
  ok "planner released cleanly"
else
  bad "planner released cleanly" "planner card did not become idle/offline within 60s"
fi

# --------------------------------------------------------------- gate 6
EXECUTOR_HTTP=000
EXECUTOR_UNSUPERVISED=""
if [ -n "$PLAN_ID" ]; then
  STATE=$(curl -s -m 2 "$BASE/state" 2>/dev/null || true)
  if ! STATE_JSON="$STATE" EXPECTED_PLAN_ID="$PLAN_ID" PLAN_FILE="$PLAN_FILE" node -e '
    const fs = require("fs");
    try {
      const s = JSON.parse(process.env.STATE_JSON || "{}");
      const plan = (s.plans || []).find(x => String(x.plan_id) === process.env.EXPECTED_PLAN_ID);
      if (!plan || typeof plan.plan_md !== "string" || !plan.plan_md.trim()) process.exit(1);
      fs.writeFileSync(process.env.PLAN_FILE, plan.plan_md);
    } catch { process.exit(1); }
  '; then
    rm -f "$PLAN_FILE"
  fi
fi

if [ -s "$PLAN_FILE" ]; then
  EXECUTOR_BODY=$(PROJECT_DIR="$PROJECT_DIR" PLAN_FILE="$PLAN_FILE" node -e '
    const fs = require("fs");
    const plan = fs.readFileSync(process.env.PLAN_FILE, "utf8");
    process.stdout.write(JSON.stringify({
      cwd: process.env.PROJECT_DIR,
      dangerously_skip_permissions: true,
      prompt: "Execute this approved plan exactly. Custom instructions: work quickly, no questions.\n\n---\n" + plan
    }));
  ')
  EXECUTOR_HTTP=$(curl -sS -m 10 -o "$SCRATCH_HOME/executor-spawn.json" -w '%{http_code}' \
    -X POST "$BASE/api/spawn" -H 'content-type: application/json' \
    -d "$EXECUTOR_BODY" 2>/dev/null || true)
  EXECUTOR_RESPONSE=$(EXECUTOR_JSON_FILE="$SCRATCH_HOME/executor-spawn.json" node -e '
    const fs = require("fs");
    try { process.stdout.write(fs.readFileSync(process.env.EXECUTOR_JSON_FILE, "utf8")); }
    catch { process.stdout.write("{}"); }
  ')
  EXECUTOR_FIELDS=$(EXECUTOR_JSON="$EXECUTOR_RESPONSE" node -e '
    try {
      const r = JSON.parse(process.env.EXECUTOR_JSON || "{}");
      if (!r.spawn_id || !r.session_id || !r.callsign || !r.tmux?.session || !r.tmux?.window) process.exit(1);
      process.stdout.write([r.spawn_id, r.session_id, r.callsign, r.tmux.session, r.tmux.window].join("\t"));
    } catch { process.exit(1); }
  ' 2>/dev/null || true)
  if [ -n "$EXECUTOR_FIELDS" ]; then
    IFS=$'\t' read -r CANDIDATE_SPAWN CANDIDATE_SID CANDIDATE_CALLSIGN CANDIDATE_TMUX CANDIDATE_WINDOW <<EOF
$EXECUTOR_FIELDS
EOF
    case "$CANDIDATE_WINDOW" in
      "$WINDOW_PREFIX"*)
        if [ "$CANDIDATE_TMUX" = "$TMUX_SESSION" ]; then
          EXECUTOR_SPAWN_ID="$CANDIDATE_SPAWN"
          EXECUTOR_SESSION_ID="$CANDIDATE_SID"
          EXECUTOR_CALLSIGN="$CANDIDATE_CALLSIGN"
          EXECUTOR_WINDOW="$CANDIDATE_WINDOW"
        fi
        ;;
    esac
  fi
fi

if [ "$EXECUTOR_HTTP" = 200 ] && [ -n "$EXECUTOR_SESSION_ID" ]; then
  for _ in $(seq 1 30); do
    STATE=$(curl -s -m 2 "$BASE/state" 2>/dev/null || true)
    if STATE_JSON="$STATE" EXPECTED_SID="$EXECUTOR_SESSION_ID" node -e '
      try {
        const s = JSON.parse(process.env.STATE_JSON || "{}");
        const card = (s.sessions || []).find(x => x.session_id === process.env.EXPECTED_SID);
        process.exit(card?.spawn?.skip_permissions === true ? 0 : 1);
      } catch { process.exit(1); }
    '; then
      EXECUTOR_UNSUPERVISED=yes
      break
    fi
    sleep 1
  done
fi

if [ -n "$EXECUTOR_UNSUPERVISED" ]; then
  ok "executor spawned unsupervised"
else
  bad "executor spawned unsupervised" "HTTP $EXECUTOR_HTTP or spawn.skip_permissions was not true within 30s"
fi

# --------------------------------------------------------------- gate 7
EXECUTED=""
: > "$EXECUTOR_SAMPLES"
if [ -n "$EXECUTOR_SESSION_ID" ]; then
  for _ in $(seq 1 180); do
    STATE=$(curl -s -m 2 "$BASE/state" 2>/dev/null || true)
    STATE_JSON="$STATE" node -e '
      try {
        const s = JSON.parse(process.env.STATE_JSON || "{}");
        process.stdout.write(JSON.stringify(s) + "\n");
      } catch { process.exit(1); }
    ' >> "$EXECUTOR_SAMPLES" 2>/dev/null || true

    HAS_SYMBOL=""
    if UTIL_FILE="$UTIL_FILE" node -e '
      const fs = require("fs");
      try { process.exit(fs.readFileSync(process.env.UTIL_FILE, "utf8").includes("slugToTitle") ? 0 : 1); }
      catch { process.exit(1); }
    '; then
      HAS_SYMBOL=yes
    fi
    if [ -n "$HAS_SYMBOL" ] && UTIL_FILE="$UTIL_FILE" node -e '
      const u = require(process.env.UTIL_FILE);
      require("assert").strictEqual(u.slugToTitle("hello-world"), "Hello World");
    ' >/dev/null 2>&1; then
      EXECUTED=yes
      break
    fi
    sleep 1
  done
fi

NO_PERMISSION_CARD=""
if SAMPLES_FILE="$EXECUTOR_SAMPLES" EXPECTED_SID="$EXECUTOR_SESSION_ID" node -e '
  const fs = require("fs");
  try {
    const lines = fs.readFileSync(process.env.SAMPLES_FILE, "utf8").split("\n").filter(Boolean);
    if (!lines.length || !process.env.EXPECTED_SID) process.exit(1);
    const seen = lines.some(line => {
      const s = JSON.parse(line);
      return (s.questions || []).some(q =>
        q.session_id === process.env.EXPECTED_SID && q.kind === "permission" && q.status === "pending");
    });
    process.exit(seen ? 1 : 0);
  } catch { process.exit(1); }
'; then
  NO_PERMISSION_CARD=yes
fi

if [ -n "$EXECUTED" ] && [ -n "$NO_PERMISSION_CARD" ]; then
  ok "plan executed without any permission card"
else
  REASON="implementation/function check did not pass within 180s"
  [ -n "$NO_PERMISSION_CARD" ] || REASON="a pending executor permission question appeared, or no valid /state samples were collected"
  bad "plan executed without any permission card" "$REASON"
fi

# --------------------------------------------------------------- gate 8
MARK_HTTP=000
if [ -n "$PLAN_ID" ]; then
  MARK_BODY=$(node -e '
    process.stdout.write(JSON.stringify({status: "executed", via: "accept-script"}));
  ')
  MARK_HTTP=$(curl -sS -m 10 -o "$SCRATCH_HOME/mark-plan.json" -w '%{http_code}' \
    -X POST "$BASE/api/plans/$PLAN_ID/mark" -H 'content-type: application/json' \
    -d "$MARK_BODY" 2>/dev/null || true)
fi

if [ "$MARK_HTTP" = 200 ]; then
  ok "plan marked executed"
else
  bad "plan marked executed" "HTTP $MARK_HTTP"
fi

# --------------------------------------------------------------- gate 9
cleanup_resources
CLEANUP_OK=yes
[ -f "$UTIL_FILE" ] || CLEANUP_OK=""
if [ -f "$UTIL_FILE" ] && [ -f "$SEED_UTIL" ] && ! cmp -s "$UTIL_FILE" "$SEED_UTIL"; then
  CLEANUP_OK=""
fi
[ ! -e "$TEST_FILE" ] || CLEANUP_OK=""
if [ -n "$DAEMON_PID" ] && kill -0 "$DAEMON_PID" 2>/dev/null; then
  CLEANUP_OK=""
fi
if command -v tmux >/dev/null 2>&1; then
  scoped_window_exists "$PLANNER_WINDOW" && CLEANUP_OK=""
  scoped_window_exists "$EXECUTOR_WINDOW" && CLEANUP_OK=""
  scoped_session_exists && CLEANUP_OK=""
fi

if [ -n "$CLEANUP_OK" ]; then
  ok "cleanup"
else
  bad "cleanup" "demo files, daemon, or scoped fleetdeck-4711 tmux resources remain"
fi

echo
echo "RESULT: $PASS pass, $FAIL fail"
[ "$FAIL" -eq 0 ]
