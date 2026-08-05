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
FLEET_HOOK_SCRIPT="$FLEETDECK_ROOT/scripts/fleet-hook.mjs"
FLEETDECK_PORT=4711
SCRATCH_HOME="$FLEETDECK_ROOT/.fleetdeck-test"
BASE="http://127.0.0.1:$FLEETDECK_PORT"
TMUX_SESSION="fleetdeck-$FLEETDECK_PORT"
WINDOW_PREFIX="fd$FLEETDECK_PORT-"
DAEMON_LOG="$SCRATCH_HOME/fleetd.log"
PLAN_FILE="$SCRATCH_HOME/plan.md"
EXECUTOR_SAMPLES="$SCRATCH_HOME/executor-state-samples.jsonl"

# Isolated tmux server for this run, NEVER the user's default server: tmux
# bakes the first client's environment into a new server's global env, and
# every later window inherits it — a test-env daemon on the default socket
# would poison production spawns. spawn.mjs honors this env and runs all its
# tmux calls as `tmux -L $FLEETDECK_TMUX_SOCKET`.
export FLEETDECK_TMUX_SOCKET="fdaccept-$$"

# Claude-session env vars that must never leak into the daemon this script
# launches: a daemon (or tmux server) that inherits them can mislead later
# spawns into reporting to the wrong fleet. Passed to `env` as -u flags.
CLAUDE_ENV_SCRUB=(
  -u CLAUDECODE -u CLAUDE_CODE_SESSION_ID -u CLAUDE_CODE_CHILD_SESSION
  -u CLAUDE_CODE_BRIDGE_SESSION_ID -u CLAUDE_CODE_ENTRYPOINT
  -u CLAUDE_CODE_EXECPATH -u CLAUDE_ENV_FILE -u CLAUDE_PROJECT_DIR
  -u CLAUDE_PLUGIN_ROOT -u CLAUDE_PLUGIN_DATA -u CLAUDE_EFFORT
  -u AI_AGENT -u CODEX_COMPANION_SESSION_ID -u CODEX_COMPANION_TRANSCRIPT_PATH
  -u TMUX -u TMUX_PANE
)

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
ARM_TOKEN=""
CLEANUP_DONE=0
PROJECT_SNAPSHOT=""

ok() {
  echo "PASS: $1"
  PASS=$((PASS + 1))
}

bad() {
  echo "FAIL: $1${2:+ -- $2}"
  FAIL=$((FAIL + 1))
}

scoped_session_exists() {
  tmux -L "$FLEETDECK_TMUX_SOCKET" list-sessions -F '#{session_name}' \
    -f "#{==:#{session_name},$TMUX_SESSION}" 2>/dev/null |
    grep -Fxq "$TMUX_SESSION"
}

scoped_window_exists() {
  local window="${1:-}"
  [ -n "$window" ] || return 1
  case "$window" in
    "$WINDOW_PREFIX"*) ;;
    *) return 1 ;;
  esac
  tmux -L "$FLEETDECK_TMUX_SOCKET" list-windows -t "$TMUX_SESSION" -F '#{window_name}' \
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

  # Direct tmux cleanup targets this run's own isolated server (-L) only;
  # the user's default tmux server is never touched.
  if command -v tmux >/dev/null 2>&1; then
    tmux -L "$FLEETDECK_TMUX_SOCKET" kill-server 2>/dev/null || true
  fi

  # Restore the pre-run bytes of every project file this gate touches (see
  # snapshot_project_files). Restoring the seed here instead would erase any
  # uncommitted local work the run overwrote.
  if [ -n "$PROJECT_SNAPSHOT" ] && [ -d "$PROJECT_SNAPSHOT" ]; then
    if [ -f "$PROJECT_SNAPSHOT/util.js" ]; then
      cp "$PROJECT_SNAPSHOT/util.js" "$UTIL_FILE" 2>/dev/null || true
    else
      rm -f "$UTIL_FILE"
    fi
    if [ -f "$PROJECT_SNAPSHOT/test.js" ]; then
      cp "$PROJECT_SNAPSHOT/test.js" "$TEST_FILE" 2>/dev/null || true
    else
      rm -f "$TEST_FILE"
    fi
    if [ -f "$PROJECT_SNAPSHOT/claude-settings.json" ]; then
      mkdir -p "$PROJECT_DIR/.claude"
      cp "$PROJECT_SNAPSHOT/claude-settings.json" "$PROJECT_DIR/.claude/settings.json" 2>/dev/null || true
    else
      rm -f "$PROJECT_DIR/.claude/settings.json"
    fi
    PROJECT_SNAPSHOT=""
  fi
  # Sweep any lingering pre-run backup dirs (this run's, or a killed run's
  # leftover that this run did not adopt). Runs AFTER the restore above —
  # the snapshot lives inside PROJECT_DIR and matches the same glob.
  if [ -d "$PROJECT_DIR" ]; then
    find "$PROJECT_DIR" -mindepth 1 -maxdepth 1 -name '.pre-accept-*' -exec rm -rf {} + 2>/dev/null || true
  fi

  if [ -n "$DAEMON_PID" ] && kill -0 "$DAEMON_PID" 2>/dev/null; then
    kill "$DAEMON_PID" 2>/dev/null || true
    for _ in $(seq 1 20); do
      kill -0 "$DAEMON_PID" 2>/dev/null || break
      sleep 0.25
    done
  fi
}

