# Report E: v1 fleet and UI documents (read from origin/fd/typescript-migration)

Method note. I read all four documents fully. I checked v2 claims in `docs/v2/PLAN.md`, `PARITY.md`, `UI-DESIGN.md`, `operations.md`, ADR-006, `crates/` and `web/src/`. Where I did not find proof, I say "not found". The v1 documents describe a Bun/TypeScript daemon with tmux. v2 drives agents through native protocols, so some v1 ideas do not apply.

## docs/v1/p5-programmable-fleet.md

- Summary: A pillar spec for v1.0. It adds one primitive (waitable, typed completions) and a privilege model.
- It carries a verified P0 finding: on default config, `POST /api/spawn` with `setup_cmd` was tokenless on loopback and gave shell execution.
- It also asks for a daemon-served, versioned control skill, so documented flags cannot rot.
- It lists acceptance tests: long-poll wakes, dead worker synthesizes `blocked`, ack is idempotent, two token classes, spawn cap.

Ideas and lessons:

1. Waitable completions: a worker posts `done`, `blocked` or `question` against a task id. A coordinator long-polls and blocks until the next one arrives.
   - v2 status: partial. Tasks with `done` and `failed` completions and a wait endpoint exist. There is no `blocked` or `question` kind.
   - Evidence: `crates/fleetdeck-deck/src/store/fleet.rs` (module header: "tasks with waitable completions"); `crates/fleetdeck-deck/src/http/fleet_routes.rs` (`GET /api/tasks/{task_id}/wait`, `DEFAULT_WAIT_MS`).
   - Value: medium. A `blocked` or `question` kind lets a coordinator react to a worker that cannot continue, without parsing mail.

2. Dead-worker synthesis: if a worker goes offline or is lost with an open task, synthesize a `blocked` completion. Otherwise the coordinator waits forever.
   - v2 status: missing. I found no code or ADR text that ends or flags an open task when its session is `lost` or `ended`. The wait endpoint has a timeout, so a coordinator does not hang forever, but it gets no reason.
   - Evidence: not found (grep of `fleet.rs`, `ADR-006-coordinator-mcp.md`, `PLAN.md`).
   - Value: high. A lost session is a normal v2 state (`lost`, `unreachable`). An open task with a dead owner should show that state to the coordinator.

3. Idempotent ack by id: a repeated completion is a no-op, not a crash.
   - v2 status: has. A completion of an ended task with the same state and result is a duplicate and changes nothing. A different one is refused.
   - Evidence: `crates/fleetdeck-deck/src/store/fleet.rs` lines 7-10.
   - Value: low (done).

4. Leased and acked transport for mail, with typed durable records (the "sibling of questions" design).
   - v2 status: has. Mail and tasks use a deck outbox with receipts and exactly-once framed delivery.
   - Evidence: `docs/v2/PLAN.md` A5; `PARITY.md` row "Mail between sessions".
   - Value: low (done, and stronger than v1).

5. Privilege model: token-gate the spawn power route, especially with `setup_cmd` or a bypass mode.
   - v2 status: has, in a different form. Board login is required on loopback (A13). `setup_cmd` is off by default and needs `allow_setup_cmd` and the owner.
   - Evidence: `docs/v2/PLAN.md` A13; `docs/v2/operations.md` (`allow_setup_cmd`); `PARITY.md` row "Setup command" (DL-148, `board_setup_cmd.rs`).
   - Value: low (done).

6. Two token classes: worker (mail, state, completions) and operator (spawn, kill, arm, drive controls). A worker token can never seize a driven peer.
   - v2 status: has, as grant classes. A coordinator grant has classes (read, mail, and more) and target stations. Defaults are read and mail only.
   - Evidence: `crates/fleetdeck-deck/src/store/grants.rs` (`GrantClass`, default `Read`, `Mail`); `docs/v2/adr/ADR-006-coordinator-mcp.md`.
   - Value: low (done). Check one point: v1 wanted the terminal route to need the operator token. In v2, see item 22 on board roles.

7. Cap agent-initiated spawns with a per-hour quota, so a runaway coordinator cannot fork-bomb the fleet.
   - v2 status: partial. v2 has a spawn cap per grant with a durable `spawn_used` counter (default 3, max 50). It is a lifetime count per grant, not per hour.
   - Evidence: `crates/fleetdeck-deck/src/store/grants.rs` (`DEFAULT_SPAWN_CAP`, `MAX_SPAWN_CAP`).
   - Value: low. A per-grant lifetime cap with an owner renewal is safer than a rate. Note the difference only.

