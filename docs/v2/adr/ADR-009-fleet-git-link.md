# ADR-009 — Fleet Git: link protocol additions

- **Status:** Proposed (2026-10-17, draft revision 1). Needs the adversary DESIGN REVIEW together with `docs/v2/design/fleet-git.md` and ADR-010 before the status changes.
- **Deciders:** owner (O5 of `/home/luismorales/fleetdeck-briefs/fleet-git-owner-spec.md`: "All link protocol changes are additive. An older peer ignores the new fields and methods. Record them in a new ADR-009."), architect.
- **Extends:** ADR-004 (link protocol v1, §2.7 data channel, §9.2 stream tokens, §9.3 command envelopes, §9.6 method table, L16 feature flags), ADR-006 and ADR-008 (coordinator queries and ops). Every rule of those ADRs stays in force.
- **Design:** `docs/v2/design/fleet-git.md` (FG-1 to FG-12). Section numbers below with "design §" point there.

## 1. Context

Fleet Git makes one repository work across several stations: fetch before a worktree start, push of session branches, cleanup, homes on many stations, placement, a predictive radar, a land flow, a session move, and Taildrive reports. The deck and the stations need new methods, fields and events for this. A fleet is upgraded one machine at a time, so a new deck meets old stations and a new station meets an old deck. This ADR lists every change and shows that each one is additive.

## 2. The additive rules that this ADR uses

These rules exist today. This ADR adds no new rule.

1. **Features (L16, ADR-004 §2.5).** A side offers `feature::ALL`; the link has the sorted intersection. A method with a feature is refused with `feature_not_negotiated` when the link lacks it (`methods::authorize`). So a new method never reaches an old peer.
2. **Unknown fields are ignored** in frames and in params (ADR-004 §2.5). A struct that has `#[serde(deny_unknown_fields)]` (for example `CoordinatorSpec`, `ProjectInput`) gets no new field from this ADR, or gets it only on a link that negotiated the feature.
3. **Unknown event kinds** are kept as `EventBody::Unknown` and pass through; the fold changes only the sequence and the time (`fleetdeck-proto/src/event.rs`, `fold.rs`).
4. **Optional fields** have `#[serde(default)]` and, when they are new on an existing type, `skip_serializing_if = "Option::is_none"` (or an empty default), so an old peer reads the same bytes as before when the feature is not used.
5. **A closed enum is not additive.** `SessionState` and `CommandOrigin` have no `#[serde(other)]`. This ADR adds **no** value to them. Every new enum of this ADR has a `#[serde(other)] Unknown` variant.
6. **A new `SessionCommand` variant** (an `op` value) is sent only on a link that negotiated its feature. An old station would reject it at decode; the feature check stops it first.
7. **The deck checks the feature before it acts** and answers the board 409 `fleet_git_unsupported` ("This station does not support Fleet Git. Update it first."), as `setup_not_supported` (DL-148) does.

## 3. New features

Added to `fleetdeck-proto/src/version.rs` (`feature::ALL`). None is in `REQUIRED`.

| Feature | Phase | What it carries |
| --- | --- | --- |
| `fleet_git.v1` | 1 | `fleet_git.projects_set`, `fleet_git.push_now`, `SessionCommand::Start.fleet_git`, `WorkspacePlan` sync field (station internal), the events of §6 rows 1 to 4 |
| `fleet_git.homes.v1` | 2 | `project.ensure_home`, `project.home_status`, `FileRoot::Artifacts`, the artifact events |
| `fleet_git.land.v1` | 3 | `project.radar`, `session.land`, `fleet_git.land_status`, the `session.land` event |
| `fleet_git.move.v1` | 4 | `session.move_out`, `session.move_export`, `session.move_prepare`, `session.move_in`, `session.move_finish`, `session.move_abort`, `session.move_discard`, `session.move_status`, the stream purposes, the `session.moved` event |
| `fleet_git.branches.v1` | 4 | `project.branches`, `project.fetch` |

The coordinator additions (§7) need `coordinator.v2` **and** the matching Fleet Git feature.

## 4. New methods

All are deck → station. The rows are added to "Method table v1 (Revision 2)" in ADR-004 §9.6 (the table test reads that table) and to `crates/fleetdeck-link/src/methods_table.rs`.