# The three project files the gate's setup mutates. The settings file lives
# under $PROJECT_DIR/.claude; the project itself is the executor's cwd, and
# its content is what the gate both clobbers and verifies.
SETTINGS_FILE="$PROJECT_DIR/.claude/settings.json"

# Snapshot every project file this gate overwrites (BUG-012). Run BEFORE any
# setup mutation; cleanup_resources restores these exact bytes, so a developer
# with uncommitted util.js edits, a local test.js, or project .claude settings
# never loses them to the gate. Files that did not exist pre-run are restored
# to nonexistence; any leftover snapshot from a previously killed run is taken
# as the true pre-run state instead of a snapshot of the wreckage.
snapshot_project_files() {
  local existing
  existing=$(find "$PROJECT_DIR" -mindepth 1 -maxdepth 1 -type d -name '.pre-accept-*' -print -quit 2>/dev/null)
  if [ -n "$existing" ]; then
    PROJECT_SNAPSHOT="$existing"
    return 0
  fi
  PROJECT_SNAPSHOT=$(mktemp -d "$PROJECT_DIR/.pre-accept-XXXXXX") || return 1
  [ ! -f "$UTIL_FILE" ] || cp -p "$UTIL_FILE" "$PROJECT_SNAPSHOT/util.js"
  [ ! -f "$TEST_FILE" ] || cp -p "$TEST_FILE" "$PROJECT_SNAPSHOT/test.js"
  [ ! -f "$SETTINGS_FILE" ] || cp -p "$SETTINGS_FILE" "$PROJECT_SNAPSHOT/claude-settings.json"
}

# The destructive setup steps, factored out so the BUG-012 regression harness
# exercises exactly what the live gate runs: seed-copy util.js, delete
# test.js, regenerate hook settings, and (later) let the executor rewrite
# util.js and create test.js.
apply_gate_fixture() {
  mkdir -p "$SCRATCH_HOME" "$PROJECT_DIR/.claude"
  cp "$SEED_UTIL" "$UTIL_FILE"
  rm -f "$TEST_FILE"
  echo "{\"gate\":\"hook-settings\",\"base\":\"$BASE\"}" > "$SETTINGS_FILE"
}