8. A daemon-served, versioned control skill with `schema_version`, so agents detect skew.
   - v2 status: has. The coordinator API is an MCP server that the deck and station serve. The link and events carry protocol versions.
   - Evidence: `docs/v2/PLAN.md` section 7.1 and Phase 6; `PARITY.md` row "`/fleet` slash command"; `crates/fleetdeck-link/tests/version_skew.rs`.
   - Value: low (done).

9. Proof by test: "an unblessed loopback caller cannot reach spawn-with-setup_cmd". The acceptance list names negative authorization tests.
   - v2 status: has. ADR-006 requires executable negative tests for grants.
   - Evidence: `crates/fleetdeck-deck/src/store/coordinator_tests.rs`; `PLAN.md` line 742.
   - Value: low (done).

10. Risk note: introducing token classes must not break the human-at-the-board single-machine case. Loopback human equals operator.
    - v2 status: has. Single-machine `fleetdeck up` uses `fleetdeck open` with a one-time login URL.
    - Evidence: `docs/v2/PLAN.md` A13; `docs/v2/operations.md` section 5.
    - Value: low (done).

## docs/v1/p6-unified-views.md

- Summary: A pillar spec for the merged views: terminal grid, a Slack-style activity stream, and optional chat.
- It corrects optimistic sizing: the stream is a new event subsystem, and chat has no in-progress turn source in v1.
- It requires a delta or per-channel path for the stream, not the full-snapshot WebSocket broadcast.
- It asks to merge the permission checkbox and the mode dropdown into one ladder.
- It sets proofs: determinism, fail-open, read-cursor math, a 15-session load test.

Ideas and lessons:

1. Structured event table (session, repo, type, payload) as the base of a stream, instead of an untyped ticker string table.
   - v2 status: has. The station event log is typed and canonical. The deck keeps a synced read model. A fleet event feed exists on the board.
   - Evidence: `docs/v2/PLAN.md` A2 and section 6; `web/src/board/feed.ts`, `FeedPanel.tsx`.
   - Value: low (done).

2. Channels as derived views: one per session, one per repo, plus a `#fleet` channel for system events.
   - v2 status: missing as a merged stream. v2 has a per-session chat timeline and a fleet "Activity" panel. It has no per-repo rollup channel and no merged skim view.
   - Evidence: `web/src/session/timeline.ts`, `web/src/board/FeedPanel.tsx`; no `channel` view in `web/src`.
   - Value: medium. Useful at 15+ sessions. The Activity tab partly covers it.

3. Read cursors: per-viewer "last seen" with unread counts and catch-up.
   - v2 status: missing at fleet level. `SessionView.tsx` mentions unread for its own timeline. I found no per-session unread badge in the sidebar or a stored cursor per viewer.
   - Evidence: `web/src/session/SessionView.tsx` (unread text only); not found in `web/src/app/Sidebar.tsx`.
   - Value: medium. On a phone, an unread badge per session answers "what did I miss". It needs a small deck table.

4. Retention policy per channel (GC by age and count).
   - v2 status: partial. The station store has retention and pruning of its log. Per-view retention is not needed without channels.
   - Evidence: `crates/fleetdeck-station/src/store/log.rs`, `store/mod.rs`.
   - Value: low.

5. Selective tool-action events: a curated subset, never the full firehose.
   - v2 status: has in spirit. Timeline groups tool calls by session. Fleet activity shows selected events (asks, completions).
   - Evidence: `web/src/board/activity.ts`, `web/src/session/timeline.ts`.
   - Value: low.

6. The stream must use a delta or per-channel path, not broadcast-everything.
   - v2 status: has. v2 syncs by cursor on both hops (station to deck, deck to board), not by full snapshots.
   - Evidence: `docs/v2/PLAN.md` section 4.1 ("Cursor-based resync"); `PARITY.md` rows on cursors and snapshots.
   - Value: low (done).

7. Post into a channel equals mail to that session.
   - v2 status: has per session. The composer sends follow-up or steer. Mail can go to a station, a repo or everyone.
   - Evidence: `web/src/session/Composer.tsx`; `PARITY.md` DL-141 note "mail to a station, a repo or everyone".
   - Value: low (done).

