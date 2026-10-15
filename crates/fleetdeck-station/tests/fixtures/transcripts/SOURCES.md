# Transcript fixtures (DL-189)

Every `.jsonl` file here comes from `tools/sanitize-transcript.py`. It keeps the
keys, the value types, the record order, the ids and the timestamps, and it
replaces every other string with a placeholder (`<text 1>`, `<path 2>`,
`<command 3>`, `<pattern 4>`). No real text is in these files.

Each `<name>.expected.json` holds the items that the parser must return for
the fixture, and the state of the answer.

The sources are local transcripts of Claude Code 2.1.299 and 2.1.300 and of
codex-cli 0.162.1. The file names below are the names of the source files; the
numbers are line ranges (first line is 0, both ends included).

| Fixture | Source | Lines |
| --- | --- | --- |
| `claude-2.1.300-basic.jsonl` | main session `00e00a5d-d49a-4946-b5bc-fcdf51bd4493.jsonl` | 2364-2371, 1380-1381 |
| `claude-2.1.300-tools.jsonl` | same main session | 1271-1276, 2582-2588, 2600-2607 |
| `claude-2.1.300-resumed.jsonl` | same main session (a session that was resumed: two versions, a gap of 12 hours) | 26-28, 2582-2586 |
| `claude-2.1.300-sidechain-meta.jsonl` | the same main session (lines 3-7, 1014, 2371) and sub-agent file `agent-a69d165204d59fa20.jsonl` (lines 14-16, side chain) | as listed |
| `codex-0.162.1-basic.jsonl` | `rollout-2026-10-14T00-52-03-01a13777-3cb3-7a42-b7ff-9572798fa426.jsonl` | 0-13 |
| `codex-0.162.1-tools.jsonl` | `rollout-2026-10-15T12-43-26-01a13f28-e384-7ed1-aee0-423548cce681.jsonl` (lines 0-29) and `rollout-2026-10-14T14-40-01-01a13a6d-4177-7011-a111-51e9b99469aa.jsonl` (lines 88-91) | as listed |
| `claude-unknown-shapes.jsonl` | made by hand: records of a made-up future format | all |

Some lines come from Claude Code 2.1.299, because the 2.1.300 transcripts on
the machine of the build hold no wrapper, meta, image or failed-tool record.
The `version` field of each record tells which one it is.

`codex-0.162.1-tools.jsonl` has one hand-made change: the function call
`exec_command` has the arguments `{"cmd": ..., "workdir": ...}`, and the
three tool outputs have `exit_code` fields (none, 2, 0), because the real
0.162.1 outputs hold no structured exit code (the parser reads only structured
fields).
