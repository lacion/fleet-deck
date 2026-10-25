# S-MOVE: move a provider conversation to another machine and folder (FG-9, O27)

- **Date:** 2026-10-17
- **CLIs:** `claude` 2.1.302 (Claude Code), `codex-cli` 0.162.1
- **Result:** Claude **PASS**, Codex **PASS**. Both resume by native id in folder B on "machine 2" with only the transcript file copied, and the model answers with the word from machine 1.
- **ADR:** [ADR-010](../../docs/v2/adr/ADR-010-session-move.md)

## 1. Question

A conversation starts in folder A on machine 1. Only the provider transcript files and the native session id move to machine 2. Does the conversation resume, with its history, in folder B (a different absolute path) on machine 2?

## 2. Method

### 2.1 The simulation of "another machine"

| Item | Machine 1 | Machine 2 |
| --- | --- | --- |
| Provider home | `HOME=<root>/<provider>-m1` (so `~/.claude`, `~/.codex` are new and empty) | A different new `HOME=<root>/<provider>-m2` (one new home for each test case) |
| Working folder | `<root>/a/repo` (folder A, `git init`) | `<root>/b/repo` (folder B, `git init`, another absolute path) |
| Environment | Empty, except `PATH` (only the tool directories, `/usr/bin`, `/bin`), `HOME`, `LANG`, `DISABLE_AUTOUPDATER=1`, and the Claude auth variables | The same |
| Files copied from machine 1 | none | Only the transcript file, mode 0600 |
| Auth | Claude: the caller's `ANTHROPIC_BASE_URL` + `ANTHROPIC_AUTH_TOKEN` (a gateway); without them a copy of `~/.claude/.credentials.json`. Codex: a copy of `~/.codex/auth.json` (ChatGPT login), mode 0600 | The same |

No `CLAUDE_CONFIG_DIR` or `CODEX_HOME` is set: a new `HOME` gives each machine its own default provider home (`$HOME/.claude`, `$HOME/.codex`). This is the layout that Fleet Deck's `history::locate::resolve_home` uses when those variables are not set. No `.claude.json`, `settings.json`, `config.toml`, plugin, hook, MCP server, or state database is copied.

### 2.2 The provider calls (the same as Fleet Deck)

- **Claude** (`crates/fleetdeck-providers/src/claude/args.rs` `cli_args`): `claude -p --input-format stream-json --output-format stream-json --verbose --include-partial-messages --replay-user-messages --permission-prompt-tool stdio --permission-mode default [--resume <id>]`, with one stream-json user message on stdin.
- **Codex** (`crates/fleetdeck-providers/src/codex/core.rs` `initialize`, `open_thread`, `turn_start`): `codex app-server` over JSON-RPC: `initialize` (experimentalApi), `initialized`, `thread/start` or `thread/resume { threadId, cwd, approvalPolicy: "untrusted", sandbox: "read-only" }`, `turn/start`. The helper is `codex_app_server.py`. The script also tests the CLI path `codex exec --json resume <id> <prompt>`.

Prompts: turn 1 "Remember the word PELICAN-7. Reply OK."; the turn on machine 2 "What word did I ask you to remember?". A test case passes when the machine-2 answer contains `PELICAN-7`.

### 2.3 The test cases

| Case | Transcript on machine 2 | Call on machine 2 |
| --- | --- | --- |
| Claude C1 (minimal) | Same relative path as on machine 1: `projects/<enc(A)>/<id>.jsonl` | `--resume <id>` in folder B |
| Claude C2 (path rewrite) | Under the directory of B: `projects/<enc(B)>/<id>.jsonl` | `--resume <id>` in folder B |
| Claude control | none | `--resume <id>` in folder B |
| Codex X1 (minimal) | Same relative path: `sessions/YYYY/MM/DD/rollout-<ts>-<id>.jsonl` | app-server `thread/resume` with `cwd` = B |
| Codex X2 | The same | `codex exec resume <id>` in folder B |
| Codex X3 | The same | app-server `thread/resume` **without** `cwd`, process in folder B (no turn) |
| Codex control | none | app-server `thread/resume` |

`enc(path)` is Claude's directory name of a path: every character that is not ASCII alphanumeric becomes `-` (`/tmp/s-move-final/b/repo` → `-tmp-s-move-final-b-repo`), the same as `history::locate::encode_cwd`.

## 3. Reproduce

