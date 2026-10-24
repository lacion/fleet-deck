# S-AWARE: what do Claude and Codex do with MCP resources, prompts, instructions and injected skills? (MCP P3)

- **Date:** 2026-10-19
- **Versions:** Claude Code 2.1.302 (`claude`, model `claude-haiku-5-5`), Codex 0.163.0 (`codex`, default model `gpt-daybreak-blue-latest`, `model_reasoning_effort="low"`), `rmcp` 3.5.1 (pinned `=3.5.1`), rustc 1.96.0, edition 2024.
- **Result:** **PASS with notes.** All eight experiments ran once. Three extra runs were added: C3B (a model turn, the baseline for C3), and X1APP and X3CFG (no model turn), see 4.3. Both hosts give the model the MCP `instructions` and MCP resources. Only Claude turns MCP prompts into slash commands. Claude speaks the 2026-07-28 era (`server/discover`) against an rmcp server; Codex 0.163.0 speaks the legacy era (`initialize`, `2025-06-18`), also with `--enable mcp_2026_07_28`. Skill injection without a write works on both: `--plugin-dir` (Claude) and the app-server request `skills/extraRoots/set` (Codex).

## 1. Question

Fleet Deck starts Claude as `claude -p --input-format stream-json ...` and Codex as `codex app-server`, and adds MCP servers by `--mcp-config` and `-c mcp_servers.<name>.*`. Which MCP features (instructions, resources, prompts) and which skill-injection paths reach the model in that mode, and which protocol era does each host use against an `rmcp` server?

## 2. Method

- `spikes/s-aware/` is a standalone crate (own `[workspace]` and `Cargo.lock`, `/target/` ignored). `src/main.rs` builds the stdio server `aware-server` with `rmcp` 3.5.1 (`server`, `macros`, `schemars`, `transport-io`). It serves:
  - instructions `Fleet test server. The magic word is PLUM.`;
  - the tool `fleet_echo { text }` returning `echo:<text>`;
  - the resource `fleet://stations` (`text/markdown`, contains `APRICOT`) and the template `fleet://sessions/{id}` (contains `MANGO-<id>`);
  - the prompt `validate_on_platform { platform }`. Its message is "Reply with the word KIWI and the platform name." **Deviation:** the server appends " The platform is <platform>." to the message, so that the argument reaches the model;
  - a log line for every request method (`initialize`, `server/discover`, `notifications/initialized`, `tools/list`, `tools/call`, `resources/list`, `resources/templates/list`, `resources/read`, `prompts/list`, `prompts/get`) to stderr and to the file named by `AWARE_LOG`.
- `run.sh` runs each experiment in a new temporary directory with a 120 s bound and records the full stream output there (`out.jsonl`, `err.txt`, `aware.log`). It prints a summary. The recorded outputs stay in the temporary directory and are not in the repository.
- Claude runs: `claude -p --input-format stream-json --output-format stream-json --verbose --permission-mode dontAsk --model claude-haiku-5-5 --no-session-persistence --strict-mcp-config --mcp-config <aware, stdio> --allowedTools mcp__aware ListMcpResourcesTool ReadMcpResourceTool Skill`. The user message goes through stdin as one stream-json line. Differences from Fleet Deck: `dontAsk` (the harness cannot answer permission requests), `--strict-mcp-config` and `--no-session-persistence` (no write to `~/.claude`), and the two resource tools and `Skill` in the allow list. Fleet Deck passes only `--allowedTools mcp__fleetdeck`. Whether `mcp__aware` alone covers `ReadMcpResourceTool` is **not measured**.
- Codex runs: `codex exec --ephemeral --ignore-user-config --skip-git-repo-check --json -s read-only -C <tmp> -c model_reasoning_effort="low" -c mcp_servers.aware.command=<binary> -c mcp_servers.aware.env={AWARE_LOG=...} -c mcp_servers.aware.default_tools_approval_mode="approve"`. The app-server probes use `appserver_probe.py` (JSON-RPC over stdio, standard library only, run with `python3 -I`).
- The user's own setup is in the baseline: Claude loaded 104 user and plugin skills and the user hooks; Codex loaded the user's other MCP servers in the app-server probe (not in `exec`, which uses `--ignore-user-config`). Both CLIs keep their own state (Codex: `logs_2.sqlite`, `models_cache.json`, `shell_snapshots`). The spike did not write any file of its own into `~/.claude` or `~/.codex`.

