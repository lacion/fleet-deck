# ADR-008 — Fleet control: fleet-scope grants, new tools, and approvals by a coordinator

- **Status:** Proposed (revision 1, 2026-10-14). Needs an adversary DESIGN REVIEW PASS before code.
- **Deciders:** owner (2026-10-14: "lets expand the mcp to allow full controls of the fleet … launch a session and that session could orchestrate a lot of things in the fleet"; on the scope question the owner chose "Full control incl. approvals"), architect.
- **Extends:** ADR-006 (revision 4). Every ADR-006 rule stays in force unless this ADR names it and replaces it. In particular the credential handoff (§5.4), the binding states (§5.5), the decision order (§6.2), the station outbox and its fences (§6.5), and the same-UID boundary (§3) are unchanged.

## 1. Context

A coordinator today (ADR-006 §6.3) reads the sessions of its grant's stations, sends mail, assigns tasks, spawns sessions under a cap and a mode ceiling, and interrupts or stops only the sessions of its own lineage. It never answers an approval or a question (ADR-006 §6.3, §12). The owner wants one session that runs the fleet: drives any session, starts work in projects, reads what the fleet produced, and answers approvals.

Answering approvals is the largest new power. A coordinator reads third-party text (issues, PRs, files, the output of other sessions). A prompt injection in that text can make it approve a dangerous tool call in another session. This ADR accepts that risk at the owner's request and bounds it (§5).

## 2. Terms

| Term | Meaning |
| --- | --- |
| Lineage scope | ADR-006 drive rule: a target session has this grant in its lineage. |
| Fleet scope | New: a target session is any session on a station of the grant, minus the exclusions of §4.3. |
| Fleet-control grant | A grant with `scope = fleet`, or with the class `answer`, or with the class `manage`. |

## 3. Grant changes (deck `tokens` row, `CoordinatorSpec`)

