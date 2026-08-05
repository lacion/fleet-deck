#!/bin/bash
# demo/run-accept-phase3.sh — Fleet Deck Phase 3 live acceptance.
#
# Gate (Phase 3 accept criteria):
#   (1) a permission prompt is approved from the board and the terminal never
#       asks;
#   (2) an idle session's trailing question shows on the board and a board
#       answer reaches the session at its next boundary.
#
# Spends real Claude usage (two short `claude -p` runs + one resume).
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FLEETDECK_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SEED_PROJECT="$SCRIPT_DIR/project"
SESSIONSTART_SCRIPT="$FLEETDECK_ROOT/scripts/fleet-sessionstart.mjs"
FLEET_HOOK_SCRIPT="$FLEETDECK_ROOT/scripts/fleet-hook.mjs"
FLEETDECK_PORT="${FLEETDECK_PORT:-4711}"
SCRATCH_HOME="${FLEETDECK_HOME_OVERRIDE:-$FLEETDECK_ROOT/.fleetdeck-test}"
BASE="http://127.0.0.1:$FLEETDECK_PORT"

# Isolated tmux server for THIS run only, never the user's default server.
# The fleetd elected by the sessions' SessionStart hook inherits this env and
# runs all its tmux calls as `tmux -L $FLEETDECK_TMUX_SOCKET`. Without it, a
# test-env daemon starting the default tmux server would bake FLEETDECK_*
# test values into that server's global env — poisoning every window (and
# production spawn) created there later.
export FLEETDECK_TMUX_SOCKET="fdaccept-$$"

# Stop the scratch daemon this run's SessionStart hooks elect (if one comes
# up): the SessionStart-launched fleetd runs detached and unref'd, so nothing
# else will ever reclaim it. Signal only the process proven by BOTH this run's
# strict pid record and the pid /health reports on this run's port, then wait
# for it to actually exit. Any doubt returns nonzero — cleanup leaves the
# process alone rather than signalling something it cannot positively identify.
stop_scratch_daemon() {
  local pidfile="$SCRATCH_HOME/fleetd.pid"
  # A SessionStart-elected daemon may still be booting (pidfile not yet
  # written) when the gate exits. Give the strict pid record a moment to land;
  # a listener with NO pid record is unprovable — fail cleanup, touch nothing.
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    [ -f "$pidfile" ] && break
    if curl -fsS --max-time 0.2 "http://127.0.0.1:$FLEETDECK_PORT/health" >/dev/null 2>&1; then
      return 1
    fi
    sleep 0.1
  done
  [ -f "$pidfile" ] || return 0
  node -e '
    const fs = require("node:fs");
    const pidfile = process.argv[1];
    const port = Number(process.argv[2]);
    const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
    const live = pid => {
      try { process.kill(pid, 0); return true; }
      catch (err) { return err?.code !== "ESRCH"; }
    };
    let record;
    try { record = JSON.parse(fs.readFileSync(pidfile, "utf8")); }
    catch { process.exit(2); }
    if (!Number.isInteger(record?.pid) || record.pid <= 0 || record.port !== port) process.exit(2);
    (async () => {
      let health = null;
      for (let i = 0; i < 20; i += 1) {
        try {
          const res = await fetch(`http://127.0.0.1:${port}/health`, { signal: AbortSignal.timeout(250) });
          const candidate = res.ok ? await res.json() : null;
          if (candidate?.pid === record.pid) { health = candidate; break; }
        } catch {}
        await sleep(100);
      }
      if (!health) { process.exitCode = 2; return; }

      try { process.kill(record.pid, "SIGTERM"); }
      catch (err) { if (err?.code !== "ESRCH") process.exitCode = 2; return; }
      for (let i = 0; i < 30; i += 1) {
        await sleep(100);
        if (!live(record.pid)) return;
      }
      // Never escalate to SIGKILL: an unproven shutdown is reported (and fails
      // the run below) instead of risking a recycled PID.
      process.exitCode = 2;
    })().catch(() => { process.exitCode = 2; });
  ' "$pidfile" "$FLEETDECK_PORT" >/dev/null 2>&1
}

# Per-run unique evidence dir + fixture copy: a concurrent acceptance run
# must never reset, delete, or report against this run's artifacts.
WORK_ROOT=''
DEMO_LOGS=''
PROJECT_DIR=''
PERM_PROOF=''
PERM_PROOF_PRE=''
TRANSCRIPT_PREFIX="fdp3-$$"

