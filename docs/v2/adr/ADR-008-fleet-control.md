# ADR-008 — Fleet control: fleet-scope grants, new tools, and approvals by a coordinator

- **Status:** Accepted design (revision 5, 2026-10-14): adversary RE-REVIEW PASS (`/tmp/adversary-report-9fbb0832.md`, Codex); its open MAJOR (test text) is fixed in this revision's tests 8 and 8d. Revision 4 (commit c89c48f5) got the extra RE-REVIEW FAIL (2 blockers, 1 major; `/tmp/adversary-report-c89c48f5.md`); the owner chose one more review after the fixes (§15). Revision 3 (commit 096fa451) got RE-REVIEW FAIL (2 blockers, 1 major; `/tmp/adversary-report-3fda6b7f.md`); the round limit was reached and the owner chose to apply the fixes and run one extra focused review (§14). Earlier: Revision 1 (commit 7cc02abe) got DESIGN REVIEW FAIL (8 blockers, 4 major; `/tmp/adversary-report-7cc02abe-codex-verbatim.md`); revision 2 (commit 0e8946d4) got RE-REVIEW FAIL (4 blockers, 1 major; `/tmp/adversary-report-0e8946d4.md`). §12 and §13 map each finding to its change. Needs a PASS before code; this is the last review round allowed for this design.
- **Deciders:** owner (2026-10-14: "lets expand the mcp to allow full controls of the fleet … launch a session and that session could orchestrate a lot of things in the fleet"; scope answer: "Full control incl. approvals"), architect.
- **Extends:** ADR-006 (revision 4). Every ADR-006 rule stays in force unless this ADR names it and replaces it. The credential handoff (§5.4), the binding states (§5.5), the decision order (§6.2), the station outbox and its fences (§6.5) and the same-UID boundary (§3) are unchanged.

## 1. Context

A coordinator today (ADR-006 §6.3) lists the sessions of its grant's stations, sends mail, assigns tasks, spawns under a cap and a mode ceiling, and interrupts or stops the sessions of its own lineage. It never answers an approval. The owner wants one session that runs the fleet: it drives any session, starts work in projects, reads what the fleet produced, and answers approvals and questions.

Answering approvals is the largest new power. A coordinator reads third-party text (issues, PRs, files, the output of other sessions). A prompt injection in that text can make it approve a dangerous tool call in another session. This ADR accepts that risk at the owner's request and bounds it (§5, §6).

## 2. Terms

| Term | Meaning |
| --- | --- |
| Lineage scope | ADR-006 drive rule: the target has this grant in its lineage. |
| Fleet scope | New: the target is any session on a station of the grant, minus the exclusions of §4.2. |
| Fleet-control class | `inspect`, `manage` or `answer` (new, §3). |
| Fleet-control grant | A grant with `scope = fleet` or with at least one fleet-control class. |
| Fleet-control op or query | Every op or query that this ADR adds, and every ADR-006 op called under a fleet-control grant. |

## 3. Grant changes

- New field `scope`: `lineage` (default, ADR-006 behavior) or `fleet`.
- New classes; `classes` ⊆ {`read`, `mail`, `drive`, `spawn`, `inspect`, `manage`, `answer`}. Unknown names fail closed.
  - `inspect`: every new read query (§4.1). The ADR-006 `read` class keeps exactly its ADR-006 queries (`fleet_status`, `list_sessions`, `list_tasks`, `wait_task`, `wait_tasks`, `command_status`); an existing grant gains nothing.
  - `manage`: resume, rename, set mode, move to project, start in a project.
  - `answer`: answer approvals and questions (§5).
