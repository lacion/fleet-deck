# MCP Apps demo server (B15)

`demo_app_server.py` is a small stdio MCP server that ships one MCP App. The live smoke (`tools/smoke-mcp-apps.sh`) and the manual checks of MCP Apps use it. It has no dependency besides Python 3.

## Tools and resource

| Name | Visibility | Read-only | What it does |
|---|---|---|---|
| `show_counter` | model, app | yes | Names the app `ui://demo/counter.html` and returns the counter. |
| `bump` | app | no | Adds 1 to the counter. |
| `slow_bump` | app | no | Waits 5 s, writes `slow_bump.txt` into the working directory of the server, adds 1 to the counter. |

The resource `ui://demo/counter.html` has the MIME type `text/html;profile=mcp-app` and an empty `_meta.ui.csp`.

The server appends every request that it receives to the file in `$DEMO_LOG` (default `/tmp/demo_app_server.log`), one JSON line each. Use the log to see what the MCP client (Codex or Claude) sent, for example the extension in `initialize`.

## Why `slow_bump` exists

The provider has a tool timeout (Claude: `timeout` in the MCP config; Codex: `tool_timeout_sec`). With a timeout of 1 s, a `slow_bump` call gets a provider error while the server goes on and writes `slow_bump.txt`. This is the case that the call barrier of the design must handle: the call stays `uncertain` with the cause `provider_error` until the owner releases it.

## Run the smoke

1. Build the binary: `umask 022 && sh tools/heavy.sh cargo build -p fleetdeck-cli`.
2. Run `sh tools/smoke-mcp-apps.sh`. The header of the script lists the steps and the environment variables.
3. Read the records in `~/fleetdeck-briefs/backlog/mcp-apps/smoke/<time>/<provider>/`:
   - `events.jsonl`: the events of the session, with `tool.app` and `revert.updated`.
   - `bump-reply.json`, `slow_bump-reply.json`, `app-calls-uncertain.json`, `release-reply.json`: the answers of the board routes.
   - `demo-server.log`: the requests of the provider to the server.
   - `station.log`, `deck.log`: the logs of the two processes.

`slow_bump-reply.json` and `station.log` show whether Codex answered a JSON-RPC error or an `isError` result after its timeout (design fact 21).
