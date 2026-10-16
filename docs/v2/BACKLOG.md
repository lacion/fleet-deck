# Backlog

Open items that are not designed or scheduled yet. Each item gets a design or a brief before code.

## B1. Changes of subagents and of other worktrees are not visible (owner report, 2026-10-15)

- **Observed:** the imported session "Fleet Deck architect" (Lighting, `v2` worktree) runs subagents that change code. The Changes tab of the session is empty.
- **Facts:**
  - The architect session gives code work to subagents. Each subagent works in its own git worktree (for example `../hist2` on branch `feat/session-history-2`), not in the session folder.
  - The session merges the finished branches into `v2` as commits. After a merge, the working tree of the session is clean again.
  - The Changes tab shows the turn diffs of the session and the working tree of the session folder against its base. Edits in another worktree and merged commits are in neither.
  - The timeline shows a subagent as one "Agent" card with "0 entries". The tool calls of the subagent are not shown.
- **Questions for the design:**
  - Show the commits that the session made (or merged) since its start, next to the working-tree diff?
  - Show the worktrees and branches that the session created, with their diffs?
  - Show the tool calls of a subagent inside its card (Claude subagent transcripts)?
- **Done, first part (2026-10-15):** an import takes the `HEAD` at import time as its base, so "Since the session started" shows the work after the import, also merged branches (be-w4). Open: worktrees that a session created, and subagent tool calls.
- **Done (2026-10-16):** DL-191: "Show its work" on a subagent card (the subagent's own messages and tool calls) and the "Branches" view in the Changes tab (the repository's branches and the diff of one against its merge base).

## B2. Background notifications show as "provider turn" (owner screenshot, 2026-10-15)

- **Observed:** in the chat of an imported Claude session, a turn that a background task starts (a task notification, not a prompt of the owner) shows as a user bubble with the text "provider turn".
- **Wanted:** a muted line that says what started the turn (for example "Background task finished: hist-2"), or no bubble.
- **Done (2026-10-15):** a muted line instead of the bubble (ui wave 2).

## B3. Tailnet card: owner certificate files (DL-190, design §2.6)

- **Missing:** when `board.crt` and `board.key` exist, the Tailnet card does not say "Your own certificate files (`board.crt`) are used for every name; remove them to use the Tailscale certificate." `GET /api/tailnet` does not tell the board that owner files exist.
- **Done (2026-10-15):** `certificate.owner_files` in `GET /api/tailnet` (be-w4) and the card note (ui-w4).

## B4. "Add station": the joined station shows its key fingerprint (2026-10-15)

- **Observed (real-stack e2e):** after `fleetdeck station join`, the dialog says "✓ ad9dca5e76ffca69 joined this deck." instead of the station name ("bravo"). The name arrives a moment later.
- **Wanted:** the dialog shows the name when it arrives (update the line), or waits a short time for it.
- **Done (2026-10-15):** the dialog renders the current name of the joined station (ui-w4).

## B5. Build identity for upgrades (Codex review, 2026-10-15)

- **Now:** `fleetdeck service start` decides "newer" or "older" by the file times of the two programs, because every build reports version `0.0.0`. A copy that keeps old file times, or an old binary copied later, can mislead it.
- **Wanted:** a build id (for example the git commit and the build time) in the binary, shown by `fleetdeck --version`, compared by `service start` and `service status`, and reported by each station to the deck, so the board can show which build a station runs.
- **Done (2026-10-15):** commit and commit time in every binary; `--version`, station info, deck version and `service start/status` use them (be-w4).

## B6. Running subagents and tasks in the chat view (owner request, 2026-10-15)

- **Observed:** the chat view has no list of the subagents and background tasks that a session runs now. The owner cannot see which are running, and cannot open the chat of one of them.
- **Wanted:** a view of all running subagents and tasks of the session, and a way to switch from agent to agent to see the chat window of each, as in Claude Code.
- **Related:** DL-191 adds "Show its work" (one read of a finished subagent transcript) and the Branches view. B6 needs live state and a switch between chats.
- **Status:** later. Needs a design first.
- **Done (2026-10-16):** design `docs/v2/design/agents-view.md`; `task.updated` from Claude and Codex, the late subagent text and open child tools fixed, the Agents switcher with subagent chats and background commands.

## B7. Browse the sessions of a station, with a preview, before an import (owner request, 2026-10-15)

