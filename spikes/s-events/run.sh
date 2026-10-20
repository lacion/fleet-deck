#!/bin/sh
# S-EVENTS: what do Claude and Codex do with MCP events (subscriptions, list
# changes, resource updates, progress, cancellation, cache TTLs)?
#
# Usage: sh spikes/s-events/run.sh [experiment ...]
#   experiments: C1 C2 C3 C4 C4B C4C C4D C5 X1A X1B X1C X2 X2B X3 X3B (default: all)
#   C4B, C4C, C4D, X2B and X3B are extra runs: a short tool timeout (10 s) against a 25 s
#   count; Claude by MCP_TOOL_TIMEOUT with progress (C4B) and without progress (C4C), and
#   by the per-server `timeout` key (C4D); Codex by tool_timeout_sec (X2B); and a 15 s
#   watch after the interrupt (X3B).
#
# Every run starts its own events-server (port chosen by the OS) and its own
# new temporary directory. The full outputs stay there (printed at the start):
#   events.log     the server log (every HTTP request, every notification, every stream)
#   out.jsonl / frames.jsonl   the full host output
#   timeline.txt   one line per host frame, with seconds since the start
# A 300 s bound applies to each experiment. Nothing is written into ~/.claude,
# ~/.codex or a repository workspace by this script (the CLIs keep their own state).
#
# Build the server first:
#   umask 022 && sh tools/heavy.sh cargo build --manifest-path spikes/s-events/Cargo.toml
set -u

HERE=$(cd "$(dirname "$0")" && pwd)
BIN=${EVENTS_BIN:-$HERE/target/debug/events-server}
TMP=${EVENTS_TMP:-$(mktemp -d /tmp/events-run.XXXXXX)}
BOUND=300
SERVER_PID=""

if [ ! -x "$BIN" ]; then
  echo "events-server is missing: $BIN (build it first)" >&2
  exit 2
fi
echo "outputs: $TMP"

# start_server ID [server flags...]: sets URL and SERVER_PID, DIR.
start_server() {
  id=$1
  shift
  DIR="$TMP/$id"
  mkdir -p "$DIR/work"
  EVENTS_DIR="$DIR" EVENTS_LOG="$DIR/events.log" "$BIN" "$@" >"$DIR/server-stdout.txt" 2>"$DIR/server-stderr.txt" &
  SERVER_PID=$!
  n=0
  PORT=""
  while [ $n -lt 100 ]; do
    PORT=$(sed -n 's/^PORT=//p' "$DIR/server-stdout.txt" 2>/dev/null | head -1)
    [ -n "$PORT" ] && break
    n=$((n + 1))
    sleep 0.1
  done
  if [ -z "$PORT" ]; then
    echo "events-server did not start: $(cat "$DIR/server-stderr.txt")" >&2
    return 1
  fi
  URL="http://127.0.0.1:$PORT/mcp"
}

stop_server() {
  if [ -n "$SERVER_PID" ]; then
    kill "$SERVER_PID" 2>/dev/null
    wait "$SERVER_PID" 2>/dev/null
    SERVER_PID=""
  fi
}

# summary ID: prints the host timeline and the server log without the wire frames.
summary() {
  id=$1
  dir="$TMP/$id"
  echo "=== $id ($(cat "$dir/exit.txt" 2>/dev/null))"
  echo "-- host timeline (without text deltas):"
  grep -v 'stream_event/' "$dir/timeline.txt" 2>/dev/null | cut -c1-330 | head -${TIMELINE_LINES:-70}
  echo "-- server log (without wire frames):"
  grep -v 'wire frame' "$dir/events.log" 2>/dev/null | cut -c1-330 | head -${LOG_LINES:-90}
  echo "-- host stderr (first lines):"
  head -3 "$dir/stderr.txt" 2>/dev/null | cut -c1-300
}

