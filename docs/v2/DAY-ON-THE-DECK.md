# A day on the deck

This is one owner story that uses each built feature of Fleet Deck v2 once. It is the script for the cutover rehearsal (R9.6). Follow it on a real machine with a second machine for the station steps. Tick a step when the result in the last column of the "Check" list is true. A step that fails is a defect: write it in the report with the time and the session.

> **Caution:** do this on throwaway repositories. Steps 8, 9, 10 and 16 write files, run shell commands and delete folders.

Setup for the story: machine A (laptop) runs the deck and its own station. Machine B (desktop) runs a station only. Both have `claude` and `codex` signed in. Repository `shop` is a small Git repository on both machines. [TESTING.md](./TESTING.md) §1 to §4 build and pair the machines.

## Morning: open the board

1. **Start and check.** On A run `fleetdeck doctor`, then `fleetdeck up`. Doctor shows `ok` or `warn` lines only. A `warn provider ...: ... is older than the tested ...` line means the CLI is older than the version that Fleet Deck is tested with (R8.2).
2. **Login.** Run `fleetdeck open` and open the link. Check the fingerprint. Set the owner password with `fleetdeck deck password`.
3. **Add a device.** On the board choose **Add a device**. Scan the QR code with the phone. The phone logs in once. Try the same link a second time: it fails.
4. **Pair machine B.** On A run `fleetdeck deck invite`. On B run `fleetdeck station join` and paste the invitation. **Stations** shows B as **Link up** with its version, capacity and providers.

## Mid-morning: start work

5. **Spawn with a worktree and a branch.** Choose **New session**: station A, provider `claude`, directory `shop`. Open the access check for a clone URL (preflight). Choose a worktree with branch `feature/SHOP-12`. The card gets a callsign `<animal>-SHOP-12`. If the provider warning shows under the select, read it.
6. **Setup command.** In the same form, add the setup command `echo ready > .setup-done` (only if `allow_setup_cmd = true`). The session starts after the command ends.
7. **Batch.** Start `2x fix the typos in README` on B. Two worktree sessions start. Import the session id of a `claude` session that you started in a terminal, as a driven session.
8. **Approval.** Ask a session to run a shell command. The request shows in **Needs you**. Answer it with a key. Ask another session for a command and answer **deny**.
9. **Steer, follow up, interrupt.** Send a steer to a working session. Queue a follow-up. Interrupt one turn. Change the permission mode and the model.
10. **Image paste.** Paste a screenshot into the composer. The session gets the image. In a terminal tab, paste an image: the path is typed and Enter is not pressed.

## Afternoon: manage the fleet

11. **Conflict radar and whisper.** Make two sessions on `shop` change the same file. The radar marks the conflict. Each session gets one mail that names the other.
12. **Mail and tasks.** Send a mail to one session, to a repository and to everyone. Create a task for a session and watch it complete.
13. **Assign auto.** Type `assign auto: add a unit test for the cart`. The deck names the session. With no good station it offers a spawn.
14. **Plans.** Ask a session to plan. Open **Plans**, read the captured plan, and start a session from it.
15. **Coordinator.** Start a coordinator session. Open its **Coordinator grants** and revoke the grant. The session can no longer call the deck.
16. **Files, checkpoints, revert.** Open **Files** of a session: browse, read, search. Open the diff of one turn. Choose **Revert to here** on an earlier turn and check that the files return.
17. **Terminal and shell.** Open a station shell (**Terminals**), type a command, reattach and close it. Open a session in the terminal (TUI handover) and return it to the board.
18. **Usage and notifications.** Read the **Usage** meters. Allow notifications and leave the tab: a needs-you request raises one.

## Evening: stop, restart, clean up

19. **Stop and resume.** Stop a session. Resume it: it keeps its memory. Restart the station on B with `host = "direct"`: its sessions become **lost** and **Resume all** brings them back. Turn auto-continue on for one session, restart again and see it continue.
20. **Deck restart.** Stop `fleetdeck up` and start it again. The board shows stale data, then reconnects with no new login. A command sent while B was unreachable goes out when B links.
21. **Archive.** Archive the ended sessions on the phone. They disappear on the laptop too.
22. **Worktree trash.** Open **Worktrees** (owner only). Move the worktree of an ended session to the trash. Restore it. Trash it again and delete it for good, with the folder name typed.
23. **Drain and upgrade.** Run `fleetdeck station drain`, then follow the upgrade steps of the [operations guide](./operations.md) with a second build. Roll back once.
24. **Audit.** Run `fleetdeck deck audit`. Each step above that changed something has a row.

## Check

- No step needed a secret on a command line or in a screenshot.
- Every refusal showed its reason on the board.
- The owner could stop each session at any time.
- No session stopped because the deck, the board or the network failed.

## Features that are not built yet

Add a step here when a feature of [FLEETDECK-V2.md](./FLEETDECK-V2.md) §6 lands: the base commit and whole-session diff (R1), forge links, PR and MR views and issues (R3), cost and the 80% chip (R4), automations (R5), unread and watching many sessions (R6), model help (R10), the shared fleet (R11) and artifacts (R12).