8. Chat is secondary to the terminal; downscope chat to a turn-level thread or defer it.
   - v2 status: not applicable. v2 is chat-first by design (D3 drive only). The terminal is a separate human shell (D10).
   - Evidence: `docs/v2/UI-PLAN.md`; `web/src/chat/ChatView.tsx`.
   - Value: low. The v1 constraint came from having no live source. v2 has the live protocol stream.

9. Live in-progress turn without a transcript tailer.
   - v2 status: has. The adapter stream gives live deltas.
   - Evidence: `crates/fleetdeck-providers/src/claude/map.rs`; `web/src/chat/ChatView.tsx`.
   - Value: low (done).

10. Consolidate the permission ladder: one control, four rungs (Supervised, Auto-accept edits, Auto, Full access), mapped to both providers, with the top rung behind an arm gate.
    - v2 status: partial. The spawn form has a "Permission mode" select. The session header has a mode ladder (`controls.test.tsx`: "the mode ladder"). The mapping is per provider in the adapters. I found no single labelled ladder shared across the spawn form and session, and no fine print that shows the resolved provider pair.
    - Evidence: `web/src/spawn/SpawnForm.tsx` line 464; `crates/fleetdeck-providers/tests/capability_matrix.rs`; `web/src/session/controls.test.tsx`.
    - Value: medium. One shared ladder with the resolved provider mode as fine print removes a documented v1 confusion.

11. Mapping table test: each rung resolves to the right Claude mode and Codex approval/sandbox pair.
    - v2 status: partial. The capability matrix test exists. I did not verify a rung-to-pair table test.
    - Evidence: `crates/fleetdeck-providers/tests/capability_matrix.rs`.
    - Value: low.

12. Fail-open proof: the stream is additive. If it fails, the grid and ticker still work.
    - v2 status: has in the form of "work never depends on the deck".
    - Evidence: `docs/v2/PLAN.md` doctrine rule 2.
    - Value: low.

13. WebSocket load test at 15 sessions as a perf bar.
    - v2 status: not found. I found no 15-session board or deck load test.
    - Evidence: not found in `docs/v2/TESTING.md` (not read in full) or `crates/*/tests`.
    - Value: medium. A scripted 15-session fake-deck run is cheap and guards the board render path.

## docs/v1/p7-drive-and-observe.md

- Summary: The design for how v1 runs agents: drive through the native protocol (Claude Agent SDK, Codex app-server) and keep observing through hooks.
- It sets "drive by default, observe as the fail-open floor". It adds the runner-in-a-pane terminal model.
- It lists control verbs: answer approval, interrupt, steer, inject, capture plan, resume/fork.
- It gives a Codex symmetry table and two honest asymmetries. It lists open questions on double-counting and identity.
- This is close to what v2 already built, natively in Rust without the SDK or hooks.

Ideas and lessons:

1. Answerable approvals from the board or phone (`canUseTool` equivalent).
   - v2 status: has. Approvals are answered from the needs-you rail.
   - Evidence: `web/e2e/session.spec.ts` "an approval is answered from the needs-you rail" (PARITY.md); `web/src/session/ApprovalDock.tsx`.
   - Value: low (done).

2. Interrupt a turn without killing the session; disabled with a reason when idle.
   - v2 status: has. The control is gated by capability.
   - Evidence: `web/src/session/SessionHeader.tsx` line 635 (`flags.canInterrupt`, label "Interrupt").
   - Value: low (done).

3. Steer mid-turn, labelled differently from mail ("steer (lands this turn)").
   - v2 status: has. Steer and follow-up are separate commands.
   - Evidence: `web/e2e/session.spec.ts` "steer, follow-up, interrupt and the mode ladder send one command each".
   - Value: low (done).

4. Capture `ExitPlanMode` as a plan-review surface.
   - v2 status: has. Plans are captured from the protocol and kept in a library.
   - Evidence: `crates/fleetdeck-providers/src/claude/map.rs`; `web/src/board/PlanLibrary.tsx`; `web/e2e/plans.spec.ts`.
   - Value: low (done). One extra: v1 wanted approve/deny of the plan inline on the card. I did not check whether the plan approval is on the card or only in the session.