# Standalone BUG-012 regression harness. Not used by the live gate; the test
# suite (tests/accept-plan-snapshot.test.mjs) generates it by invoking this
# script as `bash run-accept-plan.sh --emit-snapshot-harness <out>`, seeds
# local content in a copied project dir, runs the harness, and asserts the
# pre-run bytes and existence state survive. The harness sources the real
# snapshot/mutate/restore code by extracting it from this file, so it can
# never drift from the gate. Usage: bash <harness> [restore|legacy] —
# "legacy" replays the pre-fix behavior (seed restore, no settings restore)
# to prove the test catches the original defect.
if [ "${1:-}" = "--emit-snapshot-harness" ]; then
  {
    sed -n '/^SCRIPT_DIR=/,/^EXECUTOR_SAMPLES=/p' "$0"
    echo 'MODE="${1:-restore}"'
    sed -n '/^SETTINGS_FILE=/p' "$0"
    echo 'PROJECT_SNAPSHOT=""'
    sed -n '/^snapshot_project_files() {/,/^}$/p' "$0"
    sed -n '/^apply_gate_fixture() {/,/^}$/p' "$0"
    cat <<'HARNESS'
snapshot_project_files
apply_gate_fixture
# executor phase: the unsupervised spawn rewrites util.js and creates test.js
echo "// executor edit" >> "$UTIL_FILE"
echo "// executor test" > "$TEST_FILE"
if [ "$MODE" = "legacy" ]; then
  # pre-BUG-012-fix restore: seed bytes, no test.js, settings left clobbered
  if [ -f "$SEED_UTIL" ]; then cp "$SEED_UTIL" "$UTIL_FILE" 2>/dev/null || true; fi
  rm -f "$TEST_FILE"
  exit 0
fi
if [ -f "$PROJECT_SNAPSHOT/util.js" ]; then cp "$PROJECT_SNAPSHOT/util.js" "$UTIL_FILE"; else rm -f "$UTIL_FILE"; fi
if [ -f "$PROJECT_SNAPSHOT/test.js" ]; then cp "$PROJECT_SNAPSHOT/test.js" "$TEST_FILE"; else rm -f "$TEST_FILE"; fi
if [ -f "$PROJECT_SNAPSHOT/claude-settings.json" ]; then cp "$PROJECT_SNAPSHOT/claude-settings.json" "$SETTINGS_FILE"; else rm -f "$SETTINGS_FILE"; fi
rm -rf "$PROJECT_SNAPSHOT"
HARNESS
  } > "${2:?usage: run-accept-plan.sh --emit-snapshot-harness <output-file>}"
  exit 0
fi

trap cleanup_resources EXIT ERR
trap 'cleanup_resources; exit 130' INT

echo "== Fleet Deck v1.3 live plan acceptance =="

# --------------------------------------------------------------- reset
<<<<<<< /tmp/mf-ours
# Stop recorded daemons only after their fleetd identity is proven (strict
# pidfile + /health.pid + /proc shape — the production verifyDaemonPid gate).
# A legacy plain-PID pidfile can name a PID the OS has since recycled for an
# unrelated process; those are never signalled (BUG-008). Then clear an orphan
# listener only when Fleet Deck's health endpoint proves the process on the
# port is Fleet Deck.
REAL_HOME="${HOME:-/root}/.fleetdeck"
. "$SCRIPT_DIR/lib/kill-verified-daemon.sh"
stop_pidfile_daemon "$REAL_HOME" || { echo "ABORT: unowned live pid in $REAL_HOME/fleetd.pid — not touching it."; exit 1; }
stop_pidfile_daemon "$SCRATCH_HOME" || { echo "ABORT: unowned live pid in $SCRATCH_HOME/fleetd.pid — not touching it."; exit 1; }
if curl -s -m 1 "$BASE/health" 2>/dev/null | grep -q '"ok"'; then
  fuser -k "$FLEETDECK_PORT/tcp" 2>/dev/null || true
  sleep 0.5
=======
# Signal only a daemon proven by ALL THREE identities: the strict JSON pid
# record under the pidfile's home, a /health reply on this port that reports
# the same pid, and a live node+fleetd process shape. NEVER kill by port
# (fuser -k kills every client of the port, and a substring health grep
# matches any body containing "ok" — including {"ok":false}); any listener
# that cannot be positively identified aborts the run instead.
stop_identified_daemon() {
  local pidfile="$1"
  [ -f "$pidfile" ] || return 0
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
      // /health must report the pid recorded in the pidfile. Two resets can race (the real
      // home and the scratch home recording the same daemon), so a port that
      // goes silent mid-poll also satisfies the proof — the identified daemon
      // is already gone. Any OTHER pid on the port is a refusal.
      let health = null;
      for (let i = 0; i < 20; i += 1) {
        try {
          const res = await fetch(`http://127.0.0.1:${expectedPort}/health`, { signal: AbortSignal.timeout(250) });
          const candidate = res.ok ? await res.json() : null;
          if (candidate?.pid === record.pid) { health = candidate; break; }
          if (candidate) { process.exitCode = 2; return; }
        } catch {}
        if (!live(record.pid)) return;
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
      catch (err) { if (err?.code !== "ESRCH") { process.exitCode = 2; return; } }
      for (let i = 0; i < 30; i += 1) {
        await sleep(100);
        if (!live(record.pid)) return;
      }
      // Never escalate to SIGKILL: a graceful shutdown that cannot be proven
      // aborts the run instead of risking a recycled PID.
      process.exitCode = 2;
    })().catch(() => { process.exitCode = 2; });
  ' "$pidfile" "$FLEETDECK_PORT" >/dev/null 2>&1
}
REAL_HOME="${HOME:-/root}/.fleetdeck"
if ! stop_identified_daemon "$REAL_HOME/fleetd.pid"; then
  echo "ABORT: daemon recorded in $REAL_HOME/fleetd.pid could not be positively identified and stopped."
  exit 1