| Method | Scope | Feature | Mutating | Params → answer |
| --- | --- | --- | --- | --- |
| `fleet_git.projects_set` | admin | `fleet_git.v1` | no | `{ projects: [GitProjectView] }` → `{ stored }` |
| `fleet_git.push_now` | drive | `fleet_git.v1` | no | `{ session_id }` → `{ state }` |
| `project.ensure_home` | admin | `fleet_git.homes.v1` | no | `{ project_id, project_name, url, repo }` → `HomeStatus { state, path, code?, message? }` |
| `project.home_status` | read | `fleet_git.homes.v1` | no | `{ project_id }` → `HomeStatus` |
| `project.radar` | read | `fleet_git.land.v1` | no | `{ project_id, branches: [RadarBranch] }` → `RadarResult` |
| `session.land` | drive | `fleet_git.land.v1` | yes | `SessionCommand::Land { session_id, land_id, mode, target_branch? }` → receipt |
| `fleet_git.land_status` | read | `fleet_git.land.v1` | no | `{ land_id }` → `{ state, step?, code?, text? }` (`unknown` for an unknown id) |
| `session.move_out` | admin | `fleet_git.move.v1` | yes | `SessionCommand::MoveOut { session_id, move_id, to_station }` → receipt |
| `session.move_export` | admin | `fleet_git.move.v1` | no | `{ move_id }` → `{ manifest: TranscriptManifest, stream_token }` |
| `session.move_prepare` | admin | `fleet_git.move.v1` | no | `{ move_id, manifest, source }` → `{ stream_token }` |
| `session.move_in` | admin | `fleet_git.move.v1` | yes | `SessionCommand::MoveIn { move_id, kind, provider_instance, title, callsign_hint, from, fleet_git }` → receipt |
| `session.move_finish` | admin | `fleet_git.move.v1` | yes | `SessionCommand::MoveFinish { session_id, move_id, to_station, to_session }` → receipt |
| `session.move_abort` | admin | `fleet_git.move.v1` | no | `{ move_id }` → `{}` (idempotent: an unknown or closed id is ok) |
| `session.move_discard` | admin | `fleet_git.move.v1` | no | `{ move_id }` → `{}` (idempotent) |
| `session.move_status` | read | `fleet_git.move.v1` | no | `{ move_id }` → `{ state, session_id?, code?, text? }` |
| `project.branches` | read | `fleet_git.branches.v1` | no | `{ project_id }` → `RemoteBranches` |
| `project.fetch` | read | `fleet_git.branches.v1` | no | `{ project_id }` → `{ fetched_at, code?, text? }` |

Why the scopes: `admin` for the methods that run Git network commands that create folders or write the station's project table, or that carry transcripts; `drive` for a session action (land, push now); `read` for reads and for status calls. `project.fetch` changes only remote-tracking refs and is `read`, as the spec gives it the `inspect` class (O32).

Why mutating or not: the four `SessionCommand` methods change session state that a retry must not repeat, so they have a `command_id`, an envelope and a receipt (ADR-004 §9.3). Every other method is a read, a whole-table replace (`projects_set`), or an idempotent call keyed by an id that the deck chose (`project_id`, `move_id`, `land_id`), so a retry repeats nothing.

Error codes (new wire strings in `fleetdeck_proto::fleet_git::code`): `not_synced`, `branch_namespace_blocked`, `git_too_old`, `fetch_failed`, `fetch_timeout`, `push_failed`, `push_rejected`, `push_auth`, `push_timeout`, `home_conflict`, `clone_failed`, `clone_root_missing`, `clone_interrupted`, `land_not_ready`, `land_open`, `check_disabled`, `move_not_ready`, `move_too_large`, `move_checksum`, `move_branch_moved`, `move_resume_failed`, plus the existing `invalid_params`, `unknown_session`, `invalid_state`, `forbidden`, `busy`, `internal`.

**How an older peer behaves:**

- An old station does not offer the features, so the deck never calls these methods on it (rule 1) and answers the board 409 `fleet_git_unsupported` (rule 7). A project with a home on an old station can still start sessions there without sync (the deck sends no `fleet_git` field).
- An old deck never calls them. A new station without calls from the deck has no `git_projects` rows, so it never fetches, pushes, cleans up or lands (O1).

## 5. New fields on existing types

