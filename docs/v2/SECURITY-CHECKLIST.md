# Security checklist for the owner's review

DL-158 and R9.1: the owner does one security review at the end, before 2.0.0. This file lists every surface that is new since the last design review (DL-143 and later). Each row names the file, the risk and the proof. The architect keeps the file current: a feature that adds a surface adds its row in the same change. Each feature still gets its own adversary reviews while it is built.

How to use it: read the row, read the file, run the test, and write "reviewed" and the date in the last column of your own copy.

## Built surfaces

| DL | Surface | Where | Risk | Proof |
| --- | --- | --- | --- | --- |
| 143 | Worktree inventory (no `git status`) | `crates/fleetdeck-station/src/workspace/worktree_scan.rs`, route `stations.worktrees` | A repository can configure a program that Git runs (filter, fsmonitor, external diff, lazy fetch). The scan must run none. A path outside the allowed roots must never reach Git. Owner only. | `worktrees_tests.rs` `a_repository_with_git_filters_is_inspected_without_running_them`, `a_configured_fsmonitor_command_does_not_run`, `a_path_outside_the_roots_is_unknown_and_never_given_to_git`; `board_worktrees.rs` |
| 144 | `workspace.preflight` (`git ls-remote` with the owner's credentials) | `workspace/preflight.rs`, route `workspace.preflight`, link method `workspace.preflight` | A URL from the board makes the station contact a host with the owner's SSH keys or credentials. Risks: URLs with credentials, local paths outside the roots, a prompt that waits, secrets in the error text. | `preflight.rs` `urls_with_credentials_or_outside_the_roots_are_refused`, `the_message_is_scrubbed_and_cut_to_its_end`; `workspace_preflight.rs` |
| 144 | Branch and base names for a worktree | `workspace/provision.rs`, `manager/decide.rs` | A branch or base name that starts with `-` or holds Git syntax can become a Git option. The code uses `--end-of-options` and checks the syntax twice. | `provision.rs` tests |
| 145 | Conflict mail | `store` `apply_events`, `deck.toml` `conflict_mail` | The deck writes text into other sessions without the owner. Risks: a flood, a loop, a file name used as an instruction. One mail per pair, file and recipient; a 2 h TTL. | `conflict_mail.rs` |
| 146 | Device link | `http/auth_routes.rs` route `auth.device_link` | It mints a login token. Owner login only (a token gets `403 owner_only`), CSRF, single use, 5 minutes, 10 links per minute. The audit row holds no token. | `board_device_link.rs` |
| 147 | Image paste: `files.write_temp` and `POST .../paste` | `workspace/paste.rs`, `http/paste_routes.rs` | A write to the station disk from the board. Limits: owner only, 700 KiB, magic bytes, the extension list, a private directory (not a link), a 24 h age, 49 files, 24 MiB, 512 MiB free. | `paste_tests.rs`, `board_paste.rs` |
| 148 | Setup command | `workspace/setup.rs`, `workspace/setup_cleanup.rs`, `manager/setup_task.rs`, `station.toml` `allow_setup_cmd` | It is shell code on the station. Station opt-in (default off), the owner's board login only (a token and a coordinator cannot), 4096 characters, 10 minutes, its own process group, a marker that finds a process after a crash. A process that clears its environment is not found; macOS has no group check. | `manager/setup_tests.rs`, `board_setup_cmd.rs` |
| 149 | Archive on the deck | `store/archive.rs`, `http/archive_routes.rs` | It only hides cards. Scope `drive`, CSRF, 500 changes, stable mutation ids so that a retry cannot undo a newer action. | `board_archive.rs` |
| 150 | Worktree trash: move, restore, purge, release | `workspace/trash.rs`, `store` `worktree_ops` (V20), `http/worktree_routes.rs` | It deletes folders. Owner only, a typed folder name for a purge, a recorded device and inode checked before each step, a custody hold, no purge of a replaced folder, no purge after a release. | `trash_tests.rs`, `trash_race_tests.rs`, `board_worktree_trash.rs` |
| 151 | `session.models` | `claude/map.rs`, `codex/core.rs` | Provider-reported labels and descriptions show on the board. Treat them as untrusted text (escaped by React). `set_model` still checks the advertised value. | `claude/tests.rs`, `codex/tests.rs` |
| 158, 162 | Provider version check | `fleetdeck-providers/src/version.rs`, `manager/target.rs` `mark_broken`, `StationProvider.version_status` | A deny-listed version makes the station refuse every start of that instance. The list is empty today. A wrong entry would block the owner, so the list is code-reviewed. An older CLI only warns. | `version::tests`, `manager/version_tests.rs` |
| 162 | Scripts and bars | `tools/check.sh`, `tools/codex-schema-diff.sh`, `tools/board-repro.sh`, `tools/route-map.sh`, `tests/layering.rs`, `tests/perf.rs` | They run local commands. `codex-schema-diff.sh` runs the installed `codex` and writes to a temp directory. None sends data out. | read the scripts |

## Standing surfaces (reviewed in M2 and M3, kept for the full review)

| Surface | Where | Why it stays on the list |
| --- | --- | --- |
| Board login, CSRF, tokens, scopes | `http/guard.rs`, `store/auth.rs`, `methods.rs` | The first line of defense. A route without a table row is refused. The route map (`internals.md` §9) shows every route. |
| Owner-only routes (`owner_only`) | `http/*_routes.rs` | The scope table cannot tell the owner from an admin token. The handlers check it. |
| Link TLS and pairing | `fleetdeck-link`, `invite.rs` | A single-use invitation, a pinned key, a public address refused. |
| Human shell and TUI handover | `shell/`, `http/shell_routes.rs` | A shell on the station. Owner only. |
| Coordinator grants and MCP | `coordinator/`, `store/grants.rs` | A model with a deck credential. Scoped, capped, revocable. |
| Environment scrubbing of the provider child | `fleetdeck-core` `ChildEnv` | A secret in the station environment must not reach a child by accident. |

## Designed but not built (adversary DESIGN REVIEW required before code)

| DL | Surface | Risk to review |
| --- | --- | --- |
| 152, 156 | Forge and tracker integration (GitHub, GitLab, Jira, Linear), PR and MR writes | Stored credentials, third-party text as data (fenced), a spawn from an issue (supervised by default, no bypass path), write actions. |
| 153 | Model help through the owner's subscription | A headless CLI call with untrusted text in it; the cost; the label "generated from untrusted text". |
| 154 | A shared fleet (member principals) | A new principal type between the owner and a token: file visibility, a deny list for `.env*` and keys, mob mode, "request to drive". |
| 155 | Artifacts: native viewing and sharing | An HTML page from an agent, an opaque-origin sandbox, public links, no CDN allowlist, no size cap. |
| 157 | Automations that start sessions | A trigger that starts a session without the owner. |

Update this file when a design lands. Each row above becomes a "Built surfaces" row with its file and proof.