5. Resume and fork without the `--resume` pane step.
   - v2 status: has. Native resume, "Resume all", and fork on providers.
   - Evidence: `PARITY.md` row "Revive"; `crates/fleetdeck-providers/tests/codex_core_replay.rs`.
   - Value: low (done).

6. Usage from `result` messages (`total_cost_usd`, tokens) into the meters.
   - v2 status: has. Usage and rate-limit meters exist.
   - Evidence: `crates/fleetdeck-providers/src/claude/frames.rs`; `web/src/board/UsageMeters.tsx`; `web/e2e/usage.spec.ts`.
   - Value: low (done).

7. Observe-only fail-open floor: a broken SDK must never dark the fleet; fall back to plain `claude`.
   - v2 status: not applicable, with a replacement. v2 has no hooks and no observed lane (D3, D8: no sidecar, no fallback). The floor is the emergency local path and native resume.
   - Evidence: `docs/v2/PLAN.md` lines 55 and 335 ("This is the v2 version of v0's fail-open floor"); `fleetdeck station approve`.
   - Value: low. The risk (protocol drift) is covered by the nightly live smoke test in `crates/fleetdeck-providers/tests/live_smoke.rs`.

8. Observe-parity proof: the same task run both ways gives the same canonical events.
   - v2 status: not applicable (one path). The equivalent is replay fixtures against recorded SDK frames.
   - Evidence: `crates/fleetdeck-providers/tests/claude_sdk_reference.rs`.
   - Value: low.

9. Runner-in-a-pane: render the driven stream in a pane so the grid keeps a live view per session.
   - v2 status: not applicable. The chat view replaces the pane. A session grid is deferred.
   - Evidence: `PARITY.md` row "Terminal grid | Session grid | deferred (DL-131)".
   - Value: medium for the grid only: a "session grid" of several live chat tails remains an open owner item (DL-131).

10. Double-telemetry and identity: a driven session must not be counted twice or split into two sessions on a fallback.
    - v2 status: partial/not applicable. No hook source exists, so no double count. The v2 equivalent is handover of a session to a terminal (`shell.handover`) and import by id, which hold a native-session lock.
    - Evidence: `docs/v2/PLAN.md` handover section (`admit_resume` refuses while a terminal holds the session); `PARITY.md` row "Adopt / takeover".
    - Value: low.

11. Codex honesty: drive and observe differ per provider; label reduced cards; do not pretend parity.
    - v2 status: has. Capabilities per session; the board shows only supported controls.
    - Evidence: `crates/fleetdeck-proto/src/capability.rs`; `web/src/session/session.test.tsx` "a provider without optional capabilities gets prompt and follow-up only".
    - Value: low (done).

12. Codex multi-account config isolation (`CODEX_HOME`, `CLAUDE_CONFIG_DIR`, never `HOME`) and shadow-home overlay.
    - v2 status: has. Provider instances are routed by id. Ambient credentials are scrubbed.
    - Evidence: `docs/v2/PLAN.md` lines 373-375 (L10); `crates/fleetdeck-core` `childenv` test `the_instance_adds_back_only_its_own_settings`.
    - Value: low (done).

13. Pin the CLI/SDK version; check a minimum version.
    - v2 status: partial. The station probes provider versions. A minimum-version refusal is deferred.
    - Evidence: `PARITY.md` "Provider version gate | probe done; per-provider minimum version deferred (DL-131)".
    - Value: medium. Protocol drift is the stated main risk of the native adapter.

14. Typed generated app-server client instead of hand-written JSON-RPC.
    - v2 status: partial. v2 uses a native Rust codex adapter with ADR and replay tests. I did not check whether the schema is generated.
    - Evidence: `docs/v2/adr/ADR-002-native-codex-adapter.md`.
    - Value: low.

15. Doctrine honesty: write the thesis change down (drive default), as a visible amendment.
    - v2 status: has. PLAN section 3 restates the doctrine with owner decisions.
    - Evidence: `docs/v2/PLAN.md` section 3.
    - Value: low (done).

## docs/v1/ui-spec.md

