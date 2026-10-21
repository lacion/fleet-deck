# Fleet Sync: forge identity, project merge, secret files (FS-1 to FS-3)

Revision 1, 2026-10-21. Owner decisions: DL-257 (this design). Builds on Fleet Git (`fleet-git.md`, FG-1 to FG-12).

## 0. Why

The owner has three projects "Fleet Deck", one for each station (Lighting, dev1, mac-m4). Each has the same forge (`github.com` `lacion/fleet-deck`) and no remote URL. To use one project on all stations, the owner must today:

1. Type the remote URL again, although the forge already names the repository.
2. Use the same URL form on every station: `git@github.com:lacion/fleet-deck.git`, `ssh://git@github.com/lacion/fleet-deck` and `https://github.com/lacion/fleet-deck` are three different `dest` values (§3.8 of `fleet-git.md`), and even `…/fleet-deck` and `…/fleet-deck.git` differ. On Lighting alone, one clone uses ssh and another uses https.
3. Delete two projects and add homes again, because there is no merge.
4. Copy `.env` files by hand, because Fleet Git copies ignored files only into worktrees of one station (FG-4).

Owner decisions (DL-257):

- O1. The forge is the one setting that names the repository. The deck derives the remote URL from it.
- O2. Each home keeps the `origin` URL form that works on its station. The deck compares host and `owner/repo`, not the URL form, for GitHub and GitLab.
- O3. "Merge into project…" joins projects of the same repository into one project, with one home for each station. Each home keeps its path.
- O4. Secret files (`.env` and similar) sync between the homes of a project through the deck relay. The deck never stores or logs the content.
- O5. Newest wins, with the guards of §3.4.
- O6. Reviews: one adversary round for the design, one for the final branch (owner: "limit adversarial and codex reviews to 1 turn").

## 1. FS-1: Forge identity

### 1.1 Canonical `dest` for known forges

`fleetdeck_proto::fleet_git_identity::identity()` gets one new rule. It runs after today's rules and changes only `dest`; `repo` does not change.

A URL is a **forge URL** when all of these are true:

- the host is `github.com` or `gitlab.com` (constant `FORGE_HOSTS`, exact, lower case);
- the form is `https` with port 443 and no login name, or `ssh` (URL or scp-like) with the login name `git` and port 22;
- the path, after the removal of one leading `/~/` or `/`, one trailing `/` and one final `.git`, has two or more segments, and no segment is empty.

For a forge URL: `dest = "https://<host>:443/<path in lower case>"`, for example `https://github.com:443/lacion/fleet-deck`. Reason: GitHub and GitLab resolve `owner/repo` without regard to case, the URL form, the `.git` suffix or `~/` (an ssh login on these hosts has no home folder).

Every other URL keeps today's `dest` (strict, fail closed).

Properties (unit tests):

- `remote_identity(d).dest == d` for every forge `dest` `d` (idempotent).
- `git@github.com:Lacion/Fleet-Deck.git`, `ssh://git@github.com/lacion/fleet-deck`, `ssh://git@github.com:22/~/lacion/fleet-deck.git/`, `https://github.com/lacion/fleet-deck` and `https://GitHub.com/lacion/fleet-deck.git` give one `dest`.
- `alice@github.com:lacion/fleet-deck`, `ssh://git@github.com:2222/lacion/fleet-deck`, `https://github.com:8443/lacion/fleet-deck`, `https://github.com/lacion` and `git@gitlab.example.com:a/b` keep their strict `dest`.

### 1.2 Stored `dest` values

Deck and station compute `dest` with the same function, so both change together. No stored `dest` is rewritten. The owner's deck has no project with a remote and no push lease. On another install, a push, land or move that started before the upgrade can fail once with `remote_mismatch`; the owner starts it again. The deck start repair (§1.3) recomputes `projects.remote_dest`.

### 1.3 The remote URL comes from the forge

`fleetdeck_deck::store::projects_git::forge_remote(forge: &ForgeBinding, transport: CloneTransport) -> String`:

- `Https`: `https://<host>/<repo>.git`
- `Ssh`: `git@<host>:<repo>.git`