# Tear down everything THIS run started — the detached scratch daemon and the
# isolated tmux server. The user's daemon, default tmux server, and home are
# never cleanup targets. If the daemon cannot be verified stopped, fail the
# run: a surviving listener on :$FLEETDECK_PORT would poison later gates.
# Also remove this run's mktemp'd fixture/evidence copy — a unique path
# created below, never a shared one — and restore/remove the permission
# proof, a generated artifact that must never outlive the run.
cleanup() {
  local daemon_rc=0
  stop_scratch_daemon || daemon_rc=$?
  if command -v tmux >/dev/null 2>&1; then
    tmux -L "$FLEETDECK_TMUX_SOCKET" kill-server 2>/dev/null || true
  fi
  if [ -n "$WORK_ROOT" ]; then
    rm -rf -- "$WORK_ROOT"
  fi
  rm -rf -- "${CLAUDE_CONFIG_DIR:-$HOME/.claude}/projects/$TRANSCRIPT_PREFIX"
  cleanup_perm_proof
  if [ "$daemon_rc" -ne 0 ]; then
    echo "CLEANUP FAILED: scratch daemon not verified stopped; it may still be listening on :$FLEETDECK_PORT (home: $SCRATCH_HOME)" >&2
    exit 1
  fi
}

# A pre-existing local proof file is snapshotted below and restored verbatim
# here; otherwise the proof is removed only when its content matches what
# Part 1 asked the session to write (a tampered/unexpected file is left alone).
cleanup_perm_proof() {
  [ -n "$PERM_PROOF" ] || return 0
  if [ -n "$PERM_PROOF_PRE" ] && [ -f "$PERM_PROOF_PRE" ]; then
    cp -f "$PERM_PROOF_PRE" "$PERM_PROOF"
    rm -f "$PERM_PROOF_PRE"
  elif [ "$(cat "$PERM_PROOF" 2>/dev/null)" = "FLEET_PERMISSION_OK" ]; then
    rm -f "$PERM_PROOF"
  fi
}

# A hung curl or claude run must never hold the gate forever: on the overall
# deadline the EXIT trap still runs (verified tmux cleanup above) and the run
# exits as a failure instead of blocking the caller.
overall_deadline() {
  echo "ABORT: overall deadline (${ACCEPT_DEADLINE_S}s) hit; cleaning up."
  exit 124
}
ACCEPT_DEADLINE_S="${FLEETDECK_ACCEPT_DEADLINE_S:-600}"
trap overall_deadline ALRM
( sleep "$ACCEPT_DEADLINE_S" && kill -ALRM "$$" 2>/dev/null ) &
DEADLINE_PID=$!
trap cleanup EXIT

# Claude-session env vars that must never leak into the sessions (and through
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

echo "== Fleet Deck Phase 3 acceptance =="

# --------------------------------------------- portability preflight
# `run_with_timeout` supervises the `claude -p` runs below. GNU `timeout` is
# not part of stock macOS; accept Homebrew coreutils' `gtimeout`, and fall
# back to Node (a documented prerequisite) otherwise. Detect BEFORE any
# mutation: the reset below kills daemons and overwrites settings.
TIMEOUT_CMD=""
if command -v timeout >/dev/null 2>&1; then
  TIMEOUT_CMD=timeout
elif command -v gtimeout >/dev/null 2>&1; then
  TIMEOUT_CMD=gtimeout
fi
run_with_timeout() {
  local secs="$1"; shift
  # A scrubbed environment is mandatory: un-scrubbed vars (CLAUDECODE,
  # CLAUDE_CODE_SESSION_ID, ...) would leak through the SessionStart hook
  # into the elected daemon and misdirect later spawns.
  local env_prefix=(
    env "${CLAUDE_ENV_SCRUB[@]}"
    FLEETDECK_HOME="$SCRATCH_HOME" FLEETDECK_PORT="$FLEETDECK_PORT"
    FLEETDECK_TMUX_SOCKET="$FLEETDECK_TMUX_SOCKET"
  )
  if [ -n "$TIMEOUT_CMD" ]; then
    "${env_prefix[@]}" "$TIMEOUT_CMD" "$secs" "$@"
  else
    "${env_prefix[@]}" node -e '
      const [secs, cmd, ...args] = process.argv.slice(1);
      const child = require("node:child_process").spawn(cmd, args, { stdio: "inherit" });
      const killer = setTimeout(() => { child.kill("SIGTERM"); }, Number(secs) * 1000);
      child.on("exit", (code, signal) => {
        clearTimeout(killer);
        if (signal) process.kill(process.pid, signal);
        else process.exit(code ?? 1);
      });
    ' "$secs" "$@"
  fi
}

