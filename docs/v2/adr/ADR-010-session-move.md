# ADR-010 — Move a session to another station (FG-9)

- **Status:** Proposed (revision 3, 2026-10-17: the answers to the adversary DESIGN REVIEW round 1, DL-225: §5.2 item 4, §6 and §10.1 are rewritten; RE-REVIEW pending)
- **Date:** 2026-10-17
- **Deciders:** architect (Fleet Git Phase 0); owner decisions O27-O30 (`fleet-git-owner-spec.md`)
- **Implements:** Fleet Git FG-9, owner decisions O27 (spike first), O28 (preconditions), O29 (transcript transfer), O30 (branch-only move)
- **Evidence:** `spikes/s-move/README.md`, reproducible with `spikes/s-move/run.sh`
- **Depends on:** FG-2 push state (O9-O11), FG-5 project homes (O16-O18), the history reader of DL-189 (`crates/fleetdeck-station/src/history/locate.rs`), `session.resume` (DL-118), the link data channel (ADR-004 `data_streams.v1`); the wire additions go into ADR-009 (O5)

## 1. Context

FG-9 moves a session from station 1 to station 2. The branch moves through git (FG-2 push). The conversation is in the provider transcript files on station 1. Station 2 has another provider home, and its worktree of the project has another absolute path. O27 requires a spike: prove, for Claude and for Codex separately, that a conversation resumes in another folder on another machine when only the transcript files and the native id move. A provider that fails gets the branch-only move of O30 and a DL entry.

## 2. Spike results

The spike (`spikes/s-move/`) simulated machine 2 with a new, empty `HOME` (so a new `~/.claude` or `~/.codex`), a different folder (`<root>/a/repo` → `<root>/b/repo`), an empty environment except `PATH`, `HOME`, `LANG` and the auth, and only the transcript file copied (mode 0600). It used the exact Fleet Deck calls: `claude/args.rs` `cli_args` (stream-json, `--resume <id>`) and the `codex app-server` requests of `codex/core.rs` (`thread/resume { threadId, cwd, approvalPolicy, sandbox }`, `turn/start`). Turn 1 on machine 1: "Remember the word PELICAN-7. Reply OK."; the turn on machine 2: "What word did I ask you to remember?".

| Provider | Case | Transcript on machine 2 | Answer on machine 2 | Result |
| --- | --- | --- | --- | --- |
| Claude 2.1.302 | C1 minimal | `projects/<enc(A)>/<id>.jsonl` (unchanged path) | `PELICAN-7` | PASS |
| Claude | C2 path rewrite | `projects/<enc(B)>/<id>.jsonl` | `PELICAN-7` | PASS |
| Claude | control | none | `No conversation found with session ID: <id>` | (expected failure) |
| Codex 0.162.1 | X1 app-server | `sessions/YYYY/MM/DD/rollout-<ts>-<id>.jsonl` (unchanged path) | `PELICAN-7`, `thread_cwd` = B, `resumed_turns` 1 | PASS |
| Codex | X2 `codex exec resume` | the same | `PELICAN-7` | PASS |
| Codex | X3 `thread/resume` without `cwd` | the same | `thread_cwd` = **A** (the recorded cwd) | (hazard) |
| Codex | control | none | `no rollout found for thread id <id>` (-32600) | (expected failure) |

`enc(path)` is Claude's project directory name (`history::locate::encode_cwd`: each non-ASCII-alphanumeric character becomes `-`).

Findings that drive the decisions:

1. **Claude** finds `--resume <id>` in any project directory, but it appends the new turns to the file where it found it. Under the directory of A (C1), Fleet Deck's history reader, which opens only `projects/<encode_cwd(session cwd)>/<id>.jsonl`, does not find the moved session. Under the directory of B (C2) both the CLI and the history reader work. The records keep their old `cwd` (A); new records get B. No content rewrite is necessary.
2. **Codex** finds the rollout by its id below `sessions/`. The path does not depend on the cwd. The SQLite state (`state_5.sqlite`, `thread_history_1.sqlite`) is not necessary: Codex creates the `threads` row on resume (its `cwd` column was A or B in different runs; resume by id does not read it). `thread/resume` without `cwd` runs the thread in the recorded folder A (X3), so the target must always send `cwd` = B. Fleet Deck already does (`codex/driver.rs`: `cwd: Some(cwd_text)`).
3. **Sizes** on the owner's machine (2026-10-17): no Claude main transcript is over 64 MiB (largest 57.0 MiB, 160 sessions); 4 Claude sessions are over 64 MiB with their `<id>/subagents/` files (largest session directory total 300.3 MiB); 1 of 284 Codex rollouts is over 64 MiB (67.0 MiB).