- Summary: The board UI specification for v1.0. It lists every surface, control, state and key, each marked unchanged, changed, moved or new.
- It enumerates degraded states (empty, unauthorized, provider-limited) and a dialog inventory, an Esc order and a keyboard map.
- Key rules: capability gating over version sniffing, honesty labels (`reduced`, `inferred base`, `unknown`, `driven`), operator-gated controls that render disabled with a reason.
- Main new surfaces: Settings modal, Stream view, diff viewer with notes, forge flows, ops strip.

Ideas and lessons:

1. Capability gating: a control appears only when the daemon or session advertises the capability. An old daemon shows the old board, not dead buttons.
   - v2 status: has per session. `Capabilities` are reported by a live session. For deck-board version skew, I did not find a board-side feature flag.
   - Evidence: `crates/fleetdeck-proto/src/capability.rs`; `web/src/session/SessionHeader.tsx` line 278; `crates/fleetdeck-link/tests/version_skew.rs`.
   - Value: low for the session, medium for deck-to-board skew (the deck serves its own board, so skew is small).

2. Disabled with an explaining tooltip, instead of hidden, for gated controls.
   - v2 status: partial. `UI-DESIGN.md` principle 4 says "hide what is known unknown". For a refused control, I did not find a rule. The spawn form shows the station refusal reason.
   - Evidence: `docs/v2/UI-DESIGN.md` section 1; `web/e2e/spawn.spec.ts` "a refused start shows the station reason".
   - Value: medium. A written rule (hide when unsupported, disable with reason when permission is missing) would make owner and admin views consistent.

3. Full degraded-state tables per control ("a case not listed is a case not designed").
   - v2 status: partial. `UI-DESIGN.md` covers states of status dots and cards. There is no per-control degraded table.
   - Evidence: `docs/v2/UI-DESIGN.md` section 2.5 (`.fd-dot` states).
   - Value: medium.

4. Honesty labels: show `reduced`, `inferred`, `unknown` instead of guessing.
   - v2 status: partial/different. v2 chose to hide unknown values (principle 4), not label them. It labels degraded providers through capabilities.
   - Evidence: `docs/v2/UI-DESIGN.md` principle 4.
   - Value: low. A conscious difference. Keep.

5. Usage chip in the header with fleet burn against the tightest window, warning at 80%, `unknown` when no window.
   - v2 status: partial. Usage meters and rate-limit meters exist on the fleet home (an "Usage" tab). There is no header chip or 80% warning.
   - Evidence: `web/src/board/UsageMeters.tsx`, `usage.ts` (comment: "The latest window per station, provider instance and scope").
   - Value: medium. A phone user sees the sidebar only. A small "5h 62%" chip near the brand row shows a limit before it blocks work.

6. Ops strip: per-session rows sorted tightest window first.
   - v2 status: partial. Meters by station, instance and scope; not per session sorted by tightest window.
   - Evidence: `web/src/board/usage.ts`.
   - Value: low.

7. Diff viewer: scope selector (since spawn, turn n), per-file collapse, large-diff paging.
   - v2 status: has the turn diffs. Per-turn diffs and revert exist.
   - Evidence: `web/src/session/TurnsPanel.tsx` (`TurnDiff`, `RevertStatus`); `web/src/chat/Diff.tsx`; `web/e2e/revert.spec.ts`.
   - Value: low (done). I did not verify a "since spawn" scope or a size cap with paging.

8. Diff notes mode: click a gutter to add a line note, then "Send N notes as one mail". One mail, never a drip.
   - v2 status: missing.
   - Evidence: not found (`web/src/chat/Diff.tsx` has no note or annotation code).
   - Value: high. It turns review into a single structured follow-up to the agent. v2 already has native follow-up delivery, so the cost is UI only.

9. Revert confirm dialog that refuses if the session left idle since the dialog opened.
   - v2 status: has the stronger form. Revert is rejected before any filesystem change when a check fails, under an exclusive fence.
   - Evidence: `docs/v2/PLAN.md` section 6.6 and lines 344-346; `web/src/session/TurnsPanel.tsx`.
   - Value: low (done).

10. Settings modal with tabs: Integrations, Gateway, Providers, Accounts, Access and tokens, Board.
    - v2 status: partial. There is a Stations panel, a More screen, Prefs, and a station config file. There is no board-side settings for providers or access.
    - Evidence: `web/src/board/StationsPanel.tsx` ("Configured providers", line 523), `web/src/app/More.tsx`, `Prefs.tsx`; `docs/v2/operations.md` section 2 (file config).
    - Value: medium. v2 deliberately uses config files (`station.toml`, `deck.toml`). A read-only "Providers and access" view of the effective config would help a phone user.