fi
if ! stop_identified_daemon "$SCRATCH_HOME/fleetd.pid"; then
  echo "ABORT: daemon recorded in $SCRATCH_HOME/fleetd.pid could not be positively identified and stopped."
  exit 1
fi
if curl -s -m 1 "$BASE/health" >/dev/null 2>&1; then
  echo "ABORT: something is still listening on :$FLEETDECK_PORT after reset; refusing to kill an unidentified listener."
  exit 1
>>>>>>> /tmp/mf-theirs
fi

# Reset only this run's isolated tmux server (a per-pid socket, so normally a
# no-op); never enumerate or touch the default server's sessions or windows.
if command -v tmux >/dev/null 2>&1; then
  tmux -L "$FLEETDECK_TMUX_SOCKET" kill-server 2>/dev/null || true
fi

# Gate 1 launches a scratch daemon and then lets it spawn and control real
# (billed) Claude sessions. Readiness is therefore bound to the child this
# script launches: if anything still owns the port after the reset, refuse
# outright rather than risk steering someone else's live fleet below.
if curl -s -m 1 "$BASE/health" >/dev/null 2>&1; then
  echo "FATAL: port $FLEETDECK_PORT still answers after reset; refusing to run the plan gate against an unowned listener" >&2
  exit 1
fi

rm -rf "$SCRATCH_HOME"
mkdir -p "$SCRATCH_HOME"
if ! snapshot_project_files; then
  echo "FAIL: could not snapshot project files under $PROJECT_DIR -- refusing to run the gate" >&2
  exit 1
fi
apply_gate_fixture