| Type | Field | Shape | Older peer |
| --- | --- | --- | --- |
| `SessionCommand::Start` | `fleet_git` | `Option<FleetGitStart>`, default none, skipped when none | The deck sends it only with `fleet_git.v1`. `SessionCommand` does not refuse unknown fields, so an old station would ignore it and start without sync; rule 7 prevents that case. |
| `StartSessions` (deck HTTP body) | `home`, `requires`, `prefer`, `spread` | optional | Board → deck only; not a link type. |
| `StationInfo` | `traits` | `Option<StationTraits { os, arch, on_battery: Option<bool>, labels }>` | An old deck ignores it (rule 2); a new deck treats a missing value as "no traits" (no requirement matches, no preference counts). |
| `StationInfo` | `config` | `Option<ConfigDigest>` | Missing: the board shows "Config: Unknown". |
| `StationInfo` | `taildrive` | `Option<TaildriveReport>` | Missing: the board shows no Taildrive section. |
| `StationInfo` | `fleet_git` | `Option<FleetGitInfo { clone_root, allow_check_cmd, git_version }>` | Missing: the board hides the land check line and "Add home" for that station. |
| `SessionSummary` | `base_stale` | `bool`, default false, skipped when false | An old deck has no field; its fold ignores the new event. |
| `SessionSummary` | `push` | `Option<PushInfo { state, ref_name, oid, attempts, next_attempt_at, code, text }>` | As above. |
| `SessionSummary` | `cleanup` | `Option<CleanupInfo { state, reason }>` | As above. |
| `SessionSummary` | `land` | `Option<LandInfo>` | As above. |
| `SessionSummary` | `moved_to` | `Option<MovedTo { move_id, station_id, session_id }>` | An old deck sees an `exited` session (§8). |
| `SessionSummary` | `artifacts_dir` | `Option<ArtifactsDir { path?, url? }>` | As above. |
| `FileRoot` (files.v1 queries) | new variant `Artifacts { session_id }` | an enum variant | Sent only with `fleet_git.homes.v1`. An old station refuses the unknown root (`invalid_query`), so it fails closed. |
| `Project` (deck HTTP, board) | `homes`, `repo`, `remote_url`, `git`, `git_revision` | defaults | Not a link type. `station_id` and `path` stay (a copy of the first home), so an old board reads a project as before. |
| `Status` (`tailscale status --json`, parsed) | `CurrentTailnet.Name` | optional | Not a link type. |

`StationInfo` and `SessionSummary` are the only existing link types that get fields. Both already use optional fields for the same purpose (`resources`, `desktop`, `issue`, `batch`).

## 6. New events

All are made by the station (not by a provider). Each has a new `kind`. An old deck keeps it as `EventBody::Unknown`, stores it, replays it to boards, and its fold ignores it (rule 3). An old board shows nothing for an unknown kind.

| Kind | Payload | Fold (new deck and station) |
| --- | --- | --- |
| `workspace.base_stale` | `{ code, text }` | `summary.base_stale = true` |
| `workspace.include_skipped` | `{ files: [{ path, reason }] }` (≤ 50) | none |
| `session.push` | `{ state, ref_name, oid?, attempts, next_attempt_at?, code?, text? }` | `summary.push` |
| `session.cleanup` | `{ state, reason? }` | `summary.cleanup` |
| `artifacts.ready` | `{ path?, url? }` | `summary.artifacts_dir` |
| `artifacts.unavailable` | `{ code, text }` | none |
| `session.land` | `{ land_id, state, step?, code?, text?, files, oid?, pr_url? }` | `summary.land`; the deck also updates `land_queue` |
| `session.moved` | `{ move_id, to_station, to_session }` | `summary.moved_to` |

The enums inside them (`PushState`, `CleanupState`, `LandState`, `LandStep`) each have `#[serde(other)] Unknown`.

`event.rs` is at 788 lines; the build first moves `EventBody` into `event_body.rs` (design §19, T1.0). This is a code move with no wire change.

## 7. Coordinator additions (ADR-006, ADR-008)

| Wire | Kind | Class | Needs |
| --- | --- | --- | --- |
| `CoordinatorQuery::ProjectBranches { project_id }` | query (no receipt) | `inspect` | `coordinator.v2` + `fleet_git.branches.v1` on the links of the caller's station and of the radar home |
| `CoordinatorQuery::ProjectFetch { project_id }` | query | `inspect` | as above |
| `CoordinatorOp::Land { station, session_id }` | op (receipt, outbox) | `manage` | `coordinator.v2` + `fleet_git.land.v1` |
| `CoordinatorOp::Move { station, session_id, to_station }` | op | `manage` | `coordinator.v2` + `fleet_git.move.v1` on both stations |

MCP tools: `project_branches`, `project_fetch`, `land_session`, `move_session` (design §13.2). Why additive: ADR-008 §4.1 already says that each new query and op variant is accepted only on a `coordinator.v2` link; the station forwards a Fleet Git variant only when the deck link also negotiated the Fleet Git feature, else the tool answers `unsupported` and nothing is sent. An old deck therefore never decodes an unknown variant. The tool catalog is fixed at process start (ADR-006 §5.3): the tools are listed when the grant has the class, whatever the features. Answers fence Git text (commit subjects) with the untrusted envelope (ADR-008 §4.3).

