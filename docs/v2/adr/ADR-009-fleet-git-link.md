# ADR-009 — Fleet Git: link protocol additions

- **Status:** Proposed (2026-10-17, revision 3: the answers to the adversary DESIGN REVIEW round 1, DL-225). Needs the adversary RE-REVIEW together with `docs/v2/design/fleet-git.md` and ADR-010 before the status changes.
- **Revision 2 changes:** versioned full snapshots (`GitSnapshot.revision`, home generations, `HomeCheck`); `FleetGitStart` gets `kind`, `settings_revision`, `home_generation`, `home_path`, `remote`, `slug`, `forge: Option<ForgeBinding>` (was `bool`) and `artifact_share_state`; `project.ensure_home` is idempotent by `attempt_id` and `request_digest`, with `generation` and `retry_of`; `session.land` carries a pinned `LandPlan` and a stable deck `command_id` (outbox); new method `fleet_git.land_abandon`; `session.land` and `session.push` events carry the attempt evidence; push leases are scoped to `(target_dest, ref)` by `RemoteIdentity.dest`; new error codes; unknown enum values fail closed.
- **Revision 3 changes (DL-225):** `RemoteIdentity.dest` keeps the SSH login name and the path form, and `file://` scopes exist only with `allow_local_remotes` (B2, M13); `FleetGitStart` loses `artifact_share_state` and gets `clone_url`, `GitProjectView` gets `clone_url`, `FleetGitInfo` gets `allow_local_remotes`; `HomeCheckState` gets `no_remote` and `refused`; `project.ensure_home` gets `remote_name` (M7); `session.land` and `MoveOut`/`MoveIn` carry `CommandEnvelope.authority` and the deck re-checks it through the workflows (B3); `LandPlan` gets `publish_ref` (M9); the `session.land` event gets `reconciling` and `LandPhase` loses `reconciling` (M1); `fleet_git.land_abandon` is a fence with the answer `fenced` (M3); `session.move_export` returns a `MovePublication`, `session.move_in` carries it, and the new `session.move_release` ends the target hold (B4, M8); new event `session.move_in`; new methods `fleet_git.artifact_authorize` (station → deck) and `fleet_git.share_verdict` (deck → station) with `ArtifactAuthorization` (B6); `TaildriveReport.inventory_revision`, `TaildriveShare.exposure_state`, `candidates`, `path_state = ambiguous` (B5, M10); `FileRoot::Artifacts(SessionId)` is a newtype variant with the JSON `{"artifacts":"<id>"}` (M11); new event `artifacts.revoked`; new error codes.
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
| `fleet_git.homes.v1` | 2 | `project.ensure_home`, `project.home_status`, `fleet_git.artifact_authorize`, `fleet_git.share_verdict`, `FileRoot::Artifacts`, the artifact events |
| `fleet_git.land.v1` | 3 | `project.radar`, `session.land`, `fleet_git.land_status`, `fleet_git.land_abandon`, the `session.land` event |
| `fleet_git.move.v1` | 4 | `session.move_out`, `session.move_export`, `session.move_prepare`, `session.move_in`, `session.move_finish`, `session.move_release`, `session.move_abort`, `session.move_discard`, `session.move_status`, the stream purposes, the `session.moved` and `session.move_in` events |
| `fleet_git.branches.v1` | 4 | `project.branches`, `project.fetch` |

The coordinator additions (§7) need `coordinator.v2` **and** the matching Fleet Git feature.

## 4. New methods

All are deck → station, except `fleet_git.artifact_authorize` (station → deck, revision 3, in the same way as `coordinator.authority_check` of ADR-008). The rows are added to "Method table v1 (Revision 2)" in ADR-004 §9.6 (the table test reads that table) and to `crates/fleetdeck-link/src/methods_table.rs`.