Limitations of the spike (details in the spike README §5): one physical machine and one uid; the same CLI versions and the same provider account on both sides; one short turn without tools, subagents, child threads, compaction or images; the old absolute paths (folder A, machine-1 `tool-results/` paths) stay in the history.

## 3. Decision per provider

| Provider | Decision |
| --- | --- |
| Claude | **Transcript move + resume by native id.** The target writes the transcript under the project directory of the target worktree path (C2), then resumes with `--resume <session_id>` in that worktree. |
| Codex | **Transcript move + resume by native id.** The target writes the root rollout at the same relative path below its Codex home, then resumes with `thread/resume { threadId, cwd: <target worktree> }`. |

No provider failed the spike, so no DL entry for a branch-only provider is necessary. The branch-only move (O30) stays as the fallback for one move when a transcript move is not possible (§5.4).

## 4. The files to move

### 4.1 Claude

Source home `H1` = the session's `CLAUDE_CONFIG_DIR`, else `$HOME/.claude` (`history::locate::resolve_home`). Target home `H2` = the same rule on the target with the target launch environment. `A` = the source session cwd, `B` = the target worktree path.

| Source | Target | Required | Note |
| --- | --- | --- | --- |
| `H1/projects/<enc(A)>/<id>.jsonl` | `H2/projects/<enc(B)>/<id>.jsonl` | yes | Path rewrite of the directory name only. The bytes are not changed. |
| `H1/projects/<enc(A)>/<id>/subagents/agent-*.jsonl`, `agent-*.meta.json` | `H2/projects/<enc(B)>/<id>/subagents/` (same names) | no | For the subagent history view (DL-189). Dropped first when the total is over the cap (§5.1). Resume with subagent history is not tested by the spike. |
| `H1/projects/<enc(A)>/<id>/tool-results/` | — | not moved | The transcript refers to these files by their machine-1 absolute path, which does not exist on the target. |
| `H1/file-history/<id>/`, `session-env/`, `todos/`, `.claude.json`, settings | — | not moved | Not necessary for resume (C1, C2). Claude rewind is not offered (`rollback: false`). |

### 4.2 Codex

Source home `H1` = the session's `CODEX_HOME`, else `$HOME/.codex`; target home `H2` by the same rule.

| Source | Target | Required | Note |
| --- | --- | --- | --- |
| `H1/sessions/YYYY/MM/DD/rollout-<ts>-<id>.jsonl` (the root thread) | `H2/sessions/YYYY/MM/DD/rollout-<ts>-<id>.jsonl` (same relative path) | yes | No rewrite. The target derives the date directories and the name from the source name only after validation (§5.2). |
| Rollouts of child threads (`thread_spawn_edges`) | — | not moved in v1 | Not tested by the spike. The move requires no running turn, so no child thread is active. The child history on the target is `not_found`. |
| `state_5.sqlite`, `thread_history_1.sqlite`, `logs_*`, `memories_*`, `goals_*`, `config.toml`, `auth.json` | — | never moved | Codex rebuilds the thread row on resume (X1). Credentials never move (O2 rule for git; the same rule for provider auth). |

## 5. Transfer rules (O29)

### 5.1 Limits and integrity

1. **Size cap: 64 MiB in total** for one move (the sum of all files). The source computes the total before it sends any byte. When the required file alone is over the cap, the transcript move is refused with `transcript_too_large` (§5.4). When the optional Claude subagent files make the total go over the cap, the source drops them, and the move event records `subagents_dropped`.
2. **SHA-256 per file.** The source sends the size and the SHA-256 of each file in the transfer header. The target computes the hash while it writes and refuses a file whose size or hash is different (`transfer_corrupt`).
3. **The native id must match.** The source opens the files with the trusted open of `history::locate` (`O_NOFOLLOW`, no-follow directory walk, owner uid check, the embedded session id of the head records equals the native id). The target repeats the embedded-id check on the received bytes before the rename.
4. **Freeze.** The source session must have no live provider process during the export: the source ends an idle provider process first (graceful stop) and refuses new input while the move runs, so that no turn appends to the file during the copy.

### 5.2 Paths and file modes on the target

