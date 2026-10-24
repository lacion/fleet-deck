#!/usr/bin/env bash
# FG-9 spike (spikes/s-move): does a provider conversation that started in
# folder A on machine 1 resume, with its history, in folder B on machine 2,
# when only the transcript files and the native session id move?
#
# "Machine" = a fresh HOME (and so a fresh ~/.claude or ~/.codex) below the
# run root. Folder A and folder B are two different git repositories with
# different absolute paths. Only the transcript files and the auth are
# copied. See README.md for the method and the results.
#
# Secrets: this file holds none. Claude auth comes from the caller's
# environment (ANTHROPIC_BASE_URL, ANTHROPIC_AUTH_TOKEN or ANTHROPIC_API_KEY)
# or, when those are not set, from a copy of ~/.claude/.credentials.json.
# Codex auth is a copy of ~/.codex/auth.json. The copies are mode 0600 and
# are deleted at the end. A secret is never put on a command line and never
# printed. If a provider refreshed a copied OAuth token, the script copies the
# refreshed file back (only when the original did not change meanwhile), so
# the owner's own login keeps a valid refresh token.
#
# Usage: bash spikes/s-move/run.sh [claude|codex|all]
# Env:   S_MOVE_ROOT  run root (default: a new mktemp dir below /tmp)
#        S_MOVE_KEEP=1  keep the run root (the transcripts) at the end
set -euo pipefail

WHICH="${1:-all}"
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="${S_MOVE_ROOT:-$(mktemp -d /tmp/s-move.XXXXXX)}"
mkdir -p "$ROOT"
chmod 700 "$ROOT"
RESULTS="$ROOT/results.txt"
: > "$RESULTS"
umask 077

say() { printf '%s\n' "$*" | tee -a "$RESULTS"; }

# The PATH of a simulated machine: only the directories of the tools.
tool_dir() { dirname "$(command -v "$1")"; }
SIM_PATH="/usr/bin:/bin"
for t in claude codex node; do
  if command -v "$t" >/dev/null 2>&1; then SIM_PATH="$(tool_dir "$t"):$SIM_PATH"; fi
done

A="$ROOT/a/repo"
B="$ROOT/b/repo"
mkdir -p "$A" "$B"
git -C "$A" init -q
git -C "$B" init -q

PROMPT1="Remember the word PELICAN-7. Reply OK."
PROMPT2="What word did I ask you to remember?"

# Runs "$@" with an empty environment except PATH, HOME, the provider home
# variable (when given), LANG and the Claude auth variables. No value is put
# on a command line.
sim() {
  local home="$1"; shift
  (
    for v in $(env | sed -n 's/^\([A-Za-z_][A-Za-z0-9_]*\)=.*/\1/p'); do
      case "$v" in
        ANTHROPIC_BASE_URL|ANTHROPIC_AUTH_TOKEN|ANTHROPIC_API_KEY|LANG) ;;
        *) unset "$v" 2>/dev/null || true ;;
      esac
    done
    export PATH="$SIM_PATH" HOME="$home" DISABLE_AUTOUPDATER=1
    exec "$@"
  )
}

# Claude's project directory name of a cwd (crates/fleetdeck-station/src/
# history/locate.rs `encode_cwd`): every non-ASCII-alphanumeric char is `-`.
enc() { printf '%s' "$1" | sed 's/[^A-Za-z0-9]/-/g'; }

redact() { sed -E 's/(sk-[A-Za-z0-9_-]{6})[A-Za-z0-9_-]+/\1<redacted>/g; s/(eyJ[A-Za-z0-9_-]{6})[A-Za-z0-9._-]+/\1<redacted>/g'; }

sha() { sha256sum "$1" | cut -d' ' -f1; }

# --- auth copies -----------------------------------------------------------
CLAUDE_CRED_SRC="$HOME/.claude/.credentials.json"
CODEX_AUTH_SRC="${CODEX_HOME:-$HOME/.codex}/auth.json"
CLAUDE_CRED_SHA=""; CODEX_AUTH_SHA=""
COPIES=()
claude_auth_from_env() { [ -n "${ANTHROPIC_AUTH_TOKEN:-}" ] || [ -n "${ANTHROPIC_API_KEY:-}" ]; }

give_auth() { # $1 = provider, $2 = simulated HOME
  case "$1" in
    claude)
      mkdir -p "$2/.claude"
      if ! claude_auth_from_env; then
        [ -n "$CLAUDE_CRED_SHA" ] || CLAUDE_CRED_SHA="$(sha "$CLAUDE_CRED_SRC")"
        install -m 600 "$CLAUDE_CRED_SRC" "$2/.claude/.credentials.json"
        COPIES+=("claude:$2/.claude/.credentials.json")
      fi ;;
    codex)
      mkdir -p "$2/.codex"
      [ -n "$CODEX_AUTH_SHA" ] || CODEX_AUTH_SHA="$(sha "$CODEX_AUTH_SRC")"
      install -m 600 "$CODEX_AUTH_SRC" "$2/.codex/auth.json"
      COPIES+=("codex:$2/.codex/auth.json") ;;
  esac
}