| Method | Scope | Feature | Mutating | Params → answer |
| --- | --- | --- | --- | --- |
| `fleet_git.projects_set` | admin | `fleet_git.v1` | no | `GitSnapshot { revision, projects: [GitProjectView], homes: [StationHome] }` → `SnapshotAck { stored, revision, checks: [HomeCheck] }` |
| `fleet_git.push_now` | drive | `fleet_git.v1` | no | `{ session_id }` → `{ state }` (error `held` while a land or a move holds the session) |
| `project.ensure_home` | admin | `fleet_git.homes.v1` | no | `{ project_id, project_name, folder, url, remote: RemoteIdentity, remote_name, generation, attempt_id, request_digest, retry_of? }` (the deck resends the stored request unchanged until the first answer) → `HomeStatus { attempt_id, generation, state, path?, reserved_path, code?, message? }` |
| `project.home_status` | read | `fleet_git.homes.v1` | no | `{ project_id, attempt_id }` → `HomeStatus` |
| `fleet_git.artifact_authorize` (station → deck) | read | `fleet_git.homes.v1` | no | `{ project_id, session_id, share }` → `ArtifactAuthorization`, or the error `share_unverified` (deadline 10 s) |
| `fleet_git.share_verdict` | read | `fleet_git.homes.v1` | no | `{ share_name }` → `{ inventory_revision, checked_at, path_state, exposure_state, exposed }` from a report at most 60 s old (deadline 5 s) |
| `project.radar` | read | `fleet_git.land.v1` | no | `{ project_id, branches: [RadarBranch] }` → `RadarResult` |
| `session.land` | drive | `fleet_git.land.v1` | yes | `SessionCommand::Land { session_id, land_id, plan: LandPlan }` with the deck's stable `command_id` and, for a coordinator land, `CommandEnvelope.authority` → receipt (`land_fenced` for a fenced id) |
| `fleet_git.land_status` | read | `fleet_git.land.v1` | no | `{ land_id }` → `LandStatus { state, phase, reconciling, attempt, source_oid?, base_oid?, rebased_oid?, branch_published_oid?, default_published_oid?, pr_url?, files, code?, text? }`, `{ state: "fenced" }` for a fenced id, or `{ state: "unknown" }` for an unknown id |
| `fleet_git.land_abandon` | drive | `fleet_git.land.v1` | no | `{ land_id }` → `{ state: "fenced" }`. The station records `land_fences (land_id)` (also for an unknown id), ends a `running` run `unresolved` (`abandoned`), releases its hold and drops its queued writer jobs, in one transaction; idempotent. The deck frees the queue only on this answer (design §11.2) |
| `session.move_out` | admin | `fleet_git.move.v1` | yes | `SessionCommand::MoveOut { session_id, move_id, to_station }` (with `CommandEnvelope.authority` for a coordinator move) → receipt |
| `session.move_export` | admin | `fleet_git.move.v1` | no | `{ move_id }` → `{ publication: MovePublication, manifest?: TranscriptManifest, stream_token? }` (no manifest for `branch_only`) |
| `session.move_prepare` | admin | `fleet_git.move.v1` | no | `{ move_id, manifest, source }` → `{ stream_token }` |
| `session.move_in` | admin | `fleet_git.move.v1` | yes | `SessionCommand::MoveIn { move_id, kind, provider_instance, title, callsign_hint, from: { station_id, session_id, publication: MovePublication }, fleet_git }` (with `CommandEnvelope.authority` for a coordinator move) → receipt |
| `session.move_finish` | admin | `fleet_git.move.v1` | yes | `SessionCommand::MoveFinish { session_id, move_id, to_station, to_session }` (the deck's fixed `finish_command_id`, resent until accepted) → receipt |
| `session.move_release` | admin | `fleet_git.move.v1` | no | `{ move_id }` → `{ state: "released" }` (the target deletes the `move_in` hold; idempotent) |
| `session.move_abort` | admin | `fleet_git.move.v1` | no | `{ move_id }` → `{}` (idempotent: an unknown or closed id is ok) |
| `session.move_discard` | admin | `fleet_git.move.v1` | no | `{ move_id }` → `{ state: "discarded" | "abandoned" | "resumed" }` (by `move_in_runs.state`: before `launching` the station deletes the staged and imported files by identity; from `launching` on it ends the process and deletes no file; after `resumed` it changes nothing; idempotent) |
| `session.move_status` | read | `fleet_git.move.v1` | no | `{ move_id }` → `{ state: MoveInState | "holding" | "finished" | "aborted" | "unknown", session_id?, code?, text? }` |
| `project.branches` | read | `fleet_git.branches.v1` | no | `{ project_id }` → `RemoteBranches` |
| `project.fetch` | read | `fleet_git.branches.v1` | no | `{ project_id }` → `{ fetched_at, code?, text? }` |

Why the scopes: `admin` for the methods that run Git network commands that create folders or write the station's project table, or that carry transcripts; `drive` for a session action (land, push now); `read` for reads and for status calls. `project.fetch` changes only remote-tracking refs and is `read`, as the spec gives it the `inspect` class (O32).

Why mutating or not: the four `SessionCommand` methods change session state that a retry must not repeat, so they have a `command_id`, an envelope and a receipt (ADR-004 §9.3). For `session.land` the deck stores the `command_id` and the payload in its outbox at admission and resends the same pair, so a lost answer never makes a second land (design §11.2). Every other method is a read, a versioned whole-state replace (`projects_set`), or an idempotent call keyed by an id that the deck chose:

- `fleet_git.projects_set`: the station keeps the newest `revision`. A lower revision, or an equal revision with another digest, is refused with `stored: false` and changes nothing; the same revision and digest answers `stored: true`. The deck computes `revision = max(previous + 1, now_ms)` per station, so it grows also after a restore of a deck backup.
- `project.ensure_home`: keyed by `attempt_id`; the same `request_digest` returns the stored result (also a failure); another digest gives `attempt_reused`. A new attempt for the same `(project_id, generation)` needs `retry_of`. A lower `generation` than the station knows gives `stale_generation`. The deck applies a `HomeStatus` only when `(attempt_id, generation)` equal its home row.
- `fleet_git.land_abandon`, `session.move_abort`, `session.move_discard`, `session.move_release`: an unknown or closed id is ok; the land fence is recorded also for an unknown id.
- `fleet_git.artifact_authorize` and `fleet_git.share_verdict`: reads; each call gives a new verdict and a new authorization, never a stored one.

Error codes (new wire strings in `fleetdeck_proto::fleet_git::code`): `not_synced`, `held`, `branch_namespace_blocked`, `git_too_old`, `slug_mismatch`, `stale_settings`, `stale_generation`, `not_a_home`, `unsupported_mode`, `fetch_failed`, `fetch_timeout`, `push_failed`, `push_rejected`, `push_auth`, `push_timeout`, `remote_mismatch`, `remote_rewritten`, `remote_ambiguous`, `home_conflict`, `clone_failed`, `clone_root_missing`, `clone_interrupted`, `incomplete_folder`, `destination_reserved`, `attempt_reused`, `retry_refused`, `land_not_ready`, `land_open`, `land_pr_needs_forge_branch`, `nothing_to_land`, `check_disabled`, `check_mutated`, `branch_moved`, `pr_failed`, `pr_ambiguous`, `land_no_identity`, `land_temp_unsafe`, `station_restarted`, `share_exposed`, `share_unverified`, `move_not_ready`, `move_too_large`, `move_checksum`, `move_branch_moved`, `move_resume_failed`, and in revision 3 `no_remote`, `remote_has_credentials`, `remote_local_refused`, `remote_path_invalid`, `remote_host_invalid`, `home_not_ready`, `destination_cleared`, `land_fenced`, `authority_expired`, `move_publication_mismatch`, `move_branch_exists`, `move_ref_exhausted`, `move_resume_timeout`, `identity_changed`, `worktree_changed`, `artifacts_unavailable`, plus the existing `authority_lost`, `authority_unknown`, `invalid_params`, `unknown_session`, `invalid_state`, `forbidden`, `busy`, `internal`.

**How an older peer behaves:**

- An old station does not offer the features, so the deck never calls these methods on it (rule 1) and answers the board 409 `fleet_git_unsupported` (rule 7). A project with a home on an old station can still start sessions there without sync (the deck sends no `fleet_git` field).
- An old deck never calls them. A new station without calls from the deck has no `git_projects` rows, so it never fetches, pushes, cleans up or lands (O1).

## 5. New fields on existing types

| Type | Field | Shape | Older peer |
| --- | --- | --- | --- |
| `SessionCommand::Start` | `fleet_git` | `Option<FleetGitStart { kind, project_id, project_name, settings, settings_revision, home_generation, home_path, remote: RemoteIdentity { repo, dest }, slug, forge: Option<ForgeBinding>, clone_url }>` (revision 3: no `artifact_share_state`), default none, skipped when none | The deck sends it only with `fleet_git.v1`. `SessionCommand` does not refuse unknown fields, so an old station would ignore it and start without sync; rule 7 prevents that case. An unknown `kind` is refused (`unsupported_mode`). |
| `StartSessions` (deck HTTP body) | `home`, `requires`, `prefer`, `spread` | optional | Board → deck only; not a link type. |
| `StationInfo` | `traits` | `Option<StationTraits { os, arch, on_battery: Option<bool>, labels }>` | An old deck ignores it (rule 2); a new deck treats a missing value as "no traits" (no requirement matches, no preference counts). |
| `StationInfo` | `config` | `Option<ConfigDigest>` | Missing: the board shows "Config: Unknown". |
| `StationInfo` | `taildrive` | `Option<TaildriveReport>` | Missing: the board shows no Taildrive section. |
| `StationInfo` | `fleet_git` | `Option<FleetGitInfo { clone_root, allow_check_cmd, git_version, allow_local_remotes }>` (`allow_local_remotes` new in revision 3, default false) | Missing: the board hides the land check line and "Add home" for that station. |
| `SessionSummary` | `base_stale` | `bool`, default false, skipped when false | An old deck has no field; its fold ignores the new event. |
| `SessionSummary` | `push` | `Option<PushInfo { state, ref_name, oid, desired_oid, stop_code, attempts, next_attempt_at, code, text }>` | As above. |
| `SessionSummary` | `cleanup` | `Option<CleanupInfo { state, reason }>` | As above. |
| `SessionSummary` | `land` | `Option<LandInfo { land_id, attempt, state, phase, reconciling, code, text, files, source_oid, base_oid, rebased_oid, branch_published_oid, default_published_oid, pr_url }>` | As above. |
| `SessionSummary` | `moved_to` | `Option<MovedTo { move_id, station_id, session_id }>` | An old deck sees an `exited` session (§8). |
| `SessionSummary` | `move_in` (revision 3) | `Option<MoveInInfo { move_id, state: MoveInState, code, text }>` | As above. |
| `SessionSummary` | `artifacts_dir` | `Option<ArtifactsDir { path?, url? }>` | As above. |
| `FileRoot` (files.v1 queries) | new newtype variant `Artifacts(SessionId)`, JSON `{ "artifacts": "<session id>" }` (revision 3, the same form as `{ "session": … }`) | an enum variant | Sent only with `fleet_git.homes.v1`. An old station refuses the unknown root (`invalid_query`), so it fails closed. A member may use it only for a session shared with the member (design §15.6). |
| `StationInfo.taildrive` | `TaildriveReport.inventory_revision`, `TaildriveShare.{ candidates, exposure_state, unknown_reason }`, `SharePathState::Ambiguous` (revision 3) | defaults | An old deck ignores them; a new deck treats a missing `exposure_state` as `unknown` (fail closed). |
| `Project` (deck HTTP, board) | `homes` (with `generation`, `position`, `state`, `path?`, `reserved_path?`, `attempt_id?`, `sync_owner`), `repo`, `remote_url`, `remote_ssh_user` (revision 3), `remote_dest`, `git`, `git_revision`, `slugs` | defaults | Not a link type. `station_id` and `path` stay (a copy of the position-0 home, written by one adapter), so an old board reads a project as before. |
| `Status` (`tailscale status --json`, parsed) | `CurrentTailnet.Name` | optional | Not a link type. |

`StationInfo` and `SessionSummary` are the only existing link types that get fields. Both already use optional fields for the same purpose (`resources`, `desktop`, `issue`, `batch`).

## 6. New events

All are made by the station (not by a provider). Each has a new `kind`. An old deck keeps it as `EventBody::Unknown`, stores it, replays it to boards, and its fold ignores it (rule 3). An old board shows nothing for an unknown kind.

| Kind | Payload | Fold (new deck and station) |
| --- | --- | --- |
| `workspace.base_stale` | `{ code, text }` | `summary.base_stale = true` |
| `workspace.include_skipped` | `{ files: [{ path, reason }] }` (≤ 50) | none |
| `session.push` | `{ state, ref_name, oid?, desired_oid?, stop_code?, attempts, next_attempt_at?, code?, text? }` | `summary.push` |
| `session.cleanup` | `{ state, reason? }` | `summary.cleanup` |
| `artifacts.ready` | `{ path?, url? }` | `summary.artifacts_dir` |
| `artifacts.unavailable` | `{ code, text }` | none |
| `artifacts.revoked` (revision 3) | `{ code, text }` | `summary.artifacts_dir = None` |
| `session.move_in` (revision 3, on the target session) | `{ move_id, state, code?, text? }` | `summary.move_in` |
| `session.land` | `{ land_id, attempt, state, phase, reconciling, code?, text?, files, source_oid?, base_oid?, rebased_oid?, branch_published_oid?, default_published_oid?, pr_url? }` | `summary.land`; the deck also updates `land_queue` |
| `session.moved` | `{ move_id, to_station, to_session }` | `summary.moved_to` |

The enums inside them (`PushState`, `CleanupState`, `LandState`, `LandPhase`, `StartKind`, `HomeState`, `HomeCheckState`, `ShareVerdict`, `ExposureState`, `SharePathState`, `MoveInState`, and the settings enums `PushMode`, `CleanupMode`, `LandMode`) each have `#[serde(other)] Unknown`. **Unknown fails closed:** an unknown push or cleanup mode does nothing, an unknown land mode or start kind is refused (`unsupported_mode`), and an unknown `LandState` keeps the deck's land `running` and its queue occupied (design §3.1, §11.2).

## 6A. Shapes that carry authority (revision 2)

These shapes travel inside the methods above. They are listed here because a peer must not read them loosely.

- `RemoteIdentity { repo: String, dest: String }`: `repo` is the O16 `host/path` (grouping, slug and land-lock key; aliases may share it); `dest` is the write authority and the push lease key: `https://<host>:<port><path>`, `ssh://[<login>@]<host>:<port><path>` with the SSH login name and the path form kept (`/p` absolute, `/~/p` relative to the login folder), or, only with `allow_local_remotes`, `file://<canonical path>` (revision 3). Rules: design §3.8.
- `MovePublication { local_branch, dest, ref_name, oid }` (revision 3): captured by the source under its hold; the target checks `dest` against its own branch target and fetches `ref_name` from its own verified URL; it never uses a URL from the source.
- `ArtifactAuthorization { share, exporting_station?, inventory_revision?, verdict, exposed, checked_at?, issued_at, expires_at }` (revision 3): `expires_at = issued_at + 10 min`; only `ok` and `not_a_station` authorize; a station asks for a new one before each start, resume and artifact access and every 5 minutes for a live session.
- `GitProjectView { project_id, project_name, settings_revision, settings, remote, forge, home_generation, home_path, radar_home }` and `StationHome { project_id, generation, path, sync }`. A project absent from `projects` is removed on the station; a home absent from `homes`, or with a lower generation, is invalidated (no file is deleted).
- `HomeCheck { project_id, generation, state, canonical_path?, seen?: RemoteIdentity, code? }`: the station's verification of a home (design §3.8). Only `verified` gives write authority.
- `LandPlan { project_id, settings_revision, home_generation, mode, fetch_remote, remote, default_branch?, branch_ref, publish_ref, branch_target: PushTarget { target, dest }, forge?, check_cmd?, check_env, pr_title, pr_body }`: pinned by the deck at admission. `branch_ref` is the local branch; `publish_ref` (revision 3) is the pushed ref and the PR head. The station refuses a plan whose `branch_ref` is not `session_git.branch`, whose `publish_ref` is not `session_git.publish_ref` under `fd/<stored slug>/`, whose generation is not its own, whose mode is unknown, or whose `pr` mode has a branch target that is not the forge repository. The station adds the commits (`source_oid`, `base_oid`) in its first phase and never changes them.

`event.rs` is at 788 lines; the build first moves `EventBody` into `event_body.rs` (design §19, T1.0). This is a code move with no wire change.

## 7. Coordinator additions (ADR-006, ADR-008)

| Wire | Kind | Class | Needs |
| --- | --- | --- | --- |
| `CoordinatorQuery::ProjectBranches { project_id }` | query (no receipt) | `inspect` | `coordinator.v2` + `fleet_git.branches.v1` on the links of the caller's station and of the radar home |
| `CoordinatorQuery::ProjectFetch { project_id }` | query | `inspect` | as above |
| `CoordinatorOp::Land { station, session_id }` | op (receipt, outbox) | `manage` | `coordinator.v2` + `fleet_git.land.v1` |
| `CoordinatorOp::Move { station, session_id, to_station }` | op | `manage` | `coordinator.v2` + `fleet_git.move.v1` on both stations |

MCP tools: `project_branches`, `project_fetch`, `land_session`, `move_session` (design §13.2). **Revision 3 (B3):** the two ops start durable workflows, so the deck stores their `CommandAuthority` and an expiry of 10 minutes in `land_queue` and `session_moves`, re-checks it at admission and at each send and resend (`store/coordinator.rs::recheck`), and the stations re-check it at dispatch, before each publication and before the transcript transfer (`coordinator.authority_check`); the deck's `authority_check` handler finds a land by `land_queue.command_id`. A move is not re-checked after its commit point. Why additive: ADR-008 §4.1 already says that each new query and op variant is accepted only on a `coordinator.v2` link; the station forwards a Fleet Git variant only when the deck link also negotiated the Fleet Git feature, else the tool answers `unsupported` and nothing is sent. An old deck therefore never decodes an unknown variant. The tool catalog is fixed at process start (ADR-006 §5.3): the tools are listed when the grant has the class, whatever the features. Answers fence Git text (commit subjects) with the untrusted envelope (ADR-008 §4.3).

## 8. Why there is no new session state

O29 asks for a `moved` state. `SessionState` has no `#[serde(other)]`, so a new value `moved` would make an old deck fail to decode the summary and every snapshot of the station. The design keeps the state `exited` and adds `summary.moved_to`. A new board shows "Moved to <station>"; an old board shows an ended session. The station refuses input to a moved session in its own decision, whatever the deck version.

## 9. Data channel use (ADR-004 §2.7, §9.2)

Two new stream purposes, both authorized by a stream token from a control answer (single use, bound to the `attempt_id`, 30 s):

- `move-export:<move_id>`: the station → deck stream of `session.move_export`. The station writes the manifest files back to back, in manifest order, and closes. It never writes more than `manifest.total` bytes.
- `move-import:<move_id>`: the deck → station stream of `session.move_prepare`. The deck writes the same bytes and closes. The station resets the stream with `move_too_large` after `manifest.total` bytes and checks each file's SHA-256.

The budgets of ADR-004 §9.2 apply unchanged (64 streams, 256 KiB window, 4 MiB buffered). The deck pipes one stream into the other and holds no copy on disk. Why additive: a stream purpose is chosen by the station that issued the token, and an old station never issues one of these tokens (it does not have the methods).

## 10. Deck HTTP routes (not link changes; listed for the review)

Owner login only unless noted; CSRF on every POST, PUT and DELETE; closed to members unless noted; each mutating route is audited.

- `PUT /api/projects/{id}/git` (`project.git_settings`), `PUT /api/projects/{id}/remote` (`project.remote`, with `rebind`), `POST /api/projects/{id}/homes` (`project.home_add`), `POST /api/projects/{id}/homes/{station}/retry` (`project.home_retry`), `DELETE /api/projects/{id}/homes/{station}` (`project.home_remove`).
- `POST /api/sessions/{station}/{session}/push` (scope `drive`, a `drive` token allowed; `session.push_now`).
- `POST /api/placement` (scope `drive`; no audit).
- `GET /api/radar` (scope `read`).
- `POST /api/sessions/{station}/{session}/land` (`session.land`), `GET /api/projects/{id}/lands` (`read`), `POST /api/lands/{land_id}/cancel` (`land.cancel`), `POST /api/lands/{land_id}/abandon` (`land.abandon`), `POST /api/lands/{land_id}/forget` (`land.forget`, revision 3, only for a removed station), `POST /api/lands/{land_id}/send` (`land.send_to_session`).
- `GET /api/stations/{station}/files/list|read|search?artifacts=<session id>` (revision 3: the existing file routes gain the `artifacts` root; a member needs that exact shared session; scope as for the other roots).
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

- **A peer that loses the feature mid-flow.** A station downgrade during a land: the old build has no land state, so the deck cannot learn the outcome. The land stays `running` in phase `lost` and keeps the queue until the owner presses "Stop checking" (design §11.2). A move is aborted (`failed`) as before. A deck downgrade: the station sees a link without `fleet_git.v1` and pauses its push and cleanup loops until a deck with the feature sends a snapshot (design §3.3).
- **A delayed or replayed frame.** `projects_set` is fenced by `revision`, `ensure_home` and `home_status` by `(attempt_id, generation)`, and a start by `settings_revision` and `home_generation`; an old frame changes nothing.
- **Event volume:** `session.push` and `session.cleanup` are sent only on a state change; `session.land` once per step.
- **Two decks:** a station pairs with one deck (DL-150 context), so the move rule "the same deck" holds by construction.
- **Revision 3:** a station that is removed during an uncertain land cannot acknowledge the fence; the owner's `forget` frees the queue with a warning (design §11.2). An `artifacts.revoked` event cannot take back the environment of a running agent (design §18).

## 13. Tests

- `crates/fleetdeck-link/tests/method_table.rs`: the new rows equal the ADR-004 §9.6 rows.
- `fleetdeck-proto`: round trips of every new type; an old-shape `StationInfo` and `SessionSummary` (without the fields) decode; a new-shape one decodes with the fields dropped into a struct of the old shape (a copy of the old struct in the test); each new enum decodes an unknown value as `Unknown`; each new event kind decodes as `EventBody::Unknown` in a fold that does not know it (`event_tests.rs`, `fold_tests.rs`).
- `fleetdeck-link`: each new method is refused with `feature_not_negotiated` on a link without its feature, and with `forbidden` without its scope.
- Deck: a station without `fleet_git.v1` gets no `fleet_git` field on a start, and the board gets 409 `fleet_git_unsupported` for a sync start (`tests/board_fleet_git_settings.rs`).
- Station (revision 2): a lower `projects_set` revision and an equal revision with another digest answer `stored: false` and change nothing; `ensure_home` with a known `attempt_id` and the same digest returns the stored result, with another digest `attempt_reused`; a resent `session.land` with the same `command_id` returns the stored receipt and starts no second run; an unknown `PushMode`, `LandMode` and `StartKind` fail closed.
- Revision 3: `fleet_git.land_abandon` records the fence also for an unknown id and a later `session.land` with that id is rejected `land_fenced`; `session.move_discard` answers by state and never deletes a file from `launching` on; `session.move_release` is idempotent; `FileRoot::Artifacts("s1")` serializes as `{"artifacts":"s1"}`; `fleet_git.artifact_authorize` is refused for a share that is not the project's `artifact_share`; an old-shape `TaildriveShare` without `exposure_state` decodes as `unknown`; `HomeCheckState` decodes `no_remote` and `refused`, and an older value set decodes a new value as `Unknown`.
