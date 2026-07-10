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
PROJECT_DIR="$SCRIPT_DIR/project"
DEMO_LOGS="$SCRIPT_DIR/demo-logs"
SESSIONSTART_SCRIPT="$FLEETDECK_ROOT/scripts/fleet-sessionstart.mjs"
FLEETDECK_PORT="${FLEETDECK_PORT:-4711}"
SCRATCH_HOME="${FLEETDECK_HOME_OVERRIDE:-$FLEETDECK_ROOT/.fleetdeck-test}"
BASE="http://127.0.0.1:$FLEETDECK_PORT"

echo "== Fleet Deck Phase 3 acceptance =="

# ---------------------------------------------------------------- reset
REAL_HOME="${HOME:-/root}/.fleetdeck"
[ -f "$REAL_HOME/fleetd.pid" ] && kill "$(cat "$REAL_HOME/fleetd.pid" 2>/dev/null)" 2>/dev/null
[ -f "$SCRATCH_HOME/fleetd.pid" ] && kill "$(cat "$SCRATCH_HOME/fleetd.pid" 2>/dev/null)" 2>/dev/null
sleep 0.5
if curl -s -m 1 "$BASE/health" 2>/dev/null | grep -q '"ok"'; then
  fuser -k "$FLEETDECK_PORT/tcp" 2>/dev/null || true
  sleep 0.5
fi
if curl -s -m 1 "$BASE/health" > /dev/null 2>&1; then
  echo "ABORT: something is still listening on :$FLEETDECK_PORT after reset."
  exit 1
fi
rm -rf "$SCRATCH_HOME"; mkdir -p "$SCRATCH_HOME" "$DEMO_LOGS"
rm -f "$DEMO_LOGS"/p3-*.json "$DEMO_LOGS"/p3-*.err "$PROJECT_DIR/fleet-perm-proof.txt"

# Same proven wiring as run-smoke.sh (incl. PermissionRequest/Elicitation 65s).
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
      { "hooks": [{ "type": "http", "url": "$BASE/hook/Stop", "timeout": 5 }] }
    ],
    "SessionEnd": [
      { "hooks": [{ "type": "http", "url": "$BASE/hook/SessionEnd", "timeout": 3, "async": true }] }
    ]
  }
}
EOF

cd "$PROJECT_DIR"
PASS=0; FAIL=0
ok()  { echo "PASS: $1"; PASS=$((PASS+1)); }
bad() { echo "FAIL: $1${2:+ -- $2}"; FAIL=$((FAIL+1)); }

# ============================================== PART 1: permission relay
# NO --dangerously-skip-permissions: the Bash call needs a permission
# decision, which must come from the board via the held PermissionRequest.
S1=$(node -e 'console.log(crypto.randomUUID())')
env -u CLAUDE_CODE_SESSION_ID -u CLAUDE_CODE_CHILD_SESSION \
  FLEETDECK_HOME="$SCRATCH_HOME" FLEETDECK_PORT="$FLEETDECK_PORT" \
  timeout 240 claude -p "Use the Bash tool to create a file named fleet-perm-proof.txt containing exactly the text FLEET_PERMISSION_OK (e.g. printf 'FLEET_PERMISSION_OK' > fleet-perm-proof.txt), then cat it and report its contents. Then stop." \
  --session-id "$S1" --max-turns 6 --permission-mode default \
  --output-format json > "$DEMO_LOGS/p3-perm.json" 2> "$DEMO_LOGS/p3-perm.err" &
P1=$!
echo "T+0 permission-relay session launched sid=$S1"