# ---------------------------------------------------------------- reset
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

# The destructive reset is allowed ONLY when a home is the script's own
# scratch home: its default (.fleetdeck-test) belongs to these acceptance
# runs, and the pid inside it was started by an earlier run of this same
# gate. With FLEETDECK_HOME_OVERRIDE set the home is caller-owned state, so
# this script never kills its pidfile or deletes it — a stale pidfile is
# still ignored (fleetd claims it atomically at boot). Either way the daemon
# is stopped only after its fleetd identity is proven (strict pidfile +
# /health.pid + process shape): a legacy plain-PID pidfile can name a PID
# the OS has since recycled for an unrelated process, and those are never
# signalled. The real home is only REPORTED: killing a pidfile the user may
# be running would destroy their fleet.
REAL_HOME="${HOME:-/root}/.fleetdeck"
if [ -f "$REAL_HOME/fleetd.pid" ]; then
  echo "NOTE: a fleetd pid record exists at $REAL_HOME/fleetd.pid — left untouched."
fi
SCRATCH_DEFAULTED=0
if [ -z "${FLEETDECK_HOME_OVERRIDE:-}" ]; then
  SCRATCH_DEFAULTED=1
fi
if ! stop_identified_daemon "$SCRATCH_HOME/fleetd.pid"; then
  echo "ABORT: daemon recorded in $SCRATCH_HOME/fleetd.pid could not be positively identified and stopped."
  exit 1
fi
if [ "$SCRATCH_DEFAULTED" -eq 1 ]; then
  rm -rf "$SCRATCH_HOME"
fi
if curl -s -m 1 "$BASE/health" > /dev/null 2>&1; then
  echo "ABORT: something is still listening on :$FLEETDECK_PORT after reset; refusing to kill an unidentified listener."
  exit 1
fi
mkdir -p "$SCRATCH_HOME"

# Unique fixture copy + evidence dir for this run (mktemp'd, removed by the
# EXIT trap): two overlapping Phase 3 runs each edit and log to their own.
WORK_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/fleetdeck-p3.XXXXXX")" || {
  echo "ABORT: could not create a unique fixture/evidence dir"
  exit 1
}
PROJECT_DIR="$WORK_ROOT/project"
DEMO_LOGS="$WORK_ROOT/demo-logs"
PERM_PROOF="$PROJECT_DIR/fleet-perm-proof.txt"
PERM_PROOF_PRE="$DEMO_LOGS/p3-perm-proof.pre-existing"
mkdir -p "$PROJECT_DIR" "$DEMO_LOGS"
cp -R "$SEED_PROJECT/." "$PROJECT_DIR/"
rm -f "$PERM_PROOF_PRE"
[ -f "$PERM_PROOF" ] && cp "$PERM_PROOF" "$PERM_PROOF_PRE"
rm -f "$PERM_PROOF"

# Same proven wiring as run-smoke.sh (incl. PermissionRequest/Elicitation 65s).
# Every hook uses the current checkout's authenticated command shim. Native
# HTTP hooks cannot attach the bearer token required since 0.16.0, and the
# daemon's legacy unauthenticated /hook/* refusal would silently swallow every
# event, so a tokenless wiring here tests nothing but the refusal path.
mkdir -p "$PROJECT_DIR/.claude"
# Rendered through JSON.stringify (never a heredoc): a checkout path with a
# space, quote, or backslash must not corrupt or split the generated hook
# commands (BUG-092).
node "$SCRIPT_DIR/render-smoke-settings.mjs" \
  "$SESSIONSTART_SCRIPT" "$FLEET_HOOK_SCRIPT" \
  "$PROJECT_DIR/.claude/settings.json"

cd "$PROJECT_DIR"
PASS=0; FAIL=0
ok()  { echo "PASS: $1"; PASS=$((PASS+1)); }
bad() { echo "FAIL: $1${2:+ -- $2}"; FAIL=$((FAIL+1)); }