- **Wanted:** for each station, a list of all the provider sessions that are on it (Claude and Codex conversations, imported or not). The owner opens a preview of the conversation of one, and then decides to continue it (import it and resume) or to leave it.
- **Related:** the import flow and the earlier history (DL-189) already read native transcripts. B7 needs a station-wide list and a preview that does not import.
- **Status:** later. Needs a design first.
- **Done (2026-10-16):** design `docs/v2/design/native-sessions.md` (revision 6); the station lists and previews the sessions of its provider homes with the trust checks; the board dialog "Browse sessions on this station" with "Continue it"; a verified entry sends an import pin, and the station fails a changed transcript closed (feature `import_pin.v1`). Adversary PRE-PR: PASS in round 2.
- **Open:** an unpinned import keeps a best-effort Codex child-thread check (a timeout or a busy history worker lets it go on).

## B8. `!` shell commands in a session are sent as chat text (owner report, 2026-10-16)

- **Observed:** a prompt that starts with `!` (the shell-command line of the Claude Code and Codex terminal UIs) goes to the model as a normal message; nothing runs.
- **Cause:** the `!` line is a feature of the interactive terminal UIs. Fleet Deck drives Claude with `stream-json` (no such input) and Codex with the app-server, which has `thread/shellCommand` (not used yet).
- **Wanted:** a `!` line runs the command in the session folder, shows the command and its output in the timeline, and gives the result to the model as context, like the terminal UIs.
- **Status:** design in progress (command execution: adversary DESIGN REVIEW before code).
- **Done (2026-10-16):** DL-196, design `docs/v2/design/bang-commands.md` (revision 10). Claude: the station runs the command and gives the result to the model with the next prompt; Codex: native `thread/shellCommand`; the shell card, Stop, "Release the folder", the folder stays busy until the command settles; CLI `!` records show as shell items in the imported history. Adversary PRE-PR: PASS in round 3.
- **Open:** (1) results that the 64 KiB budget of one prompt defers stay attachable on the station, but the board removes their chips, so they never reach the model; (2) an Interrupt after a Codex `thread/shellCommand` was sent but before its turn started only stores the cancel; the command runs to its end; (3) no recorded live Codex fixture of `!pwd` yet (the tests use the fake app-server).

## B9. Verified launch directory for every start (hardening, from the B7 review, 2026-10-16)

- **Gap:** the station checks a session folder against the allowed roots by path, and the provider later resolves the same path; a process of the station user that replaces a folder component with a link in between can move the launch outside the roots. The same holds for the provider's own transcript lookup at a resume.
- **Wanted:** descriptor custody of the checked folder across the direct and the shim host launch (the process cwd and the provider workspace), or a fail-closed recheck.
- **Status:** later. Needs a design and an adversary DESIGN REVIEW.
- **Done (2026-10-17):** design `docs/v2/design/launch-dir.md` rev 4 (DL-200, Landlock for provisioning). Built in four parts (B9a base `bd810721`, identities at the dispatch `8deec2b0`, provisioning under `confine-exec` `dbad0841`, the other launches and the Codex check `40ccb315`); PRE-PR r1 FAIL (checkpoints, provisioning Git and the provisioned identity by path), fixed `7a61460f`, RE-REVIEW r2 PASS. Open: macOS (Seatbelt and `libproc`, with the Mac work); later checkpoint Git by the retained toplevel path (detection only, design §2).

## B10. The chat shows only the first line of a prompt (owner report, 2026-10-16)

- **Observed:** in session `01M52FVQCYB6K40KARF2NTMC2Q` the user bubble shows "this is an exploration brainstorm session no code changes"; the prompt had several paragraphs.
- **Cause:** the agent got the full text (the stored input of the start effect holds every paragraph). The board draws the user bubble from `run.started.input_summary`, which is the first line cut to 120 characters (`claude/core.rs` `summary_of`; Codex the same idea). No event carries the full prompt text.
- **Wanted:** the user bubble shows the whole prompt (with line breaks), like Claude Code and Codex; long prompts fold with "Show more". The run label elsewhere (cards, lists) may stay the summary.
- **Notes:** an additive event field (for example `run.started.input_text`, bounded) or an `input.received` event; the same visibility as the summary today (members of a shared session see prompts already). Imported history already shows full user text.
- **Status:** open.
- **Done (2026-10-16):** the chat shows the whole prompt, folded when long (`20fc4ce6`).
- **Done (2026-10-16):** `run.started.input_text` (up to 32 KiB) from both providers; the chat bubble shows the whole prompt, folded after 12 lines with "Show more".