1. The target computes every target path itself, from its own provider home, its own worktree path `B` and the native id. It never uses a path or a directory name that the source sent.
2. Accepted names: Claude `<uuid>.jsonl`, `agent-<id>.jsonl`, `agent-<id>.meta.json` with `[A-Za-z0-9_-]` ids; Codex `rollout-YYYY-MM-DDTHH-MM-SS-<uuid>.jsonl`, and the date directories come from that name. Any other name, `/`, `..`, or a NUL refuses the transfer (`transfer_refused`).
3. Directories that the target creates have mode 0700. Each file is written to a temporary name in the final directory with `O_CREAT|O_EXCL|O_NOFOLLOW`, mode **0600**, `fsync`, then renamed to the final name. An existing final file refuses the move (`transcript_exists`): the target never overwrites a transcript.
4. **Deletion only before a provider could write (revision 3, B4).** The target records each temporary and renamed file with its `dev` and `ino` (`move_in_runs.imported`, design §12.3). On a failure **before** it stores the state `launching`, it removes these files by their recorded identity (a file whose identity changed is kept and logged). From the moment it stores `launching` (before the provider process starts), it **never** removes a transcript file of this move: a failure then ends the provider process, marks the move `abandoned`, and keeps the new session read only with its files. Fleet Deck never deletes a transcript after a provider could have appended to it.

### 5.3 Who may transfer

1. Only between two stations that are paired with the same deck. The deck relays the bytes from the source data channel to the target data channel (ADR-004 `data_streams.v1`); the stations do not connect to each other. The deck does not write the bytes to disk.
2. Only for an owner command (O28), or a coordinator with fleet-control `manage` (O32). A member can never start a move. Each move is audited with the source and target station, the session, the file count, the total size and the SHA-256 values (never the contents).

### 5.4 Fallback to the branch-only move (O30)

A transcript move that is refused before the target resumes (`transcript_too_large`, `transcript_exists`, `transfer_corrupt`, `transfer_refused`, the provider resume error `No conversation found` or `no rollout found`) leaves the source session unchanged. The board shows the reason and offers the branch-only move: a new session on the same branch with a first prompt that has the session title and the last assistant summary of the source.

## 6. Move sequence (revision 3, B4, M8, M9)

1. The deck checks O28: owner (or `manage` on a grant that covers both stations), no running turn, no open approval, the branch is pushed (the FG-2 push state is `pushed` at the session head), the target has a home of the project. A coordinator move keeps its authority and the deck checks it again before each step up to the commit point (design §12.3).
2. The source holds and freezes the session (§5.1 rule 4), captures the publication `{ local branch, destination, publication ref, pushed commit }`, and exports the files (§4, §5.1).
3. The target checks that the publication's destination is its own branch target, fetches the exact publication ref from its **own** verified URL into a private ref, checks the pushed commit, makes a worktree on the **same local branch** as the source at that commit (path `B`, §10.1), and writes the files (§5.2). The new session is created with a **target hold**: it accepts no input until step 6.
4. The target stores `launching`, then starts the session with the native resume (Claude `--resume <id>` with cwd `B`; Codex `thread/resume` with `cwd: B`). Only `session.started { resumed: true }` with the same native id within 120 s stores `resumed`: **this is the commit point** of the move. A different native id ends the new process and the move fails (`identity_changed`), as in ADR-007 §2.4; the files stay (§5.2 item 4). Before the commit point the deck can abort: it discards the target first (by state, §5.2 item 4), then releases the source hold, so the source stays the only writable continuation. After the commit point the move only goes forward.
5. The source session gets the state `moved` (read only, with a link to the new session) through `session.move_finish`, which the deck resends with the same command id until it is accepted. The source worktree and the source transcript stay until the owner trashes the worktree.
6. The deck sends `session.move_release` to the target until it answers; the target deletes its hold and the new session becomes writable. At no moment do both sessions accept input.

## 7. Required decisions

| Decision | Choice |
| --- | --- |
| Claude transfer | Main transcript (required) + `subagents/` (optional), placed under `projects/<enc(B)>/` (§4.1) |
| Codex transfer | Root rollout only, same relative path (§4.2) |
| Content rewrite | None for both providers (§2 findings 1, 2) |
| Codex resume cwd | Always `cwd = B` in `thread/resume` (§2 finding 2, X3) |
| Cap | 64 MiB total; optional files dropped first; else branch-only (§5.1, §5.4) |
| Integrity | Size + SHA-256 per file; embedded native id on both sides (§5.1) |
| Target paths | Computed by the target; names validated; 0700 dirs, 0600 files, no overwrite (§5.2) |
| Peers | Same-deck stations only, deck relay, no disk copy on the deck (§5.3) |
| Failed provider | None in the spike; O30 per move on a refusal (§5.4) |

