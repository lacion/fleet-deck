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

## B2. Background notifications show as "provider turn" (owner screenshot, 2026-10-15)

- **Observed:** in the chat of an imported Claude session, a turn that a background task starts (a task notification, not a prompt of the owner) shows as a user bubble with the text "provider turn".
- **Wanted:** a muted line that says what started the turn (for example "Background task finished: hist-2"), or no bubble.
- **Done (2026-10-15):** a muted line instead of the bubble (ui wave 2).

## B3. Tailnet card: owner certificate files (DL-190, design §2.6)

- **Missing:** when `board.crt` and `board.key` exist, the Tailnet card does not say "Your own certificate files (`board.crt`) are used for every name; remove them to use the Tailscale certificate." `GET /api/tailnet` does not tell the board that owner files exist.

## B4. "Add station": the joined station shows its key fingerprint (2026-10-15)

- **Observed (real-stack e2e):** after `fleetdeck station join`, the dialog says "✓ ad9dca5e76ffca69 joined this deck." instead of the station name ("bravo"). The name arrives a moment later.
- **Wanted:** the dialog shows the name when it arrives (update the line), or waits a short time for it.

## B5. Build identity for upgrades (Codex review, 2026-10-15)

- **Now:** `fleetdeck service start` decides "newer" or "older" by the file times of the two programs, because every build reports version `0.0.0`. A copy that keeps old file times, or an old binary copied later, can mislead it.
- **Wanted:** a build id (for example the git commit and the build time) in the binary, shown by `fleetdeck --version`, compared by `service start` and `service status`, and reported by each station to the deck, so the board can show which build a station runs.
- **Done (2026-10-15):** commit and commit time in every binary; `--version`, station info, deck version and `service start/status` use them (be-w4).