# Verdict logic for the permission-relay check (BUG-011: proof on disk, not
# model prose). Sourced so tests/accept-phase3-perm-proof.test.mjs can
# exercise the exact function this gate runs.
. "$SCRIPT_DIR/perm-proof-check.sh"

# ============================================== PART 1: permission relay
# NO --dangerously-skip-permissions: the Bash call needs a permission
# decision, which must come from the board via the held PermissionRequest.
# --plugin-dir "$FLEETDECK_ROOT": a --plugin-dir plugin shadows an installed
# marketplace plugin of the same name for the session, so any installed
# Fleet Deck plugin's duplicate hooks can never mask the checkout under test
# with cached code (and the settings.json above disables it outright).
S1=$(node -e 'console.log(crypto.randomUUID())')
run_with_timeout 240 claude -p "Use the Bash tool to create a file named fleet-perm-proof.txt containing exactly the text FLEET_PERMISSION_OK (e.g. printf 'FLEET_PERMISSION_OK' > fleet-perm-proof.txt), then cat it and report its contents. Then stop." \
  --session-id "$S1" --max-turns 6 --permission-mode default \
  --plugin-dir "$FLEETDECK_ROOT" \
  --output-format json > "$DEMO_LOGS/p3-perm.json" 2> "$DEMO_LOGS/p3-perm.err" &
P1=$!
echo "T+0 permission-relay session launched sid=$S1"

