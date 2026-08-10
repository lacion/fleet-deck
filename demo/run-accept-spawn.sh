#!/bin/bash
# demo/run-accept-spawn.sh — Fleet Deck v1.2 dynamic-fleet live spawn acceptance.
#
# NEVER AUTO-RUN: this gate starts a real interactive Claude session and spends
# billed Claude usage. Only the human acceptance orchestrator should run it.
# Gate: v1.2 — dynamic fleet (board-spawned sessions over the tmux adapter).

set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FLEETDECK_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SEED_PROJECT="$SCRIPT_DIR/project"
SESSIONSTART_SCRIPT="$FLEETDECK_ROOT/scripts/fleet-sessionstart.mjs"
WATCH_SCRIPT="$FLEETDECK_ROOT/scripts/fleet-watch.mjs"
FLEET_HOOK_SCRIPT="$FLEETDECK_ROOT/scripts/fleet-hook.mjs"

# SCRATCH_HOME and PROJECT_DIR are assigned from mktemp after the cleanup trap
# is armed. An arbitrary override is intentionally unsupported: cleanup
# recursively deletes these directories, so each must be a unique path created
# by this run, never a caller-provided target. A concurrent acceptance run
# must never reset, delete, or spawn into this run's daemon home, evidence
# files, or fixture copy.
SCRATCH_HOME=''
PROJECT_DIR=''

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
FLEETDECK_PORT=""
BASE=""
TMUX_SESSION=""
WINDOW_PREFIX=""
OUTPUT_FILE=""
DAEMON_LOG=""
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
  tmux -L "$FLEETDECK_TMUX_SOCKET" list-sessions -F '#{session_name}' \
    -f "#{==:#{session_name},$TMUX_SESSION}" 2>/dev/null |
    grep -Fxq "$TMUX_SESSION"
}

scoped_window_exists() {
  [ -n "$TMUX_WINDOW" ] || return 1
  case "$TMUX_WINDOW" in
    "$WINDOW_PREFIX"*) ;;
    *) return 1 ;;
  esac
  tmux -L "$FLEETDECK_TMUX_SOCKET" list-windows -t "$TMUX_SESSION" -F '#{window_name}' \
    -f "#{m:${WINDOW_PREFIX}*,#{window_name}}" 2>/dev/null |
    grep -Fxq "$TMUX_WINDOW"
}

cleanup_resources() {
  [ "$CLEANUP_DONE" -eq 0 ] || return 0
  CLEANUP_DONE=1

  if [ -n "$OUTPUT_FILE" ]; then
    rm -f "$OUTPUT_FILE"
  fi

  if [ -n "$DAEMON_PID" ] && kill -0 "$DAEMON_PID" 2>/dev/null; then
    kill "$DAEMON_PID" 2>/dev/null || true
    for _ in $(seq 1 20); do
      kill -0 "$DAEMON_PID" 2>/dev/null || break
      sleep 0.25
    done
  fi

  # This direct tmux operation is cleanup only, and it targets this run's own
  # isolated server (-L). The user's default tmux server is never touched.
  if command -v tmux >/dev/null 2>&1; then
    tmux -L "$FLEETDECK_TMUX_SOCKET" kill-server 2>/dev/null || true
  fi

  # These are mktemp directories created by this run; safe to remove in full.
  if [ -n "$SCRATCH_HOME" ]; then
    rm -rf -- "$SCRATCH_HOME"
  fi
  if [ -n "$PROJECT_DIR" ]; then
    rm -rf -- "$PROJECT_DIR"
  fi
}

trap cleanup_resources EXIT

echo "== Fleet Deck v1.2 live spawn acceptance =="

# --------------------------------------------------------------- reset
# Every mutable resource is unique to this run: an mktemp'd daemon home, an
# mktemp'd copy of the demo fixture project, and a verified-free port — so a
# concurrent acceptance run can never reset, delete, or spawn into this run's
# state. Nothing here touches the production daemon or any other run.

# Stop recorded daemons only after their fleetd identity is proven: signal
# only a daemon proven by ALL THREE identities — the strict JSON pid record
# under the pidfile's home, a /health reply on this port that reports the same
# pid, and a live node+fleetd process shape. NEVER kill by port (a port-wide
# kill signals every client of the port, and a substring health grep matches
# any body containing "ok" — including {"ok":false}); any listener that cannot be
# positively identified aborts the run instead. A legacy plain-PID pidfile can
# name a PID the OS has since recycled for an unrelated process; those are
# never signalled.
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