# Regenerate the proven local demo hook wiring. The Stop command hook keeps
<<<<<<< /tmp/mf-ours
# the v1.1 asyncRewake fields verbatim from hooks/hooks.json. Every other
# hook uses the current checkout's authenticated command shim: native HTTP
# hooks cannot attach the bearer token required since 0.16.0, and the
# daemon's legacy unauthenticated /hook/* refusal would silently swallow
# every event. enabledPlugins disables any installed Fleet Deck plugin so
# its duplicate hooks can never mask the checkout under test with cached
# code.
cat > "$PROJECT_DIR/.claude/settings.json" <<EOF
=======
# the v1.1 asyncRewake fields verbatim from hooks/hooks.json.
cat > "$SETTINGS_FILE" <<EOF
>>>>>>> /tmp/mf-theirs
{
  "enabledPlugins": { "fleetdeck@fleetdeck": false },
  "hooks": {
    "SessionStart": [
      { "hooks": [{ "type": "command", "command": "node $SESSIONSTART_SCRIPT", "timeout": 15 }] }
    ],
    "UserPromptSubmit": [
      { "hooks": [{ "type": "command", "command": "node $FLEET_HOOK_SCRIPT UserPromptSubmit", "timeout": 3 }] }
    ],
    "PostToolUse": [
      { "matcher": "Edit|Write|MultiEdit|NotebookEdit|Bash", "hooks": [{ "type": "command", "command": "node $FLEET_HOOK_SCRIPT PostToolUse", "timeout": 3 }] }
    ],
    "PreToolUse": [
      { "matcher": "AskUserQuestion", "hooks": [{ "type": "command", "command": "node $FLEET_HOOK_SCRIPT AskUserQuestion", "timeout": 65 }] }
    ],
    "PermissionRequest": [
      { "hooks": [{ "type": "command", "command": "node $FLEET_HOOK_SCRIPT PermissionRequest", "timeout": 65 }] }
    ],
    "Elicitation": [
      { "hooks": [{ "type": "command", "command": "node $FLEET_HOOK_SCRIPT Elicitation", "timeout": 65 }] }
    ],
    "Notification": [
      { "hooks": [{ "type": "command", "command": "node $FLEET_HOOK_SCRIPT Notification", "timeout": 3, "async": true }] }
    ],
    "Stop": [
      { "hooks": [
        { "type": "command", "command": "node $FLEET_HOOK_SCRIPT Stop", "timeout": 5 },
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
      { "hooks": [{ "type": "command", "command": "node $FLEET_HOOK_SCRIPT SessionEnd", "timeout": 3, "async": true }] }
    ],
    "FileChanged": [
      { "hooks": [{ "type": "command", "command": "node $FLEET_HOOK_SCRIPT FileChanged", "timeout": 3, "async": true }] }
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
  env -u FLEETDECK_SPAWN_CMD "${CLAUDE_ENV_SCRUB[@]}" \
    FLEETDECK_HOME="$SCRATCH_HOME" FLEETDECK_PORT="$FLEETDECK_PORT" \
    FLEETDECK_TMUX_SOCKET="$FLEETDECK_TMUX_SOCKET" \
    node "$FLEETDECK_ROOT/scripts/fleetd/fleetd.mjs" > "$DAEMON_LOG" 2>&1 &
  DAEMON_PID=$!
  for _ in $(seq 1 40); do
    # Readiness must name THIS run's child, not merely any qualifying /state:
    # /health.pid has to equal DAEMON_PID and the scratch pidfile has to
    # record the same pid on this port. A listener that survives the reset or
    # a supervisor restart can otherwise answer first (the scratch child then
    # loses the bind and exits 3) and the billed gates below would steer that
    # foreign fleet.
    HEALTH=$(curl -s -m 1 "$BASE/health" 2>/dev/null || true)
    if HEALTH_JSON="$HEALTH" EXPECTED_PID="$DAEMON_PID" EXPECTED_PORT="$FLEETDECK_PORT" \
      PID_FILE="$SCRATCH_HOME/fleetd.pid" node -e '
      const fs = require("fs");
      try {
        const h = JSON.parse(process.env.HEALTH_JSON || "{}");
        const expectedPid = Number(process.env.EXPECTED_PID);
        if (h.ok !== true || h.spawn?.available !== true || h.pid !== expectedPid) process.exit(1);
        const record = JSON.parse(fs.readFileSync(process.env.PID_FILE, "utf8"));
        process.exit(record?.pid === expectedPid && record?.port === Number(process.env.EXPECTED_PORT) ? 0 : 1);
      } catch { process.exit(1); }
    '; then
      DAEMON_READY=yes
      break
    fi
    # The child losing the bind (EADDRINUSE) is terminal: whatever answered
    # above is not ours, so abort instead of running the plan gates against it.
    if ! kill -0 "$DAEMON_PID" 2>/dev/null; then
      echo "FATAL: scratch fleetd (pid $DAEMON_PID) exited before becoming ready; the port is owned by another listener — aborting to avoid steering a foreign fleet" >&2
      exit 1
    fi
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

# The bearer the daemon minted at boot (FLEETDECK_HOME/token — the same file
# the hook shims read). Needed for the token-gated powers this gate exercises:
# arming an unsupervised spawn (POST /api/spawn/arm-unsupervised).
TOKEN="$(cat "$SCRATCH_HOME/token" 2>/dev/null || true)"
if [ -n "$DAEMON_READY" ] && [ -z "$TOKEN" ]; then
  bad "daemon bearer token" "$SCRATCH_HOME/token missing or empty"
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

if [ -s "$PLAN_FILE" ] && [ -n "$TOKEN" ]; then
  # Unsupervised spawns are refused (403) without a fresh single-use arm token
  # — the API half of the board's two-step confirmation. Mint one over the
  # bearer-gated arm endpoint and fail this gate immediately if refused.
  ARM_HTTP=$(curl -sS -m 10 -o "$SCRATCH_HOME/arm-unsupervised.json" -w '%{http_code}' \
    -X POST "$BASE/api/spawn/arm-unsupervised" \
    -H 'content-type: application/json' -H "authorization: Bearer $TOKEN" \
    -d '{}' 2>/dev/null || true)
  ARM_TOKEN=$(ARM_JSON_FILE="$SCRATCH_HOME/arm-unsupervised.json" node -e '
    const fs = require("fs");
    try {
      const r = JSON.parse(fs.readFileSync(process.env.ARM_JSON_FILE, "utf8"));
      if (typeof r.arm_token !== "string" || !r.arm_token) process.exit(1);
      process.stdout.write(r.arm_token);
    } catch { process.exit(1); }
  ' 2>/dev/null || true)
  if [ "$ARM_HTTP" != 200 ] || [ -z "$ARM_TOKEN" ]; then
    bad "arm unsupervised spawn" "HTTP $ARM_HTTP or no arm_token in response"
  fi
fi

if [ -s "$PLAN_FILE" ] && [ -n "$ARM_TOKEN" ]; then
  EXECUTOR_BODY=$(PROJECT_DIR="$PROJECT_DIR" PLAN_FILE="$PLAN_FILE" ARM_TOKEN="$ARM_TOKEN" node -e '
    const fs = require("fs");
    const plan = fs.readFileSync(process.env.PLAN_FILE, "utf8");
    process.stdout.write(JSON.stringify({
      cwd: process.env.PROJECT_DIR,
      dangerously_skip_permissions: true,
      arm_token: process.env.ARM_TOKEN,
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
  REASON="HTTP $EXECUTOR_HTTP or spawn.skip_permissions was not true within 30s"
  [ -n "$ARM_TOKEN" ] || REASON="unsupervised arm token was refused, so the executor spawn was never attempted"
  bad "executor spawned unsupervised" "$REASON"
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
# Mark the plan executed ONLY when gate 7 actually proved execution: the
# implementation + function artifacts passed mechanical validation (EXECUTED)
# and no executor permission card was observed across the sampled boundary
# (NO_PERMISSION_CARD). Marking on PLAN_ID alone would record failed or
# never-ran work as executed — and against the wrong daemon could corrupt a
# real production plan's state.
MARK_HTTP=000
if [ -n "$PLAN_ID" ] && [ -n "$EXECUTED" ] && [ -n "$NO_PERMISSION_CARD" ]; then
  MARK_BODY=$(node -e '
    process.stdout.write(JSON.stringify({status: "executed", via: "accept-script"}));
  ')
  MARK_HTTP=$(curl -sS -m 10 -o "$SCRATCH_HOME/mark-plan.json" -w '%{http_code}' \
    -X POST "$BASE/api/plans/$PLAN_ID/mark" -H 'content-type: application/json' \
    -d "$MARK_BODY" 2>/dev/null || true)
fi

if [ -z "$PLAN_ID" ]; then
  bad "plan marked executed" "no plan ID was captured"
elif [ -z "$EXECUTED" ] || [ -z "$NO_PERMISSION_CARD" ]; then
  bad "plan marked executed" "gate 7 did not prove execution; refusing to mark a non-executed plan"
elif [ "$MARK_HTTP" = 200 ]; then
  ok "plan marked executed"
else
  bad "plan marked executed" "HTTP $MARK_HTTP"
fi

# --------------------------------------------------------------- gate 9
# Verify cleanup restored the exact pre-run bytes and existence state. Stash
# a copy of the snapshot under SCRATCH_HOME first — cleanup_resources removes
# the in-project snapshot dir as part of its restore. Note: re-running the
# gate re-seeds these files from .seed before the next snapshot, so a run
# whose pre-run state was a previous run's wreckage restores that wreckage —
# this gate compares against the snapshot, not the seed.
SNAPSHOT_CHECK="$SCRATCH_HOME/pre-run-snapshot"
rm -rf "$SNAPSHOT_CHECK"
if [ -n "$PROJECT_SNAPSHOT" ] && [ -d "$PROJECT_SNAPSHOT" ]; then
  cp -r "$PROJECT_SNAPSHOT" "$SNAPSHOT_CHECK" 2>/dev/null || true
fi

cleanup_resources
CLEANUP_OK=yes
if [ -d "$SNAPSHOT_CHECK" ]; then
  if [ -f "$SNAPSHOT_CHECK/util.js" ]; then
    cmp -s "$UTIL_FILE" "$SNAPSHOT_CHECK/util.js" || CLEANUP_OK=""
  else
    [ ! -e "$UTIL_FILE" ] || CLEANUP_OK=""
  fi
  if [ -f "$SNAPSHOT_CHECK/test.js" ]; then
    cmp -s "$TEST_FILE" "$SNAPSHOT_CHECK/test.js" || CLEANUP_OK=""
  else
    [ ! -e "$TEST_FILE" ] || CLEANUP_OK=""
  fi
  if [ -f "$SNAPSHOT_CHECK/claude-settings.json" ]; then
    cmp -s "$SETTINGS_FILE" "$SNAPSHOT_CHECK/claude-settings.json" || CLEANUP_OK=""
  else
    [ ! -e "$SETTINGS_FILE" ] || CLEANUP_OK=""
  fi
else
  CLEANUP_OK=""
fi
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
  bad "cleanup" "demo files, daemon, or isolated tmux server resources remain"
fi

echo
echo "RESULT: $PASS pass, $FAIL fail"
[ "$FAIL" -eq 0 ]