`transport` is `deck.toml` `clone_transport` (today's setting for a clone by short name).

When the deck derives:

1. `create_project` and `update_project`: the project has a forge and `remote_url IS NULL`. The deck runs `remote_write(forge_remote(..), LocalRemotes::Deny)` and stores `remote_url`, `remote_ssh_user`, `repo`, `remote_dest` in the same transaction. An error (for example a forge repo that `remote_identity` refuses) stores nothing and the project save still succeeds.
2. Deck start (`projects_legacy::repair`, which runs at each start): the same for every project with a forge and no remote URL. Then, for every project with a remote URL, `remote_dest` and `repo` are recomputed from `clone_url(remote_url, remote_ssh_user)`; a changed value is written and logged (`remote_dest_recomputed`).
3. A later forge change does not change a stored remote URL. The owner changes it in "Advanced" (§4.2) with the existing `PUT /api/projects/{id}/remote` and its rebind rule.

The home check (§3.8 item 3 of `fleet-git.md`) does not change. With §1.1, a home whose `origin` is `https://github.com/lacion/fleet-deck` is `verified` in a project whose remote is `git@github.com:lacion/fleet-deck.git`. A home whose `origin` names another repository stays `mismatch` with today's text.

## 2. FS-2: Merge into project

### 2.1 Route

`POST /api/projects/{id}/merge { from: String, command_id: String }`. `id` is the target project; `from` is the project that ends. Scope `admin`, owner board login only, CSRF, audited `project.merge` (detail: `from <from> homes <n>`), board-API method `projects.merge`, mutating. Closed to members and tokens. The `command_id` is checked and audited, not stored (as `project.remote`): a repeat after success answers 404 `unknown_project` for `from`.

Answer 200: the target `Project` (after the merge). After the commit, the deck sends snapshots to every station of the target (`fleet_git_sync::spawn_project`) and calls `board_changed()`.

### 2.2 Checks, in this order (one transaction)

| # | Check | Answer |
|---|---|---|
| 1 | `id` and `from` exist | 404 `unknown_project` |
| 2 | `id != from` | 400 `invalid_body`, "Choose another project." |
| 3 | Both projects have `remote_dest` (after §1.3 derivation) | 409 `needs_remote`, "Set the forge or the remote URL of both projects first." |
| 4 | `remote_dest` of both projects is equal | 409 `not_same_repo`, "<from name> uses <from repo>, not <repo>." |
| 5 | `from` has sync off | 409 `merge_sync_on`, "Turn sync off in <from name> first." |
| 6 | No station has a home of both projects | 409 `home_exists`, "Both projects have a home on <station name>. Remove one of the homes first." |
| 7 | Every home of `from` is `ready` or `unverified` (has a path) | 409 `home_busy`, "A home of <from name> is not ready." |
| 8 | `from` has no `land_queue` row in `queued` or `running` | 409 `home_busy`, "A land of <from name> is not finished." |
| 9 | The slug rule (§3.2 of `fleet-git.md`) for each station of `from`, in the target's `repo` | 409 `slug_taken` (today's text) |

A live session in a home of `from` does **not** block the merge: `from` has sync off, so its sessions have no Fleet Git state.

### 2.3 Changes (same transaction)

For each home `h` of `from`, in position order:

1. `generation = next_generation(target, h.station_id)` (today's `home_generations` upsert).
2. `INSERT INTO project_homes` for the target: `station_id`, `generation`, `position = max + 1`, `path = h.path`, `canonical_path = h.canonical_path`, `attempt_id = NULL`, `sync_owner = 0`, and:
   - target sync off: `state = 'ready'`, `code = NULL`;
   - target sync on: `state = 'unverified'`, `code = 'rebind_pending'`, message "Waiting for the station to check the new remote." The next snapshot check makes it `ready` and takes the sync ownership (today's code).
3. `UPDATE project_sessions SET project_id = <target> WHERE project_id = <from>`.
4. `UPDATE schedules SET project_id = <target> WHERE project_id = <from>`.
5. `DELETE FROM radar_pairs WHERE project_id = <from>`.
6. `UPDATE home_attempts SET state = 'superseded' WHERE project_id = <from> AND state NOT IN (terminal states)` (none exist after check 7; the update is a guard).
7. `DELETE FROM secret_files WHERE project_id = <from>` (§3; the target keeps its own list).
8. Delete `from` with `projects_legacy::delete_project` (the cascade deletes its `project_homes` rows; `home_generations` of `from` go).
9. `set_copy(target)`.

The target keeps its name, forge, trackers, prompts, Git settings and remote. The settings of `from` end. Ended `land_queue` rows of `from` stay as history.

### 2.4 Station side

No new station code. The snapshot of each station of `from` now lists the home under the target project with a new generation. The station invalidates the old project's rows (today's rule: no file is deleted).

## 3. FS-3: Secret files

### 3.1 Rules

- R1. Opt-in for each project: the owner lists the files. No list, no traffic.
- R2. A secret file is a relative path in the home (for example `.env`, `apps/web/.env.local`). Path rules: 1 to 256 bytes, UTF-8, no NUL, not absolute, no empty segment, no `.` or `..` segment, no `\`. At most 20 paths in a project. No glob.
- R3. The file must be ignored by Git and untracked in that home (`git check-ignore -z --stdin`, the FG-4 rule). Else the station refuses it (`not_ignored`) and never reads or writes it.
- R4. Regular file only, opened without link following at every segment (`openat` `O_NOFOLLOW`, as FG-4). At most 1 MiB for each file.
- R5. The deck holds content only in memory, during one relay. It never writes content to its database, a log, an audit row, an event or an error text. The link transport (mTLS) carries it. The board never receives content.
- R6. Digests: `HMAC-SHA256(key = salt, message = content)`, lower-case hex. `salt` is 32 random bytes for each `(project, path)`, made by the deck at the first sync and stored in `secret_files.salt`. An audit row and the board show only the first 12 hex characters.
- R7. Only the owner can list, sync or see the state. Members and tokens are refused (scope `admin`, owner only).
- R8. The station writes a received file with mode 0600, and keeps the replaced copy in its own data folder (§3.5), never in the repository.

### 3.2 Deck store

New deck migration (the next list position):

```sql
CREATE TABLE secret_files (
    project_id     TEXT    NOT NULL REFERENCES projects (project_id) ON DELETE CASCADE,
    path           TEXT    NOT NULL,
    position       INTEGER NOT NULL,
    salt           BLOB    NOT NULL,          -- 32 bytes (R6)
    last_digest    TEXT,                      -- the baseline: the digest of the last complete sync
    last_source    TEXT,                      -- the station id of the last winner
    last_synced_at INTEGER,
    last_report    TEXT    NOT NULL DEFAULT '{}', -- SecretFileReport JSON of the last run (no content)
    PRIMARY KEY (project_id, path)
) STRICT;
```

`PUT` of the list (§3.6) keeps the rows of paths that stay (salt and baseline stay), inserts new paths with a new salt, and deletes removed paths.

### 3.3 Link methods (deck → station)

New feature `fleet_git.secrets.v1` (station offers it; the deck uses it only when negotiated). Three methods, scope `admin`, not mutating (as `project.ensure_home`), in the link method table. Every request names the home by `(project_id, generation)`; the station finds the path in `station_homes` and refuses another generation (`stale_generation`) or an unknown project (`not_a_home`).

Wire types in new `crates/fleetdeck-proto/src/fleet_git_secrets.rs` (ts export to `fleet_git/`). Every struct has `deny_unknown_fields`.

```rust
pub struct SecretScanParams { pub project_id: String, pub generation: u64, pub files: Vec<SecretFileKey> }
pub struct SecretFileKey { pub path: String, pub salt: String /* 64 hex */ }
pub struct SecretScanAnswer { pub files: Vec<SecretFileState> }
pub struct SecretFileState {
    pub path: String,
    pub state: SecretPresence,             // present, missing, refused (+ Unknown)
    pub code: Option<String>,              // refused: not_ignored, symlink, not_regular, too_large, invalid_path, unreadable
    pub digest: Option<String>,            // present only
    pub size: Option<u64>,                 // present only
    pub mtime_ms: Option<i64>,             // present only
}
pub struct SecretReadParams { pub project_id: String, pub generation: u64, pub path: String, pub salt: String, pub digest: String }
pub struct SecretReadAnswer { pub content_b64: String }
pub struct SecretWriteParams {
    pub project_id: String, pub generation: u64, pub path: String, pub salt: String,
    pub digest: String,                    // of content
    pub content_b64: String,
    pub expect: Option<String>,            // None: the file must be missing; Some(d): the current digest must be d
}
pub struct SecretWriteAnswer { pub backup: bool }
```

Method names: `project.secret_scan`, `project.secret_read`, `project.secret_write`.

Station behaviour:

- `secret_scan`: for each file, R2 to R4, then the digest. A missing parent folder or file is `missing`. `git check-ignore` runs once for all paths (a path that does not exist can still be checked).
- `secret_read`: R2 to R4; the current digest must equal `digest`, else `changed_meanwhile` ("The file changed during the sync."). Answers the content.
- `secret_write`: R2 to R4 for the destination; the HMAC of the decoded content must equal `digest` (`digest_mismatch`); size at most 1 MiB. The current state must equal `expect` (`changed_meanwhile`). Missing parent folders are made with `mkdirat` mode 0700 inside the home (no link following). If a file exists, the station copies it to the backup (§3.5) first. Then it writes `.<name>.fleetdeck-tmp-<16 random hex>` in the same folder (`O_CREAT|O_EXCL|O_NOFOLLOW`, mode 0600), `fsync`, `renameat` over the name, `fsync` of the folder. On any error after the temp file exists, the station removes the temp file.
- The station never logs a content or a digest. It logs the path, the project and the outcome code.

Error answers use the link error object with the code and the text (as the home methods).

### 3.4 The sync run (deck, newest wins)

New module `crates/fleetdeck-deck/src/fleet_git_secrets.rs`. One run for one project at a time (an in-memory lock by project; a second request waits for the first and then runs).

1. Load the project, its ready or unverified homes, and its `secret_files` rows. Stations without a link or without the feature are `offline` for this run.
2. `secret_scan` on each online home, in parallel, 30 s timeout. A failure makes that station `offline` for this run.
3. For each file, the pure function `decide(baseline: Option<&str>, states: &[(StationId, SecretFileState)]) -> Decision` (unit tested, no I/O):
   - Candidates: `present` states. With one or more non-empty candidates, an empty file (size 0) is not a candidate.
   - No candidate: `Decision::Nothing` (code `missing_everywhere`). A delete never spreads.
   - One distinct digest among candidates: that digest wins, no conflict.
   - Several distinct digests: `changed` = candidates whose digest differs from `baseline` (all candidates when there is no baseline). If the changed candidates have one distinct digest, it wins, no conflict. Else the newest `mtime_ms` among the changed candidates wins, `conflict = true`. A tie: the home with the lowest position wins.
   - Source: the winning station (newest `mtime_ms` among stations with the winning digest; tie: lowest position).
   - Targets: every station with `present` and another digest, or `missing`. `refused` and `offline` stations are not targets.
4. For each file with a winner and targets: `secret_read` from the source (30 s), then `secret_write` to each target in position order (30 s each), with `expect` = the target's scanned digest (`None` for missing). The content is dropped from memory at the end of the file.
5. The baseline: when every target was written (or there were no targets), `last_digest = winner`, `last_source`, `last_synced_at = now`. When any target failed or any station was offline, the baseline does **not** change. Reason: a stale copy must never count as "changed" in the next run.
6. `last_report` = the `SecretFileReport` of the file; one audit row `project.secret_sync` for each file that had a write or a conflict (detail: path, source station, conflict, written stations, failed stations with codes, digest prefix of 12 characters).

Triggers:

- `POST /api/projects/{id}/secret-files/sync` (owner, §3.6).
- When a home of the project becomes `ready` (an add, a merge, a check), the deck starts a run for that project (debounced 5 s).
- A deck task every 15 minutes runs every project with at least one secret file and two or more homes.

### 3.5 Backups on the station

Folder: `<station data dir>/secret-backups/<project_id>/<hex of SHA-256(path), first 16>/` (folders mode 0700). File name: `<unix ms>` (mode 0600, `O_EXCL`). After each backup, the station keeps the 5 newest files in that folder and deletes the older ones (only regular files whose name is all digits, in that folder). The backup is the full old content; it never leaves the station.

### 3.6 Board routes

All: scope `admin`, owner board login only; the writes are CSRF-checked and audited.

| Method | Route | Body | Answer |
|---|---|---|---|
| `projects.secret_files` | `GET /api/projects/{id}/secret-files` | — | `SecretFilesView` |
| `projects.secret_files_set` | `PUT /api/projects/{id}/secret-files` | `{ paths: Vec<String>, command_id }` | `SecretFilesView` (audit `project.secret_files`) |
| `projects.secret_sync` | `POST /api/projects/{id}/secret-files/sync` | `{ command_id }` | `SecretSyncReport` (audit per file, §3.4 step 6) |

```rust
pub struct SecretFilesView { pub paths: Vec<String>, pub files: Vec<SecretFileReport> }
pub struct SecretSyncReport { pub files: Vec<SecretFileReport> }
pub struct SecretFileReport {
    pub path: String,
    pub synced_at: Option<i64>,
    pub source: Option<String>,            // station id
    pub conflict: bool,
    pub outcome: SecretOutcome,            // same, copied, partial, nothing, not_run (+ Unknown)
    pub stations: Vec<SecretStationReport>,
}
pub struct SecretStationReport {
    pub station_id: String,
    pub state: SecretStationState,         // same, written, missing, different, refused, offline, failed (+ Unknown)
    pub code: Option<String>,
    pub message: Option<String>,
    pub mtime_ms: Option<i64>,
    pub size: Option<u64>,
}
```

`PUT` refuses a bad path with 400 `invalid_secret_path` (field `paths`, the R2 text "A secret file is a relative path inside the home, without `..`.") and more than 20 paths with 400 `too_many_secret_files`.

### 3.7 Board

Project page, new section "Secret files" (below "Homes"):

- Text: "Files that Git ignores, such as .env, copied between the homes of this project. The newest copy wins; the replaced copy is kept on its station."
- A text area, one path per line, and "Save".
- "Sync now". While it runs: "Syncing…".
- A table: one row per file; one column per home station; each cell shows "Same", "Copied", "Missing", "Different", "Not ignored by Git", "Offline" or "Failed: <text>". Under the row: "Last sync <time> from <station>"; with a conflict: "Two homes changed this file. <station> had the newest copy; the other copies are in the station backups."

Project page, new panel "Same repository" (above "Homes"): shown when other projects have the same `remote_dest`. One row for each: "<name> · <station> · <path>" and the button "Merge into this project". The dialog: title "Merge <from> into <target>?", text "The homes of <from> become homes of <target>, with their folders. Sessions and schedules move to <target>. The settings, prompts, trackers and secret file list of <from> are deleted.", buttons "Merge" and "Cancel". A refusal shows its text in the dialog.

Project form: the field "Remote URL" moves into a closed "Advanced" part, with the hint "Built from the forge. Change it only for a mirror or a self-hosted server."

## 4. Tests (acceptance)

- Proto: §1.1 properties; the wire types round-trip and refuse unknown fields.
- Deck store: derivation on create, update and repair; a forge that the identity refuses saves the project without a remote; repair recomputes a changed `remote_dest`; merge: every check of §2.2 in order, the changes of §2.3, sync on target gives `unverified rebind_pending`, the session and schedule rows move, `from` is gone; `decide()` table tests (one changed copy wins against a newer unchanged copy; two changed copies: newest wins with conflict; an empty file never wins; a missing file is a target; no baseline; tie by position); the baseline does not change after a failed target.
- Deck routes: owner only, CSRF, audit rows, the refusal codes, the method table entries.
- Station: scan, read and write with a temp home (a git repository with `.gitignore`): `not_ignored` for a tracked or not ignored file, `symlink` for a link at the file and at a parent, `too_large`, `changed_meanwhile` for read and write, `digest_mismatch`, backup made and only 5 kept, mode 0600, a missing parent folder made with 0700, `stale_generation`.
- Deck run with the fake station (`tests/common/fleet_git_fake.rs` or a new fake): a three-station sync copies to a missing and a stale station and sets the baseline; one offline station keeps the baseline.

## 5. Build plan

| Task | Worker | Content |
|---|---|---|
| T1 | backend-dev | FS-1 and FS-2 (proto identity rule, derivation, repair, merge store, route, tests) |
| T2 | backend-dev | FS-3 proto, link table, station methods and backups, deck migration, store, run, routes, tests |
| T3 | ux-dev | Board: "Same repository" panel and merge dialog, "Advanced" remote, "Secret files" section |

T1 and T2 run in parallel (their shared files: the route list and the method table; the architect joins them at the merge). T3 starts after T1 and T2 are merged.

## 6. Security notes

- Secret content crosses the deck process in memory only (R5); the adversary checks every log, error and audit path of the three methods.
- The station is the only judge of a path: the deck never names a host path, only `(project_id, generation, relative path)`.
- A compromised deck can already start sessions on every station; the secret methods give it read and write access to listed ignored files in homes. They do not widen what an owner-level deck can do.
- Forge canonicalization joins only URL forms that GitHub and GitLab resolve to one repository; every other host keeps the strict rule.