## 8. Tests (written with the code)

1. `a_moved_claude_transcript_is_written_under_the_project_directory_of_the_target_worktree`
2. `a_moved_codex_rollout_keeps_its_relative_path_below_sessions`
3. `the_target_never_uses_a_path_from_the_source_and_refuses_bad_names_and_dot_dot`
4. `a_transfer_with_a_wrong_size_or_sha256_is_refused_and_leaves_no_file`
5. `a_transfer_over_64_mib_is_refused_before_any_byte_is_sent`
6. `optional_subagent_files_are_dropped_first_when_the_total_is_over_the_cap`
7. `the_target_refuses_to_overwrite_an_existing_transcript`
8. `moved_files_have_mode_0600_and_new_directories_0700`
9. `a_transcript_whose_embedded_id_differs_from_the_native_id_is_refused`
10. `a_codex_resume_after_a_move_always_sends_the_target_cwd`
11. `a_move_between_stations_of_different_decks_is_refused`
12. `a_member_cannot_start_a_move`
13. `a_refused_transcript_move_leaves_the_source_session_unchanged_and_offers_branch_only`
13a. `an_abort_after_launching_keeps_the_target_transcript_and_the_session_read_only` (revision 3)
13b. `the_target_accepts_no_input_until_the_release` (revision 3)
13c. `the_target_worktree_is_on_the_source_branch_and_pushes_under_its_own_slug` (revision 3)
14. `the_history_reader_finds_a_moved_session_on_the_target` (Claude and Codex)
15. Live check (manual, not in CI): `spikes/s-move/run.sh` passes with the installed CLIs.

## 9. Risks and open questions

1. **Branch ownership.** The moved session keeps the branch `fd/<source-slug>/<callsign>`, but O10 lets a station push only `fd/<own-slug>/*`. Decided in §10.1 (revision 3): the local branch stays, the publication ref is under the target's slug.
2. **Accounts and versions.** The spike used the same provider account and the same CLI versions on both sides. A target with another account (Codex encrypted reasoning items) or an older CLI can fail at resume; §5.4 then applies. Recommendation: the target refuses a move when its provider version is older than the source version (from the FG-11 version report).
3. **Stale paths in the history.** The old turns name folder A and machine-1 paths. The model can try to use them. The new turns get the new cwd. Open question: does the target add a fixed station note to the first prompt after the move ("This session moved from <station 1> to <station 2>; the worktree is now <B>")?
4. **Subagents and child threads.** Resume with Claude subagent history or Codex child threads is not tested. Run the spike again with one subagent turn before the code freezes §4.
5. **Personal data.** Transcripts can contain secrets from tool output. The deck relay keeps them in memory only, the audit holds no contents, and the files are 0600 on the target.

## 10. Architect decisions on the open questions (2026-10-17)

1. **The branch of a moved session (§9.1; rewritten in revision 3, architect decision DL-225, design review finding M9).** The target keeps the **source branch name as its local branch**: the new worktree is on `fd/<source-slug>/<callsign>` (or a legacy `fd/<callsign>`) at the commit that the source pushed, so the worktree is on the same branch (O29, O30). The target **publishes** that local branch only under its own slug, with an explicit refspec: `refs/heads/<local branch>:refs/heads/fd/<target-slug>/<callsign>`, with the suffix `-2`, `-3` (up to `-99`) when that remote ref, or a ref below it, already belongs to another session, so a station still pushes only `refs/heads/fd/<own-slug>/*` (O10). The moved session records both: `session_git.branch` (the local branch) and `session_git.publish_ref` (the publication ref). The push lease is kept per publication ref (`(target_dest, publish_ref)`); the target's first push uses the empty lease, and the source's lease is never used by the target. A local branch with the source name that already exists in the target home refuses the move (`move_branch_exists`); Fleet Deck never renames or deletes a branch. The source's remote branch stays (Fleet Deck never deletes a branch, O13). Design §12.5 gives the exact rules.
2. **The extra rules of this ADR are accepted:** the source stops an idle provider process of the session before the copy (no writer during the transfer), and the deck relays the transfer bytes without writing them to disk.
3. **Size:** subagent transcripts are the first files that are dropped when a move would pass 64 MiB; a move that is still too large is refused with the text "This conversation is too large to move (over 64 MiB). Use a branch-only move." and the owner can choose the branch-only move (O30).