# claude_run ID PLAN_JSON "server flags" [KEY=VALUE for the child ...]
claude_run() {
  id=$1
  plan=$2
  flags=$3
  shift 3
  # shellcheck disable=SC2086
  start_server "$id" $flags || return
  # EVENTS_MCP_TIMEOUT (ms): the per-server `timeout` key of the Claude MCP config.
  config=$(jq -nc --arg url "$URL" --argjson t "${EVENTS_MCP_TIMEOUT:-null}" \
    '{mcpServers:{events:({type:"http",url:$url} + (if $t == null then {} else {timeout:$t} end))}}')
  # The plan can name the trigger file as @DIR@.
  plan=$(printf '%s' "$plan" | sed "s#@DIR@#$DIR#g")
  timeout $BOUND python3 -I "$HERE/claude_session.py" "$DIR" "$DIR/work" "$config" "$plan" "$@" \
    >"$DIR/driver.txt" 2>&1
  echo "exit=$?" >"$DIR/exit.txt"
  stop_server
  summary "$id"
}

# codex_run ID MODE "server flags" [--flag FEATURE ...]
codex_run() {
  id=$1
  mode=$2
  flags=$3
  shift 3
  # shellcheck disable=SC2086
  start_server "$id" $flags || return
  timeout $BOUND python3 -I "$HERE/codex_events.py" "$mode" "$DIR" "$DIR/work" "$URL" "$@" \
    >"$DIR/driver.txt" 2>&1
  echo "exit=$?" >"$DIR/exit.txt"
  stop_server
  echo "-- driver output:"
  cat "$DIR/driver.txt" | cut -c1-600
  summary "$id"
}

LATE='Is there a tool named late_tool? If yes, call it.'

run_one() {
  case "$1" in
    C1) claude_run C1 "[[\"turn\",\"Call tick.\"],[\"sleep\",3],[\"turn\",\"$LATE\"]]" '--stateless --sse' ;;
    C2) claude_run C2 "[[\"turn\",\"Call tick.\"],[\"sleep\",8],[\"turn\",\"$LATE\"]]" '--stateless --sse --no-listen' ;;
    C3) claude_run C3 '[["turn","Read fleet://board and tell me the word."],["touch","@DIR@/bump"],["sleep",3],["turn","Read it again."]]' '--stateless --sse' ;;
    C4) claude_run C4 '[["turn","Call slow_count with seconds 90 and tell me the result."]]' '--stateless --json' ;;
    C4B) claude_run C4B '[["turn","Call slow_count with seconds 25 and tell me the result."]]' '--stateless --json' MCP_TOOL_TIMEOUT=10000 ;;
    C4C)
      (
        export EVENTS_NO_PROGRESS=1
        claude_run C4C '[["turn","Call slow_count with seconds 25 and tell me the result."]]' '--stateless --json' MCP_TOOL_TIMEOUT=10000
      )
      ;;
    C4D)
      (
        export EVENTS_MCP_TIMEOUT=10000
        claude_run C4D '[["turn","Call slow_count with seconds 25 and tell me the result."]]' '--stateless --json'
      )
      ;;
    C5) claude_run C5 '[["turn_interrupt","Call slow_count with seconds 60.",5]]' '--stateless --json' ;;
    X1A) codex_run X1A events '--stateless --json' ;;
    X1B) codex_run X1B events '--stateful --sse' ;;
    X1C) codex_run X1C events '--stateful --sse' --flag mcp_2026_07_28 ;;
    X2) codex_run X2 slow '--stateless --json' ;;
    X2B)
      (
        export EVENTS_SLOW_SECONDS=25 EVENTS_CODEX_ARGS='-c mcp_servers.events.tool_timeout_sec=10'
        codex_run X2B slow '--stateless --json'
      )
      ;;
    X3) codex_run X3 cancel '--stateless --json' ;;
    X3B)
      (
        export EVENTS_WATCH_AFTER=15
        codex_run X3B cancel '--stateless --json'
      )
      ;;
    *) echo "unknown experiment $1" >&2 ;;
  esac
}

if [ "$#" -eq 0 ]; then
  set -- C1 C2 C3 C4 C4B C4C C4D C5 X1A X1B X1C X2 X2B X3 X3B
fi
for e in "$@"; do
  run_one "$e"
done
echo "outputs: $TMP"