11. Access and tokens tab: show-once token mint, blessed coordinator list, revoke, spawn cap input.
    - v2 status: has. The owner grants and revokes coordinator access on the board. Pairing invitations are shown once.
    - Evidence: `web/e2e/fleet.spec.ts` "the owner revokes a coordinator grant from the phone"; `crates/fleetdeck-deck/src/http/grant_routes.rs`; `web/src/board/PairingDialog.tsx`; `crates/fleetdeck-cli/src/auth.rs`.
    - Value: low (done).

12. Codex consent dialog with exact files written, an uninstall button, and a degraded state.
    - v2 status: not applicable. v2 writes no hook config to the user's Codex files.
    - Evidence: `docs/v2/PLAN.md` D3 (no hooks).
    - Value: low.

13. Gateway profile moves out of the spawn form to Settings; the form keeps one toggle.
    - v2 status: has, differently. Gateway routing is a station provider profile in config. The form has no gateway fields.
    - Evidence: `PARITY.md` row "Gateway routing | Station provider profiles".
    - Value: low (done).

14. Forge flows: "From issues..." fan-out stepper, "Review a PR..." spawn, and the PR-write confirm dialog (verb, target, identity, body, no batch confirm).
    - v2 status: missing. v2 only clones with station credentials. The PLAN keeps a risk row: forge text is fenced and issue-driven spawns default to supervised.
    - Evidence: `docs/v2/PLAN.md` lines 341 and 536; no `issue` or forge flow in `web/src/spawn`.
    - Value: medium. It is the main v1 feature set that v2 has not planned. It needs an owner decision. The confirm-dialog rules (never batch, identity preview) are worth keeping if built.

15. Issue-mode spawn: the issue text is a read-only fenced block, and the permission ladder is locked to Supervised.
    - v2 status: missing (depends on item 14). The principle is in the threat table only.
    - Evidence: `docs/v2/PLAN.md` line 536.
    - Value: medium, only with item 14.

16. Spawn form `setup_cmd` is not in the form; API only with operator token.
    - v2 status: partial/different. v2 has an opt-in setup field in the form, gated by `allow_setup_cmd` and the owner.
    - Evidence: `web/src/spawn/SetupField.tsx`; `docs/v2/operations.md` (`allow_setup_cmd`); `PARITY.md` DL-148.
    - Value: low. A conscious difference, with the gate in config.

17. Split-button spawn menu (Quick spawn, From issues, Review a PR).
    - v2 status: not applicable now; "New session" is one screen. Revisit with item 14.
    - Evidence: `web/src/app/Sidebar.tsx`.
    - Value: low.

18. Provider radio and account select in the spawn form; the account select renders only with 2+ accounts and the capability.
    - v2 status: has. Provider instances are selectable by station.
    - Evidence: `web/src/spawn/SpawnForm.tsx`; `docs/v2/PLAN.md` lines 373-375.
    - Value: low (done).

19. Session card anatomy: provider badge, `driven` chip, usage chip, review strip with checkpoint ticks, idle-gated revert.
    - v2 status: partial. Cards show a provider badge, model, tool and usage. Checkpoint ticks on the card are not present; checkpoints are in the session Turns panel. A `driven` chip does not apply (all sessions are driven).
    - Evidence: `docs/v2/UI-DESIGN.md` section 4.1 (card lines); `web/src/session/TurnsPanel.tsx`.
    - Value: low.

20. Driven-session control strip is absent, not broken, on the floor.
    - v2 status: has (as capability flags).
    - Evidence: `web/src/session/SessionHeader.tsx` (`flags.canInterrupt`); `session.test.tsx` "a Codex session shows the controls of its capabilities".
    - Value: low (done).

21. Terminal access matrix: eight paths to a terminal, and `/ws/term` needs the operator token even on loopback.
    - v2 status: has differently. A human shell is a separate admin-only PTY with its own spec.
    - Evidence: `web/e2e/shell.spec.ts` "an admin opens a station shell"; `crates/fleetdeck-station/src/shell/mod.rs`.
    - Value: low (done). It shows that v2 has "admin" and "owner" roles; the shell is limited to the admin.