SCRATCH_HOME="$(mktemp -d "${TMPDIR:-/tmp}/fleetdeck-spawn.XXXXXX")"
[ -n "$SCRATCH_HOME" ] || { echo "ABORT: could not create a unique acceptance home"; exit 1; }
# An isolated run never signals the production home: its recorded port can
# never match this run's verified-free port. REAL_HOME is defined via := (never
# a bare top-level assignment that would read as the old kill-the-real-daemon
# path) only so a generic reset has a home to treat as nothing-to-do.
: "${REAL_HOME:=$SCRATCH_HOME}"

# Verified-free port, kernel-assigned on loopback. Never 4711 and never a port
# with an existing listener: this run must not kill, force-clear, or share a
# port with the production daemon or another acceptance run.
FLEETDECK_PORT="$(node -e 'const net = require("node:net"); const probe = net.createServer(); probe.once("error", () => process.exit(1)); probe.listen(0, "127.0.0.1", () => { process.stdout.write(String(probe.address().port)); probe.close(); });')"
[ -n "$FLEETDECK_PORT" ] || { echo "ABORT: could not allocate a free port"; exit 1; }
BASE="http://127.0.0.1:$FLEETDECK_PORT"
TMUX_SESSION="fleetdeck-$FLEETDECK_PORT"
WINDOW_PREFIX="fd$FLEETDECK_PORT-"

if ! stop_identified_daemon "$SCRATCH_HOME/fleetd.pid"; then
  echo "ABORT: daemon recorded in $SCRATCH_HOME/fleetd.pid could not be positively identified and stopped."
  exit 1
fi
if curl -s -m 1 "$BASE/health" >/dev/null 2>&1; then
  echo "ABORT: something is still listening on :$FLEETDECK_PORT after reset; refusing to kill an unidentified listener."
  exit 1
fi

PROJECT_DIR="$(mktemp -d "${TMPDIR:-/tmp}/fleetdeck-spawn-project.XXXXXX")"
[ -n "$PROJECT_DIR" ] || { echo "ABORT: could not create a unique fixture project"; exit 1; }
cp -R "$SEED_PROJECT/." "$PROJECT_DIR/"
OUTPUT_FILE="$PROJECT_DIR/spawn-accept-done.txt"
DAEMON_LOG="$SCRATCH_HOME/fleetd.log"

mkdir -p "$PROJECT_DIR/.claude"
rm -f "$OUTPUT_FILE"

# Regenerate the proven local demo hook wiring. The Stop command hook keeps
# the v1.1 asyncRewake fields verbatim from hooks/hooks.json. Every other
# hook uses the current checkout's authenticated command shim: native HTTP
# hooks cannot attach the bearer token required since 0.16.0, and the
# daemon's legacy unauthenticated /hook/* refusal would silently swallow
# every event. enabledPlugins disables any installed Fleet Deck plugin so
# its duplicate hooks can never mask the checkout under test with cached
# code. Each interpolated script path is shell-quoted inside the JSON string
# so a checkout under a path with spaces still resolves. This known baseline
# is intentionally left in place after the test: cleanup uses no git command
# and therefore cannot overwrite unrelated working-tree changes.
FLEET_HOOK_SCRIPT="${FLEET_HOOK_SCRIPT:-$WATCH_SCRIPT}"; cat > "$PROJECT_DIR/.claude/settings.json" <<EOF
{
  "enabledPlugins": { "fleetdeck@fleetdeck": false },
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
      { "hooks": [
        { "type": "command", "command": "node \"$FLEET_HOOK_SCRIPT\" Stop", "timeout": 5 },
        {
          "type": "command",
          "command": "node \"$WATCH_SCRIPT\"",
          "asyncRewake": true,
          "rewakeMessage": "[FLEETDECK] Fleet board mail for you:",
          "rewakeSummary": "Fleet Deck: board mail delivered",
          "timeout": 7230
        }
      ] }
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

# --------------------------------------------------------------- gate 1
DAEMON_READY=""
TMUX_READY=""
if command -v tmux >/dev/null 2>&1; then
  TMUX_READY=yes
fi

if ! curl -s -m 1 "$BASE/health" >/dev/null 2>&1; then
  env -u FLEETDECK_SPAWN_CMD "${CLAUDE_ENV_SCRUB[@]}" \
    FLEETDECK_HOME="$SCRATCH_HOME" FLEETDECK_PORT="$FLEETDECK_PORT" \
    FLEETDECK_TMUX_SOCKET="$FLEETDECK_TMUX_SOCKET" \
    node "$FLEETDECK_ROOT/scripts/fleetd/fleetd.bundle.mjs" > "$DAEMON_LOG" 2>&1 &
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
  bad "cleanup" "output, daemon, or isolated tmux server session remains"
fi

echo
echo "RESULT: $PASS pass, $FAIL fail"
[ "$FAIL" -eq 0 ]