# Poll for a pending permission question for S1, approve it from "the board".
APPROVED=""
for i in $(seq 1 90); do
  QID=$(curl -s -m 1 "$BASE/state" 2>/dev/null | node -e "
let d='';process.stdin.on('data',c=>d+=c).on('end',()=>{
  try{ const s=JSON.parse(d);
    const q=(s.questions||[]).find(q=>q.session_id==='$S1'&&q.kind==='permission'&&q.status==='pending');
    console.log(q?q.id:'');
  }catch{console.log('')}})" 2>/dev/null)
  if [ -n "$QID" ]; then
    R=$(curl -s -X POST "$BASE/api/questions/$QID/answer" -H 'content-type: application/json' -d '{"behavior":"allow"}')
    echo "T+$i board approved permission question #$QID → $R"
    APPROVED=yes
    break
  fi
  sleep 1
done
[ -n "$APPROVED" ] && ok "permission question appeared on board and was approved" \
                   || bad "permission question appeared on board" "no pending permission question for $S1 within 90s"

wait "$P1"; RC1=$?
echo "permission session done rc=$RC1"
if grep -q "FLEET_PERMISSION_OK" "$DEMO_LOGS/p3-perm.json"; then
  ok "command executed after board approval (terminal never asked)"
else
  bad "command executed after board approval" "marker not in p3-perm.json (rc=$RC1)"
fi

# ============================================== PART 2: freeform Q&A
S2=$(node -e 'console.log(crypto.randomUUID())')
env -u CLAUDE_CODE_SESSION_ID -u CLAUDE_CODE_CHILD_SESSION \
  FLEETDECK_HOME="$SCRATCH_HOME" FLEETDECK_PORT="$FLEETDECK_PORT" \
  timeout 240 claude -p "You need one decision from the human before doing anything: should the project use bcrypt or argon2 for password hashing? Do not decide yourself and do not do any other work. End your reply with that single question addressed to me." \
  --session-id "$S2" --max-turns 4 --dangerously-skip-permissions \
  --output-format json > "$DEMO_LOGS/p3-freeform.json" 2> "$DEMO_LOGS/p3-freeform.err"
echo "freeform session first run done rc=$? sid=$S2"

# The trailing question should now be a freeform card. Answer it.
QID2=$(curl -s "$BASE/state" | node -e "
let d='';process.stdin.on('data',c=>d+=c).on('end',()=>{
  try{ const s=JSON.parse(d);
    const q=(s.questions||[]).find(q=>q.session_id==='$S2'&&q.kind==='freeform'&&q.status==='pending');
    console.log(q?q.id:'');
  }catch{console.log('')}})")
if [ -n "$QID2" ]; then
  ok "trailing question detected as freeform needs-you card"
  curl -s -X POST "$BASE/api/questions/$QID2/answer" -H 'content-type: application/json' \
    -d '{"text":"Use argon2 (argon2id). Do not use bcrypt."}' > /dev/null
  echo "board answered freeform question #$QID2"
else
  bad "trailing question detected as freeform needs-you card" "no pending freeform question for $S2"
fi

# Next boundary: resume the same session; UserPromptSubmit must deliver the answer.
env -u CLAUDE_CODE_SESSION_ID -u CLAUDE_CODE_CHILD_SESSION \
  FLEETDECK_HOME="$SCRATCH_HOME" FLEETDECK_PORT="$FLEETDECK_PORT" \
  timeout 240 claude -p --resume "$S2" "Continue based on my answer. State which algorithm you will use and why, in one sentence." \
  --max-turns 4 --dangerously-skip-permissions \
  --output-format json > "$DEMO_LOGS/p3-resume.json" 2> "$DEMO_LOGS/p3-resume.err"
echo "freeform session resume done rc=$?"

if grep -qi "argon2" "$DEMO_LOGS/p3-resume.json"; then
  ok "board answer reached the session at its next boundary (model acted on argon2)"
else
  bad "board answer reached the session at its next boundary" "argon2 not referenced in resume output"
fi
TRANSCRIPT_DIR="$HOME/.claude/projects/$(echo "$PROJECT_DIR" | sed 's|/|-|g')"
if grep -q "FLEETDECK ANSWER" "$TRANSCRIPT_DIR/$S2.jsonl" 2>/dev/null; then
  ok "[FLEETDECK ANSWER] visible in resumed session transcript"
else
  bad "[FLEETDECK ANSWER] visible in resumed session transcript" "not found in $TRANSCRIPT_DIR/$S2.jsonl"
fi

# ============================================== evidence + wrap
curl -s "$BASE/state" > "$DEMO_LOGS/p3-final-state.json"
echo
echo "hook-payloads.jsonl captured event shapes (first 3 per event):"
node -e "
const fs=require('fs');
try{ const lines=fs.readFileSync('$SCRATCH_HOME/hook-payloads.jsonl','utf8').trim().split('\n');
  for(const l of lines){ const j=JSON.parse(l); console.log(' ', j.event || j.hook_event_name, '→ keys:', (j.keys||[]).join(',')); }
}catch(e){ console.log('  (no capture file:', e.message+')') }"
echo
echo "RESULT: $PASS pass, $FAIL fail"
[ "$FAIL" -eq 0 ]