22. Esc and overlay order, plus a rule that every new overlay is added to the hotkey-suppression list in the same commit.
    - v2 status: partial. There are key helpers and a help overlay with tests. I did not find a written Esc order spec or a test that every overlay blocks hotkeys.
    - Evidence: `web/src/app/keys.ts`, `keys.test.ts`, `HelpOverlay.tsx`; `web/src/app/Layout.tsx` line 118.
    - Value: medium. v2 has many overlays (palette, help, pairing, device link, files). One written order and one test stop a regression class.

23. Help text and hotkey list must update in the same commit as a new hotkey.
    - v2 status: partial. `HelpOverlay.tsx` exists. A test that ties the list to the key map was not found.
    - Evidence: `web/src/app/HelpOverlay.tsx`, `keys.test.ts`.
    - Value: low.

24. Keyboard map that is minimal: reuse the inbox keys (y/n/1-9) on every needs-you card, in the stream too.
    - v2 status: has. The rail has keys.
    - Evidence: `web/src/board/NeedsYouRail.tsx`, `rail.test.tsx`; DL-141 in `PARITY.md`.
    - Value: low (done).

25. Deep links: `#session/<sid>` and `#stream/<channel>` open the view on load.
    - v2 status: has for sessions through routes.
    - Evidence: `web/src/app/routes.ts`, `routes.test.ts`.
    - Value: low (done).

26. Accessibility: `role="dialog" aria-modal`, radiogroups labelled, the stream is `role="log"`, ladder uses `aria-describedby` for the hazard.
    - v2 status: partial. The chat view has `role="log"`.
    - Evidence: `web/src/chat/ChatView.tsx`; `web/src/chat/chat.test.tsx`.
    - Value: low. A check of dialogs for `aria-modal` was not done.

27. Both themes and both densities for every new surface from day one.
    - v2 status: has. Tokens, density and theme exist.
    - Evidence: `web/src/ui/density.ts`; `docs/v2/UI-DESIGN.md` section 2.4 (light/dark tokens); `Prefs.tsx`.
    - Value: low (done).

28. Fleet line with per-provider split ("7 sessions (5 claude . 2 codex)").
    - v2 status: partial. The fleet line shows sessions, needs-you and stations. A provider filter exists.
    - Evidence: `docs/v2/UI-DESIGN.md` section 4.1 (header and filter row).
    - Value: low.

29. All new payload shapes come from the contracts crate; no new comment-contracts.
    - v2 status: has. Types are generated from Rust.
    - Evidence: `web/src/gen/`; `docs/v2/PLAN.md` doctrine rule 9.
    - Value: low (done).

30. Header audit rule: every grant, revoke and rotate writes a feed line.
    - v2 status: partial. The deck has an audit log (rule 7). I did not verify that grant changes appear in the board feed.
    - Evidence: `docs/v2/PLAN.md` doctrine rule 7; `web/src/board/feed.ts`.
    - Value: low.

## Top 10 for v2

1. Dead-worker synthesis for tasks (p5 item 2): a lost or ended session with an open task gives the coordinator a `blocked` result with a reason. Not found in v2.
2. Diff notes sent as one mail (ui-spec item 8): line notes in the diff viewer become one structured follow-up. UI only; the follow-up path exists.
3. Per-session unread state with a stored read cursor (p6 item 3): "what did I miss" on a phone with 15 sessions.
4. `blocked` and `question` completion kinds (p5 item 1): the coordinator can react to a worker that needs help, not only done or failed.
5. One shared permission ladder in the spawn form and session header, with the resolved provider mode as fine print (p6 item 10).
6. Forge flows: issue fan-out, PR review spawn and the PR-write confirm dialog (ui-spec items 14 and 15). Needs an owner decision; keep fenced text and Supervised lock if built.
7. Usage chip in the header or sidebar with an 80% warning (ui-spec item 5): a rate limit is visible before it stops work.
8. Per-provider minimum CLI version gate (p7 item 13): already deferred under DL-131; protocol drift is the stated risk of the native adapter.
9. Written Esc and overlay order plus a test that every overlay suppresses hotkeys (ui-spec item 22): v2 has many overlays now.
10. A 15-session board load test (p6 item 13), and a merged fleet stream with per-repo channels (p6 item 2) after the load numbers are known.