finish() {
  for entry in "${COPIES[@]+"${COPIES[@]}"}"; do
    kind="${entry%%:*}"; copy="${entry#*:}"
    [ -f "$copy" ] || continue
    if [ "$kind" = claude ]; then src="$CLAUDE_CRED_SRC"; orig="$CLAUDE_CRED_SHA"; else src="$CODEX_AUTH_SRC"; orig="$CODEX_AUTH_SHA"; fi
    if [ "$(sha "$copy")" != "$orig" ] && [ "$(sha "$src")" = "$orig" ]; then
      install -m 600 "$copy" "$src"
      say "NOTE: $kind refreshed its token in a simulated home; the refreshed file was copied back to $src"
    fi
    rm -f "$copy"
  done
  if [ "${S_MOVE_KEEP:-0}" != 1 ]; then rm -rf "$ROOT"; else echo "run root kept: $ROOT"; fi
}
trap finish EXIT

verdict() { # $1 label, $2 text that must contain PELICAN-7
  if printf '%s' "$2" | grep -q 'PELICAN-7'; then say "$1: PASS"; else say "$1: FAIL"; fi
}

# --- Claude ------------------------------------------------------------------
claude_turn() { # $1 home, $2 cwd, $3 out file, $4 prompt, [$5 resume id]
  local args=(-p --input-format stream-json --output-format stream-json --verbose
              --include-partial-messages --replay-user-messages
              --permission-prompt-tool stdio --permission-mode default)
  [ -n "${5:-}" ] && args+=(--resume "$5")
  python3 -I -c 'import json,sys; print(json.dumps({"type":"user","message":{"role":"user","content":[{"type":"text","text":sys.argv[1]}]}}))' "$4" \
    | (cd "$2" && sim "$1" timeout 300 claude "${args[@]}") > "$3" 2> "$3.err" || true
  python3 -I -c '
import json,sys
init=res=None
for l in open(sys.argv[1]):
    try: o=json.loads(l)
    except ValueError: continue
    if o.get("type")=="system" and o.get("subtype")=="init": init=o
    if o.get("type")=="result": res=o
print(json.dumps({"session_id":(init or res or {}).get("session_id"),"cwd":(init or {}).get("cwd"),
  "subtype":(res or {}).get("subtype"),"result":(res or {}).get("result"),"errors":(res or {}).get("errors")}))
' "$3"
}

run_claude() {
  say "== Claude $(claude --version 2>/dev/null | head -1)"
  local m1="$ROOT/claude-m1" m2="$ROOT/claude-m2" m3="$ROOT/claude-m3" m4="$ROOT/claude-m4"
  for h in "$m1" "$m2" "$m3" "$m4"; do mkdir -p "$h"; give_auth claude "$h"; done

  local out id src
  out="$(claude_turn "$m1" "$A" "$ROOT/claude-1.jsonl" "$PROMPT1")"
  say "machine 1, folder A, turn 1: $out"
  id="$(printf '%s' "$out" | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["session_id"] or "")')"
  src="$m1/.claude/projects/$(enc "$A")/$id.jsonl"
  [ -f "$src" ] || { say "Claude: FAIL (no transcript at ${src#$ROOT/})"; return; }
  say "transcript on machine 1: ${src#$ROOT/} ($(stat -c %s "$src") bytes)"
  say "files of machine 1 below projects/: $(cd "$m1/.claude/projects" && find . -type f | tr '\n' ' ')"

  # C1: minimal. Same relative path as on machine 1 (directory of A).
  mkdir -p "$m2/.claude/projects/$(enc "$A")"
  install -m 600 "$src" "$m2/.claude/projects/$(enc "$A")/"
  out="$(claude_turn "$m2" "$B" "$ROOT/claude-c1.jsonl" "$PROMPT2" "$id")"
  say "C1 (file under the directory of A), machine 2, folder B: $out"
  verdict "Claude C1" "$out"
  say "C1 files after the turn: $(cd "$m2/.claude/projects" && find . -type f | tr '\n' ' ')"

  # C2: rewrite. The file under the directory of B (what Fleet Deck history
  # locate expects: projects/<encode_cwd(cwd)>/<id>.jsonl).
  mkdir -p "$m3/.claude/projects/$(enc "$B")"
  install -m 600 "$src" "$m3/.claude/projects/$(enc "$B")/"
  out="$(claude_turn "$m3" "$B" "$ROOT/claude-c2.jsonl" "$PROMPT2" "$id")"
  say "C2 (file under the directory of B), machine 2, folder B: $out"
  verdict "Claude C2" "$out"

  # Negative control: no transcript on machine 2.
  out="$(claude_turn "$m4" "$B" "$ROOT/claude-neg.jsonl" "$PROMPT2" "$id")"
  say "control (no transcript): $out"
}

