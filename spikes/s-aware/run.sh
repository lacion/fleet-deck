#!/bin/sh
# S-AWARE: what do Claude and Codex do with MCP resources, prompts,
# instructions and injected skills?
#
# Usage: sh spikes/s-aware/run.sh [experiment ...]
#   experiments: C1 C2 C3 C3B C4 X1 X1APP X2 X3 X3CFG X4 (default: all)
#
# Every run starts in its own new temporary directory, with one tiny prompt and
# a 120 s bound. The full stream output of each run stays in the temporary
# directory (printed at the start); only the summary goes to the terminal.
# Nothing is written into ~/.claude, ~/.codex or a repository workspace by this
# script (the CLIs keep their own state; see the README).
#
# Build the server first:
#   umask 022 && sh tools/heavy.sh cargo build --manifest-path spikes/s-aware/Cargo.toml
set -u

HERE=$(cd "$(dirname "$0")" && pwd)
BIN=${AWARE_BIN:-$HERE/target/debug/aware-server}
PLUGIN=$HERE/plugin
CLAUDE_MODEL=${CLAUDE_MODEL:-claude-haiku-5-5}
TMP=${AWARE_TMP:-$(mktemp -d /tmp/aware-run.XXXXXX)}
BOUND=120

if [ ! -x "$BIN" ]; then
  echo "aware-server is missing: $BIN (build it first)" >&2
  exit 2
fi
echo "outputs: $TMP"

PROMPT_C1='What is the magic word from your fleet server instructions? Then read the fleet://stations resource and tell me the fruit word in it.'
PROMPT_C3='Which skills do you have? If you have a fleet guide skill, read it and tell me the fruit word in it.'
PROMPT_STATION='What is the station word?'

# A new directory for one run: prints its path.
newdir() {
  mkdir -p "$TMP/$1/work"
  echo "$TMP/$1"
}

# claude_run ID MESSAGE_TEXT [extra claude args...]
# The MCP server is always configured (one of the two arguments below).
claude_run() {
  id=$1
  text=$2
  shift 2
  dir=$(newdir "$id")
  config=$(jq -nc --arg bin "$BIN" --arg log "$dir/aware.log" \
    '{mcpServers:{aware:{type:"stdio",command:$bin,args:[],env:{AWARE_LOG:$log}}}}')
  message=$(jq -nc --arg t "$text" '{type:"user",message:{role:"user",content:$t}}')
  (
    cd "$dir/work" || exit 2
    printf '%s\n' "$message" | timeout $BOUND claude -p \
      --input-format stream-json --output-format stream-json --verbose \
      --permission-mode dontAsk --model "$CLAUDE_MODEL" \
      --no-session-persistence --strict-mcp-config \
      --mcp-config "$config" \
      --allowedTools mcp__aware ListMcpResourcesTool ReadMcpResourceTool Skill \
      "$@" >"$dir/out.jsonl" 2>"$dir/err.txt"
    echo "exit=$?" >"$dir/exit.txt"
  )
  summarize_claude "$id" "$dir"
}

# claude_noaware ID MESSAGE_TEXT [extra claude args...]: no MCP server.
claude_noaware() {
  id=$1
  text=$2
  shift 2
  dir=$(newdir "$id")
  message=$(jq -nc --arg t "$text" '{type:"user",message:{role:"user",content:$t}}')
  (
    cd "$dir/work" || exit 2
    printf '%s\n' "$message" | timeout $BOUND claude -p \
      --input-format stream-json --output-format stream-json --verbose \
      --permission-mode dontAsk --model "$CLAUDE_MODEL" \
      --no-session-persistence --strict-mcp-config \
      --allowedTools Skill \
      "$@" >"$dir/out.jsonl" 2>"$dir/err.txt"
    echo "exit=$?" >"$dir/exit.txt"
  )
  summarize_claude "$id" "$dir"
}