- New field `answer_cap` (0–5000, default 500) with a durable lifetime counter `answer_used`. Accounting is the ADR-006 §6.4 rule: counted in the decision transaction with the receipt and the intent; never returned; `answer_cap_reached` is a stored rejection.
- **Creation needs elevation.** The spawn form shows "Fleet control" with switches: fleet scope, inspect, manage, answer approvals. Any switch on needs the owner's elevation (DL-172: the owner password, or a `fleetdeck open --elevate` code) in the same request; the deck refuses a start whose grant has a fleet-control field without a fresh elevation (`elevation_required`, stored). A member, a share guest or an API token never creates one.
- **Version gate.** Link feature `coordinator.v2` (the existing feature is `coordinator.v1`, `crates/fleetdeck-proto/src/version.rs:48`). The deck refuses to create a fleet-control grant (`unsupported`, stored) unless the coordinator's station and every station in the grant's `stations` negotiated `coordinator.v2` on their current link. `CoordinatorSpec` serializes `scope`, `answer_cap` and the new classes only when the grant is a fleet-control grant (`skip_serializing_if` the ADR-006 defaults), so a lineage grant's start command stays byte-compatible with a `coordinator.v1` station (`CoordinatorSpec` has `deny_unknown_fields`).
- **Every send needs v2, not only creation.** Every fleet-control send or resend requires current `coordinator.v2` negotiation on the caller's and the target's links at that moment; loss of negotiation prevents transmission (the item stays queued until its 10-minute expiry, then `dead`, `authority_lost`), and the authority is never omitted or downgraded to run a fleet-control command through a legacy method.
- **Kill switches (two, no mirror).**
  - **Deck setting `fleet_control`** (default `false`, `apply: Live`, `danger: true`; on needs elevation, off does not). It is the authority for every fleet-control decision: the deck decision, the check before every send (§6.2) and `coordinator.authority_check` read the deck's own live value, so it is always current. Off stops every fleet-control op and query in the fleet, including accepted ones whose provider write has not begun.
  - **Station setting `allow_fleet_control`** (default `false`, `apply: Live`, `danger: true`) is read **only live from its own station**, never from a copy: at intake on that station (its coordinators' fleet-control tools answer `not_granted`, never forwarded); for a target on that station, at decision and at effect dispatch; and, for a caller on that station, at the target's effect dispatch through the caller-policy step of `coordinator.authority_check` (§6.2). Turning it off stops new calls from its coordinators and every accepted call from them whose provider write has not begun.
  - The tool catalog is fixed at process start (ADR-006 §5.3): fleet-control tools are listed when the grant has their classes, whatever the switches; calls fail while a switch is off.

## 4. Authorization

### 4.1 Tool and class matrix (adds to ADR-006 §6.3)

| MCP tool | Classes | Wire | Target rule |
| --- | --- | --- | --- |
| `read_session` | inspect | query `session_tail { session_id, max_items ≤ 200 }` (≤ 64 KiB) | §4.2 rule 1 |
| `session_diff` | inspect | query `session_diff { session_id }` (≤ 256 KiB) | rule 1; privacy rule §4.4 |
| `list_needs_you` | inspect + answer | query `needs_you` | open requests of sessions that pass §4.2 |
| `list_projects`, `project_items` | inspect | query `projects`, `project_items { project_id, kind: issues \| prs }` | projects whose station is in the grant |
| `list_artifacts`, `read_artifact` | inspect | query `artifacts { session_id? }`, `artifact { artifact_id, version? }` (text kinds, ≤ 256 KiB) | artifacts of sessions on grant stations |
| `read_file`, `search_files` | inspect | query `file_read { session_id, path }` (≤ 256 KiB), `file_search { session_id, query, mode }` (≤ 50 hits) | the session root of a rule-1 session; DL-173 private paths unchanged |
| `send_mail`, `create_task` | mail | unchanged | lineage: unchanged; fleet: rule 1 |
| `interrupt_session`, `stop_session` | drive | unchanged | §4.2 (both scopes) |
| `resume_session` | manage | call `resume { station, session_id }` | §4.2; mode rule §5.2 |
| `rename_session` | manage | call `rename { station, session_id, title }` | §4.2 |
| `set_mode` | manage | call `set_mode { station, session_id, mode }` | §4.2; mode rule §5.2 |
| `move_to_project` | manage | call `project_assign { station, session_id, target: project(id) \| none \| auto }` | §4.2; the project's station equals the session's station (else `wrong_station`, DL-176) |
| `start_in_project` | spawn + manage | call `project_start { project_id, prompt, provider_instance?, worktree: bool }` | the project's station is in the grant; counts against `spawn_cap`; mode rule §5.2; the new session is in the grant's lineage |
| `answer_request` | answer | call `answer { station, session_id, request_id, option_id?, question_answers?, text? }` | §4.2; §5 |

- Every new query is a `coordinator.query` (no receipt); every new op is a `coordinator.call` (receipt, outbox). New `CoordinatorOp` variants: `resume`, `rename`, `set_mode`, `project_assign`, `project_start`, `answer`. New `CoordinatorQuery` variants: `session_tail`, `session_diff`, `needs_you`, `projects`, `project_items`, `artifacts`, `artifact`, `file_read`, `file_search`. Each is accepted only on a `coordinator.v2` link.
- Every fleet-control op and query also requires the deck setting `fleet_control` on, `allow_fleet_control` on at the caller's station intake, and for ops `allow_fleet_control` on at the target station (§3).

### 4.2 Target exclusions (both scopes; replaces the ADR-006 §6.3 lineage drive rule where it would allow more)

For `drive`, `manage` and `answer` ops, and for `inspect` queries where noted ("rule 1"), a target session is allowed only when all hold, checked in the deck decision transaction and again at target-station dispatch (§6.2):

1. Its station is in the grant's `stations` (and, with `scope = lineage`, it has this grant in its lineage).
2. It is not the caller itself.
3. It holds no active grant with a mutating class (`mail`, `drive`, `spawn`, `manage`, `answer`). A coordinator never drives, manages or answers for another coordinator, in either scope; this replaces the ADR-006 lineage rule that let a coordinator stop a lineage child that the owner later made a coordinator.
4. It is an owner session. Share-guest and member drive rights never extend a coordinator.

### 4.3 Third-party text into a coordinator

Every text that reaches a coordinator session and that it did not write itself uses the DL-164 untrusted envelope (`crates/fleetdeck-proto/src/untrusted.rs`), with the source metadata (station, session, kind) outside the fenced text:

- query results that hold text from another session, a file, a diff, an issue, a PR or an artifact;
- pushed inputs: worker questions (`question_frame`, `crates/fleetdeck-proto/src/orchestration.rs:83`), mail (`mail_frame`, :67), task results, conflict mail.

`mail_frame` and `question_frame` fence their text for every recipient (not only coordinators), so the rule does not depend on knowing the recipient's grant at framing time. `list_needs_you` gives per request: station, session, title, request id, request kind, tool name, a fenced summary of the tool input (≤ 8 KiB), the offered options with their semantic kinds (§5.1), and the age.

### 4.4 Privacy of coordinator reads

- `file_read` and `file_search` use the station file routes unchanged (identity-aware, DL-173).
- `session_diff` for a coordinator does **not** use the board's snapshot diff (`git add -u` + `write-tree`, `crates/fleetdeck-station/src/workspace/checkpoint.rs:356`). It is built per file, and privacy is decided before any byte is read:
  1. List the changed paths against the session base (`git diff --name-status -z <base>` on the work tree, plus untracked files within the existing limits).
  2. Drop every path whose old or new name matches the private-path name rules or the member deny list.
  3. New side: open the work-tree file without following links; check that open file's identity with the private-path registry and the DL-173 hard-link rule (nlink > 1 outside `.git` refused); read the new-side bytes **from that same open file descriptor** (so a later replacement of the path cannot change what was checked). A refused file appears as `(hidden: private file)`.
  4. Old side: the blob of `<base>:<old path>` (`git cat-file blob`) is used only to compute the diff; **none of its bytes leave the station.** Committed history is not exempt from privacy: its provenance cannot be established, so its content is never shown.
  5. Render each file pair with `git diff --no-index` over two copies in a fresh private temp directory (mode 0700, removed after), with the same size limits as today, then rewrite the output before it leaves the station: every run of removed lines (`-` lines) becomes one line `-[<n> lines removed]`; added lines (`+`) and context lines come from the checked new-side bytes (context lines are equal to new-side lines). A deleted file shows only `-[<n> lines removed]` (or `(deleted file)` for a binary). Every `@@` hunk header is rebuilt from the line numbers only (`@@ -<a>,<b> +<c>,<d> @@`, no function or context text), and no other git metadata derived from the old side (`index` lines with blob ids, mode lines of the old side, rename similarity text) is kept; the file header shows only the new path, or `(renamed)` with both paths when both passed step 2. A file pair whose old-side handling cannot be done this way is suppressed with `(hidden: diff unavailable)`. The result is a review aid, not an applicable patch.
  6. If any identity check cannot run, the whole query fails with `privacy_unknown`.
  The DL-173 open point (a tracked hard link can show in a board diff) does not apply to this interface.

## 5. Approvals and questions by a coordinator

### 5.1 Semantic option kinds

- `ApprovalOption` (`crates/fleetdeck-proto/src/event.rs:128`) gains `kind: Option<OptionKind>` (serde default, skipped when `None`), with `OptionKind = allow_once | allow_session | allow_always | deny | amend | other`. The provider adapter sets it from the native decision when it builds the request:
  - Claude: allow → `allow_once`; an option that carries permission-rule updates or "always" → `allow_always`; deny → `deny`.
  - Codex: `accept` → `allow_once`; `acceptForSession` → `allow_session`; any option with an amendment (`acceptWithExecpolicyAmendment`, `applyNetworkPolicyAmendment`, object-valued decisions with amendments) → `amend`; `decline`/`cancel` → `deny`; anything else → `other`.
- A coordinator may choose only an option with `kind = allow_once` or `kind = deny`. `None` (a request stored before this ADR, or an option the adapter cannot classify), `allow_session`, `allow_always`, `amend` and `other` are refused (`forbidden`, stored). Adapter tests cover every native decision shape, including object-valued amendments.

### 5.2 Modes

- Superseding ADR-006 §6.3's spawn-mode rule for bypass modes: no coordinator operation may start, resume, or set a session into a bypass or full-access mode (Claude `bypassPermissions`; Codex `never` approval with `danger-full-access`), whatever `max_modes` or `allow_bypass_permissions` say.
- `set_mode`, `start_in_project` and `spawn` need the target provider family to have a ceiling in `max_modes` and the mode within it.
- **No implicit continuation by a coordinator.** An input whose origin is a coordinator (`RunOrigin::Coordinator`, or a routed command with `authority`), including mail, task assignment and steer, never resumes a hibernated or ended session. The station's hibernated-input resume (DL-180, `resume_hibernated` in `crates/fleetdeck-station/src/manager/decide.rs`) applies only to the origins `Board`, `Local` and owner/member mail (`RunOrigin::Mail` without `authority`); for a coordinator origin the input is refused `invalid_state` and the deck outbox holds it until the session runs again (DL-180 hold). To continue such a session the coordinator calls `resume_session`, which has the checks below. This also applies to ADR-006 (`coordinator.v1`) coordinators.
- `resume_session`: the deck computes the effective resumed mode (the session's stored mode, or the provider instance's default when none is stored) and refuses the resume (`forbidden`, stored) unless the family has a ceiling, the mode is within it, and it is not a bypass mode. The station's resume admission checks the same at dispatch with the mode it will really use.

### 5.3 Answers

- **Approvals:** `option_id` of an allowed kind; no edited input (`updated_input` is never sent for an approval).
- **Questions** (`RequestKind::Question`): `question_answers` (a JSON object ≤ 16 KiB) for structured questions, validated by the station against the complete question schema with the existing `validate_question_answer` (`crates/fleetdeck-station/src/manager/decide.rs:326`) and translated into the provider's answer payload; or `text` (≤ 4 KiB) for a free-text question. A question answer never carries an approval option.
- **First answer wins, with a clear result.** For an answer whose origin is a coordinator, a request that already has an answer gives a stored rejection `already_answered` (with the winner's `by`), instead of the general repeated-answer acceptance (`AnswerClaim::Repeated`, decide.rs:224), which stays for board answers. The tool result separates: accepted intent, the claim result, and the provider delivery.
- **Attribution.** The station records `approval.resolved { by: coordinator }`; the deck sets `answered_by = "coordinator:<session id>"` on the command, so the chat shows "Allowed by <coordinator title>". Every answer writes a deck audit row (grant, caller, target, request id, option kind, outcome).
- **Owner visibility.** The needs-you rail keeps showing open requests. The coordinator's card shows a "Fleet control" chip with the count of answers in the last hour; its session page lists its last 50 answers (from the audit rows).

## 6. Authority until dispatch

### 6.1 Provenance

Every deck intent created by a fleet-control op carries `authority { grant_id, caller_station, caller_session, generation }` in the outbox item and in the routed command (new optional field on the routed session command envelope, sent only on `coordinator.v2` links). Fleet-control intents expire after 10 minutes (not 24 h).

### 6.2 Rechecks until the provider write

- **Deck, at `start_attempt` and again before every send and every resend** of a routed command with `authority` (`DeckLink::send`, `crates/fleetdeck-deck/src/link/router.rs:291`): the grant is `active` and not past `expires_at`, its binding generation equals `authority.generation`, the target still passes §4.2, the deck setting `fleet_control` is on, and both links negotiated `coordinator.v2`. Any failure: the command is not sent; the outbox item ends `dead` (`authority_lost`).
- **Target station, at decision:** a command with `authority` is refused (`forbidden`, stored) when the target station's live `allow_fleet_control` is off, when the target session holds an active binding with a mutating class on this station (§4.2 rule 3), or when the target is the caller session (rule 2).
- **Target station, at effect dispatch (authority round trip):** right before the provider write of an effect whose command had `authority`, the station asks the deck over the link with a new non-mutating method `coordinator.authority_check { grant_id, generation, caller_station, caller_session, target_session, command_id }` (feature `coordinator.v2`, deadline 5 s). The deck answers `valid` only if every deck-side check above still holds at that moment (deck-local values) **and** the caller-policy step succeeds: the deck asks the caller station over its link with a new non-mutating method `coordinator.policy_now { caller_session }` (feature `coordinator.v2`, deadline 3 s), and the caller station answers from its live policy whether `allow_fleet_control` is on and the caller session still holds the binding. No answer, a link without `coordinator.v2`, or `off` → `invalid`. The whole round trip has the 5 s deadline. `invalid`, no answer within 5 s, or no link → the effect is `NotDispatched("authority_lost")` or `NotDispatched("authority_unknown")` and the session gets an `error` event with that code; it is never retried with a new id. The station also rechecks its own live `allow_fleet_control` and rule 3 at that point.
- **Revocation** (owner, deck first, ADR-006 §7) and turning `allow_fleet_control` off take effect for every effect whose provider write has not begun: queued deck items end at the next send check, sent commands are refused at target decision or dispatch by the round trip. Bounded windows, stated in the operations guide: (a) between a `valid` answer and the provider write inside the target station process (no await on another process; milliseconds); (b) between the caller station's `policy_now` answer and that `valid` answer (one deck step, at most the 5 s deadline); (c) none from settings propagation, because every switch is read live from its own process.

## 7. Lifecycle

- ADR-006 §7 applies: idle expiry, renewal, revocation, session end.
- `allow_fleet_control` off is a global stop without revoking grants (§3, §6.2).
- A resume never restores a grant (DL-165 continued).

## 8. Failure behaviour

| Failure | Behaviour |
| --- | --- |
| The target request closed or was answered between `list_needs_you` and `answer_request` | `already_answered` or the existing closed-request rejection, stored. |
| A widening option, an unclassified option, or a bypass mode | `forbidden`, stored. |
| The link drops after an answer was sent | ADR-006 §6.5: `ambiguous`, never resent; the tool says the outcome is unknown. |
| `allow_fleet_control` turned off, or the grant revoked, before an attempt or a dispatch | §6.2: never sent, or `authority_lost` at dispatch. |
| A peer without `coordinator.v2` | No fleet-control grant can name it (§3); a stray `authority` command is `unsupported`. |

## 9. Tests (written with the code)

Deck (`crates/fleetdeck-deck/src/store/fleet_control_tests.rs`):
1. A per-operation matrix: for every new op and query and each ADR-006 op under a fleet-control grant: class present/absent, scope lineage/fleet, target on/off a grant station, target = caller, target holds an active mutating grant, target is a member or guest session → the expected accept or stored rejection.
2. Elevation: a fleet-control grant without a fresh elevation → `elevation_required`; with it → stored with the new fields.
3. Version gate: a grant naming a station without `coordinator.v2` → `unsupported`; a lineage grant's start command serializes without the new fields (byte check against a recorded v1 command).
4. `inspect`: an ADR-006 `read`-only grant gets `not_granted`/`forbidden` for every new query; with the setting off every fleet-control query is refused and the old reads still work.
5. Answers: `allow_once` and `deny` accepted, `answer_used` +1, audit row, `answered_by`; `allow_session`, `allow_always`, `amend`, `other`, `None` refused without a count; cap reached (and two concurrent decisions for the last slot: exactly one wins); structured `question_answers` accepted and validated; a bad one refused.
6. Modes: `set_mode`, `spawn`, `start_in_project` above the ceiling or bypass refused; `resume_session` of a session stored in a mode above the ceiling, in bypass, or with no stored mode and a default above the ceiling refused.
7. `start_in_project`: counts against `spawn_cap`, the new session is in the lineage; `move_to_project` with a project of another station → `wrong_station`.
8. Deck-visible provenance: revocation, a generation change, a target that becomes a coordinator, the deck setting `fleet_control` turned off, and a link that drops to `coordinator.v1`, each (a) between acceptance and the first attempt, (b) between the first send and a resend, → never sent, `dead` (`authority_lost`); an item past 10 minutes expires. (Station switches are not deck-visible: tests 8d, 8e and 11 cover them at decision and dispatch.)
8a. `coordinator.authority_check`: valid for a current grant; invalid after revocation, after a generation change, after the deck setting turned off, and for a target that became a coordinator.
8c. Revocation and the deck setting turned off **after `start_attempt` committed the pending routed command and before the first `DeckLink::send`** → not sent, `dead` (`authority_lost`).
8d. A caller station's `allow_fleet_control` turned off while the deck setting stays on, the grant stays active, and the caller station's settings publication to the deck is held back by a test hook: new calls from its coordinators are refused at intake; an accepted effect queued on another station reaches dispatch, `coordinator.policy_now` reads the caller station's live value (off) despite the held-back publication, the round trip answers `invalid`, and no provider write occurs (`authority_lost`); with the caller station unreachable → `authority_unknown`, no write.
8e. The target station's `allow_fleet_control` turned off between decision and dispatch → `NotDispatched("authority_lost")`.
8b. Answer accounting: a duplicate call id does not count twice; receipt pruning does not change `answer_used`.
9. Fencing: every new query result with third-party text, and `mail_frame`/`question_frame` output, is enveloped with the source outside the fence.
Station (`crates/fleetdeck-station/src/coordinator/fleet_control_tests.rs`):
10. Intake refuses fleet-control tools with the setting off; the catalog lists tools by class.
11. Dispatch recheck: a command with `authority` that the target station accepted, then the grant is revoked, the deck setting turned off, or the caller station's setting turned off (test 8d) before dispatch → the round trip answers invalid → `NotDispatched("authority_lost")`; with the deck unreachable at dispatch → `NotDispatched("authority_unknown")`; a target that became a coordinator → refused.
11a. Implicit resume: coordinator mail, a coordinator task and a coordinator steer to a hibernated session are refused `invalid_state` and held by the deck; board and owner-mail inputs still resume it (DL-180); `resume_session` above the ceiling is refused.
12. `already_answered` for a coordinator answer after a board answer; a board answer after a board answer keeps today's result.
13. `session_diff` for a coordinator: never contains a byte of the old side, also not in `@@` hunk headers (a historical credential in the old side's function context does not appear) or `index`/mode lines (a committed credential under an ordinary name, replaced by a safe file, appears only as `-[n lines removed]`); hides a tracked work-tree file hard-linked to a private file; a path replaced between the identity check and the read cannot change the bytes (the read is from the checked descriptor); a private-named path on the old side only is dropped; fails with `privacy_unknown` when the identity check cannot run; the temp directory is removed.
14. Resume admission refuses a coordinator resume into a mode above the ceiling or into bypass.
Providers: 15. `OptionKind` classification for every Claude and Codex decision shape, including object-valued amendments.
Board (`web/src/spawn/fleetcontrol.test.tsx`): 16. The switches ask for elevation; the start command carries the fields; the chip and the answers list.

## 10. Alternatives

- **No approvals (owner option 1).** Safer; the owner rejected it.
- **Answer only in the lineage.** Kept as `scope = lineage` + `answer`.
- **Any option, including "always" or amendments.** Rejected: one injected answer would widen permissions for all later calls.

## 11. Risks

- **Prompt injection approves a dangerous call.** Bounded, not removed: one-time allow or deny only, no edited input, no bypass, the mode ceiling, the cap, no coordinator-to-coordinator control, fenced inputs, owner visibility, revoke and the kill switch. The operations guide says to run an `answer` grant only on trusted work.
- **Exfiltration across stations.** An `inspect` grant reads files and diffs of every session on its stations and can send them on with its own tools. Accepted; bounded by the station list; private files stay refused (§4.4).
- **Spend.** `start_in_project` counts against `spawn_cap`.
- **Residual dispatch window** (§6.2) on the target station's local effect queue.

## 12. Review map (revision 1 → revision 2)

| Finding (rev 1) | Change |
| --- | --- |
| B1 new reads escape elevation and the kill switch | New class `inspect` (fleet-control, elevated, kill switch); `read` keeps only its ADR-006 queries (§3, §4.1, test 4). |
| B2 bypass mode allowed through `max_modes` | §5.2 first rule (adopted replacement sentence). |
| B3 resume bypasses the mode ceiling | §5.2 resume rule, deck and station (tests 6, 14). |
| B4 lineage scope can control another coordinator | §4.2 applies to both scopes and replaces the lineage drive rule (test 1). |
| B5 structured questions cannot be answered | §5.3 `question_answers` (test 5). |
| B6 accepted intents survive revoke and kill switch | §6 provenance, rechecks before attempt and at dispatch, 10-minute expiry (tests 8, 11). |
| B7 pushed inputs are not fenced | §4.3 fences mail, questions, task results and conflict mail for every recipient (test 9). |
| B8 diff privacy inherits the tracked-hard-link gap | §4.4 identity check per file, `privacy_unknown` (test 13). |
| M1 option kinds undefined | §5.1 `OptionKind`, provider-owned, fail closed (test 15). |
| M2 start serialization breaks v1 stations | §3 version gate and conditional serialization (test 3). |
| M3 first answer wins has no rejection | §5.3 `already_answered` for coordinator answers (test 12). |
| M4 tests do not cover every rule | §9 matrix and the new tests. |

## 13. Re-review map (revision 2 → revision 3)

| Finding (rev 2) | Change |
| --- | --- |
| B1 implicit resume through mail | §5.2 "No implicit continuation by a coordinator" (also for ADR-006 coordinators); test 11a. |
| B2 queued effects survive revocation | §6.2 recheck before every send and resend, and the `coordinator.authority_check` round trip at target dispatch; tests 8, 8a, 11. |
| B3 diff bytes not covered by the identity check | §4.4 per-file diff: names first, new side read from the checked descriptor, old side from committed blobs, `--no-index` render; test 13. |
| B4 reconnect on v1 bypasses the authority | §3 "Every send needs v2"; §6.2 deck send check; test 8. |
| M1 tests | Tests 8, 8a, 8b, 11, 11a, 13. |

## 14. Extra review map (revision 3 → revision 4)

| Finding (rev 3) | Change |
| --- | --- |
| B2 stale caller kill switch | §3 two switches: a deck setting `fleet_control` that every deck check reads locally, and the station switch checked only on its own station; no mirror. §6.2 windows (a)–(c). Tests 8a, 8c–8e. |
| B3 committed history exempt | §4.4 steps 4–5: no old-side byte leaves the station; removed lines are counted only. Test 13. |
| M1 two missing boundaries | Tests 8c (after `start_attempt`, before the first send), 8d/8e (policy changes), 13 (historical alias). |

## 15. Extra review map (revision 4 → revision 5)

| Finding (rev 4) | Change |
| --- | --- |
| B2 caller switch does not stop accepted effects | §3 station switch read live for the caller too; §6.2 `coordinator.policy_now` step in `authority_check`, fail closed; windows (a)–(c). Tests 8, 8d, 11 aligned. |
| B3 history in hunk headers | §4.4 step 5 rebuilds `@@` headers from numbers only and drops old-side metadata; suppression when not possible. Test 13. |
| M1 delayed caller-policy test, contradictions | Test 8d rewritten (deck setting on, grant active, caller switch off, no write); tests 8 and 11 aligned. |

## 16. PRE-PR fixes

### Station (decisions 1 to 4)

1. **Gate placement.** The `coordinator.authority_check` round trip of an effect runs after every awaited preparation and fence, immediately before the provider write or launch, with the session lock held. A prompt or a follow-up gates after the workspace fence and `begin_input()`; a steer, after the fence; `set_mode`, `interrupt`, `answer` and `stop`, right before the provider call; a start or a resume (direct, durable and import), right before the process launch; the first input of a start, after its fence and checkpoint work. The effect has one gate per write path and no earlier call. A refused input is `input_not_sent`; a refused direct start drops its `ready` signal and revokes the session; a refused start or resume ends the session as a failed start.
2. **Reread after the round trip.** After the deck answers `valid`, the station reads its live setting `allow_fleet_control`, rule 3 and rule 2 once more, with no await between that read and the provider call. A change refuses the effect with `authority_lost`. The recheck reads the live setting itself (a closure on the policy hub), never a copy taken before the round trip.
3. **Launch mode.** An effect that launches a provider (start, resume, import) or changes its mode (`set_mode`) sends `launch = { provider, mode }` in `coordinator.authority_check`. `provider` is the kind of the adapter of the spec's instance (`set_mode`: the kind in the stored session summary); `mode` is the mode that the launch really uses (`None`: the provider default). A provider that is not known fails closed (`authority_unknown`). Every other effect sends `launch: null`. A deck answer `valid: false` (also `mode_not_allowed`) refuses the effect. The ceiling is the deck's check.
4. **Coordinator answers to questions.** A coordinator answers a question with the option `answer` plus its answers or text; any other option is a stored `forbidden`. A Claude question needs `question_answers` (an object whose keys are the `questions[].question` strings of the request and whose values are strings of at most 4096 characters); the station validates them before the claim and rewrites the decision to the one-time `allow`, with the tool input and `answers` set, never `allow_always`. `ClaudeCore::answer` sends no `updatedPermissions` for an answer whose `by` is the coordinator. Codex questions keep the existing validation. Board and owner answers are unchanged.

**Round 2 fixes (station).** (1) A durable write gates inside the driver: `apply_gated` runs the gate after the core lock, the transition and the claim of the first frame, immediately before the first byte; a refusal rebuilds the core, drops the transition like a `Retired` result and settles the effect `authority_lost`. The first input of a durable start gates inside `first_input`, after the core lock and right before the commit of the input, because that commit appends events to the log and cannot be undone. (2) A durable launch gates inside `launch_shim_for`, after the `starting` row and immediately before `host.start`; a refusal ends the row `never_started` and fails the start with `authority_lost` (`LaunchError::Refused`, no revoke). (3) Every gate of a start, a resume, an import or a `set_mode` sends its `launch` mode, the first-input gate of a start too; every other effect sends `launch: null`.

## 17. PRE-PR fixes (deck)

The deck parts of the PRE-PR findings:

- **Stale authority after `policy_now` (blocker 2).** `coordinator.policy_now` can wait up to 3 s. `coordinator.authority_check` therefore runs the deck-local checks (grant, generation, deck setting `fleet_control`, the command, the launch mode, the target rule, and the links of the caller station and of the target station) twice: before the `policy_now` round trip, which saves the round trip for an authority that is invalid already, and again after it. The deck answers `valid: true` only when the second run also passes; a change in between gives the reason of the failed check.
- **Launch mode (blocker 4).** `AuthorityCheck.launch` carries the provider and the mode that the target station will really use. `authority_local` applies the mode rules of section 5.2 (the ceiling of the family and the bypass rule) to it, in the transaction of the grant check, and answers `mode_not_allowed` when they fail. A check without `launch` makes no mode check.
- **Answer to a question (blocker 3).** A coordinator answers a question with `question_answers` or `text`, never with an option: an answer with `option_id` is `forbidden`, because an option of a question can carry a standing allow (for example `allow_always`). An answer without `option_id` gets the literal option id `answer`, which the station maps for the question.
- **`needs_you` text (major 5).** The tool title, each option label and the session title come from a provider or from a session and are inside the untrusted fence. `option_id` and `kind` stay outside, because the coordinator needs them to answer.