# --- Codex -------------------------------------------------------------------
codex_app() { # $1 home, then the codex_app_server.py arguments
  local home="$1"; shift
  sim "$home" timeout 300 python3 -I "$HERE/codex_app_server.py" "$@" 2>/dev/null || true
}

run_codex() {
  say "== Codex $(codex --version 2>/dev/null | head -1)"
  local m1="$ROOT/codex-m1" m2="$ROOT/codex-m2" m3="$ROOT/codex-m3" m4="$ROOT/codex-m4" m5="$ROOT/codex-m5"
  for h in "$m1" "$m2" "$m3" "$m4" "$m5"; do mkdir -p "$h"; give_auth codex "$h"; done

  local out id src rel
  out="$(codex_app "$m1" start "$A" "$PROMPT1")"
  say "machine 1, folder A, turn 1 (app-server thread/start): $out"
  id="$(printf '%s' "$out" | python3 -I -c 'import json,sys; print(json.load(sys.stdin).get("thread_id") or "")')"
  src="$(find "$m1/.codex/sessions" -name "rollout-*-$id.jsonl" 2>/dev/null | head -1)"
  [ -n "$src" ] || { say "Codex: FAIL (no rollout for $id)"; return; }
  rel="${src#$m1/.codex/}"
  say "rollout on machine 1: $rel ($(stat -c %s "$src") bytes)"

  # X1: minimal. Only the rollout, at the same relative path. No state DB.
  mkdir -p "$m2/.codex/$(dirname "$rel")"
  install -m 600 "$src" "$m2/.codex/$rel"
  out="$(codex_app "$m2" resume "$B" "$PROMPT2" "$id")"
  say "X1 (app-server thread/resume, cwd B), machine 2: $out"
  verdict "Codex X1" "$out"
  say "X1 cwd records in the rollout after the turn: $(python3 -I -c '
import json,sys
print(" ".join(o["type"]+"="+str(o["payload"].get("cwd")) for o in map(json.loads,open(sys.argv[1])) if o.get("type") in ("session_meta","turn_context")))' "$m2/.codex/$rel")"
  if command -v sqlite3 >/dev/null; then
    say "X1 state DB row on machine 2: $(sqlite3 "$m2/.codex/state_5.sqlite" "select cwd from threads where id='$id'" 2>/dev/null | sed "s#$ROOT/##")"
  fi

  # X2: the CLI path, codex exec resume <id> in folder B.
  mkdir -p "$m3/.codex/$(dirname "$rel")"
  install -m 600 "$src" "$m3/.codex/$rel"
  out="$( (cd "$B" && sim "$m3" timeout 300 codex exec --json resume "$id" "$PROMPT2" </dev/null 2>/dev/null) \
          | python3 -I -c '
import json,sys
t=[]
for l in sys.stdin:
    try: o=json.loads(l)
    except ValueError: continue
    i=o.get("item") or {}
    if i.get("type")=="agent_message": t.append(i.get("text",""))
print(json.dumps({"agent_message":" ".join(t)}))' || true)"
  say "X2 (codex exec resume, cwd B), machine 2: $out"
  verdict "Codex X2" "$out"

  # X3: thread/resume without the cwd parameter (the process runs in B).
  mkdir -p "$m5/.codex/$(dirname "$rel")"
  install -m 600 "$src" "$m5/.codex/$rel"
  out="$(cd "$B" && codex_app "$m5" resume-no-cwd "$B" "-" "$id")"
  say "X3 (thread/resume without cwd, process in B), machine 2: $out"

  # Negative control: no rollout on machine 2.
  out="$(codex_app "$m4" resume "$B" "$PROMPT2" "$id")"
  say "control (no rollout): $out"
}

say "run root: $ROOT (folder A = a/repo, folder B = b/repo)"
case "$WHICH" in
  claude) run_claude ;;
  codex) run_codex ;;
  all) run_claude; run_codex ;;
  *) echo "usage: $0 [claude|codex|all]" >&2; exit 2 ;;
esac
say "results: $RESULTS"
redact < "$RESULTS" > "$RESULTS.redacted" && mv "$RESULTS.redacted" "$RESULTS"