# Poll for a pending permission question for S1, approve it from "the board".
APPROVED=""
for i in $(seq 1 90); do
  QID=$(curl -s --connect-timeout 1 -m 1 "$BASE/state" 2>/dev/null | node -e "
let d='';process.stdin.on('data',c=>d+=c).on('end',()=>{
  try{ const s=JSON.parse(d);
    const q=(s.questions||[]).find(q=>q.session_id==='$S1'&&q.kind==='permission'&&q.status==='pending');
    console.log(q?q.id:'');
  }catch{console.log('')}})" 2>/dev/null)
  if [ -n "$QID" ]; then
    # Bounded + status-validated: a listener that accepts but never answers
    # must fail the run, not hang it outside the 240s Claude watchdogs. A 4xx
    # body would leave the hold to expire and fail open to the native
    # terminal prompt, so only a 2xx answer counts as board approval.
    R=$(curl -sS --connect-timeout 2 -m 10 -w '\n%{http_code}' \
      -X POST "$BASE/api/questions/$QID/answer" -H 'content-type: application/json' \
      -d '{"behavior":"allow"}' 2>&1) \
      && [ "${R##*$'\n'}" -ge 200 ] && [ "${R##*$'\n'}" -lt 300 ]
    if [ $? -eq 0 ]; then
      echo "T+$i board approved permission question #$QID → ${R%$'\n'*}"
      APPROVED=yes
    else
      echo "T+$i permission answer POST failed or non-2xx: $R"
    fi
    break
  fi
  sleep 1
done
[ -n "$APPROVED" ] && ok "permission question appeared on board and was approved" \
                   || bad "permission question appeared on board" "no pending permission question for $S1 within 90s"

wait "$P1"; RC1=$?
echo "permission session done rc=$RC1"
# Proof of execution is the file on disk, not the model's prose: p3-perm.json
# contains the marker simply because the prompt names it.
PROOF_DETAIL=$(perm_proof_check "$RC1" "$PROJECT_DIR/fleet-perm-proof.txt" "$DEMO_LOGS/p3-perm.json" "$APPROVED") \
  && ok "command executed after board approval (proof file on disk, terminal never asked)" \
  || bad "command executed after board approval (proof file on disk)" "$PROOF_DETAIL"

# ============================================== PART 2: freeform Q&A
S2=$(node -e 'console.log(crypto.randomUUID())')
run_with_timeout 240 claude -p "You need one decision from the human before doing anything: should the project use bcrypt or argon2 for password hashing? Do not decide yourself and do not do any other work. End your reply with that single question addressed to me." \
  --session-id "$S2" --max-turns 4 --dangerously-skip-permissions \
  --plugin-dir "$FLEETDECK_ROOT" \
  --output-format json > "$DEMO_LOGS/p3-freeform.json" 2> "$DEMO_LOGS/p3-freeform.err"
echo "freeform session first run done rc=$? sid=$S2"

# The trailing question should now be a freeform card. Answer it.
QID2=$(curl -s --connect-timeout 2 -m 5 "$BASE/state" 2>/dev/null | node -e "
let d='';process.stdin.on('data',c=>d+=c).on('end',()=>{
  try{ const s=JSON.parse(d);
    const q=(s.questions||[]).find(q=>q.session_id==='$S2'&&q.kind==='freeform'&&q.status==='pending');
    console.log(q?q.id:'');
  }catch{console.log('')}})")
if [ -n "$QID2" ]; then
  ok "trailing question detected as freeform needs-you card"
  ANS_HTTP=$(curl -sS --connect-timeout 2 -m 10 -o /dev/null -w '%{http_code}' \
    -X POST "$BASE/api/questions/$QID2/answer" -H 'content-type: application/json' \
    -d '{"text":"Use argon2 (argon2id). Do not use bcrypt."}' 2>/dev/null) || ANS_HTTP=000
  if [ "$ANS_HTTP" -ge 200 ] && [ "$ANS_HTTP" -lt 300 ]; then
    echo "board answered freeform question #$QID2 (HTTP $ANS_HTTP)"
  else
    bad "board answer reached the session at its next boundary" "freeform answer POST returned HTTP $ANS_HTTP"
  fi
else
  bad "trailing question detected as freeform needs-you card" "no pending freeform question for $S2"
fi

# Next boundary: resume the same session; UserPromptSubmit must deliver the answer.
run_with_timeout 240 claude -p --resume "$S2" "Continue based on my answer. State which algorithm you will use and why, in one sentence." \
  --max-turns 4 --dangerously-skip-permissions \
  --plugin-dir "$FLEETDECK_ROOT" \
  --output-format json > "$DEMO_LOGS/p3-resume.json" 2> "$DEMO_LOGS/p3-resume.err"
echo "freeform session resume done rc=$?"

if grep -qi "argon2" "$DEMO_LOGS/p3-resume.json"; then
  ok "board answer reached the session at its next boundary (model acted on argon2)"
else
  bad "board answer reached the session at its next boundary" "argon2 not referenced in resume output"
fi
# Claude Code stores sessions under ${CLAUDE_CONFIG_DIR:-$HOME/.claude} —
# honor the override or a contributor with it set gets a false "missing
# relay". The project directory name uses the SAME slash-and-dot cwd munging
# the daemon uses (helpers.mjs — a dot in the checkout path would otherwise
# point the gate at a directory Claude never writes), prefixed with this
# run's TRANSCRIPT_PREFIX so a concurrent acceptance run never reads or
# reaps another run's transcript evidence.
TRANSCRIPT_ROOT="${CLAUDE_CONFIG_DIR:-$HOME/.claude}"
MUNGED_PROJECT=$(node --input-type=module -e "import { mungeClaudeProjectCwd } from '$FLEETDECK_ROOT/scripts/fleetd/helpers.mjs'; console.log(mungeClaudeProjectCwd(process.argv[1]))" "$PROJECT_DIR")
TRANSCRIPT_DIR="$TRANSCRIPT_ROOT/projects/$TRANSCRIPT_PREFIX$MUNGED_PROJECT"
if grep -q "FLEETDECK ANSWER" "$TRANSCRIPT_DIR/$S2.jsonl" 2>/dev/null; then
  ok "[FLEETDECK ANSWER] visible in resumed session transcript"
else
  bad "[FLEETDECK ANSWER] visible in resumed session transcript" "not found in $TRANSCRIPT_DIR/$S2.jsonl"
fi

# ============================================== evidence + wrap
curl -s --connect-timeout 2 -m 10 "$BASE/state" > "$DEMO_LOGS/p3-final-state.json" 2>/dev/null || true
echo
echo "hook-payloads.jsonl captured event shapes (first 3 per event):"
node -e "
const fs=require('fs');
try{ const lines=fs.readFileSync('$SCRATCH_HOME/hook-payloads.jsonl','utf8').trim().split('\n');
  for(const l of lines){ const j=JSON.parse(l); console.log(' ', j.event || j.hook_event_name, '→ keys:', (j.keys||[]).join(',')); }
}catch(e){ console.log('  (no capture file:', e.message+')') }"
echo
# The gate's work is done: disarm the deadline watcher so a finished run
# exits now instead of lingering until ACCEPT_DEADLINE_S.
kill "$DEADLINE_PID" 2>/dev/null || true
echo "RESULT: $PASS pass, $FAIL fail"
[ "$FAIL" -eq 0 ]