summarize_claude() {
  id=$1
  dir=$2
  echo "=== $id ($(cat "$dir/exit.txt" 2>/dev/null))"
  jq -c 'select(.type=="system" and .subtype=="init")
         | {model, mcp_servers, tool_count: (.tools|length),
            mcp_tools: [.tools[]|select(startswith("mcp__"))],
            resource_tools: [.tools[]|select(test("Resource"))],
            skill_tool: (.tools|index("Skill") != null),
            skills: (.skills // null), slash_commands: (.slash_commands // null)}' \
    "$dir/out.jsonl" 2>/dev/null | head -1 | cut -c1-1500
  echo "-- tool calls:"
  jq -r 'select(.type=="assistant") | .message.content[]? | select(.type=="tool_use")
         | "\(.name) \(.input|tostring)"' "$dir/out.jsonl" 2>/dev/null | cut -c1-200
  echo "-- result:"
  jq -c 'select(.type=="result")
         | {is_error, result, usage: {in: .usage.input_tokens, out: .usage.output_tokens,
            cache_create: .usage.cache_creation_input_tokens, cache_read: .usage.cache_read_input_tokens},
            cost: .total_cost_usd, turns: .num_turns}' "$dir/out.jsonl" 2>/dev/null | cut -c1-1200
  echo "-- server log:"
  cat "$dir/aware.log" 2>/dev/null
  echo "-- stderr (first lines):"
  head -3 "$dir/err.txt" 2>/dev/null | cut -c1-300
}

# codex_run ID PROMPT [extra codex exec args...]
codex_run() {
  id=$1
  prompt=$2
  shift 2
  dir=$(newdir "$id")
  (
    cd "$dir/work" || exit 2
    timeout $BOUND codex exec --ephemeral --ignore-user-config --skip-git-repo-check \
      --json -s read-only -C "$dir/work" \
      -c model_reasoning_effort='"low"' \
      "$@" "$prompt" </dev/null >"$dir/out.jsonl" 2>"$dir/err.txt"
    echo "exit=$?" >"$dir/exit.txt"
  )
  summarize_codex "$id" "$dir"
}

summarize_codex() {
  id=$1
  dir=$2
  echo "=== $id ($(cat "$dir/exit.txt" 2>/dev/null))"
  echo "-- event types:"
  jq -r '.type + (if .item then "/" + .item.type else "" end)' "$dir/out.jsonl" 2>/dev/null | sort | uniq -c
  echo "-- mcp / tool items:"
  jq -c 'select(.type=="item.completed") | .item
         | select(.type=="mcp_tool_call" or .type=="command_execution" or .type=="tool_call")
         | {type, server, tool, status, command, arguments}' "$dir/out.jsonl" 2>/dev/null | cut -c1-300
  echo "-- answer:"
  jq -r 'select(.type=="item.completed") | .item | select(.type=="agent_message") | .text' \
    "$dir/out.jsonl" 2>/dev/null | cut -c1-600
  echo "-- usage:"
  jq -c 'select(.type=="turn.completed") | .usage' "$dir/out.jsonl" 2>/dev/null
  echo "-- server log:"
  cat "$dir/aware.log" 2>/dev/null
  echo "-- stderr (first lines):"
  head -3 "$dir/err.txt" 2>/dev/null | cut -c1-300
}

run_one() {
  case "$1" in
    C1) claude_run C1 "$PROMPT_C1" ;;
    C2) claude_run C2 '/mcp__aware__validate_on_platform macOS' ;;
    C3) claude_run C3 "$PROMPT_C3" --plugin-dir "$PLUGIN" ;;
    C3B) claude_run C3B "$PROMPT_C3" ;;
    C4) claude_noaware C4 "$PROMPT_STATION" --append-system-prompt 'The station word is LIME.' ;;
    X1)
      codex_run X1 "$PROMPT_C1" \
        -c "mcp_servers.aware.command=\"$BIN\"" \
        -c "mcp_servers.aware.env={AWARE_LOG=\"$TMP/X1/aware.log\"}" \
        -c 'mcp_servers.aware.default_tools_approval_mode="approve"'
      ;;
    X2)
      codex_run X2 "$PROMPT_C1" --enable mcp_2026_07_28 \
        -c "mcp_servers.aware.command=\"$BIN\"" \
        -c "mcp_servers.aware.env={AWARE_LOG=\"$TMP/X2/aware.log\"}" \
        -c 'mcp_servers.aware.default_tools_approval_mode="approve"'
      ;;
    X1APP)
      dir=$(newdir X1APP)
      AWARE_LOG="$dir/aware.log" python3 -I "$HERE/appserver_probe.py" mcp "$dir/out" "$dir/work" "$BIN" \
        >"$dir/summary.txt" 2>&1
      echo "=== X1APP"
      cat "$dir/summary.txt"
      echo "-- server log:"
      cat "$dir/aware.log" 2>/dev/null
      ;;
    X3)
      dir=$(newdir X3)
      python3 -I "$HERE/appserver_probe.py" skills "$dir/out" "$dir/work" "$PLUGIN/skills" \
        "$PROMPT_C3" >"$dir/summary.txt" 2>&1
      echo "=== X3 (app-server skills/extraRoots/set)"
      cat "$dir/summary.txt"
      ;;
    X3CFG)
      dir=$(newdir X3CFG)
      python3 -I "$HERE/appserver_probe.py" config "$dir/out" "$dir/work" \
        "$PLUGIN/skills/fleet-guide/SKILL.md" >"$dir/summary.txt" 2>&1
      echo "=== X3CFG (-c skills.config path entry)"
      cat "$dir/summary.txt"
      ;;
    X4) codex_run X4 "$PROMPT_STATION" -c 'developer_instructions="The station word is LIME."' ;;
    *) echo "unknown experiment $1" >&2 ;;
  esac
}

if [ "$#" -eq 0 ]; then
  set -- C1 C2 C3 C3B C4 X1 X1APP X2 X3 X3CFG X4
fi
for e in "$@"; do
  run_one "$e"
done
echo "outputs: $TMP"