```sh
bash spikes/s-move/run.sh            # all; or: run.sh claude | run.sh codex
S_MOVE_KEEP=1 bash spikes/s-move/run.sh   # keep the run root to look at the files
```

The script needs `claude`, `codex`, `node` (the Codex npm launcher), `python3`, `git`, and optionally `sqlite3`. It costs 3 short Claude turns and 3 short Codex turns (the controls and X3 make no model call). It holds no secret. It never puts a secret on a command line. It deletes the auth copies at the end. If a provider refreshes a copied OAuth token, the script copies the refreshed file back, so the owner's login keeps a valid refresh token (in the runs below no token was refreshed: the SHA-256 of every copy stayed equal to the original).

## 4. Results (output excerpts, from `results.txt`)

### 4.1 Claude: PASS

```
== Claude 2.1.302 (Claude Code)
machine 1, folder A, turn 1: {"session_id": "06c861ec-638f-4e3e-b2ea-e9b391d83d7f", "cwd": "/tmp/s-move-final/a/repo", "subtype": "success", "result": "OK", "errors": null}
transcript on machine 1: claude-m1/.claude/projects/-tmp-s-move-final-a-repo/06c861ec-638f-4e3e-b2ea-e9b391d83d7f.jsonl (233995 bytes)
files of machine 1 below projects/: ./-tmp-s-move-final-a-repo/06c861ec-638f-4e3e-b2ea-e9b391d83d7f.jsonl
C1 (file under the directory of A), machine 2, folder B: {"session_id": "06c861ec-…", "cwd": "/tmp/s-move-final/b/repo", "subtype": "success", "result": "PELICAN-7", "errors": null}
Claude C1: PASS
C1 files after the turn: ./-tmp-s-move-final-a-repo/06c861ec-638f-4e3e-b2ea-e9b391d83d7f.jsonl
C2 (file under the directory of B), machine 2, folder B: {"session_id": "06c861ec-…", "cwd": "/tmp/s-move-final/b/repo", "subtype": "success", "result": "PELICAN-7", "errors": null}
Claude C2: PASS
control (no transcript): {"subtype": "error_during_execution", "errors": ["No conversation found with session ID: 06c861ec-638f-4e3e-b2ea-e9b391d83d7f"]}
```

Facts:

1. **Location.** `$CLAUDE_CONFIG_DIR` (else `$HOME/.claude`) `/projects/<enc(cwd)>/<session id>.jsonl`. The directory name comes from the cwd at the start. A one-turn session without tools has only this file. A real session can also have `projects/<enc(cwd)>/<id>/subagents/agent-*.jsonl` + `agent-*.meta.json` and `projects/<enc(cwd)>/<id>/tool-results/*.txt` (seen in the owner's own `~/.claude/projects`), and `file-history/<id>/` (rewind snapshots).
2. **Resume works in both places.** `--resume <id>` found the file under the directory of A (C1) and under the directory of B (C2). The CLI looks in all project directories for the id. Without the file, resume fails with `No conversation found with session ID` (control): the history comes from the copied file, not from somewhere else.
3. **Where new turns go.** In C1 the CLI appended the new turn to the file under the directory of **A** (no file under the directory of B appeared). Fleet Deck's history reader (`history::locate`) looks only at `projects/<encode_cwd(session cwd)>/<id>.jsonl`, so with C1 the board history of the moved session is `not_found`. With C2 the file and the new turns are where Fleet Deck looks.
4. **Contents need no rewrite.** Each record has a `cwd` field. The old records keep `cwd = A`, the new records get `cwd = B` (C2 file after the turn: 12 records with A, 4 with B). The `init` frame reports `cwd = B`.
5. **The Fleet Deck flags work.** C1 and C2 used the exact `cli_args` list with stream-json input.

### 4.2 Codex: PASS

```
== Codex codex-cli 0.162.1
machine 1, folder A, turn 1 (app-server thread/start): {"thread_id": "01a14732-e722-7f93-8fb7-c6b49aa40ffb", "thread_cwd": "/tmp/s-move-final/a/repo", "resumed_turns": 0, "turn_status": "completed", "agent_message": "OK"}
rollout on machine 1: sessions/2026/10/17/rollout-2026-10-17T02-11-20-01a14732-e722-7f93-8fb7-c6b49aa40ffb.jsonl (35623 bytes)
X1 (app-server thread/resume, cwd B), machine 2: {"thread_id": "01a14732-…", "thread_cwd": "/tmp/s-move-final/b/repo", "resumed_turns": 1, "turn_status": "completed", "agent_message": "PELICAN-7"}
Codex X1: PASS
X1 cwd records in the rollout after the turn: session_meta=/tmp/s-move-final/a/repo turn_context=/tmp/s-move-final/a/repo turn_context=/tmp/s-move-final/b/repo
X1 state DB row on machine 2: a/repo
X2 (codex exec resume, cwd B), machine 2: {"agent_message": "PELICAN-7"}
Codex X2: PASS
control (no rollout): {"step": "thread/resume", "error": {"code": -32600, "message": "no rollout found for thread id 01a14732-e722-7f93-8fb7-c6b49aa40ffb"}}
```

Second run (X3 added, `/tmp/s-move-final2`): X1 PASS, X2 PASS, and

```
X3 (thread/resume without cwd, process in B), machine 2: {"cwd_param": null, "thread_cwd": "/tmp/s-move-final2/a/repo", "resumed_turns": 1}
```

Facts:

1. **Location.** `$CODEX_HOME` (else `$HOME/.codex`) `/sessions/YYYY/MM/DD/rollout-<local start time>-<thread id>.jsonl`. The path does not depend on the cwd. The first record (`session_meta`) holds the thread id and `cwd`; each turn has a `turn_context` record with its `cwd`.
2. **Codex also keeps SQLite state** (`state_5.sqlite` table `threads` with `rollout_path` and `cwd`; `thread_history_1.sqlite`). Machine 2 had no database. On resume, Codex found the rollout by its id in `sessions/` and created the database row itself. The row's `cwd` was A after an app-server resume in both full runs, and B after an earlier app-server run and after `codex exec resume`. The row is list metadata (the `codex resume` picker filters by cwd); resume by id does not use it.
3. **Resume works with only the rollout,** at the same relative path, with no rewrite. Without the rollout: `no rollout found for thread id` (control).
4. **The `cwd` parameter is required.** With `cwd` = B the thread runs in B (`thread_cwd` B, a new `turn_context` with B). Without `cwd` (X3) Codex takes the recorded cwd **A**, also when the process runs in B. Fleet Deck always sends `cwd` (`codex/driver.rs`: `cwd: Some(cwd_text)` for start, resume and fork).
5. **Contents need no rewrite.** `session_meta.cwd` stays A; Fleet Deck's Codex history reader finds a rollout by the `-<id>.jsonl` suffix below `sessions/`, not by cwd.

### 4.3 Transcript sizes on the owner's machine (for the 64 MiB rule of O29)

Measured on 2026-10-17 in `~/.claude/projects` and `~/.codex/sessions` (sizes only, no contents read):

| Item | Count | Over 64 MiB | Largest |
| --- | --- | --- | --- |
| Claude main transcripts `<id>.jsonl` | 160 | 0 | 57.0 MiB |
| Claude main transcript + `<id>/subagents/` | 160 | 4 | — |
| Claude main transcript + all of `<id>/` (subagents + tool-results) | 160 | 4 | 300.3 MiB |
| Codex rollouts | 284 | 1 | 67.0 MiB |

## 5. Limitations of the simulation

1. One physical machine, one OS user (same uid), the same CLI versions on both sides. A version skew (an older CLI on machine 2) is not tested.
2. The same account on both sides: Claude through the same gateway token, Codex through the same ChatGPT login. A station signed in to another account is not tested. Codex rollouts can carry encrypted reasoning items (`encrypted_content`; none in these short runs) that may be bound to the account.
3. One short turn with no tools, no subagents, no compaction, no images, no MCP. Resume of a transcript with subagent (Claude) or child-thread (Codex) history is not tested.
4. The old absolute paths in the history stay: folder A, and for Claude the machine-1 paths of `tool-results/` files that the model saw in earlier tool output. The model can refer to them after the move. The new turn gets the new cwd (Claude system prompt; Codex `turn_context`).
5. `/tmp` paths: Codex prints `WARNING: proceeding, even though we could not create PATH aliases: Refusing to create helper binaries under temporary dir "/tmp"`. This comes from the `/tmp` home of the simulation, not from the move.
6. The transfer itself (link data channel, SHA-256, size cap, file modes) is not part of the spike; the files were copied with `install -m 600`.

## 6. Files

- `run.sh`: the reproducible spike (Claude and Codex).
- `codex_app_server.py`: the minimal `codex app-server` client with Fleet Deck's request shapes.