## 3. Reproduce

```sh
umask 022 && sh tools/heavy.sh cargo build --manifest-path spikes/s-aware/Cargo.toml
sh spikes/s-aware/run.sh            # all experiments, one run each
sh spikes/s-aware/run.sh C1 X3      # chosen experiments
```

`run.sh` needs `jq`, `python3`, `timeout`, and both CLIs signed in. It starts real model runs.

## 4. Results

| Id | Experiment | Result | Evidence |
| --- | --- | --- | --- |
| C1 | Claude, MCP instructions and resource | PASS | Answer: "The magic word ... is **PLUM**" and "Its fruit word is **APRICOT**". Tool calls: `ToolSearch select:ReadMcpResourceTool`, then `ReadMcpResourceTool {"server":"aware","uri":"fleet://stations"}`. Server log: `server/discover`, `prompts/list`, `tools/list`, `resources/list`, `resources/read uri=fleet://stations`. `system/init` lists `mcp__aware__fleet_echo`, and the built-in tools `ListMcpResourcesTool`, `ReadMcpResourceDirTool`, `ReadMcpResourceTool` (they are absent from a run without an MCP server, C4: 25 tools, C1: 29). `mcp_servers`: `aware` `connected`. |
| C2 | Claude, MCP prompt as a slash command | PASS | The user message `/mcp__aware__validate_on_platform macOS` in stream-json: the model answered `KIWI macOS`. Server log ends with `prompts/get name=validate_on_platform arguments={"platform":"macOS"}`. No tool call. The init `slash_commands` list contains `mcp__aware__validate_on_platform`. |
| C3 | Claude, `--plugin-dir` skill | PASS | The init `skills` (105 entries) and `slash_commands` list `fleetdeck-aware:fleet-guide` (name `<plugin>:<skill>`); `plugins` lists `fleetdeck-aware@inline` `0.0.0`. The model called `Skill {"skill":"fleetdeck-aware:fleet-guide"}` and answered "Its fruit word is **GUAVA**". Without the plugin (C3B): 104 skills, no fleet guide, the model searched with `Bash` (denied) and found nothing. |
| C4 | Claude, `--append-system-prompt` | PASS | Answer: "The station word is **LIME**." Run without MCP: 25 tools. |
| X1 | Codex `exec`, MCP instructions and resource | PASS | Answer: "magic word is **PLUM** ... `fleet://stations` is **APRICOT**". The model called the Codex built-in tools `list_mcp_resources` (all servers), `list_mcp_resource_templates`, then `read_mcp_resource {"server":"aware","uri":"fleet://stations"}`. Server log: `initialize protocolVersion=2025-06-18 client={"name":"codex-mcp-client","version":"0.163.0"}`, `tools/list`, `notifications/initialized`, `resources/list`, `resources/templates/list`, `resources/read`. |
| X1APP | Codex `app-server`, MCP (no model turn) | PASS | `mcpServerStatus/list` returns for `aware`: tools `fleet_echo`, resource `fleet://stations`, template `fleet://sessions/{id}`. `mcpServer/resource/read` returns the `APRICOT` text. The server log is the same legacy sequence as X1 (`initialize` `2025-06-18`). The log has two `start` blocks: the app-server started the server process twice (one short block with `initialize` and `tools/list` only). |
| X2 | Codex `exec --enable mcp_2026_07_28` | PARTIAL | Same result as X1 (PLUM, APRICOT). The server log still shows `initialize protocolVersion=2025-06-18`, no `server/discover`. The only difference: the model also called `list_mcp_resources {"server":"aware"}`. The run printed the warning item "Under-development features enabled: mcp_2026_07_28". The flag changed nothing against this server in 0.163.0. |
| X3 | Codex, skill injection | PASS (app-server request); FAIL (config key) | App-server `skills/extraRoots/set {"extraRoots":["<tmp>/plugin/skills"]}` answered `{}`. `skills/list` then returned `fleetdeck-aware:fleet-guide` (the name has the plugin prefix because the root's parent holds `.claude-plugin/plugin.json`). One turn: the model listed the skill, read it and answered "Its fruit word is **GUAVA**". Tokens of that turn: 27369 input (24704 cached), 196 output. |
| X3CFG | Codex, `-c skills.config=[{path=<SKILL.md>,enabled=true}]` (no model turn) | FAIL | `skills/list` returned only `find-skills`, `imagegen`, `openai-docs`, `review-agent`, `skill-creator`, `skill-installer`. The key switches a skill on or off; it does not add a root. |
| X4 | Codex `exec -c developer_instructions=...` | PASS | Answer: `LIME`. 14439 input tokens (12288 cached), 6 output. No MCP server, no tool call. |

### 4.1 Methods that each host called against the `rmcp` server

| Host | Opening request | After it | On use |
| --- | --- | --- | --- |
| Claude 2.1.302 | `server/discover` (never `initialize`) | `prompts/list`, `tools/list`, `resources/list` at start (order varies between runs: the three run in parallel) | `resources/read` (C1), `prompts/get` (C2) |
| Codex 0.163.0 `exec` and `app-server` | `initialize` with `protocolVersion` `2025-06-18` (client `codex-mcp-client`), then `tools/list`, `notifications/initialized` | `resources/list`, `resources/templates/list` when the model calls the list tools | `resources/read` |
| Codex 0.163.0 with `--enable mcp_2026_07_28` | same as above (`initialize` `2025-06-18`) | same | same |

Codex never called `prompts/list` or `prompts/get`. Claude never called `resources/templates/list` (the model did not need it; `fleet://sessions/{id}` was not read by any run). The log does not record the `protocolVersion` that Claude sent in `server/discover` or the version that the server answered.

### 4.2 Token numbers of C3 against the run without the plugin (C3B)

| Run | First request: input + cache create + cache read | Whole run: input / cache create / cache read / output | Turns | Cost (USD) |
| --- | --- | --- | --- | --- |
| C3 (plugin) | 2 + 21098 + 10960 = 32060 | 6 / 42408 / 21920 / 1119 | 3 | 0.00608 |
| C3B (no plugin) | 2 + 21086 + 10960 = 32048 | 4 / 42527 / 21920 / 1420 | 2 | 0.00625 |

The plugin skill costs about 12 tokens in the first request (one line in the skill list: name and description). The whole-run numbers are not comparable: the models used 3 and 2 turns and different thinking lengths. The skill body (`GUAVA` text, about 30 tokens) enters the context only when the model calls `Skill`. The 32k tokens of the first request come mostly from the user's own setup (104 skills, `CLAUDE.md`, tools), not from the spike. C4 without any MCP server: 31924 cache-create tokens in the first request.

### 4.3 What the extra rows are

X1APP and X3CFG answer the "does `codex app-server` behave differently" question of the brief; they start `codex app-server` but run no model turn. C3B is the "run without the plugin" of the brief. X3 is the one run of X3 (a turn through the app-server, because `exec` has no request for extra skill roots).

## 5. Findings

1. **MCP `instructions` reach the model in both hosts.** Claude (C1, C3B) and Codex (X1, X2) answered PLUM without any tool call that returns it. In C3B the model quoted the text unprompted ("The `aware` MCP server instructions say ... PLUM").
2. **Resources reach the model as tools, not as context.** Claude adds three built-in tools when an MCP server is configured: `ListMcpResourcesTool`, `ReadMcpResourceTool`, `ReadMcpResourceDirTool`. In C1 `ReadMcpResourceTool` was deferred and the model loaded it with `ToolSearch`. Codex always has `list_mcp_resources`, `list_mcp_resource_templates`, `read_mcp_resource` (they appear as `mcp_tool_call` items with `server` `codex` or the server name). Neither host puts resource content into the context by itself.
3. **MCP prompts are slash commands in Claude only.** `/mcp__<server>__<prompt> <args>` in a stream-json user message expands through `prompts/get` (C2). Codex 0.163.0 never called `prompts/list`. The host, not the model, calls `prompts/get`, so a prompt is a user-side feature: Fleet Deck would have to send the slash text as the user input.
4. **The two hosts use different eras against an rmcp server that serves both.** Claude 2.1.302 opens with `server/discover` (2026-07-28 lifecycle). Codex 0.163.0 opens with `initialize` `2025-06-18` and ignores `mcp_2026_07_28` against this server. A station MCP server must keep both eras (as `spikes/s-rmcp` showed rmcp can).
5. **Skill injection without a write to the workspace or the user config.**
   - Claude: `--plugin-dir <dir>` works in `-p` stream-json mode. The plugin needs `.claude-plugin/plugin.json` and `skills/<name>/SKILL.md`. The skill is listed in `system/init` and the model loads it through the `Skill` tool. The cost is about 12 tokens for the listing. The `--allowedTools` list must allow `Skill` (it was allowed here).
   - Codex: no `-c` key adds a skill root (X3CFG: `skills.config` only enables or disables). The app-server JSON-RPC request `skills/extraRoots/set { extraRoots: [<abs path>] }` adds a root for the process (X3). The request exists in the 0.163.0 schema (`codex app-server generate-json-schema`). Not measured: whether the roots stay across `thread/start` calls of other threads, and what happens on a reload (`forceReload` was set in the `skills/list` call, the turn came after it).
   - `CODEX_HOME`: the binary honours it (strings of the binary and `initialize` answer `codexHome`). A temporary `CODEX_HOME` needs the auth file copied, so this spike did not try it (the brief tells to skip).
6. **System-prompt-style injection works with one flag or key on both.** Claude `--append-system-prompt` (C4) and Codex `-c developer_instructions=...` (X4) both answered LIME. For the Codex app-server, `thread/start` also has `developerInstructions` and `baseInstructions` in its schema; that route was not run.
7. **Prompt-cache effect.** In X1, X2 and X4 most Codex input tokens were cached (54272 of 69529, 78208 of 92190, 12288 of 14439). Cost comparisons of the injection paths on Codex are not made here.

## 6. Answers

**(a) Do the hosts give the model MCP resources (as tools or context) and prompts (as slash commands) in Fleet Deck's headless mode?**
Resources: yes, in both hosts, as tools (Claude `ReadMcpResourceTool`, Codex `read_mcp_resource`), not as automatic context. Claude C1 ran in the stream-json mode of Fleet Deck (without the Fleet Deck permission-prompt tool); Codex was run through `exec` (X1) and checked through `app-server` (X1APP: `mcpServerStatus/list` and `mcpServer/resource/read` work). Prompts: Claude yes, as the slash command `/mcp__<server>__<prompt>` in a stream-json user message (C2). Codex: no evidence that it lists or calls prompts (no `prompts/list` in X1, X1APP, X2).

**(b) Which era does each host use against an rmcp server?**
Claude 2.1.302: the modern era (`server/discover`, no `initialize`). Codex 0.163.0: the legacy era (`initialize`, `protocolVersion` `2025-06-18`), also with `--enable mcp_2026_07_28`. Not measured: how Claude behaves against a server that answers `server/discover` with an error.

**(c) What is the cheapest working skill-injection path per host with no write into the workspace or the user's config?**
Claude: `--plugin-dir <dir>` with one skill (about 12 tokens in the first request; the body loads on use). Codex: the app-server request `skills/extraRoots/set` with a directory of skills (no config key does it). On Codex `exec` there is no such path in 0.163.0 from the options tested. The alternative that works on both is a short text in `--append-system-prompt` (Claude) or `developer_instructions` (Codex), at the price of always being in the context.

**(d) Does MCP `instructions` reach the model in each host?**
Yes in both (C1, C3B on Claude; X1, X2 on Codex). Keep the server instructions short: they are in the context of every session that has the server.

## 7. Limits of this spike

- One run per experiment, one model per host. A model answer can vary; the evidence is the tool calls and the server log, not only the answer text.
- Fleet Deck's permission prompt tool (`--permission-prompt-tool stdio`), `--include-partial-messages` and `--replay-user-messages` were not used. `--permission-mode dontAsk` replaced the permission flow.
- The prompt `validate_on_platform` was tested only on Claude. The template `fleet://sessions/{id}` was listed (Codex X1, X1APP) but no run read it.
- Codex `exec` and the Codex `app-server` could differ in more ways than the MCP checks here; only the MCP listing, the resource read and the skill request were run through the app-server.