## 8. Why there is no new session state

O29 asks for a `moved` state. `SessionState` has no `#[serde(other)]`, so a new value `moved` would make an old deck fail to decode the summary and every snapshot of the station. The design keeps the state `exited` and adds `summary.moved_to`. A new board shows "Moved to <station>"; an old board shows an ended session. The station refuses input to a moved session in its own decision, whatever the deck version.

## 9. Data channel use (ADR-004 §2.7, §9.2)

Two new stream purposes, both authorized by a stream token from a control answer (single use, bound to the `attempt_id`, 30 s):

- `move-export:<move_id>`: the station → deck stream of `session.move_export`. The station writes the manifest files back to back, in manifest order, and closes. It never writes more than `manifest.total` bytes.
- `move-import:<move_id>`: the deck → station stream of `session.move_prepare`. The deck writes the same bytes and closes. The station resets the stream with `move_too_large` after `manifest.total` bytes and checks each file's SHA-256.

The budgets of ADR-004 §9.2 apply unchanged (64 streams, 256 KiB window, 4 MiB buffered). The deck pipes one stream into the other and holds no copy on disk. Why additive: a stream purpose is chosen by the station that issued the token, and an old station never issues one of these tokens (it does not have the methods).

## 10. Deck HTTP routes (not link changes; listed for the review)

Owner login only unless noted; CSRF on every POST, PUT and DELETE; closed to members unless noted; each mutating route is audited.

- `PUT /api/projects/{id}/git` (`project.git_settings`), `POST /api/projects/{id}/homes` (`project.home_add`), `DELETE /api/projects/{id}/homes/{station}` (`project.home_remove`).
- `POST /api/sessions/{station}/{session}/push` (scope `drive`, a `drive` token allowed; `session.push_now`).
- `POST /api/placement` (scope `drive`; no audit).
- `GET /api/radar` (scope `read`).
- `POST /api/sessions/{station}/{session}/land` (`session.land`), `GET /api/projects/{id}/lands` (`read`), `POST /api/lands/{land_id}/cancel` (`land.cancel`), `POST /api/lands/{land_id}/send` (`land.send_to_session`).
- `POST /api/sessions/{station}/{session}/move` (`session.move`).
- `GET /api/projects/{id}/branches` (`read`), `POST /api/projects/{id}/fetch` (`drive`).
- `GET /api/fleet-git/prefs` (`read`), `PUT /api/fleet-git/drift-reference` (`drift.reference`).

## 11. Alternatives considered

- **One feature `fleet_git.v1` for everything.** Rejected: the phases merge one by one, and a station of phase 2 must not get land calls from a deck of phase 3.
- **A new `SessionState::Moved`.** Rejected (§8).
- **A new `CommandOrigin::Station` for the cleanup.** Rejected: `CommandOrigin` is closed; the cleanup calls the trash decision internally (design §6.3).
- **Station-to-station transfer of transcripts.** Rejected: stations do not link to each other; the deck already authenticates both ends (O29).
- **The station pushes its state to the deck as station-level frames.** Rejected: there is no station-level event stream; session events and deck polling (`project.home_status`, `fleet_git.land_status`) need no new frame type.

## 12. Risks

- **A deck that loses the feature mid-flow** (a station downgrade during a land or a move): the deck marks the land `land_lost` or the move `failed` when the station answers `feature_not_negotiated`; the station of the old build has no state to clean.
- **Event volume:** `session.push` and `session.cleanup` are sent only on a state change; `session.land` once per step.
- **Two decks:** a station pairs with one deck (DL-150 context), so the move rule "the same deck" holds by construction.

## 13. Tests

- `crates/fleetdeck-link/tests/method_table.rs`: the new rows equal the ADR-004 §9.6 rows.
- `fleetdeck-proto`: round trips of every new type; an old-shape `StationInfo` and `SessionSummary` (without the fields) decode; a new-shape one decodes with the fields dropped into a struct of the old shape (a copy of the old struct in the test); each new enum decodes an unknown value as `Unknown`; each new event kind decodes as `EventBody::Unknown` in a fold that does not know it (`event_tests.rs`, `fold_tests.rs`).
- `fleetdeck-link`: each new method is refused with `feature_not_negotiated` on a link without its feature, and with `forbidden` without its scope.
- Deck: a station without `fleet_git.v1` gets no `fleet_git` field on a start, and the board gets 409 `fleet_git_unsupported` for a sync start (`tests/board_fleet_git_settings.rs`).