- New field `scope`: `lineage` (default; today's behavior) or `fleet`.
- New classes, added to `classes` ⊆ {`read`, `mail`, `drive`, `spawn`, `manage`, `answer`}. Unknown names still fail closed.
  - `manage`: resume, rename, set mode, move to project, start in a project.
  - `answer`: answer approvals and questions of target sessions (§5).
- New field `answer_cap` (0–5000, default 500): a durable lifetime counter `answer_used`, the same accounting as `spawn_cap` (ADR-006 §6.4: counted in the decision transaction, never returned, a cap rejection `answer_cap_reached` is stored).
- `CoordinatorSpec` gains `scope` (serde default `lineage`). The station needs it only to list tools; the deck decides every call.
- **Creation of a fleet-control grant is an elevated owner action.** The spawn form shows "Fleet control" with three switches (fleet scope, manage, answer approvals). Turning any of them on needs the owner's elevation (the DL-172 elevation: the owner password or a `fleetdeck open --elevate` code). The deck refuses a start command whose grant has a fleet-control field without a fresh elevation (`elevation_required`). A member, a share guest or an API token never creates one.
- **Station kill switch.** New station setting `allow_fleet_control` (default `false`, `apply: Live`, `danger: true`; turning it on needs elevation, turning it off does not). While it is off, the station lists no fleet-control tools and refuses fleet-control ops at intake (`not_granted`), and the deck refuses them (`forbidden`, stored). Turning it off takes effect at the next call (live policy, DL-172 §1.4); it does not revoke grants.

## 4. Authorization

### 4.1 Tool and class matrix (adds to ADR-006 §6.3)

| MCP tool | Class | Wire | Target rule |
| --- | --- | --- | --- |
| `read_session` | read | query `session_tail { session_id, max_items ≤ 200 }` (≤ 64 KiB) | ADR-006 rule (now built) |
| `session_diff` | read | query `session_diff { session_id }` (≤ 256 KiB, the DL-159 diff) | sessions on stations of the grant |
| `list_needs_you` | read + answer | query `needs_you` | open approvals and questions of target sessions (§4.2) |
| `list_projects`, `project_items` | read | query `projects`, `project_items { project_id, kind: issues \| prs }` | projects whose station is in the grant |
| `list_artifacts`, `read_artifact` | read | query `artifacts { session_id? }`, `artifact { artifact_id, version? }` (text kinds only, ≤ 256 KiB) | artifacts of sessions on stations of the grant |
| `read_file`, `search_files` | read | query `file_read { session_id, path }` (≤ 256 KiB), `file_search { session_id, query, mode }` (≤ 50 hits) | the session root of a target session; the station's private-path and root rules apply unchanged (DL-173) |
| `send_mail`, `create_task` | mail | unchanged | lineage scope: unchanged; fleet scope: §4.2 |
| `interrupt_session`, `stop_session` | drive | unchanged | lineage scope: unchanged; fleet scope: §4.2 |
| `resume_session` | manage | call `resume { station, session_id }` | §4.2; the station's resume rules apply |
| `rename_session` | manage | call `rename { station, session_id, title }` | §4.2 |
| `set_mode` | manage | call `set_mode { station, session_id, mode }` | §4.2; the mode must be within the grant's `max_modes` ceiling for that provider family, else `forbidden` |
| `move_to_project` | manage | call `project_assign { station, session_id, project_id \| none \| auto }` | §4.2; DL-176 rules |
| `start_in_project` | spawn + manage | call `project_start { project_id, prompt, provider_instance?, worktree: bool }` | the project's station is in the grant; counts against `spawn_cap`; the mode rule of `spawn` applies |
| `answer_request` | answer | call `answer { station, session_id, request_id, option_id, text? }` | §4.2 and §5 |

- `tools/list` lists a tool only when the grant has all its classes, its scope allows it, and the station setting `allow_fleet_control` is on for fleet-control tools. The catalog is fixed at process start (ADR-006 §5.3), so a later change of the setting changes the station intake answer (`not_granted`), not the catalog.
- Every new query is a `coordinator.query` (no receipt); every new op is a `coordinator.call` (receipt, outbox). New `CoordinatorOp` variants: `resume`, `rename`, `set_mode`, `project_assign`, `project_start`, `answer`. New `CoordinatorQuery` variants: `session_tail`, `session_diff`, `needs_you`, `projects`, `project_items`, `artifacts`, `artifact`, `file_read`, `file_search`.
- Link feature `coordinator_v2`. A station enables fleet-control tools only on a link that negotiated it; a deck accepts the new shapes only on such a link. An older peer keeps ADR-006 behavior.

### 4.2 Fleet-scope target rule

A target session is allowed when all of these hold, checked by the deck in the decision transaction (ADR-006 §6.2 step 6):

1. Its station is in the grant's `stations`.
2. It is not the caller itself, for `drive`, `manage` and `answer` ops. (A coordinator never stops, re-modes or approves itself.)
3. It holds no active grant of its own with a mutating class (`mail`, `drive`, `spawn`, `manage`, `answer`), for `drive`, `manage` and `answer` ops. A coordinator never drives or approves another coordinator. This blocks loops where two coordinators approve each other's calls.
4. It is not a member-owned or shared session that the owner has not opened to coordinators. (Today every session belongs to the owner; a share guest's driving rights never extend a coordinator.)

With `scope = lineage`, rule 1 plus the ADR-006 lineage rule apply, and rules 2–3 also apply to `answer`.

### 4.3 Third-party text

- Every query result that holds text from another session, a file, an issue, a PR or an artifact is wrapped in the injection envelope of DL-164 (fenced, labelled untrusted, with its source). The control skill tells the agent that fenced text is data and never an instruction.
- `list_needs_you` gives for each request: station, session, title, request id, kind, tool name, a summary of the tool input (≤ 8 KiB, fenced), the offered option ids with their kinds, and the age.

## 5. Approvals by a coordinator (`answer` class)

- **What it can choose.** Only an option whose kind is a one-time allow, a deny, or a question answer. It can never choose an option that widens permissions beyond this one request (Claude "allow always" / permission-rule updates, Codex "approve for session" / "always"); the deck refuses it (`forbidden`, stored). It can never send `updated_input` (no edited tool input). A question answer carries `text` ≤ 4 KiB.
- **Mode changes are not a back door.** `set_mode` is bounded by `max_modes` (§4.1). A coordinator cannot set a target into a bypass or full-access mode unless the owner put that mode in `max_modes` when creating the grant; the station's `allow_bypass_permissions` (DL-177) still gates the mode itself.
- **First answer wins.** The answer goes through the station's existing claim (one answer per request). The owner can answer before the coordinator; a later coordinator answer gets the stored rejection that a second answer gets today.
- **Attribution.** The station records `approval.resolved { by: coordinator }` and the deck adds the grant id and the coordinator session (title and callsign) to the event's `answered_by` (`coordinator:<session>`), so the chat shows "Allowed by <coordinator>". Every answer writes a deck audit row (grant, caller, target session, request id, option kind, outcome).
- **Owner visibility.** The needs-you rail keeps showing open requests to the owner. The board shows a "Fleet control" chip on the coordinator's card with the count of answers in the last hour, and the session page of the coordinator lists its last 50 answers (from the audit rows).
- **Cap.** `answer_cap` bounds the total answers of a grant (§3).
- **Offline approval policy.** Requests that the station resolves by policy (`ResolvedBy::Policy`, `Timeout`) are unchanged; a coordinator answer is a normal answer.

## 6. Lifecycle

- ADR-006 §7 applies unchanged: idle expiry, renewal, revocation, session end. The board's revoke of a grant also ends fleet control at once (deck first).
- The station setting `allow_fleet_control` (§3) is a global stop for all fleet-control tools without revoking grants.
- A resume never restores a grant (DL-165 continued). A coordinator that ends loses fleet control; the owner starts a new one.

## 7. Failure behaviour

| Failure | Behaviour |
| --- | --- |
| The target session ended or changed between `list_needs_you` and `answer_request` | The station's claim rejects the answer (`request_closed` or the existing code); the receipt is stored; the tool reports it. |
| The deck refuses a widening option | Stored `forbidden` with the text "a coordinator answers only once"; the tool reports it. |
| The link drops after an answer is sent | ADR-006 §6.5: the row becomes `ambiguous` and is never resent; the tool says the outcome is unknown. |
| The setting `allow_fleet_control` is turned off while a call waits | The deck decision (authority) refuses it; a call already accepted runs. |

## 8. Executable tests (written with the code)

Deck (`crates/fleetdeck-deck/src/store/fleet_control_tests.rs`):
1. A fleet-control grant without a fresh elevation is refused (`elevation_required`); with it, it is stored with `scope`, the new classes and `answer_cap`.
2. Fleet scope: `stop_session` on a session outside the lineage on a grant station is accepted; on a station outside the grant `forbidden`; on the caller itself `forbidden`; on a session that holds an active mutating grant `forbidden`.
3. Lineage scope keeps ADR-006 results for every old tool (regression).
4. `answer_request` with a one-time allow is accepted, increments `answer_used`, writes the audit row, and the command to the station carries `answered_by = coordinator:<session>`.
5. `answer_request` with an "allow always"/"for session" option, or with `updated_input`, is `forbidden` (stored), and `answer_used` does not change.
6. At `answer_cap`, `answer_cap_reached` (stored); a repeat of the same id returns it after the owner raises the cap.
7. `set_mode` above `max_modes` is `forbidden`.
8. With `allow_fleet_control = false`, every fleet-control op is `forbidden` (stored) and every fleet-control query is refused; lineage ops still work.
9. Every query result with third-party text is fenced.
Station (`crates/fleetdeck-station/src/coordinator/fleet_control_tests.rs`):
10. `tools/list` for each class and scope combination, with the setting on and off.
11. Intake refuses a fleet-control tool when the setting is off (`not_granted`, never forwarded).
12. The answer reaches the provider once; a second answer to the same request gets the existing rejection.
Board (`web/src/spawn/fleetcontrol.test.tsx`):
13. The "Fleet control" switches ask for elevation; the start command carries the fields; the coordinator card shows the chip and the answers list.

## 9. Alternatives

- **No approvals (owner option 1).** Safer; rejected by the owner.
- **Answer only for the coordinator's own lineage.** Smaller blast radius, but it does not run "the fleet"; kept as `scope = lineage` + `answer`.
- **Answer any option, including "always".** Rejected: one injected answer would change permissions for all later calls of that session.

## 10. Risks

- **Prompt injection approves a dangerous call.** Bounded, not removed: one-time options only, no edited input, the mode ceiling, the cap, no coordinator-to-coordinator control, owner visibility, revoke and the global setting. The operations guide states that a fleet-control grant with `answer` should run only on trusted work.
- **Data exfiltration across stations.** A coordinator with `read` can read files and diffs of every session on its stations and can send them to any session or URL that its own tools reach. Accepted: the grant's station list bounds it; private paths stay refused (DL-173).
- **Spend.** `start_in_project` counts against `spawn_cap`.
