# Fleet Git: one repository across the fleet (FG-1 to FG-12)

- **Status:** Draft, revision 1 (2026-10-17). High risk: pushes to remotes, worktree deletion, deck migrations, transfer of transcripts, a public link API. The adversary DESIGN REVIEW runs before the status changes to "Accepted" (owner spec, phase 0).
- **Owner decisions:** `/home/luismorales/fleetdeck-briefs/fleet-git-owner-spec.md`, O1 to O39. They are binding. This design does not change them. §20 lists every choice that this draft makes where the spec is silent.
- **Related:** ADR-004 (link protocol), ADR-006 (coordinator MCP), ADR-008 (fleet control), ADR-009 (the link additions of this design, new), ADR-010 (session move, written in parallel with this draft; §12 refers to it by name), DL-142/DL-143 (worktree inventory), DL-144 (branch choice, preflight), DL-148 (setup command), DL-150 (worktree trash), DL-163 (honest feedback), DL-164 (integrations), DL-166 (member deny list), DL-176 (projects organize sessions), DL-193 (resources), DL-200 (provisioning, Landlock), DL-201 "Repo catalog fields and repo grouping (M3 6-1)" (the `[repo_groups]` group; the decisions log has two entries with the number DL-201, and the spec means this one), DL-306 (conflict radar), DL-145 (conflict mail).
- **Branch:** `fleet-git` (from `v2` at `3d1c0cd3`).

## 1. Facts in the code today

These facts were read on `fleet-git` at `3d1c0cd3`. The design depends on them.

### 1.1 Projects (deck)

- `crates/fleetdeck-deck/src/store/projects.rs` (282 lines): table `projects (project_id, name, station_id, path, forge, trackers, prompt_template, review_prompt, created_at)` (deck migration V18, `store/schema.rs:467`). One project has exactly one station and one path. `create_project`, `update_project`, `delete_project` call `board_changed()`.
- `crates/fleetdeck-proto/src/integrations/projects.rs`: `Project`, `ProjectInput` (`deny_unknown_fields`), `StartSessions`.
- `crates/fleetdeck-deck/src/store/project_sessions.rs`: `project_of(projects, assignments, card)` (DL-176): a manual row, else the path rule among the projects **of the session's station**, else the origin rule.
- Routes: `http/projects_routes.rs` (673 lines), `http/project_writes.rs` (631 lines, the batch start `POST /api/projects/{id}/sessions`), `http/project_session_routes.rs`.
- The deck migration list (`store/schema.rs::migrations`, 773 lines) has 24 entries; the last is the display tables (R13). A new migration is the next list position. The architect orders the positions at the merge.

### 1.2 Session start on the station

- The decision (`manager/decide.rs`, 798 lines) checks the branch choice (`provision::check_branch_choice`), picks the callsign (`new_callsign`, `provision::callsign_candidates`) and builds the plan with `provision::worktree_plan` (`workspace/provision.rs:160`). Today the default branch of a worktree is `fd/<callsign>` (`provision.rs:192`); the folder is `<parent>/<repo>--fd-<callsign>`.
- The plan travels in `EffectPayload::Start.workspace: Option<WorkspacePlan>` (`store/payload.rs:62`). `WorkspacePlan::Worktree { repo, path, branch, base }` and `WorkspacePlan::Clone { url, path }`.
- The Git work runs in the start task (`manager/setup_task.rs::provision_plan`, 652 lines) through `provision::provision_cancellable`. Each Git call runs in its own process group and, on Linux, under `fleetdeck confine-exec` with Landlock (DL-200). The new branch starts at `base` or `HEAD` (`provision.rs:348`).
- The base commit of a session comes from `workspace/base.rs::planned` (`manager/target_launch.rs:240`, `:259`) and reaches the summary as `session.base`.
- `SessionSummary.branch` exists: "the git branch of the workspace when the session started" (`fleetdeck-proto/src/session.rs:63`). `SessionSummary.repo: Option<RepoInfo { root, origin }>` exists (DL-306).
- Clone reuse (`workspace/clone_reuse.rs`): a clone start into an existing folder reuses it when `[remote "origin"] url` normalizes to the same `host/path`. It reads `.git/config` and runs no Git.

### 1.3 The hardened Git runner

`crates/fleetdeck-station/src/workspace/git.rs` (602 lines):

- `REDIRECTING_VARS` (15 names: `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`, ..., `GIT_EXTERNAL_DIFF`, `GIT_DIFF_OPTS`) are removed from every child.
- `SAFE_CONFIG`: `core.hooksPath=/dev/null`, `core.fsmonitor=false`, `core.quotePath=false`, `gc.auto=0`, `maintenance.auto=false`, `diff.external=`. So no Git hook runs for any station Git command today. This includes `pre-push`, `post-rewrite` and `reference-transaction`.
- Every call has `GIT_TERMINAL_PROMPT=0`, `GIT_OPTIONAL_LOCKS=0`, `LC_ALL=C` and a timeout (`GitOptions.timeout`). `Git::status` keeps the last 2048 bytes of stderr (`STDERR_KEEP`). `Git::bounded_with_env` caps stdout.
- `GitOptions.isolated = false` keeps the user configuration, so credential helpers and the SSH agent work (the clone path, DL-144 preflight).
- `GitOptions.confine: Option<Confine>` runs Git under `fleetdeck confine-exec --write-root <root>...` (B9, DL-200).
- `workspace/branches.rs`: `HARDENED` options and `HARDENED_ENV` (`GIT_NO_LAZY_FETCH=1`); the branch view needs Git 2.45 (`git_too_old`).

### 1.4 The worktree trash and the inventory

- DL-150: `manager/trash.rs::decide` accepts only `CommandOrigin::Board` ("only the owner's board changes worktrees"). The trash checks (ended, no run or turn, no terminal hold, no unsettled effect, no open row, no revert, no other session, no shell, no pending shell open, inside the roots, a real folder, a `.git` file) are in the private function `trash()`. The file work is `workspace/trash.rs::run` (identity checks, `.fleetdeck-trash/<name>--<12 chars of the op id>`, `git worktree repair`). Branches are never deleted.
- DL-143: `workspace/worktree_scan.rs` decides "clean" without `git status`. `WorktreeInfo.unpushed` counts commits on no fetched remote ref (`fleetdeck-proto/src/link.rs:485`).

### 1.5 The conflict radar and the Branches view

- DL-306: the deck folds file touches into `file_touches (group_key, file, repo, station_id, session_id, session_title, path, touched_at)` (`store/conflicts.rs`). `GET /api/conflicts` lists files that two or more sessions touched in 30 minutes. The group is `origin:<origin>` across stations, else `root:<station>:<root>`. The board panel is `web/src/board/ConflictRadar.tsx` (90 lines); its row label is "Conflict".
- DL-201 (repo grouping): `[repo_groups]` in `station.toml` maps a repository path to a group name (`StationConfig.repo_groups`, `fleetdeck-core/src/config.rs:257`).
- DL-191: link method `session.branches` (feature `session_branches.v1`) answers `RepoBranches { head, branch, branches: [RepoBranch { name, head, ahead, behind, at, subject }], truncated }` for the local branches. The board view is `web/src/session/BranchesView.tsx` (256 lines).

### 1.6 Link, features, station info, routing

- Method table: `crates/fleetdeck-link/src/methods.rs` (769 lines), `METHODS: &[MethodSpec]`. `tests/method_table.rs` compares it with the table "Method table v1 (Revision 2)" in ADR-004 §9.6.
- Features: `crates/fleetdeck-proto/src/version.rs` (`feature::ALL`, 33 features, the last is `desktop.v1`).
- Events: `fleetdeck-proto/src/event.rs` (788 lines). An unknown event kind is kept as `EventBody::Unknown` and the fold changes only the sequence and the time (L16). `SessionState` (`Starting`, `Running`, `Stopping`, `Exited`, `Lost`) has **no** `#[serde(other)]`: a new state value is not additive.
- `StationInfo` (`fleetdeck-proto/src/link.rs:244`) has optional fields (`resources`, `desktop`, ...). `providers[].version` already carries the probed `claude` and `codex` versions.
- `station.info` is asked again every 60 s while the link is live (DL-193).
- `crates/fleetdeck-deck/src/routing.rs` (155 lines): `pick_session` and `spawn_candidates` for `assign auto` (free places, then the name).
- The data channel (ADR-004 §2.7, §9.2): 64 streams, 256 KiB window, stream tokens of 30 s.

### 1.7 Coordinator, Tailscale, config

- ADR-008 classes `inspect`, `manage`, `answer`. The station tool list is `crates/fleetdeck-station/src/coordinator/fleet_tools.rs` (279 lines). New queries and ops are `CoordinatorQuery` and `CoordinatorOp` variants in `fleetdeck-proto/src/mutation.rs` (654 lines).
- `crates/fleetdeck-core/src/tailscale.rs` (794 lines): `TailscaleCli` runs one child per call with a minimal environment, no stdin, 3 s, at most 2 MiB of stdout and 64 KiB of stderr; `TailscaleError::{NotInstalled, NotRunning, Timeout, TooLarge, Parse, Exit, Signal, Spawn}`.
- `StationConfig` (`fleetdeck-core/src/config.rs`, 799 lines) has `allowed_roots`, `allow_setup_cmd`, `member_deny`, `repo_groups`, `tailscale_bin`. The provider credential scrub is `fleetdeck-core/src/childenv.rs`: names `CLAUDECODE`, `AI_AGENT` and prefixes `ANTHROPIC_`, `CLAUDE_`, `OPENAI_`, `CODEX_`, `FLEETDECK_`.

### 1.8 `tailscale drive list` has no JSON output (verified)

- **What I checked.** The local machine has no `tailscale` program (WSL), so I read the source of the CLI at `github.com/tailscale/tailscale`, branch `main`, `VERSION.txt` 1.105.0, last commit of `cmd/tailscale/cli/drive.go` `3ec5be3f` (2026-01-23). The `list` subcommand has no `FlagSet`: it has no `--json` flag. `runDriveList` prints a table with `fmt.Printf("%-<N>s    %-<M>s    %s\n", name, path, as)`: a header line `name`, `path`, `as`, then a line of `-` runs (one run per column, the length of the longest value of the column, at least 4, 4 and 2), then one line per share. The columns are separated by exactly 4 spaces. The Tailscale KB page "Taildrive" (kb/1369) also names only the columns `name`, `path`, `as` and no JSON flag; it says that the `as` column is blank on Windows.
- **Share names** (`drive/remote.go`, `NormalizeShareName`): lower case, trimmed, only `a-z`, `0-9`, `_`, `(`, `)` and space. A name can hold a space, so a split at white space is wrong.
- **Widths.** Go computes the column width with `len()` (bytes) and pads with `%-Ns` (runes). A path with non-ASCII characters therefore gets extra padding, but every field still takes exactly the width of its dash run when it is counted in characters. The parser (§15.3) counts characters, not bytes.
- The LocalAPI (`/localapi/v0/drive/shares`) answers JSON, but the spec (O36) requires `TailscaleCli`, and the CLI calls the API through its own socket rules. The design uses the CLI table.

## 2. General rules (O1 to O5)

### 2.1 Opt-in (O1)

Every Fleet Git behavior depends on the project setting `sync` (default `false`). The station acts only for a session whose start carried a `FleetGitStart` (§3.3). The deck attaches it only for a worktree start in a home of a project with `sync = true`. A project with `sync = false` sends nothing new, stores nothing new on the station, and keeps every text and route of today. Acceptance A4 runs the existing suites with no change.

### 2.2 Credentials (O2)

The station runs every network Git command with `GitOptions.isolated = false` (the user configuration, the SSH agent, the credential helper of the station user). The deck never stores, sends or logs a Git credential. A `wip_remote` URL, a `remote_url` and a clone URL with user information are refused by the existing clone URL rules (`provision::check_url`, DL-200) and by the deck validation (§3.1).

### 2.3 Hooks (O3)

Every new Git command uses the runner of §1.3, so `core.hooksPath=/dev/null` applies to fetch, push, rebase, merge-tree and worktree add. The build adds a DL entry ("Git hooks stay disabled for every station Git command, also for push; a project that needs a pre-push check uses `check_cmd` of the land flow") and a paragraph in `operations.md`. A unit test runs a push with a `pre-push` hook that writes a marker file and checks that the file does not exist.

### 2.4 The Git commands of Fleet Git (O4)

Every command below runs through `Git` with `SAFE_CONFIG`, the removed `REDIRECTING_VARS`, `GIT_TERMINAL_PROMPT=0`, its own process group, and the limits in the table. A command that writes into a repository runs under `confine-exec` with the allowed roots as write roots (DL-200). Network commands add `-c protocol.ext.allow=never` and the environment `GIT_SSH_COMMAND` is not set by the station (the user value applies).

| Use | Command | Timeout | Output cap | Confined |
| --- | --- | --- | --- | --- |
| FG-1, FG-3, FG-7, FG-8, FG-10 fetch | `git fetch --prune --no-tags --no-recurse-submodules --no-write-fetch-head -- <remote>` | 30 s | stderr 2 KiB | yes |
| FG-1 default branch | `git symbolic-ref --quiet refs/remotes/<remote>/HEAD`, `git rev-parse --verify --quiet --end-of-options <remote>/<branch>^{commit}` | 10 s | 4 KiB | no |
| O6 namespace check | `git for-each-ref --format=%(refname) -- refs/heads/fd/<slug> refs/remotes/<remote>/fd/<slug>` | 10 s | 4 KiB | no |
| FG-2 push | `git push --porcelain --no-verify --no-follow-tags --recurse-submodules=no --force-with-lease=refs/heads/<ref>:<lease> -- <target> <oid>:refs/heads/<ref>` | 60 s | stdout 16 KiB, stderr 2 KiB | yes |
| FG-2 recovery | `git ls-remote --refs -- <target> refs/heads/<ref>` | 20 s | 4 KiB | no |
| FG-3 merged | `git merge-base --is-ancestor <tip> <remote>/<default>`; `git merge-tree --write-tree --no-messages <remote>/<default> <tip>` | 20 s each | 4 KiB | no (writes objects only through `--write-tree`; see §6.3) |
| FG-7 radar | `git merge-tree --write-tree --name-only --no-messages <a> <b>` | 20 s each, 120 s per run | 64 KiB | no |
| FG-8 land | `git worktree add --detach`, `git rebase`, `git rebase --abort`, `git diff --name-only --diff-filter=U`, `git push` | 120 s (worktree add, rebase), 60 s (push) | 2 KiB stderr, 16 KiB stdout | yes |
| FG-10 branches | `git for-each-ref --format=... refs/remotes/<remote>/fd/`, `git rev-list --left-right --count` | 10 s per call, 30 s per answer | 512 KiB | no |

`git merge-tree --write-tree` writes tree and blob objects into the object store. It runs inside the repository of a home, which is inside the allowed roots. It does not run under `confine-exec`, because it changes no ref and no work tree file; Landlock would also allow it. Fleet Git needs Git 2.45 or newer for all of these commands (the floor of `branches.rs`; `merge-tree --write-tree --name-only` needs 2.38). An older Git gives the code `git_too_old`.

Repository configuration can still run programs (`core.sshCommand`, `credential.helper`, `url.<base>.insteadOf` to an `ext::` URL is blocked by `protocol.ext.allow=never`). Writing `.git/config` needs local write access as the station user, and the agents already run as that user. This is the DL-142 position: it is stated, not solved.

### 2.5 Redacted errors (O4, DL-163)

Every failure that reaches the board has a code and a text. The text is built by one function, `fleet_git::redact(stderr) -> String`:

1. Take the last non-empty line of the captured stderr (at most 2 KiB were kept).
2. Remove URL user information (`provision::scrub`).
3. Mask known token prefixes with the DL-164 scrub (`ghp_`, `gho_`, `github_pat_`, `glpat-`, `xox`, Bearer values).
4. Remove control characters. Cut to 300 characters.

The board shows `<sentence>: <redacted line>`. The sentences are in the failure table of each section. The codes are new wire strings in `fleetdeck_proto::fleet_git::code` (§3.6).

### 2.6 Additive link changes (O5)

ADR-009 lists every link change. Summary: new features `fleet_git.v1`, `fleet_git.homes.v1`, `fleet_git.land.v1`, `fleet_git.move.v1`, `fleet_git.branches.v1`; new methods only behind those features; new optional fields with `#[serde(default)]`; new event kinds (an older deck keeps them as `EventBody::Unknown`); no new `SessionState` value. The deck refuses a Fleet Git start or action for a station that did not negotiate the feature (409 `fleet_git_unsupported`, "This station does not support Fleet Git. Update it first."), in the same way as `setup_not_supported` (DL-148).

## 3. Common data shapes

### 3.1 Project settings on the deck

New type in `crates/fleetdeck-proto/src/fleet_git.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct GitSettings {
    #[serde(default)] pub sync: bool,                                   // false
    #[serde(default)] pub push: PushMode,                               // Off
    #[serde(default = "origin")] pub push_remote: String,               // "origin"
    #[serde(default)] pub wip_remote: Option<String>,                   // None (a URL)
    #[serde(default)] pub default_branch: Option<String>,               // None: from the remote
    #[serde(default)] pub cleanup: CleanupMode,                         // Off
    #[serde(default = "h24")] pub cleanup_after_hours: u32,             // 24
    #[serde(default)] pub worktree_include: Vec<String>,                // []
    #[serde(default)] pub worktree_env: BTreeMap<String, String>,       // {}
    #[serde(default)] pub land: Option<LandMode>,                       // None: Pr with a forge, else Merge
    #[serde(default)] pub check_cmd: Option<String>,                    // None
    #[serde(default)] pub artifact_share: Option<String>,               // None ("<machine>/<share>")
    #[serde(default)] pub radar_home: Option<StationId>,                // None: §10.1
}
#[serde(rename_all = "snake_case")] pub enum PushMode { #[default] Off, OnTurn, OnExit }
#[serde(rename_all = "snake_case")] pub enum CleanupMode { #[default] Off, Merged }
#[serde(rename_all = "snake_case")] pub enum LandMode { Pr, Merge }
```

`fleet_git::check_settings(&GitSettings) -> Result<(), SettingsError { field, message }>` (the deck answers 400 `invalid_git_settings` with the field):

| Field | Rule | Message |
| --- | --- | --- |
| `push_remote` | 1 to 64 characters of `[A-Za-z0-9._-]`, not starting with `-` or `.` | "The push remote must be a remote name, for example origin." |
| `wip_remote` | `provision::check_url` rules (https, ssh, scp-like; no `file://`); no user information or password | "The WIP remote must be an https, ssh or user@host:path URL without a password." |
| `default_branch` | `check_branch_choice` syntax | "The default branch name is not valid." |
| `cleanup_after_hours` | 1 to 720 | "Clean up after 1 to 720 hours." |
| `worktree_include` | at most 32 entries; each 1 to 200 characters; relative; no `..` segment; no empty segment; no `\`, NUL or control character; segments may hold `*` and `?`; no `**`; no `[` | "Use relative paths without .. (at most 32)." |
| `worktree_env` | at most 32 entries; name `[A-Z_][A-Z0-9_]*`, at most 64 characters; value at most 4096 characters without NUL; refused names: `PATH`, `HOME`, `LD_*`, `DYLD_*`, the names and prefixes of `childenv::SCRUBBED_NAMES` and `SCRUBBED_PREFIXES` (`CLAUDECODE`, `AI_AGENT`, `ANTHROPIC_*`, `CLAUDE_*`, `OPENAI_*`, `CODEX_*`, `FLEETDECK_*`), and `GIT_*` | "The variable <name> is not allowed." |
| `check_cmd` | 1 to 4096 characters without NUL (the DL-148 rule) | "The check command must be 1 to 4096 characters without NUL." |
| `artifact_share` | `<machine>/<share>`: machine `[a-z0-9-]{1,63}`; share 1 to 64 characters of `[a-z0-9_() ]`, no leading or trailing space | "Use <machine>/<share>, for example nas/artifacts." |
| `radar_home` | a station with a home of the project | "The radar home must be a station with a home of this project." |
| `sync` with `push != Off` or `cleanup != Off` | allowed only with `sync = true` | "Turn on sync first." |

### 3.2 Station slug and branch names (O6)

- `station_slug(name) -> String` in `fleet_git.rs`: the station `name` in lower case; every character outside `[a-z0-9-]` becomes `-`; the result is cut to 40 characters. Consecutive `-` are kept (the spec says "changed to `-`").
- The deck computes the slug from the station name that it stores; the station computes it from `StationConfig.name`. Both use the same function.
- A new session branch in a project with sync on is `fd/<slug>/<callsign>`, unless the owner chose a branch (DL-144); an owner branch is never pushed by FG-2 (O10).
- The slug and the branch are fixed in the decision of the start and stored in `session_git` (§3.5). A rename of the station later changes nothing for the session.
- **Two stations with one slug.** The deck refuses a home (§8) and a sync start when another station with a home of the same project has the same slug: 409 `slug_taken`, "Station <a> and station <b> have the same Git name <slug>. Rename one station."
- **The ref D/F conflict.** Git stores `refs/heads/fd/x` as a file, so `refs/heads/fd/x/y` cannot exist beside it (and the reverse). The design handles each case:
  1. A local branch `fd/<slug>` or a remote-tracking ref `<remote>/fd/<slug>` blocks the new branch. The start effect runs the namespace check of §2.4 after the fetch and before `worktree add`. A match fails the start with `start_failed` and the code `branch_namespace_blocked`: "The branch fd/<slug> blocks the session branch fd/<slug>/<callsign>. Rename it, for example: git branch -m fd/<slug> fd-<slug>." The deck cannot see this before the start, so it is a start failure (DL-163 banner).
  2. A remote branch `fd/<slug>` that was not fetched (a fetch failure, O8) blocks the first push. The push fails with Git's message, gets the code `push_rejected`, and retries (O11). The card shows "Not pushed".
  3. A stale remote-tracking directory or file: `--prune` deletes stale refs before it writes new ones, so a removed remote `fd/x` does not block a fetched `fd/x/y`.
  4. A legacy sync-off session that later chooses the branch name `fd/<slug>` (the old `fd/<callsign>` form with a callsign equal to a slug) fails in `worktree add` with Git's message. This is an existing start failure, not a new state.
  5. A callsign never holds `/` (`callsign_candidates` uses `[a-z0-9-]`), so `fd/<slug>/<callsign>` has exactly three components.

### 3.3 How the settings reach the station

- `FleetGitStart` is a new optional field of `SessionCommand::Start` (`#[serde(default, skip_serializing_if = "Option::is_none")]`):

```rust
pub struct FleetGitStart {
    pub project_id: String,
    pub project_name: String,          // for the artifact folder and the clone folder
    pub settings: GitSettings,         // a snapshot with its revision
    pub revision: u64,
    pub repo: Option<String>,          // the project repo identity (FG-5)
    pub forge: bool,                   // the project has a forge binding (land default)
}
```

- New link method `fleet_git.projects_set` (deck → station, scope `admin`, feature `fleet_git.v1`, not mutating): params `{ projects: Vec<GitProjectView> }` (every project with sync on and a home on that station; `GitProjectView { project_id, project_name, revision, settings, repo, forge, home_path, radar_home: bool }`), answer `{ stored: u32 }`. The deck sends it after the link is up (after the first `station.info`) and after every change of a project's settings or homes. The station replaces its `git_projects` table in one transaction. It is idempotent, so a retry is safe.
- The station uses the snapshot of the start for the decision and the start effect. The push loop, the cleanup loop, the radar and the land read the newest row of `git_projects` (a later change of `push` or `cleanup` applies to existing sessions). A session whose project is no longer in `git_projects` (the deck turned sync off or deleted the project) gets no more pushes and no cleanup; its push row goes to `stopped`.

### 3.4 Station config (`station.toml`)

New keys, parsed in a new file `crates/fleetdeck-core/src/config_fleet_git.rs` (config.rs is at 799 lines; see §19 task T2.0):

| Key | Type | Default | Rule |
| --- | --- | --- | --- |
| `clone_root` | path | the first allowed root + `/projects` | absolute, inside an allowed root (O17) |
| `traits` | list of strings | `[]` | at most 16; each `[a-z0-9_-]{1,32}` (O19) |
| `allow_check_cmd` | bool | `false` | O24; reported in `station.info` |
| `[taildrive_mounts]` | table `"<machine>/<share>" = "/abs/path"` | empty | at most 16; key as `artifact_share`; value absolute, no `..` (O38) |

### 3.5 Station store migrations

`crates/fleetdeck-station/src/store/schema.rs` is at 797 lines. Task T1.0 moves `migrations()` to a new `store/migrations.rs` before any new migration. Each phase adds one migration in its own file:

- **V28 (phase 1, `store/schema_v28.rs`):**

```sql
CREATE TABLE git_projects (
    project_id   TEXT PRIMARY KEY,
    revision     INTEGER NOT NULL,
    view         TEXT    NOT NULL,   -- GitProjectView JSON
    updated_at   INTEGER NOT NULL
) STRICT;
CREATE TABLE session_git (
    session_id     TEXT PRIMARY KEY REFERENCES sessions(session_id),
    project_id     TEXT NOT NULL,
    slug           TEXT NOT NULL,
    branch         TEXT NOT NULL,
    created_branch INTEGER NOT NULL CHECK (created_branch IN (0, 1)), -- 1: fd/<slug>/<callsign> made by the station
    repo_root      TEXT NOT NULL,   -- the main checkout (the home)
    base_stale     INTEGER NOT NULL DEFAULT 0,
    ended_at       INTEGER,         -- the first exited/lost time the station saw
    cleanup_state  TEXT,            -- NULL, waiting, kept:<reason>, trashed
    cleanup_at     INTEGER,
    artifacts_path TEXT,
    artifacts_url  TEXT,
    created_at     INTEGER NOT NULL
) STRICT;
CREATE TABLE git_pushes (
    session_id      TEXT PRIMARY KEY REFERENCES sessions(session_id),
    target          TEXT NOT NULL,  -- the remote name or the wip_remote URL
    ref             TEXT NOT NULL,  -- fd/<slug>/<callsign>
    last_pushed_oid TEXT,           -- the lease (O10)
    last_push_kind  TEXT,           -- push, land
    pending_oid     TEXT,           -- the commit of the push in progress or due
    state           TEXT NOT NULL,  -- idle, due, pushing, pushed, failing, stopped
    attempts        INTEGER NOT NULL DEFAULT 0,
    next_attempt_at INTEGER,
    code            TEXT,
    message         TEXT,
    updated_at      INTEGER NOT NULL,
    UNIQUE (target, ref)
) STRICT;
```

- **V29 (phase 2, `store/schema_v29.rs`):** `project_homes_local (project_id TEXT PRIMARY KEY, path TEXT NOT NULL, url TEXT NOT NULL, state TEXT NOT NULL /* cloning, ready, adopted, failed */, code TEXT, message TEXT, dev INTEGER, ino INTEGER, started_at INTEGER NOT NULL, updated_at INTEGER NOT NULL) STRICT`.
- **V30 (phase 3, `store/schema_v30.rs`):** `land_runs` (§11.4).
- **V31 (phase 4, `store/schema_v31.rs`):** `session_moves` (§12.4).

No existing row is rewritten. A sync-off station never writes these tables.

### 3.6 New events (all station-made, all additive)

In `fleetdeck-proto/src/event.rs`. `event.rs` is at 788 lines: task T1.1 first moves `EventBody` and its serde code (lines 700 to 788) into a new `event_body.rs`.

| Kind | Fields | Fold | Section |
| --- | --- | --- | --- |
| `workspace.base_stale` | `code: String, text: String` | `summary.base_stale = true` (new optional field) | FG-1 |
| `workspace.include_skipped` | `files: Vec<SkippedFile { path, reason }>` (at most 50) | ignored | FG-4 |
| `session.push` | `state: PushState, ref_name: String, oid: Option<String>, attempts: u32, next_attempt_at: Option<i64>, code: Option<String>, text: Option<String>` | `summary.push = Some(PushInfo { .. })` (new optional field) | FG-2 |
| `session.cleanup` | `state: CleanupState, reason: Option<String>` | `summary.cleanup` (new optional field) | FG-3 |
| `artifacts.unavailable` | `code: String, text: String` | ignored | FG-12 |
| `artifacts.ready` | `path: Option<String>, url: Option<String>` | `summary.artifacts_dir = Some(..)` (new optional field) | FG-12 |
| `session.land` | `land_id: String, state: LandState, step: Option<LandStep>, code: Option<String>, text: Option<String>, files: Vec<String>, oid: Option<String>, pr_url: Option<String>` | `summary.land` (new optional field) | FG-8 |
| `session.moved` | `move_id: String, to_station: StationId, to_session: SessionId` | `summary.moved_to = Some(..)` (new optional field) | FG-9 |

`PushState`: `due`, `pushing`, `pushed`, `failing`, `stopped`. `CleanupState`: `waiting`, `kept`, `trashed`. `LandState`, `LandStep`: §11.3. Each enum has a `#[serde(other)] Unknown` variant, so a newer station does not break an older deck's fold. The fold of `SessionSummary` gets the new fields with `#[serde(default, skip_serializing_if = "Option::is_none")]` (`session.rs`, 375 lines).

### 3.7 New `station.info` fields (no feature needed, L16)

```rust
#[serde(default, skip_serializing_if = "Option::is_none")] pub traits: Option<StationTraits>,     // FG-6
#[serde(default, skip_serializing_if = "Option::is_none")] pub config: Option<ConfigDigest>,      // FG-11
#[serde(default, skip_serializing_if = "Option::is_none")] pub taildrive: Option<TaildriveReport>,// FG-12
#[serde(default, skip_serializing_if = "Option::is_none")] pub fleet_git: Option<FleetGitInfo>,   // clone_root, allow_check_cmd, git_version
```

`FleetGitInfo { clone_root: String, allow_check_cmd: bool, git_version: Option<String> }`.

## 4. FG-1: Fetch before spawn (O7, O8)

### 4.1 Facts

Today a worktree start runs `git worktree add -b fd/<callsign> -- <path> HEAD` (or `<base>`) with no fetch (`provision.rs:348`). The base commit is recorded after the Git work (`base::planned`).

### 4.2 Design

1. **Decision** (`manager/decide.rs`, through a new helper file `manager/decide_fleet_git.rs`): when `spec` has a `fleet_git` start and a worktree request without an owner branch, the plan branch is `fd/<slug>/<callsign>` and the plan gets a new optional field `WorkspacePlan::Worktree.sync: Option<SyncPlan { remote: String, default_branch: Option<String> }>` (`#[serde(default, skip_serializing_if = "Option::is_none")]`; an effect stored before it has none). The decision inserts the `session_git` row in the same transaction. A start without a worktree request (a start in place) gets no `session_git` row and no Fleet Git behavior.
2. **Start effect** (`setup_task.rs::provision_plan`, before `provision_cancellable`; the code lives in `workspace/fleet_git/fetch.rs`):
   1. `git fetch --prune ... -- <remote>` in the pinned source repository (30 s, confined).
   2. On success: the start point. A `base` that the owner gave wins (O7). Else `default_branch` of the settings, else the target of `refs/remotes/<remote>/HEAD`, else `main`, else `master`. The start point is `<remote>/<branch>` after `rev-parse --verify --quiet --end-of-options <remote>/<branch>^{commit}`.
   3. The namespace check of §3.2 item 1.
   4. `worktree add -b <branch> -- <path> <start point>`.
3. **Fetch failure** (O8): a timeout, a non-zero exit, or no default branch on the remote. The start goes on from the local `HEAD` (or the owner's `base`). The station appends `workspace.base_stale { code, text }` to the session after `session.started` (the event needs the session stream) and sets `session_git.base_stale = 1`. The start does not fail.
4. A cancel during the fetch stops it with the provision cancel (TERM, then KILL of the group), as for the other Git calls.

### 4.3 Failure states

| Code | Event / place | Text on the board |
| --- | --- | --- |
| `fetch_failed` | `workspace.base_stale` | "Fetch failed; this session started from the local HEAD: <redacted line>" |
| `fetch_timeout` | `workspace.base_stale` | "Fetch took more than 30 s; this session started from the local HEAD." |
| `no_default_branch` | `workspace.base_stale` | "The remote has no default branch; this session started from the local HEAD." |
| `branch_namespace_blocked` | `start_failed` | §3.2 |
| `git_too_old` | `start_failed` | "Fleet Git needs Git 2.45 or newer on this station." |

### 4.4 Board

- The session card shows a warning chip "Stale base" when `summary.base_stale` is true; its title is the event text.
- The chat shows the event as a notice line with the same text.

### 4.5 Tests

- Unit (`workspace/fleet_git/fetch_tests.rs`): the start point order (owner base, setting, remote HEAD, `main`, `master`); a fetch timeout with a fake `git` that sleeps; the namespace check finds `refs/heads/fd/<slug>` and `refs/remotes/origin/fd/<slug>`.
- Station (`manager/fleet_git_start_tests.rs`): a sync start makes `fd/<slug>/<callsign>`; a start without `fleet_git` keeps `fd/<callsign>` (A4); an owner branch wins; an effect stored without `sync` runs without a fetch.
- A2 FG-1 (`crates/fleetdeck-cli/tests/fleet_git_fetch.rs`): station A has a clone whose local `main` is one commit behind the bare remote; a sync start's `session.base` equals the remote commit. The bare remote is then moved away; the next start's base is the local `HEAD`, and the session has one `workspace.base_stale` with code `fetch_failed`. A third test: a local `fd/<slug>` branch gives `start_failed` with `branch_namespace_blocked`.

## 5. FG-2: Push (O9, O10, O11)

### 5.1 Facts

Fleet Deck never pushes today. `WorktreeInfo.unpushed` (DL-143) counts commits on no fetched remote ref.

### 5.2 Settings

`push = off | on_turn | on_exit` (default `off`), `push_remote` (default `origin`), `wip_remote` (default none). The push target is `wip_remote` when it is set, else `push_remote` (O9). The fetch of FG-1 always uses `push_remote`.

### 5.3 What the station pushes (O10)

- Only a ref in `git_pushes`. A row exists only for a `session_git` row with `created_branch = 1`, so the ref is always `fd/<own slug>/<callsign>` made by this station for its own session. `push.rs` refuses (an internal error, logged, no push) any ref that does not start with `fd/<own slug>/`.
- The refspec is explicit: `<oid>:refs/heads/<ref>`. The station never uses `--all`, `--mirror`, `--tags` or a configured push refspec. A repository with `remote.<name>.mirror = true` makes Git refuse the explicit refspec; the push fails with `push_failed` and retries.
- The lease is explicit: `--force-with-lease=refs/heads/<ref>:<last_pushed_oid>`, or `refs/heads/<ref>:` (the ref must not exist) before the first push. A revert or a rewrite of the session branch therefore pushes with force, but only over the commit that this station pushed last.
- FG-8 in `merge` mode is the only other push (§11.4 step 7). FG-9 makes a new ref on the target and stops the source ref (§12.5).

### 5.4 Triggers

- `on_turn`: each `turn.completed` of the session, and the end of the session.
- `on_exit`: the end of the session (`exited` or `lost`, the first time the station sees it).
- After a station restart: every row in `due`, `pushing` or `failing` is due again at its `next_attempt_at` (or at once).
- The trigger reads the branch tip in the session worktree's repository (`git rev-parse --verify --quiet refs/heads/<ref>`). It pushes commits only; it never commits. No change of the tip since `last_pushed_oid` gives no push.

### 5.5 Lease bookkeeping (O10)

The push loop (`manager/fleet_git_push.rs`, one task per station, one push at a time) does, for a row that is due:

1. Store `pending_oid = <tip>`, `state = pushing` (one transaction).
2. Run the push.
3. Success: `last_pushed_oid = pending_oid`, `last_push_kind = push`, `state = pushed`, `attempts = 0`, `code = NULL`. Append `session.push { state: pushed, oid }`.
4. Failure: run the recovery check `git ls-remote --refs -- <target> refs/heads/<ref>`. If the remote tip equals `pending_oid`, the push succeeded before a crash or a lost answer: record it as step 3. Otherwise: `state = failing`, `attempts += 1`, `next_attempt_at` from §5.6, the code and the redacted text. Append `session.push { state: failing, .. }`.
5. A station crash between steps 1 and 3 leaves `pushing`; the restart runs step 4's recovery check first.

A lease failure (the remote moved, for example another machine pushed) is a normal failure: it retries with backoff (O11). It never takes the remote tip as a new lease, except when the remote tip equals `pending_oid`.

### 5.6 Backoff (O11)

- Delay after attempt `n` (n = 1, 2, ...): `min(2^(n-1), 10)` minutes: 1, 2, 4, 8, 10, 10, ... There is no last attempt.
- A new trigger while a row is `failing` replaces `pending_oid` with the new tip and keeps `attempts` and `next_attempt_at` (no burst against a failing remote).
- The owner can press "Push now" on the card: the deck sends `fleet_git.push_now { session_id }` (§5.8); the row is due at once and `attempts` stays.
- The state is in `git_pushes` and survives a restart (O11).
- `stopped`: the project left `git_projects`, or the session moved (FG-9), or a land left the local branch behind the landed commit (§11.5). A stopped row never pushes. The card shows no badge for `stopped`.

### 5.7 Failure states

| Code | When | Text |
| --- | --- | --- |
| `push_rejected` | Git exit 1 with `[rejected]` or `stale info` in the porcelain output | "Push was rejected: <redacted line>" |
| `push_auth` | the DL-144 hint table says SSH key or https credentials | "Push needs credentials on this station: <redacted line>" |
| `push_failed` | another non-zero exit | "Push failed: <redacted line>" |
| `push_timeout` | 60 s | "Push took more than 60 s." |
| `git_too_old` | | §4.3 |

### 5.8 Link and deck

- `fleet_git.push_now` (deck → station, scope `drive`, feature `fleet_git.v1`, not mutating; params `{ session_id }`, answer `{ state }`; errors `unknown_session`, `not_synced`). It only moves `next_attempt_at`; a repeat is harmless.
- Deck route `POST /api/sessions/{station}/{session}/push` (scope `drive`, owner login or a `drive` token, CSRF, audited `session.push_now`; closed to members).

### 5.9 Board

- Card badge (new `web/src/board/PushBadge.tsx`): `failing` shows "Not pushed" (warning tone), title "Push failed <n> times: <text>. Next try <relative time>." and a "Push now" button in the card menu. `due` or `pushing` for more than 60 s shows "Pushing…". `pushed` shows nothing on the card; the session header shows "Pushed <oid7>".
- Sessions with sync off show nothing new.

### 5.10 Tests

- Unit (`workspace/fleet_git/push_tests.rs`, bare repositories in a temp dir): the first push with an empty lease; a second push with the lease; a revert (a reset to an older commit) then a push succeeds with the lease; a push after another clone moved the ref fails with `push_rejected`; the recovery check takes a remote tip equal to `pending_oid`; a ref outside `fd/<slug>/` is refused; a `pre-push` hook does not run; the backoff table (1, 2, 4, 8, 10, 10).
- Store (`store/git_state_tests.rs`): the restart turns `pushing` into a recovery check; the unique `(target, ref)`.
- A2 FG-2 (`fleet_git_push.rs`): stations B and C with the same callsign push `fd/b/<callsign>` and `fd/c/<callsign>` with no collision; a revert then a push succeeds; a push to a stale lease (a test moves the remote ref) fails, shows `failing` with attempts 1 and a next attempt, and retries; at the end, `git for-each-ref` of the bare repository lists only `refs/heads/main` and refs under `refs/heads/fd/<slug of a test station>/`.
- Board (`web/src/board/pushbadge.test.tsx`): the three badge states and the title.

## 6. FG-3: Automatic cleanup (O12, O13)

### 6.1 Facts

The trash flow (DL-150) accepts only a board command. Its checks are in `manager/trash.rs::trash()`. The file work is `workspace/trash.rs::run`. The inventory scan is `workspace/worktree_scan.rs`.

### 6.2 Settings

`cleanup = off | merged` (default `off`), `cleanup_after_hours` (default 24, 1 to 720).

### 6.3 The cleanup loop

`manager/fleet_git_cleanup.rs`: one task per station. It runs 5 minutes after the station start and then every 15 minutes. For each `session_git` row of a project with `cleanup = merged` and `cleanup_state` not `trashed`, it checks O13 in this order and stops at the first false item. It records `cleanup_state = kept:<reason>` (or `waiting`) and appends `session.cleanup` only when the state changes.

1. `ended`: the session is `exited` or `lost` and `now - ended_at >= cleanup_after_hours`. Else `waiting`.
2. `fetched`: one fetch of `push_remote` per repository per loop run (30 s). A failed fetch keeps the worktree (`kept:fetch_failed`).
3. `merged`: the branch tip is an ancestor of `<remote>/<default>` (`merge-base --is-ancestor`), **or** `git merge-tree --write-tree --no-messages <remote>/<default> <tip>` exits 0 and prints the same tree as `<remote>/<default>^{tree}`. The second rule finds a squash merge and a rebase merge of a forge: merging the branch adds nothing. Else `kept:not_merged`.
4. `clean`: the DL-143 scan of the worktree gives no unmerged path, no index or work tree change, no submodule change and no untracked file that is not ignored. A scan verdict `unknown` keeps it. Else `kept:dirty`.
5. `pushed` (only when `push != off`): `git_pushes.state = pushed` and `last_pushed_oid` equals the branch tip. Else `kept:not_pushed`.
6. `trash checks`: the station calls the trash decision with a new internal caller (below). Every DL-150 check runs in the decision transaction. A refusal keeps it (`kept:trash_refused`, the refusal text in `reason`).

Then the normal trash effect runs: the folder moves to `.fleetdeck-trash/`, the owner can restore it, branches are never deleted. `cleanup_state = trashed`.

**The internal caller.** `CommandOrigin` is a wire enum without `#[serde(other)]`, so the design adds no origin value. `manager/trash.rs` gets `pub fn decide_cleanup(d, policy, shells, session_id) -> Result<String /* op id */, DecideError>` that runs the same private `trash()` function and inserts the same row; the op id is `cleanup-<session_id>-<unix ms>`. Only the cleanup loop calls it. The board commands keep the Board-only rule.

### 6.4 Failure states and texts

`session.cleanup` reasons and the card menu texts:

| Reason | Text |
| --- | --- |
| `waiting` | "Cleanup waits until <time>." |
| `fetch_failed` | "Kept: the fetch failed." |
| `not_merged` | "Kept: the branch is not merged into <remote>/<default>." |
| `dirty` | "Kept: the worktree has changes or new files." |
| `not_pushed` | "Kept: the last commit is not pushed." |
| `trash_refused` | "Kept: <trash refusal text>" |
| `trashed` | "Moved to the trash by cleanup. Restore it from Stations → Worktrees." |

### 6.5 Board

The Stations page Worktrees section (DL-150) shows the existing `trashed` verdict. The session menu shows the cleanup line of the table above as a disabled info item when `summary.cleanup` is set.

### 6.6 Tests

- Station (`manager/fleet_git_cleanup_tests.rs`, real Git): each of the six conditions false keeps the worktree with its reason; all true moves it to the trash and `workspace.worktrees` shows `trashed`; the branch still exists; a squash-merged branch counts as merged; a restore from the board works for a cleanup row; a project with `cleanup = off` is never checked.
- A2 FG-3 (`fleet_git_cleanup.rs`): a merged, clean, pushed worktree whose session ended 25 hours ago (the test clock of the station) goes to the trash; five more tests each make one condition false (too new, not merged, an untracked file, an unpushed commit, an open shell) and the folder stays.

## 7. FG-4: Worktree bootstrap (O14, O15)

### 7.1 Settings

`worktree_include` (relative glob paths) and `worktree_env` (a map). Rules in §3.1.

### 7.2 Include files

`workspace/fleet_git/include.rs`, in the start effect after `worktree add` and before the setup command:

1. Source: the main checkout of the home (`session_git.repo_root`), opened with `files::open_root`. Destination: the new worktree, opened with `LaunchDir::open` (B9).
2. Each pattern is matched segment by segment from the source root with `openat` and no link following; `*` and `?` match inside one segment; hidden names match only a pattern segment that starts with `.`. At most 200 matches in total.
3. Each match: `openat(O_NOFOLLOW)`, `fstat`: a regular file only (a link, a directory, a FIFO or a device is skipped with `symlink` or `not_regular`); at most 1 MiB (`too_large`); the running total at most 10 MiB (`total_too_large`, every later file is skipped).
4. The destination file is created with `openat(O_CREAT | O_EXCL | O_NOFOLLOW)` below the destination descriptor. A file that exists (for example a tracked file) is skipped with `exists`, never overwritten. Missing parent folders are made with `mkdirat` mode 0700. The mode is the source mode `& 0o777`.
5. The skipped files are sent as one `workspace.include_skipped` after `session.started`. The start does not fail.

The member deny list (DL-166) needs no change: it hides these files from members by name.

### 7.3 Environment

`worktree_env` reaches the provider process and the setup command (DL-148) of each new session of the project. The station checks the names again with the same rule as the deck (§3.1) and drops a refused name with a log line. It adds the map after the child environment scrub (`ChildEnv::scrubbed`) and before the provider instance `env`, so the instance `env` wins on a name clash. It is not applied to a resume of a session that started before the setting changed (the effect carries the snapshot).

### 7.4 Board

The project settings (§17.1) have "Copy ignored files into new worktrees" (one path per line) and "Environment for agents and setup" (one `NAME=value` per line). The chat shows `workspace.include_skipped` as: "Not copied: <path> (<reason text>)" with the reason texts "a link", "not a regular file", "larger than 1 MiB", "over the 10 MiB total", "already in the worktree", "not readable".

### 7.5 Tests

- Unit (`workspace/fleet_git/include_tests.rs`): `.env` and `config/*.local` copy with their modes; a link, a FIFO, a 1 MiB + 1 byte file, the 10 MiB total, an existing tracked file; a `..` pattern and an absolute pattern are refused by `check_settings`; a link swapped into a parent folder of the destination is not followed.
- Unit (`fleet_git_tests.rs` in proto): every refused env name; `[A-Z_][A-Z0-9_]*`.
- Station: the provider and the setup command see `RUSTC_WRAPPER=sccache`; the instance `env` wins.
- A2 FG-4 (`fleet_git_include.rs`): include files are copied; a symlink, a `..` path (refused by the deck with 400 `invalid_git_settings`), and a too-large file are refused; the session starts.

## 8. FG-5: One project on many stations (O16, O17, O18)

### 8.1 Data shapes

`Project` (wire, `integrations/projects.rs`) gains fields; the old fields stay, so an older board still reads a project:

```rust
pub struct Project {
    // existing fields: project_id, name, station_id, path, forge, trackers,
    // prompt_template, review_prompt, created_at.
    // `station_id` and `path` are now the first home (position 0).
    #[serde(default)] pub homes: Vec<ProjectHome>,
    #[serde(default)] pub repo: Option<String>,        // "host/path", lower-case host
    #[serde(default)] pub remote_url: Option<String>,  // scrubbed clone URL
    #[serde(default)] pub git: GitSettings,
    #[serde(default)] pub git_revision: u64,
}
pub struct ProjectHome {
    pub station_id: StationId,
    pub path: String,
    pub position: u32,                 // 0 = the first home
    pub state: HomeState,              // ready, cloning, failed (+ Unknown)
    pub code: Option<String>,
    pub message: Option<String>,
}
```

Rules (O16):

- At most one home for each station (the primary key).
- A project with more than one home has `repo` and `remote_url`. `remote_url` passes `provision::check_url` with no user information and no password, and `repo == normalize_remote(remote_url)` (`fleetdeck_core::git::normalize_remote`).
- The board fills `remote_url` from `WorkspaceEntry.origin` of the first home (DL-201 repo catalog); the owner can change it.

### 8.2 Deck migrations (each one the next list position; SQL in `store/fleet_git_schema.rs`)

Phase 1 adds (V25 at the time of writing):

```sql
ALTER TABLE projects ADD COLUMN git TEXT NOT NULL DEFAULT '{}';
ALTER TABLE projects ADD COLUMN git_revision INTEGER NOT NULL DEFAULT 0;
```

Phase 2 adds (V26 at the time of writing):

```sql
ALTER TABLE projects ADD COLUMN repo TEXT;
ALTER TABLE projects ADD COLUMN remote_url TEXT;
CREATE TABLE project_homes (
    project_id TEXT    NOT NULL REFERENCES projects(project_id) ON DELETE CASCADE,
    station_id TEXT    NOT NULL,
    path       TEXT    NOT NULL,
    position   INTEGER NOT NULL,
    state      TEXT    NOT NULL DEFAULT 'ready',
    code       TEXT,
    message    TEXT,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (project_id, station_id)
) STRICT;
CREATE INDEX project_homes_station ON project_homes (station_id);
INSERT INTO project_homes (project_id, station_id, path, position, state, created_at)
    SELECT project_id, station_id, path, 0, 'ready', created_at FROM projects;
CREATE TABLE fleet_git_prefs (key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;
```

- The migration moves each existing `station_id` + `path` into its first home (O16). `projects.station_id` and `projects.path` stay as a copy of the position-0 home. Every write of homes updates the copy in the same transaction, so code that reads `Project.station_id` keeps working.
- An existing project has `repo = NULL`, `remote_url = NULL`, one home and `git = '{}'` (sync off). It behaves as today (O1, A4).
- `fleet_git_prefs` holds the drift reference station (§14).
- To reverse: drop the table and the columns; the copy in `projects` is still correct.

### 8.3 `project_of` (DL-176)

The path rule and the origin rule look at every home of the project on the session's station (was: the project's one station). A project with a `repo` also matches a session whose `summary.repo.origin` equals `repo` on any station. `POST /api/projects/{id}/sessions/assign` answers 400 `wrong_station` ("This project has no home on that station.") when the project has no home on the session's station.

### 8.4 Add home (O17)

- Route `POST /api/projects/{id}/homes { station, command_id }` (scope `admin`, owner login only, CSRF, audited `project.home_add`, closed to members and tokens). Checks: the project has `repo` and `remote_url` (400 `needs_remote`, "Set the remote URL of the project first."); the station has no home of the project (409 `home_exists`); the station negotiated `fleet_git.homes.v1` (409 `fleet_git_unsupported`); the slug rule (409 `slug_taken`, §3.2). The deck inserts the home with `state = cloning` and a position after the last one, then calls the station.
- Link method `project.ensure_home` (deck → station, scope `admin`, feature `fleet_git.homes.v1`, not mutating): params `{ project_id, project_name, url, repo }`, answer `{ state, path, code?, message? }`. The station:
  1. Computes the folder `<clone_root>/<folder>`. `folder` is the project name with every character outside `[A-Za-z0-9._-]` changed to `-` and a leading `.` changed to `-`, cut to 80 characters.
  2. When the folder exists: the `clone_reuse::clone_or_reuse` rules (a real folder inside the roots, a plain clone, origin equal by normalization). Equal: `adopted`. Another repository or not a clone: `failed` with `home_conflict`.
  3. When it does not exist: it stores `project_homes_local` with `cloning` and starts the clone in a task with the DL-200 provisioning (`confine-exec`, `CLONE_TIMEOUT` 15 min, `--no-recurse-submodules`, `protocol.ext.allow=never`, the URL rules). It answers `cloning` at once.
  4. The same call again returns the stored state (idempotent by project id). A `failed` row is cleared by the next call, which starts again.
  5. `clone_root` that does not exist is made with mode 0700 when its parent is an allowed root; else `failed` with `clone_root_missing`.
- Link method `project.home_status` (deck → station, scope `read`, feature `fleet_git.homes.v1`): params `{ project_id }`, answer as `ensure_home`. The deck asks every 10 s while a home is `cloning`, and once after each link start. It stores the result in `project_homes` and calls `board_changed()`. A done home triggers `fleet_git.projects_set` for that station.
- A station restart during a clone: the station marks the row `failed` with `clone_interrupted` and does not delete the partial folder (DL-200: no recursive delete). The owner sees "The clone stopped when the station restarted. Remove <path>, then add the home again."
- Remove a home: `DELETE /api/projects/{id}/homes/{station}` (owner only, audited `project.home_remove`). It refuses the last home (409 `last_home`) and a home with a live session (409 `home_busy`). It removes only the deck row; the station folder stays.

### 8.5 New session in a project (O18)

- `StartSessions` (tickets and reviews) gains `#[serde(default)] home: Option<String>` (a station id or `auto`; absent: the first home), `requires`, `prefer` and `spread` (§9).
- The board's "New session in this project" opens the spawn form with a Home select: the ready homes ("<station> · <path>") and "Auto (best station)". The form uses the home path as `cwd`.
- The deck attaches `FleetGitStart` to a worktree start when the start's station and `cwd` equal a home of a project with `sync = true`. The start body may name `project_id`; the deck checks it against the homes (400 `not_a_home`, "This folder is not a home of the project.").

### 8.6 The conflict radar grouping (O18)

`GET /api/radar` (§10.5) and the board panel group rows by project first (`project_of` of the sessions; the two sessions of a row must have the same project), then by the DL-201 group key of DL-306 (`origin:<origin>`, else `root:<station>:<root>`; a `[repo_groups]` name of the station replaces the origin key). A row without a project falls in the group "No project". With no Fleet Git project, the panel looks as today.

### 8.7 Failure states

| Code | Where | Text |
| --- | --- | --- |
| `needs_remote` | add home | "Set the remote URL of the project first." |
| `home_exists` | add home | "This station already has a home of the project." |
| `slug_taken` | add home, start | §3.2 |
| `home_conflict` | ensure home | "<path> exists and is not a clone of <repo>." |
| `clone_failed` | ensure home | "The clone failed: <redacted line>" |
| `clone_root_missing` | ensure home | "The clone folder <clone_root> does not exist and cannot be made." |
| `clone_interrupted` | restart | see §8.4 |
| `last_home`, `home_busy` | remove home | "A project needs one home." / "A session runs in this home." |

### 8.8 Board

Project page, new section "Homes" (`web/src/integrations/ProjectHomes.tsx`): one row per home, "<station> · <path> · Ready" / "Cloning…" / "Failed: <text>", with "Remove" (not on the last home). Button "Add home" opens a dialog "Add a home on another station": a station select (stations with the feature and no home), the text "The station clones <remote_url> into <clone_root>/<folder>. When that folder is already a clone of <repo>, the station uses it.", buttons "Add home" and "Cancel". The project form gets the field "Remote URL" (required when a second home is added).

### 8.9 Tests

- Deck store (`store/project_homes_tests.rs`): the migration of a database at the phase 1 version moves two projects into homes and keeps every `Project` field; the copy follows a home change; the cascade; one home per station; the scrub and `repo` rules.
- Deck routes (`tests/board_project_homes.rs`): owner only, CSRF, audit rows, `needs_remote`, `home_exists`, `slug_taken`, `last_home`.
- Station (`workspace/fleet_git/home_tests.rs`): a clone into `clone_root`; an adopt of an existing clone; a conflict; the folder name rule; the restart mark.
- A2 FG-5 (`fleet_git_homes.rs`): an existing project made before the migration (the test writes a V24 database) keeps working: its sessions keep their project and a start in it works; "add home" clones on station B; on station C the folder exists as a clone and is adopted (no clone runs: the bare repo's access log, or no new `.git` mtime).

## 9. FG-6: Placement by station traits (O19, O20)

### 9.1 Traits (station)

`StationTraits { os: String, arch: String, on_battery: Option<bool>, labels: Vec<String> }` in `station.info` (`crates/fleetdeck-station/src/traits.rs`):

- `os` = `std::env::consts::OS`, `arch` = `std::env::consts::ARCH`.
- `on_battery`, read at most every 60 s:
  - Linux, not WSL: `Some(true)` when no `/sys/class/power_supply/*` with `type = Mains` has `online = 1` and at least one with `type = Battery` has `status = Discharging`; `Some(false)` when a `Mains` supply is online or there is no battery; `None` when the files cannot be read.
  - WSL (`/proc/sys/kernel/osrelease` contains `microsoft`, any case): always `Some(false)` (O19).
  - macOS: `pmset -g batt` (3 s, 4 KiB): the first line contains `'Battery Power'` → `Some(true)`, `'AC Power'` → `Some(false)`, else `None`.
- `labels` = `traits` of `station.toml`.

### 9.2 Placement (deck, pure)

New file `crates/fleetdeck-deck/src/routing_place.rs`:

```rust
pub struct PlaceNeeds<'a> { pub requires: &'a [String], pub prefer: &'a [Prefer], pub provider: Option<ProviderKind>, pub homes: Option<&'a [StationId]> }
pub enum Prefer { Idle, NotOnBattery, AlwaysOn }  // wire: "idle", "not_on_battery", "always_on"
pub fn place(stations: &[StationView], sessions: &[DeckSession], needs: &PlaceNeeds, count: u32, spread: bool) -> Vec<StationId>
```

1. Candidates: `spawn_candidates` (link up, a free place, a signed-in provider), and a home of the project when `homes` is set.
2. `requires`: every entry must match. An entry is a label of `traits.labels`, or `os:<os>`, or `arch:<arch>`. A station without `traits` matches no requirement.
3. Score, compared in this order (O20):
   1. The number of satisfied preferences (more first). `idle`: no session of the station has the card status `working`, `verifying` or `queued`. `not_on_battery`: `on_battery == Some(false)`. `always_on`: the label `always_on`.
   2. The free resources of DL-193: `min(1 - load5/cores, mem_available/mem_total)`, higher first; a station without `resources` counts 0.
   3. The fewest live sessions.
   4. The station name (a stable order).
4. `count > 1`: the deck places one session at a time. After each placement the chosen station counts one more live session. With `spread`, a station that already got a session of this batch sorts after every station that got none (then the score).
5. No candidate: 409 `no_station`, "No station matches: <the first unmet requirement>."

Routing never calls a model (O20).

### 9.3 Where it is used

- The spawn form: Station "Auto (best station)", with "Needs" (labels, comma-separated) and "Prefer" (three switches: "Idle station", "Not on battery", "Always-on station"). A batch or `Nx` adds the switch "Spread over stations". The board calls `POST /api/placement { count, requires, prefer, spread, provider?, project_id? }` (scope `drive`, not audited, closed to members) and gets `{ stations: [StationId] }`; then it sends the starts as today (DL-202: a batch is a board expansion).
- The project start (`StartSessions.home = "auto"`).
- `assign auto` (`POST /api/assign`, `routing::spawn_candidates`): the body gains `requires`, `prefer`; `place` replaces the sort of `spawn_candidates` when the task starts a new session.

### 9.4 Board

The station card shows one line "<os> · <arch>[ · on battery][ · <label>, <label>]". The spawn form texts are in §9.3.

### 9.5 Tests

- Unit (`routing_place_tests.rs`): a requirement removes a station; `os:`/`arch:`; each preference; the resource order; the live-session order; `spread` with 3 stations and `count = 3` gives 3 stations, and with `count = 4` the fourth goes to the best one; no candidate.
- Station (`traits_tests.rs`): the `power_supply` reader with fake folders; the WSL rule; the `pmset` parser.
- A2 FG-6 (`fleet_git_placement.rs`): stations A (`gpu`), B (`always_on`), C (none). `requires = [gpu]` places on A; `prefer = [always_on]` places on B; `spread` with 3 sessions uses 3 stations.

## 10. FG-7: Predictive conflict radar (O21, O22)

### 10.1 The radar home

`GitSettings.radar_home`: a station of a home. Default: the home on a station with the label `always_on` (the first by home position), else the first home (O21). The owner changes it in the project settings. `GitProjectView.radar_home = true` tells the station that it is the radar home.

### 10.2 The run

The deck drives the radar (the deck knows the live sessions and their branches on every station):

- A deck task (`crates/fleetdeck-deck/src/fleet_git_radar.rs`) runs for each project with `sync = true` and at least two live pushed branches, or one live pushed branch: every 5 minutes, and 30 s after a `session.push` with `state = pushed` of the project (one run for a burst).
- It calls `project.radar` on the radar home (deck → station, scope `read`, feature `fleet_git.land.v1`, not mutating; at most one run per project at a time): params `{ project_id, branches: [RadarBranch { key, ref_name }] }` (at most 50 branches, newest session first), answer `RadarResult { checked_at, base_oid, pairs: [RadarPair { a, b, conflict: bool, files: Vec<String>, files_truncated: bool }], skipped: u32, code?, text? }`. `b = "@default"` for the default branch.
- The station fetches `push_remote` (30 s), then for each pair `git merge-tree --write-tree --name-only --no-messages <remote>/<a> <remote>/<b>`: exit 0 → no conflict; exit 1 → conflict, the files are the lines after the first (the tree oid), at most 100 per pair; another exit → the pair is skipped. It runs at most 50 pairs, ordered: each branch against `<remote>/<default>` first (newest branch first), then the pairs of branches, newest pair first (by the newer, then the older start time). It stops at 120 s and reports the rest as `skipped`.
- Only pushed branches take part. The deck sends only sessions whose `summary.push.state` is `pushed` or `failing` with a known `oid` and whose branch exists on the remote; a session with push off is never sent (O22).

### 10.3 Deck store (phase 3 migration)

```sql
CREATE TABLE radar_pairs (
    project_id TEXT NOT NULL,
    a_station  TEXT NOT NULL, a_session TEXT NOT NULL,
    b_station  TEXT NOT NULL, b_session TEXT NOT NULL,  -- '' and '@default' for the default branch
    conflict   INTEGER NOT NULL,
    files      TEXT NOT NULL,       -- JSON list
    checked_at INTEGER NOT NULL,
    PRIMARY KEY (project_id, a_station, a_session, b_station, b_session)
) STRICT;
```

Each run replaces the rows of its project. Rows of an ended session are removed at the next run.

### 10.4 Levels (O22)

- "Will conflict": a `radar_pairs` row with `conflict = 1`, with its files.
- "Touch the same files": the DL-306 rows, unchanged. A session with push off appears only here.

### 10.5 Deck route and board

- `GET /api/radar` (scope `read`, closed to members): `{ groups: [RadarGroup { project_id: Option<String>, group_key: String, will_conflict: [WillConflict { a: SessionRef, b: SessionRef | "default", files, checked_at }], same_files: [FileConflict] }] }`. `GET /api/conflicts` stays as it is.
- `ConflictRadar.tsx`: when any group has a project, a header per group ("<project name>", or "No project"). A "Will conflict" row: label "Will conflict" (danger tone), the first 3 files and "+<n>", "<callsign A> × <callsign B>" or "<callsign A> × <remote>/<default>", the age "<n> min ago". The DL-306 rows keep the label "Conflict" and their layout.

### 10.6 Failure states

| Code | Text (panel footer for the project) |
| --- | --- |
| `fetch_failed` | "Radar: the fetch failed on <station>: <redacted line>" |
| `radar_busy` | not shown (the next run comes) |
| `git_too_old` | "Radar: Git on <station> is older than 2.45." |
| station offline | "Radar: <station> is offline; results from <time>." |

### 10.7 Tests

- Station unit (`workspace/fleet_git/radar_tests.rs`): two branches that change the same lines of one file → conflict with that file; two branches that change different files → no conflict; a branch that changes other lines of the same file → no conflict; the pair order and the 50 limit; the 120 s stop with `skipped`.
- Deck (`fleet_git_radar_tests.rs`): the trigger after `session.push`; only pushed sessions are sent; the rows are replaced.
- A2 FG-7 (`fleet_git_radar.rs`): sessions on B and C push branches that change the same lines → `GET /api/radar` has a "will conflict" row with the file; two branches that change different files of the same file set (the same three files, each branch one other file) → no such row.

## 11. FG-8: Land flow (O23 to O26)

### 11.1 Who and when (O23)

- Action "Land" on a session that is ended (`exited`, `lost`), or idle (`running` with no current run or turn, no open request). The project has `sync = true`, the session has a `session_git` row with `created_branch = 1`, and the station negotiated `fleet_git.land.v1`.
- Callers: the owner's board login (route below), and a coordinator with `manage` (ADR-008 rules, §13.2). A member or a token is refused.

### 11.2 The deck queue

Deck table (phase 3 migration):

```sql
CREATE TABLE land_queue (
    land_id     TEXT    PRIMARY KEY,
    project_id  TEXT    NOT NULL,
    station_id  TEXT    NOT NULL,
    session_id  TEXT    NOT NULL,
    mode        TEXT    NOT NULL,          -- pr, merge
    state       TEXT    NOT NULL,          -- queued, running, landed, conflict, check_failed, remote_moved, failed, cancelled
    step        TEXT,
    attempt     INTEGER NOT NULL DEFAULT 0,
    requested_by TEXT   NOT NULL,          -- owner, coordinator:<grant>
    code        TEXT,
    message     TEXT,
    files       TEXT    NOT NULL DEFAULT '[]',
    target_oid  TEXT,                      -- <remote>/<default> at the rebase
    result_oid  TEXT,
    pr_url      TEXT,
    created_at  INTEGER NOT NULL,
    started_at  INTEGER,
    ended_at    INTEGER
) STRICT;
CREATE INDEX land_queue_project ON land_queue (project_id, state, created_at);
CREATE UNIQUE INDEX land_queue_open ON land_queue (station_id, session_id) WHERE state IN ('queued', 'running');
```

- One land per project runs at a time (O23): the deck task (`crates/fleetdeck-deck/src/fleet_git_land.rs`) starts the oldest `queued` row of a project only when the project has no `running` row.
- The deck sends `session.land` (deck → station, scope `drive`, feature `fleet_git.land.v1`, **mutating**, a `SessionCommand::Land { session_id, land_id, mode, target_branch: Option<String> }` with a receipt). The receipt is only the admission.
- The station reports progress and the end with `session.land` events. The deck folds them into the row in the event transaction. A terminal state frees the project for the next land.
- A deck restart keeps the queue. A `running` row whose station reports no land after the link is up (the station answers `fleet_git.land_status { land_id }` with `unknown`) becomes `failed` with `land_lost`.
- Cancel: `POST /api/lands/{land_id}/cancel` (owner only) for a `queued` row only (409 `land_running` for a running one).

### 11.3 States and steps

`LandState`: `queued`, `running`, `landed`, `conflict`, `check_failed`, `remote_moved`, `failed`, `cancelled` (+ `Unknown`). `LandStep`: `fetch`, `worktree`, `rebase`, `check`, `push_branch`, `pull_request`, `push_default`, `cleanup` (+ `Unknown`).

### 11.4 The station run (O24)

Station table (V30):

```sql
CREATE TABLE land_runs (
    land_id     TEXT PRIMARY KEY,
    session_id  TEXT NOT NULL REFERENCES sessions(session_id),
    state       TEXT NOT NULL,
    step        TEXT,
    attempt     INTEGER NOT NULL DEFAULT 0,
    temp_path   TEXT,
    temp_dev    INTEGER,
    temp_ino    INTEGER,
    lease       TEXT,
    target_oid  TEXT,
    result_oid  TEXT,
    code        TEXT,
    message     TEXT,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
) STRICT;
```

The decision (`manager/land.rs`) checks the state of §11.1 again, one open land per session, and queues `EffectPayload::Land { land_id }` (a new effect kind that needs no provider process, routed like `Worktree`). The effect is `Dispatch::Deferred`; a task runs it (the DL-148 pattern), so the effect worker is never blocked. Steps:

1. `fetch`: `git fetch` of `push_remote` (30 s). A failure ends `failed` with `fetch_failed`.
2. `worktree`: the temporary worktree `<parent of the home>/.fleetdeck-land/<home name>--<first 12 chars of land_id>` (inside the allowed roots, so `confine-exec` allows it; mode 0700 for `.fleetdeck-land`). `git worktree add --detach -- <temp> <branch tip>`. The station records `temp_dev` and `temp_ino`. The session worktree is never used (O24).
3. `rebase`: `git -c rebase.autoStash=false -c rebase.updateRefs=false -c core.editor=true rebase --no-autosquash --no-fork-point -- <remote>/<default>` in the temporary worktree. Git needs a committer identity: when `user.name` or `user.email` is missing, the land ends `failed` with `land_no_identity`. A conflict (exit 1 with unmerged paths): the station reads `git diff --name-only --diff-filter=U` (at most 200 files), runs `git rebase --abort`, and ends `conflict` with the files (O26). Nothing was pushed.
4. `check`: when `check_cmd` is set. It runs only when `allow_check_cmd = true` in `station.toml` (else `failed` with `check_disabled`). It uses the DL-148 runner (`workspace/setup.rs`): `/bin/sh -c`, its own process group, the scrubbed environment with `worktree_env`, the `setup_runs` row and marker (`FLEETDECK_SETUP_ID=<land_id>`), 10 minutes, the restart cleanup. A non-zero exit, a signal or a timeout ends `check_failed` with the last 50 lines of the output, redacted line by line (§2.5 steps 2 to 4), at most 16 KiB (O26).
5. `push_branch`: the push of §5.5 with the lease from `git_pushes.last_pushed_oid` (or the empty lease), of the rebased commit `R` to `fd/<slug>/<callsign>` on the push target. Success: `last_pushed_oid = R`, `last_push_kind = land`.
6. Mode `pr` (O25): `prs.list` with the head branch; an open PR exists → it is updated by the push; else `prs.create` (DL-164) with the head `fd/<slug>/<callsign>`, the base `<default>`, the title = the session title (else the branch), the body = the session digest (else "Landed by Fleet Deck from session <callsign>."). The land ends `landed` with `pr_url`. A PR write failure ends `failed` with `pr_failed` (the branch push stays).
7. Mode `merge` (O25): `git push --porcelain -- <push_remote> <R>:refs/heads/<default>` with no force and no lease (a fast-forward only). A rejection (`[rejected]`, non-fast-forward): the land runs again from step 1 **one time** (`attempt = 1`); a second rejection ends `remote_moved` (O25). Success: `landed` with `result_oid = R`. This is the only push outside `fd/<own slug>/` (O10). With `wip_remote` set, the branch goes to the WIP remote and the default branch goes to `push_remote`.
8. `cleanup`: the temporary worktree is removed in every end state: `git worktree remove --force` is not used; the station deletes the folder only when its `dev`/`ino` equal the recorded ones (the DL-150 `delete_checked` pattern, in one blocking call), then runs `git worktree prune`. A changed identity leaves the folder and logs a warning; the land state does not change.

Restart: a `land_runs` row that is not terminal becomes `failed` with `station_restarted`, its temporary worktree goes through step 8, and the station appends the `session.land` event. A check command that was running is stopped by the DL-148 restart cleanup (the marker).

The default land mode (O25): `GitSettings.land`, else `pr` when the project has a forge binding (`FleetGitStart.forge`), else `merge`.

### 11.5 The local session branch after a land

The land never changes the session worktree. After `landed`, the local branch `fd/<slug>/<callsign>` still points at the old tip, and the remote has `R`. The push loop treats the row so: when `last_push_kind = land` and the local tip is not `R` and `R` is not an ancestor of the local tip, the row goes to `stopped` with the code `landed`, and the card shows "Landed; new commits are not pushed" (no automatic force push over the landed branch). A new session or a "Send to session" rebase continues the work.

### 11.6 Send to session (O26)

- Shown for a land in `conflict`, when the session is live and idle. For an ended session the button reads "Resume the session first".
- `POST /api/lands/{land_id}/send` (owner only, audited `land.send_to_session`) sends a `prompt` with this fixed text (no third-party text):

```
Fleet Deck could not land this branch: the rebase onto <remote>/<default> (<target_oid12>) has conflicts in these files:
- <file>
...
Rebase this branch onto <remote>/<default>, fix the conflicts, run the checks, and commit. Then tell me that it is ready to land.
```

### 11.7 Routes

- `POST /api/sessions/{station}/{session}/land { command_id }` (scope `admin`, owner login only, CSRF, audited `session.land`): 202 with `{ land_id, position }`; 409 `land_not_ready` (with the reason), `land_open` (a land of the session is queued or runs), `fleet_git_unsupported`.
- `GET /api/projects/{id}/lands` (scope `read`): the queue and the last 20 ends.

### 11.8 Failure states

| State / code | Text |
| --- | --- |
| `conflict` | "Conflict in <n> files with <remote>/<default>. Nothing was pushed." |
| `check_failed` | "The check command failed (exit <n>). Nothing was pushed." + the output tail |
| `check_disabled` | "This station does not run check commands (allow_check_cmd = false)." |
| `remote_moved` | "<remote>/<default> moved twice during the land. Land again." |
| `fetch_failed`, `push_failed`, `push_rejected` | §4.3, §5.7 |
| `pr_failed` | "The branch is pushed, but the pull request failed: <redacted line>" |
| `land_no_identity` | "Git has no user.name or user.email on this station. Set them with git config --global." |
| `station_restarted` | "The station restarted during the land. Land again." |
| `land_lost` | "The station does not know this land any more. Land again." |

### 11.9 Board

`web/src/session/LandPanel.tsx`: the session header has "Land" (owner, conditions of §11.1). The confirm dialog: title "Land <branch>?", text for `pr`: "Fleet Deck rebases the branch onto <remote>/<default> in a separate folder, runs the check command, pushes the branch and opens or updates a pull request." For `merge`: "... pushes the branch, then pushes the result to <remote>/<default>." Buttons "Land" and "Cancel". The status line: "Queued (<n> ahead)", "Landing: <step text>" (step texts: "fetching", "preparing a folder", "rebasing", "running the check", "pushing the branch", "opening the pull request", "pushing <default>", "cleaning up"), "Landed <oid7>" or "Pull request #<n>" (a link), and the failure texts of §11.8. A conflict shows the file list and "Send to session". The project page has a "Lands" list (the queue).

### 11.10 Tests

- Station (`workspace/fleet_git/land_tests.rs`, `manager/land_tests.rs`, real Git, a bare remote): a clean merge-mode land advances the remote default branch to `R`; a conflict stops with the files, `git rebase --abort` ran, and the bare repo's refs are unchanged; a failing check stops with 50 lines; `check_disabled`; the remote moved once → second attempt lands; twice → `remote_moved`; the temp folder is gone at the end; a replaced temp folder is not deleted; a restart in `rebase` ends `station_restarted`; no hook runs.
- Deck (`fleet_git_land_tests.rs`): one running land per project; the next starts after a terminal event; cancel of a queued land; `land_lost`.
- A2 FG-8 (`fleet_git_land.rs`): a clean land in `merge` mode advances `main` of the bare repo; a conflict stops with the file list and no ref of the bare repo changes; a failed check stops the land; two lands of one project run one after the other (the second `started_at` ≥ the first `ended_at`).

## 12. FG-9: Move a session to another station (O27 to O30)

### 12.1 Spike and ADR-010

The spike `spikes/s-move/` and **ADR-010 (`docs/v2/adr/ADR-010-session-move.md`)** are written in parallel with this draft. ADR-010 decides, for Claude and for Codex separately: which transcript files belong to a conversation, where they go on the target (the target folder path differs), which content must be rewritten (for example a stored `cwd`), and whether a native resume works. This section defines the transfer and the session states. It uses these names from ADR-010:

- `MoveSupport::Native` or `MoveSupport::BranchOnly` for each provider kind;
- `TranscriptLayout`: the provider function that lists the files of a native session on the source (`export_files`) and that places them on the target (`import_files`), with the rewrite rules.

A provider that fails the spike gets `BranchOnly` and a DL entry (O27). Until ADR-010 is accepted, both providers are `BranchOnly`.

### 12.2 Conditions (O28)

Checked by the deck (for the answer) and again by the source station in its decision:

1. The session has no running turn (`current_run` and `current_turn` are empty) and no open request (`open_requests` is empty).
2. The branch is pushed: `git_pushes.state = pushed` and `last_pushed_oid` equals the local tip.
3. The target station has a ready home of the project, negotiated `fleet_git.move.v1`, and is not the source.
4. The caller is the owner's board login, or a coordinator with `manage` (ADR-008 §4.2 rules). Members and tokens get 403.
5. No other move of the session is open; the session is not `moved`.

The route is `POST /api/sessions/{station}/{session}/move { to_station, command_id }` (scope `admin`, CSRF, audited `session.move` with the target `<station>/<session> -> <to_station>`). Answers: 202 `{ move_id }`; 409 `move_not_ready` with the first failed condition: "A turn is running.", "An approval is open.", "The branch is not pushed.", "<station> has no home of this project.", "The session is already moving."

### 12.3 Steps

The two stations never connect to each other: every byte goes through the links of the one deck that both stations are paired with (O29). The deck drives the move (`crates/fleetdeck-deck/src/fleet_git_move.rs`) and stores each step in `session_moves` (deck, phase 4 migration):

```sql
CREATE TABLE session_moves (
    move_id       TEXT PRIMARY KEY,
    src_station   TEXT NOT NULL, src_session TEXT NOT NULL,
    dst_station   TEXT NOT NULL, dst_session TEXT,
    kind          TEXT NOT NULL,      -- native, branch_only
    state         TEXT NOT NULL,      -- holding, exporting, importing, starting, finishing, done, failed
    code          TEXT, message TEXT,
    bytes         INTEGER,
    requested_by  TEXT NOT NULL,
    created_at    INTEGER NOT NULL, ended_at INTEGER
) STRICT;
CREATE UNIQUE INDEX session_moves_open ON session_moves (src_station, src_session) WHERE state NOT IN ('done', 'failed');
```

1. **Hold** (source, `session.move_out`, a mutating `SessionCommand::MoveOut { session_id, move_id, to_station }`): the decision checks §12.2 items 1, 2 and 5 and inserts the station row `session_moves` (role `source`, state `holding`). While the row is open, `session.prompt`, `steer`, `follow_up`, `resume`, `import`, `fork`, `shell.open`, `shell.handover`, the auto-continue and the push loop refuse or skip the session (`invalid_state`, "The session is being moved."). An idle live session is stopped with `StopMode::Graceful` as part of this step, so that the transcript does not change.
2. **Export** (native only; source, `session.move_export { move_id }`, scope `admin`, not mutating): the station lists the files with `TranscriptLayout::export_files` and answers `{ manifest: TranscriptManifest, stream_token }`. `TranscriptManifest { provider: ProviderKind, native_session_id: String, files: Vec<ManifestFile { key: String, rel: String, size: u64, sha256: String }>, total: u64 }`. Every file is opened without following a link, must be a regular file, and is read from that descriptor. `total` over 64 MiB fails with `move_too_large` (O29). The stream (purpose `move-export:<move_id>`) carries the files in manifest order, back to back.
3. **Import** (target, `session.move_prepare { move_id, manifest, source: { station_id, session_id } }`, scope `admin`, not mutating): the target checks the manifest (at most 64 MiB, at most 256 files, `rel` is relative with no `..`, the provider is supported) and answers `{ stream_token }` for an upload stream (purpose `move-import:<move_id>`). The deck pipes the export stream into the upload stream; it keeps no copy on disk and holds at most the two stream windows in memory. The target writes the files into `<station data>/moves/<move_id>/` (folder mode 0700, files mode 0600, `O_CREAT | O_EXCL | O_NOFOLLOW`), counts the bytes and checks each SHA-256. A mismatch or a short stream fails with `move_checksum`.
4. **Start** (target, `session.move_in`, a mutating `SessionCommand::MoveIn { move_id, kind, provider_instance, title, callsign_hint, from: { station_id, session_id, ref_name, oid }, fleet_git: FleetGitStart }`):
   - The decision makes a new session id and a callsign (the source callsign when it is free on the target, else a new one), and the branch `fd/<target slug>/<callsign>`.
   - The effect fetches `push_remote`, checks that `<remote>/<from.ref_name>` equals `from.oid` (else `move_branch_moved`), and runs `git worktree add -b fd/<target slug>/<callsign> -- <path> <from.oid>` in the target home.
   - `Native`: `TranscriptLayout::import_files` moves the staged files into the provider home of the target instance (mode 0600) with the ADR-010 rewrites, then the station starts the provider with a native resume of `native_session_id` in the new worktree (the existing `session.resume` launch path). A failed resume fails the move with `move_resume_failed`; the source session stays.
   - `BranchOnly` (O30): a new session on the new branch with the first prompt of §12.6.
   - The new session's `git_pushes` row starts with `last_pushed_oid = NULL` (the new ref does not exist yet), so its first push uses the empty lease.
5. **Finish** (source, `session.move_finish { move_id, to_station, to_session }`, mutating): the source appends `session.moved { move_id, to_station, to_session }`, sets the push row to `stopped`, and closes the hold into the permanent state `moved`.
6. **Abort**: any failure before step 5 sends `session.move_abort { move_id }` to the source (the hold ends; the session is as before) and `session.move_discard { move_id }` to the target (the staging folder is deleted by identity, the DL-150 pattern). A deck restart reads `session_moves` and continues or aborts each open row: steps 1 to 3 abort; step 4 asks the target for the move state (`session.move_status { move_id }`) and finishes when the new session exists.

### 12.4 The `moved` state (O29)

- `SessionState` gets no new value (§1.6: it is not additive). The source session stays `exited` and gets `summary.moved_to = Some(MovedTo { move_id, station_id, session_id })`.
- The station refuses every input, resume, import, fork, shell and land of a moved session (`invalid_state`, "The session moved to <station>. Open it there.").
- The source worktree stays until the owner trashes it (O29). The cleanup loop (FG-3) skips a moved session (its push row is `stopped`).
- An older deck sees an `exited` session and ignores the `session.moved` event.

### 12.5 Branches of a move

The target pushes only its own new ref `fd/<target slug>/<callsign>` (O10). The source ref `fd/<source slug>/<callsign>` stays on the remote and is never pushed again by anyone. Cleanup never deletes a branch (O13), so the old ref stays until the owner deletes it.

### 12.6 Branch-only first prompt (O30)

```
This session continues the work of the session "<title>" from station <source station>. The branch <new branch> starts at its last pushed commit <oid12>.
The last summary of that session:
<untrusted source="session:<source station>/<source session>">
<digest, else the text of the last assistant message, cut to 4000 characters>
</untrusted>
Continue from there.
```

The summary is fenced with the DL-164 envelope, because it can hold text from tools and files (ADR-008 §4.3).

### 12.7 Failure states

| Code | Text |
| --- | --- |
| `move_not_ready` | §12.2 |
| `move_too_large` | "The conversation files are larger than 64 MiB. Use a branch-only move." |
| `move_checksum` | "The conversation files did not arrive complete. Try again." |
| `move_branch_moved` | "The branch changed on the remote after the hold. Try again." |
| `move_resume_failed` | "<provider> could not resume the conversation on <station>: <redacted line>. The session stays on <source station>." |
| `move_lost` | "The move stopped when the deck restarted. The session stays on <source station>." |

### 12.8 Board

The session menu has "Move to station…" (owner only). The dialog: title "Move <callsign> to another station", a station select (stations with a ready home), the checklist of §12.2 with a mark for each condition, the kind line ("The conversation moves with its history." or "Only the branch moves; a new session continues from the last summary."), buttons "Move" and "Cancel". The progress line: "Moving: holding", "copying the conversation (<n> MiB)", "starting on <station>", "finishing". The source session header shows "Moved to <station>" with "Open" and no composer.

### 12.9 Tests

- Station (`workspace/fleet_git/move_tests.rs`, `manager/move_tests.rs`): the hold refuses input, resume and shell; export refuses a link and a file over the limit; the import writes mode 0600 and refuses a bad SHA-256, a `..` path and an existing file; abort releases the hold; finish makes the session `moved` and refuses input.
- Deck (`fleet_git_move_tests.rs`): the conditions; the pipe holds no file; restart abort; audit rows.
- A2 FG-9 (`fleet_git_move.rs`): a session of the fake provider (`fake-claude sim`) with three turns moves from B to C; the C session resumes with its history (the sim resume reads the moved transcript and `session.history` lists the three turns); the B session has `moved_to` and refuses a prompt. The real-CLI proof is the spike evidence of ADR-010.

## 13. FG-10: The architect across the fleet (O31, O32, backlog B1)

### 13.1 Branches of other stations (O31)

- Link method `project.branches` (deck → station, scope `read`, feature `fleet_git.branches.v1`, not mutating): params `{ project_id }`, answer `RemoteBranches { remote, default_branch, default_oid, fetched_at: Option<i64>, branches: [RemoteBranch { name, slug, callsign, head, ahead, behind, at, subject }], truncated }`. The radar home lists `refs/remotes/<remote>/fd/*/*` (names with exactly three components), at most 200, newest tip first. `ahead`/`behind` are counted against `<remote>/<default>` with `git rev-list --left-right --count`. The station runs no fetch in this call. `subject` is cut to 200 characters.
- Link method `project.fetch` (deck → station, scope `read`, feature `fleet_git.branches.v1`, not mutating): params `{ project_id }`, answer `{ fetched_at, code?, text? }`. One fetch of `push_remote` on the radar home (30 s).
- Deck routes `GET /api/projects/{id}/branches` and `POST /api/projects/{id}/fetch` (scope `read` and `drive`, closed to members). The deck maps `slug` to the station with that slug, and `(station, name)` to the session whose `summary.branch` is `name` (a link when known). It adds `conflict: none | will_conflict` from `radar_pairs` and the files.
- Board: `BranchesView.tsx` gets a second group "Other stations" (new `web/src/session/OtherStations.tsx`) when the session belongs to a project with sync on. Rows: "<branch> · <station> · <session link or "no session"> · ↑<ahead> ↓<behind> · Will conflict (<n> files)". The group has the button "Fetch" and the line "Fetched <relative time>". Empty: "No branches of other stations. Push is off, or no other station pushed yet." The branches of the session's own station are not in this group.

### 13.2 Coordinator tools (O32)

New MCP tools in `crates/fleetdeck-station/src/coordinator/git_tools.rs` (the list of `fleet_tools.rs` names them):

| MCP tool | Classes | Wire | Target rule |
| --- | --- | --- | --- |
| `project_branches` | inspect | query `project_branches { project_id }` | every home of the project is on a grant station |
| `project_fetch` | inspect | query `project_fetch { project_id }` | as above |
| `land_session` | manage | op `land { station, session_id }` | ADR-008 §4.2; §11.1 |
| `move_session` | manage | op `move { station, session_id, to_station }` | ADR-008 §4.2; both stations in the grant; §12.2 |

- The names follow the ADR-008 tool style (snake_case). They are the spec's `project.branches`, `project.fetch`, `session.land` and `session.move`.
- New `CoordinatorQuery` variants `project_branches`, `project_fetch` and `CoordinatorOp` variants `land`, `move` (`fleetdeck-proto/src/mutation.rs`). They need `coordinator.v2` and the matching Fleet Git feature on the links; else the tool answers `unsupported`.
- Text from Git (branch names, commit subjects) is third-party text: the answer fences each `subject` with the untrusted envelope (ADR-008 §4.3).
- The kill switches, renewal, revocation, elevation at creation, the decision order and the station rechecks of ADR-006 and ADR-008 apply unchanged.

### 13.3 Tests

- Station: the listing of three-component refs only; ahead and behind; the 200 limit; no fetch in `project.branches`.
- Deck: the slug and session mapping; the conflict column; members refused.
- Coordinator (`crates/fleetdeck-deck/tests/fleet_git_coordinator.rs`): a grant with `inspect` lists branches; without it `not_granted`; `land_session` with `manage` queues a land; a target that holds a mutating grant is refused (§4.2 rule 3); the kill switch off refuses.
- A2 FG-10 (`fleet_git_api.rs`): the answers of `GET /api/projects/{id}/branches` and of the coordinator query match §13.1 for two pushed sessions on B and C.

## 14. FG-11: Config drift report (O33, O34)

### 14.1 Data

`ConfigDigest` in `station.info` (`crates/fleetdeck-station/src/drift.rs`):

```rust
pub struct ConfigDigest {
    pub claude_version: Option<String>,   // the probe version of the first Claude instance
    pub codex_version: Option<String>,    // the probe version of the first Codex instance
    pub items: Vec<DigestItem>,           // always the 4 items, in this order
    pub computed_at: i64,
}
pub struct DigestItem { pub name: DriftItem, pub hash: String }
pub enum DriftItem { ClaudeMd, ClaudeAgents, ClaudeSkills, CodexAgentsMd } // + Unknown
```

The items are `$HOME/.claude/CLAUDE.md`, `$HOME/.claude/agents/`, `$HOME/.claude/skills/`, `$HOME/.codex/AGENTS.md` (O33; `$HOME` of the station process).

### 14.2 The tree hash

- `hash` is 64 lower-case hex characters, or `absent` (the item does not exist), or `unreadable` (an I/O error), or `too_large` (a limit).
- A file: SHA-256 of `"f\0" + <size as decimal> + "\0" + <bytes>`.
- A folder: walk it without following links; skip nothing; sort the entries by the bytes of the relative path (with `/`). For each entry add to one SHA-256: a file `"f\0" <rel> "\0" <size> "\0" <bytes>`; a link `"l\0" <rel> "\0" <target bytes>`; a folder `"d\0" <rel> "\0"`; another type `"o\0" <rel> "\0"`. Limits: 10000 entries, 4 MiB for each file, 64 MiB in total.
- The station computes the digest at start and every 10 minutes, in a blocking task, and never sends a file content (O33).

### 14.3 Deck and board (O34)

- The reference station is `fleet_git_prefs.drift_reference`. Route `PUT /api/fleet-git/drift-reference { station }` (owner only, CSRF, audited `drift.reference`); `GET /api/fleet-git/prefs` (scope `read`).
- The board compares each station with the reference: the two versions and the four hashes. Labels: `claude`, `codex`, `CLAUDE.md`, `agents`, `skills`, `AGENTS.md`.
- Stations page (`web/src/board/StationConfigDrift.tsx`): a line "Config: Reference" on the reference station; "Config: Same"; "Config: Differs: skills, agents" (the labels that differ, in the order above); "Config: Unknown" for a station without `config`. The page header has "Config reference: <station>" with "Change", or "Pick a reference station for the config report." Each station menu has "Use as config reference". No sync happens (O34).

### 14.4 Tests

- Station (`drift_tests.rs`): equal trees give equal hashes; a renamed file, a changed byte, a new empty folder and a link each change the hash; `absent`; the limits give `too_large`; the hash never includes the absolute path.
- Board (`stationdrift.test.tsx`): same, differs with two labels, reference, unknown.
- A2 FG-11 (`fleet_git_api.rs`): stations A and B with equal homes report equal digests; a new file in B's `skills/` makes B differ in `skills` only.

## 15. FG-12: Taildrive artifact share (O35 to O39)

### 15.1 Rules (O35)

Fleet Deck runs only `tailscale drive list` and `tailscale status --json`. It never runs `tailscale drive share`, `rename` or `unshare`, and never changes the tailnet policy. `TailscaleCli` gets one new method, `drive_list()`; there is no other `drive` call in the code. A test greps the source tree of `crates/` for `"share"`, `"unshare"` and `"rename"` next to `"drive"` in an argument list and fails on a match. Taildrive is never used for code, worktrees, homes, remotes, moves or config sync.

### 15.2 The report (O36)

```rust
pub struct TaildriveReport {
    pub state: TaildriveState,              // ok, unknown, unavailable (+ Unknown)
    pub reason: Option<String>,             // for unknown and unavailable
    pub machine: Option<String>,            // this station's tailnet machine name
    pub shares: Vec<TaildriveShare>,
    pub checked_at: i64,
}
pub struct TaildriveShare {
    pub name: String,
    pub path: String,
    pub as_user: Option<String>,            // None: blank (Windows)
    pub exposed: Vec<Exposure>,             // empty: not exposed
    pub path_state: SharePathState,         // ok, missing (the path does not exist; exposure unknown)
}
pub enum Exposure { ProjectHome, Worktree, DataDir, ClaudeHome, CodexHome, Ssh, Gnupg, Aws, MemberDeny } // + Unknown
```

The station runs `drive_list()` at start and every 10 minutes (O36), with the `TailscaleCli` limits (3 s, 2 MiB, 64 KiB, the minimal environment, `tailscale_bin`), and puts the newest report in `station.info`.

| Result | State and reason |
| --- | --- |
| `TailscaleError::NotInstalled` | `unavailable`, "tailscale is not installed" |
| `NotRunning`, `Exit(n)`, `Signal`, `Spawn` | `unavailable`, "tailscale drive list failed (exit <n>)" (no stderr text; Taildrive off and no `drive:share` attribute end here) |
| `Timeout` | `unavailable`, "tailscale drive list took more than 3 s" |
| `TooLarge` or a parser refusal | `unknown`, "Fleet Deck cannot read the share list" |
| table read | `ok` |

### 15.3 The strict parser (`crates/fleetdeck-core/src/tailscale_drive.rs`)

Input: the stdout bytes. It must be UTF-8.

1. Split at `\n`. The last line must be empty (the output ends with `\n`). Every other line has no `\r` and no control character.
2. Line 2 (the dash line) must match `^(-{a})    (-{b})    (-{c})$` with `a ≥ 4`, `b ≥ 4`, `c ≥ 2`. The widths `a`, `b` are character counts.
3. Line 1 must equal `"name"` padded with spaces to `a` characters, 4 spaces, `"path"` padded to `b`, 4 spaces, `"as"`.
4. Each next line (a share): characters `[0, a)` are the name field, `[a, a+4)` must be 4 spaces, `[a+4, a+4+b)` the path field, `[a+4+b, a+8+b)` 4 spaces, the rest is `as`. A line shorter than `a+8+b` characters is refused.
5. The name is the name field without trailing spaces; it must be 1 to 64 characters of `[a-z0-9_() ]` with no leading space (the CLI normalizes names). Equal names are refused.
6. The path is the path field without trailing spaces; it must be absolute (`/` at the start, or a Windows drive `X:\`). A path that ends with a space cannot be told apart from the padding: the station checks the trimmed path, and a path that does not exist gets `path_state = missing` and `exposed = []`.
7. `as` is the rest as it is; empty → `None`; else 1 to 64 characters with no space.
8. At most 64 shares. Any refusal makes the whole report `unknown` (no partial list, no guess).

### 15.4 Exposure (O37)

`crates/fleetdeck-station/src/taildrive.rs`. The station canonicalizes the share path and each protected path, then compares by path components. A share **exposes** a protected path when the share path is the protected path, contains it, or is inside it. The protected paths:

- `ProjectHome`: the home paths of `git_projects` and `project_homes_local`.
- `Worktree`: the `worktree` of every session in the station store (all states).
- `DataDir`: the station data directory.
- `ClaudeHome`, `CodexHome`: `$HOME/.claude`, `$HOME/.codex`, and the `CLAUDE_CONFIG_DIR` and `CODEX_HOME` of every provider instance.
- `Ssh`, `Gnupg`, `Aws`: `$HOME/.ssh`, `$HOME/.gnupg`, `$HOME/.aws`.
- `MemberDeny`: a component of the share path matches a name pattern of the member deny list (`fleetdeck_proto::member_deny` plus `member_deny` of `station.toml`). The station does not scan the content of a share.

A protected path that does not exist is skipped. Exposure is computed again with each report and when a session starts.

### 15.5 The artifact share (O38)

Project setting `artifact_share = "<machine>/<share>"` (default none). At each start of a session of the project (the start effect, after the setup command and before the provider):

1. **Local path.** `machine` equals this station's machine name (the first label of `Self.DNSName` from `tailscale status --json`, lower case) and the newest report lists `share` with `path_state = ok` → the share path. Else `[taildrive_mounts]` has the key → the mount path. Else there is no local path.
2. A share on this station with `exposed` not empty → `artifacts.unavailable` with `share_exposed`; no variables (O37: an exposed share is never an artifact share).
3. **With a local path:** the path must exist, be a folder (`metadata`, links followed for a mount point) and be writable (`access(W_OK)`); else `artifacts.unavailable` with `mount_missing`, `mount_not_folder` or `mount_not_writable`, and the session starts without the variables. Then the station opens the local path and makes `<project folder>/<callsign>/` below it with `mkdirat` mode 0700 for each missing level, without following links (`project folder` is the §8.4 folder name). It sets `FLEETDECK_ARTIFACTS=<local path>/<project folder>/<callsign>/` for the provider and the setup command (after the child environment scrub, which removes `FLEETDECK_*`) and appends `artifacts.ready { path }`.
4. **Without a local path:** `FLEETDECK_ARTIFACTS_URL=http://100.100.100.100:8080/<tailnet>/<machine>/<share>/<project folder>/<callsign>/`, with each segment percent-encoded (a share name can hold a space). `<tailnet>` is `CurrentTailnet.Name` of `tailscale status --json` (a new optional field of the parsed `Status`). No folder is made. When the tailnet name is unknown: `artifacts.unavailable` with `tailnet_unknown` and no variables. The station appends `artifacts.ready { url }`.
5. A resume sets the same variables again from `session_git.artifacts_path` or `artifacts_url` after the same checks.

The deck also refuses to save an `artifact_share` that a station reports as exposed (400 `share_exposed`, "This share exposes <what>. Pick another share.").

### 15.6 The Artifacts link (O39)

- `FileRoot` gets a new variant `Artifacts { session_id }` (`fleetdeck-proto/src/files.rs`; an older station refuses the unknown root, so it fails closed). The station resolves it to `session_git.artifacts_path`, opens it without following links, and applies every rule of the file service below it, with the member deny list for a member. The allowed-roots check does not apply to this root (the station made the folder itself in step 3); it is the only exception, and it covers only that recorded folder.
- The session view gets "Artifacts" (new `web/src/session/ArtifactsLink.tsx`): with a local path it opens the existing file browser on the root `{ "artifacts": "<session id>" }`; with a URL only it shows "The artifacts are on <machine>. Open this address on a tailnet device: <url>" with "Copy address". A member of a shared session sees the link with the same deny list.
- The project prompt template can name `$FLEETDECK_ARTIFACTS` (O39). No other agent behavior changes.

### 15.7 Board (Stations page)

New `web/src/board/StationTaildrive.tsx` in each station card:

- `ok`: "Taildrive shares" and one row per share: "<name> · <path> · as <user>". An exposed share has a warning row: "This Taildrive share exposes <what>. Devices in your tailnet policy can read it, outside Fleet Deck's file rules." (O37). `<what>` joins the labels: "a project home", "a session worktree", "the Fleet Deck data folder", "the Claude home", "the Codex home", "~/.ssh", "~/.gnupg", "~/.aws", "a member-hidden path".
- No shares: "No Taildrive shares."
- `unknown`: "Taildrive: Fleet Deck cannot read the share list."
- `unavailable`: "Taildrive is not available: <reason>."
- A share with `path_state = missing`: "<name>: the path does not exist on this station."

The chat shows `artifacts.unavailable` with these texts: `share_exposed` "The artifact share exposes protected folders; the session started without FLEETDECK_ARTIFACTS.", `mount_missing` "The artifact mount <path> does not exist; …", `mount_not_folder` "… is not a folder; …", `mount_not_writable` "… is not writable; …", `tailnet_unknown` "The tailnet name is not known; the session started without FLEETDECK_ARTIFACTS_URL."

### 15.8 Tests

- Core (`tailscale_drive_tests.rs`): the CLI table from the source of 1.105.0 with 0, 1 and 3 shares; a name with spaces and parentheses; a non-ASCII path; a blank `as`; a wrong header; a wrong dash line; a short row; a tab; `\r`; a duplicate name; an upper-case name; 65 shares; a relative path → each refusal gives `unknown`.
- Station (`taildrive_tests.rs`): each exposure kind with real folders (is, contains, inside); a member-deny component; a missing protected path is skipped.
- A2 FG-12 (`crates/fleetdeck-cli/tests/fleet_git_taildrive.rs`, a fake `tailscale` program as in `station_tailnet_join.rs`, which writes each argv to a log file):
  - shares are reported in `station.info`;
  - unreadable output gives `unknown`;
  - a missing program gives `unavailable`;
  - a share over a project home, over `~/.claude` (a fake `HOME`) and over `~/.ssh` is `exposed`, and the deck refuses it as an artifact share;
  - a local share and a configured mount each set `FLEETDECK_ARTIFACTS` (the sim provider writes its environment to the stdin log) and make the session folder with mode 0700;
  - no local path sets `FLEETDECK_ARTIFACTS_URL`;
  - a missing mount emits `artifacts.unavailable` and the session still starts;
  - the argv log holds only `drive list` and `status --json` (no command that changes shares).

## 16. Test plan: acceptance map

### 16.1 The Rust rig (A2)

New `crates/fleetdeck-cli/tests/common/fleet_git.rs`: one deck and three stations (`alpha`, `bravo`, `charlie`, separate data dirs, `fake-claude sim` as the provider), one bare repository `remote.git` in the temp dir as `origin`, and one clone of it as the first home on each station that a test needs. It uses `common::deck::Deck` and `common::Station` (`deck_two_stations.rs` pattern). Helpers: `project(sync settings)`, `start_in(project, station)`, `bare_refs() -> BTreeMap<String, String>`, `advance_remote(commit)`, `set_station_clock(+h)` (a test-only `FLEETDECK_TEST_CLOCK_OFFSET_MS` read by the cleanup loop; the variable is ignored in a release build). All tests run on loopback.

| Acceptance item | Test file | Tests |
| --- | --- | --- |
| A2 FG-1 | `crates/fleetdeck-cli/tests/fleet_git_fetch.rs` | `an_old_local_head_starts_from_the_fetched_remote`, `a_gone_remote_starts_from_head_with_base_stale`, `a_blocking_fd_slug_branch_fails_the_start` |
| A2 FG-2 | `fleet_git_push.rs` | `two_stations_with_one_callsign_push_without_collision`, `a_revert_then_a_push_succeeds_with_the_lease`, `a_stale_lease_fails_and_retries`, `no_ref_outside_the_own_slug_is_pushed` |
| A2 FG-3 | `fleet_git_cleanup.rs` | `a_merged_clean_pushed_old_worktree_goes_to_the_trash`, `too_new_keeps_it`, `not_merged_keeps_it`, `untracked_file_keeps_it`, `unpushed_commit_keeps_it`, `a_trash_check_refusal_keeps_it` |
| A2 FG-4 | `fleet_git_include.rs` | `include_files_are_copied_with_their_mode`, `a_symlink_is_refused`, `a_dotdot_path_is_refused_by_the_deck`, `a_too_large_file_is_refused` |
| A2 FG-5 | `fleet_git_homes.rs` | `the_migration_keeps_every_existing_project_working`, `add_home_clones_on_a_second_station`, `add_home_adopts_an_existing_clone` |
| A2 FG-6 | `fleet_git_placement.rs` | `requires_places_on_the_labelled_station`, `prefer_always_on_wins`, `spread_uses_three_stations` |
| A2 FG-7 | `fleet_git_radar.rs` | `same_lines_will_conflict`, `different_files_of_one_file_set_do_not` |
| A2 FG-8 | `fleet_git_land.rs` | `a_clean_merge_land_advances_main`, `a_conflict_stops_with_files_and_changes_nothing`, `a_failed_check_stops_the_land`, `two_lands_of_one_project_run_in_order` |
| A2 FG-9 | `fleet_git_move.rs` | `a_claude_session_moves_with_its_history`, `the_source_session_is_moved` |
| A2 FG-10, FG-11 | `fleet_git_api.rs` | `branches_route_matches_the_design`, `coordinator_project_branches_matches_the_route`, `drift_same_and_differs` |
| A2 FG-12 | `fleet_git_taildrive.rs` | the eight items of §15.8 |
| A4 | the existing suites | no change; plus `fleet_git_off.rs::sync_off_sends_and_stores_nothing` (no `fleet_git.projects_set`, no `session_git` row, branch `fd/<callsign>`) |

Unit and module tests are listed in each section. Each new store table has a schema test (`the_migrations_are_valid` already covers the list).

### 16.2 Playwright (A3, phone viewport 390 × 844)

- `web/e2e/fleet-git.spec.ts` with fixtures in a new `web/e2e/fake-deck-git.ts` (`fake-deck.ts` is at 750 lines; it gets one import line):
  - project homes: the Homes list, "Add home" dialog, a `cloning` then `ready` row, a failed row text;
  - the land button, the confirm dialog, "Queued (1 ahead)", the conflict state with the files and "Send to session";
  - the "Not pushed" badge with its title, "Push now";
  - the radar levels: a "Will conflict" row above a "Conflict" row under a project header;
  - the Branches "Other stations" group with a session link and "Will conflict (2 files)";
  - the drift line "Config: Differs: skills, agents" and "Use as config reference";
  - the Taildrive shares and the exposed warning text of O37;
  - the session "Artifacts" link (local folder and URL-only).
- `web/e2e-real/fleet-git.spec.ts` (a real deck and one real station with a bare repository, `e2e-real/fleet.ts` helpers): the project settings save; a sync start shows the branch `fd/<slug>/<callsign>`; after a turn the session header shows "Pushed <oid7>"; a land in merge mode ends "Landed".

## 17. Board screens

### 17.1 Project settings: "Fleet Git" section

New `web/src/integrations/ProjectGitSettings.tsx`, inside the project form (owner only). The fields, their labels and their help lines:

| Field | Label | Control | Help line |
| --- | --- | --- | --- |
| `sync` | "Sync this project across stations" | switch | "Branches get the station name. Fleet Deck fetches before each new worktree." |
| `push` | "Push branches" | select "Off" / "After each turn" / "When the session ends" | "Fleet Deck pushes only fd/<station>/… branches that it made." |
| `push_remote` | "Remote" | text, placeholder `origin` | |
| `wip_remote` | "WIP remote URL (optional)" | text | "Pushes go here instead of the remote, for example a bare repository on an always-on server." |
| `default_branch` | "Default branch (optional)" | text | "Empty: the branch that the remote names." |
| `cleanup` | "Clean up merged worktrees" | select "Off" / "When merged" | "Moves the worktree to the trash. It never deletes a branch." |
| `cleanup_after_hours` | "After (hours)" | number 1–720 | |
| `worktree_include` | "Copy ignored files into new worktrees" | textarea, one path per line | "For example .env. Each file at most 1 MiB." |
| `worktree_env` | "Environment for agents and setup" | textarea, one NAME=value per line | "PATH, HOME, LD_*, DYLD_* and provider keys are not allowed." |
| `land` | "Land as" | select "Pull request" / "Merge" (the default shows "(default)") | |
| `check_cmd` | "Check command before landing (optional)" | text | "Runs on the station only when allow_check_cmd = true." |
| `artifact_share` | "Artifact share (optional)" | text, placeholder `nas/artifacts` | "A Taildrive share as <machine>/<share>." |
| `radar_home` | "Radar station" | select of the homes | "This station checks the branches for conflicts every 5 minutes." |

Save sends `PUT /api/projects/{id}/git` (owner only, CSRF, audited `project.git_settings`; 400 `invalid_git_settings` shows the message under its field). A save increments `git_revision` and sends `fleet_git.projects_set` to every home station.

### 17.2 Other screens

| Screen | Section | New UI |
| --- | --- | --- |
| Project page | §8.8 | "Homes", "Add home", "Lands" |
| Spawn form | §8.5, §9.3 | Home select, "Auto (best station)", Needs, Prefer, "Spread over stations" (new `web/src/spawn/PlacementFields.tsx`) |
| Fleet card | §4.4, §5.9 | "Stale base", "Not pushed", "Pushing…" |
| Session header and menu | §5.9, §6.5, §11.9, §12.8, §15.6 | "Pushed <oid7>", cleanup line, "Land", "Move to station…", "Moved to <station>", "Artifacts" |
| Conflict radar | §10.5 | project headers, "Will conflict" rows |
| Branches view | §13.1 | "Other stations" |
| Stations page | §9.4, §14.3, §15.7 | traits line, "Config: …", Taildrive section |

The near-limit board files get only an import and one element each: `SessionView.tsx` (739 lines), `SpawnForm.tsx` (710), `StationsPanel.tsx` (698).

## 18. Security notes, risks and open items

- **Pushes** (O10): the only refs are `fd/<own slug>/*` from `git_pushes` rows and, in merge mode, the default branch without force. A unit test asserts the refspec builder refuses every other ref. The A2 test checks the bare repository's refs.
- **Deletion:** cleanup uses the trash (restorable). The land temp folder and the move staging folder are station-made and deleted only by recorded identity. No recursive delete of a folder that the station did not make.
- **Deck migration:** additive columns and a new table; the `projects` copy keeps old readers correct.
- **Transcripts:** they pass through the deck in memory only, between two stations of the same deck, with SHA-256 and mode 0600. A transcript can hold secrets that the agent saw; the move is owner-only (or `manage`) and audited.
- **Repository configuration** can run programs during fetch and push (§2.4): same-UID, out of the model, as DL-142.
- **Landlock and network Git:** a credential helper that writes outside the allowed roots (for example `git-credential-store`, the credential cache socket, a `known_hosts` update) fails under `confine-exec`. Git ignores a failed `credential approve`, and SSH only warns about `known_hosts`. `operations.md` documents it. `fleetdeck doctor` gets a line that runs a `git ls-remote` of each sync project under `confine-exec`.
- **Taildrive:** read-only; a share over protected paths is a warning and is never an artifact share. Fleet Deck cannot stop the owner's tailnet policy from exposing a folder.
- **Open item:** ADR-010 may change §12.3 step 4 (where the files go, what is rewritten). The DESIGN REVIEW must read both documents together.
- **Open item:** the spec's "DL-201 group" is read as the `[repo_groups]` grouping (the first DL-201 entry), not the Omarchy entry with the same number.

## 19. Build plan

Rules for every task: one scoped brief (the CLAUDE.md delegation protocol); heavy commands through `sh /home/luismorales/projects/obsurvive/fleet-deck-worktrees/v2/tools/heavy.sh`; at most 2 workers at a time (machine limit); no file goes over 799 lines (`tools/module-size.sh`). Two tasks of one phase run in parallel only when their owned files do not overlap; the table names the order. "(new)" marks a new file. The near-limit files are marked with their line count today.

### Phase 1: FG-1, FG-2, FG-3, FG-4

| Task | Worker | Owned files | After |
| --- | --- | --- | --- |
| T1.0 Make room (no behavior change) | implementer | `crates/fleetdeck-proto/src/event.rs` (788) → `event_body.rs` (new) gets `EventBody` and its serde code; `crates/fleetdeck-station/src/store/schema.rs` (797) → `store/migrations.rs` (new) gets `migrations()`; `store/mod.rs`; `crates/fleetdeck-link/src/methods.rs` (769) → `methods_table.rs` (new) gets `METHODS`; `crates/fleetdeck-station/src/manager/decide.rs` (798) → `manager/decide_workspace.rs` (new) gets the worktree and callsign block of the start decision | — |
| T1.1 Proto and link | backend-dev | `crates/fleetdeck-proto/src/fleet_git.rs` (new), `fleet_git_tests.rs` (new), `event.rs`, `event_tests.rs`, `fold.rs`, `fold_tests.rs`, `session.rs`, `command.rs` (709), `version.rs`, `lib.rs`; `crates/fleetdeck-link/src/methods_table.rs`, `crates/fleetdeck-link/tests/method_table.rs`; `docs/v2/adr/ADR-004-link-protocol-v1.md` (§9.6 rows); `web/src/gen/**` (generated) | T1.0 |
| T1.2 Station Git operations | backend-dev | `crates/fleetdeck-station/src/workspace/fleet_git/mod.rs`, `fetch.rs`, `push.rs`, `include.rs`, `redact.rs` and their `*_tests.rs` (all new); `workspace/mod.rs` | T1.0 (parallel with T1.1) |
| T1.3 Station wiring | backend-dev | `store/schema_v28.rs` (new), `store/migrations.rs`, `store/git_state.rs` (new), `store/git_state_tests.rs` (new), `store/payload.rs`, `manager/decide_workspace.rs`, `manager/decide_fleet_git.rs` (new), `workspace/provision.rs` (751), `manager/setup_task.rs` (652), `manager/target_launch.rs`, `manager/policy.rs`, `manager/fleet_git_push.rs` (new), `manager/fleet_git_start_tests.rs` (new), `serve.rs` (637), `link/intake.rs` (745, dispatch lines only), `link/intake_fleet_git.rs` (new) | T1.1, T1.2 |
| T1.4 Cleanup | backend-dev | `manager/fleet_git_cleanup.rs` (new), `manager/fleet_git_cleanup_tests.rs` (new), `manager/trash.rs` | T1.3 |
| T1.5 Deck | backend-dev | `crates/fleetdeck-deck/src/store/fleet_git_schema.rs` (new), `store/schema.rs` (773, one list line), `store/projects.rs`, `store/projects_git.rs` (new), `http/project_git_routes.rs` (new), `http/mod.rs` (783, route lines only), `http/methods_table_rest.rs`, `link/fleet_git_sync.rs` (new), `link/mod.rs` (763, one call), `http/command_routes.rs`, `http/project_writes.rs` (631), `http/session_git_routes.rs` (new, push now), `tests/board_fleet_git_settings.rs` (new) | T1.1 (parallel with T1.3) |
| T1.6 Board | ux-dev | `web/src/integrations/ProjectGitSettings.tsx` (new), `ProjectForm.tsx`, `integrations/actions.ts`, `web/src/board/PushBadge.tsx` (new), `board/cardParts.tsx`, `web/src/session/timeline_fleet_git.ts` (new), `session/timeline.ts` (one call), the tests `projectgit.test.tsx`, `pushbadge.test.tsx` (new), `web/e2e/fleet-git.spec.ts` (new), `web/e2e/fake-deck-git.ts` (new), `web/e2e/fake-deck.ts` (750, one import) | T1.1 (parallel with T1.4) |
| T1.7 Integration tests | implementer | `crates/fleetdeck-cli/tests/common/fleet_git.rs` (new), `common/mod.rs` (one line), `fleet_git_fetch.rs`, `fleet_git_push.rs`, `fleet_git_cleanup.rs`, `fleet_git_include.rs`, `fleet_git_off.rs` (all new) | T1.4, T1.5 |

### Phase 2: FG-5, FG-6, FG-11, FG-12

| Task | Worker | Owned files | After |
| --- | --- | --- | --- |
| T2.0 Make room | implementer | `crates/fleetdeck-core/src/config.rs` (799) → `config_raw.rs` (new) gets `RawStation` and its key list; `crates/fleetdeck-core/src/tailscale.rs` (794) → `tailscale_parse.rs` (new) gets `parse_status`, `parse_whois` and the strict JSON visitor; `run` becomes `pub(crate)` | — |
| T2.1 Proto | backend-dev | `fleetdeck-proto/src/fleet_git.rs`, `fleet_git_homes.rs` (new), `station_traits.rs` (new), `drift.rs` (new), `taildrive.rs` (new), `integrations/projects.rs`, `link.rs` (704, four fields), `files.rs` (`FileRoot::Artifacts`), `version.rs`, `lib.rs`; `crates/fleetdeck-link/src/methods_table.rs`, `tests/method_table.rs`; ADR-004 §9.6 rows; `web/src/gen/**` | T2.0 |
| T2.2 Core and station | backend-dev | `fleetdeck-core/src/config_fleet_git.rs` (new), `config.rs`, `config_raw.rs`, `config_tests.rs`, `tailscale_drive.rs` (new), `tailscale_drive_tests.rs` (new), `tailscale_parse.rs` (CurrentTailnet), `lib.rs`; station `traits.rs`, `drift.rs`, `taildrive.rs`, `artifact_share.rs` (all new, with tests), `workspace/fleet_git/home.rs` (new), `store/schema_v29.rs` (new), `store/migrations.rs`, `store/homes_local.rs` (new), `link/intake_fleet_git.rs`, `link/intake.rs` (station.info lines), `manager/files.rs` (artifacts root), `workspace/files.rs`, `manager/setup_task.rs` (artifact env) | T2.1 |
| T2.3 Deck | backend-dev | `store/fleet_git_schema.rs`, `store/schema.rs`, `store/project_homes.rs` (new), `store/project_homes_tests.rs` (new), `store/projects.rs`, `store/project_sessions.rs`, `routing_place.rs` (new), `routing_place_tests.rs` (new), `http/project_home_routes.rs` (new), `http/placement_routes.rs` (new), `http/assign_routes.rs`, `http/projects_routes.rs` (673), `http/fleet_git_pref_routes.rs` (new), `http/mod.rs`, `http/methods_table_rest.rs`, `link/fleet_git_sync.rs`, `tests/board_project_homes.rs` (new) | T2.1 (parallel with T2.2) |
| T2.4 Board | ux-dev | `web/src/integrations/ProjectHomes.tsx` (new), `ProjectPage.tsx`, `ProjectForm.tsx`, `ProjectGitSettings.tsx`, `web/src/spawn/PlacementFields.tsx` (new), `spawn/SpawnForm.tsx` (710), `spawn/spawn.ts`, `web/src/board/StationTraits.tsx` (new), `StationConfigDrift.tsx` (new), `StationTaildrive.tsx` (new), `board/StationsPanel.tsx` (698), `web/src/session/ArtifactsLink.tsx` (new), `session/SessionView.tsx` (739), their tests, `web/e2e/fleet-git.spec.ts`, `web/e2e/fake-deck-git.ts` | T2.1 (parallel with T2.2 or T2.3) |
| T2.5 Integration tests | implementer | `fleet_git_homes.rs`, `fleet_git_placement.rs`, `fleet_git_taildrive.rs`, `fleet_git_api.rs` (drift part) (all new), `common/fleet_git.rs` | T2.2, T2.3 |

### Phase 3: FG-7, FG-8

| Task | Worker | Owned files | After |
| --- | --- | --- | --- |
| T3.1 Proto | backend-dev | `fleetdeck-proto/src/fleet_git_land.rs` (new: radar and land types), `command.rs` (`Land`), `event.rs`, `fold.rs`, `session.rs`, `version.rs`, `lib.rs`; link `methods_table.rs`, `tests/method_table.rs`; ADR-004 rows; `web/src/gen/**` | — |
| T3.2 Station | backend-dev | `workspace/fleet_git/radar.rs`, `land.rs`, `land_temp.rs` (all new, with tests), `manager/land.rs` (new), `manager/land_tests.rs` (new), `manager/target.rs` (788, the effect route line only), `store/payload.rs` (`EffectPayload::Land`), `store/schema_v30.rs` (new), `store/land_runs.rs` (new), `store/migrations.rs`, `link/intake_fleet_git.rs`, `manager/fleet_git_push.rs` (the `landed` rule) | T3.1 |
| T3.3 Deck | backend-dev | `store/fleet_git_schema.rs`, `store/schema.rs`, `store/land_queue.rs` (new), `store/radar.rs` (new), `fleet_git_land.rs` (new), `fleet_git_radar.rs` (new), `http/land_routes.rs` (new), `http/radar_routes.rs` (new), `http/mod.rs`, `http/methods_table_rest.rs`, `store/read_model.rs` (fold of `session.land` into the queue), `tests/fleet_git_land_deck.rs` (new) | T3.1 (parallel with T3.2) |
| T3.4 Board | ux-dev | `web/src/board/ConflictRadar.tsx`, `board/RadarGroups.tsx` (new), `web/src/session/LandPanel.tsx` (new), `session/SessionView.tsx`, `web/src/integrations/ProjectLands.tsx` (new), `ProjectPage.tsx`, tests, `web/e2e/fleet-git.spec.ts`, `web/e2e/fake-deck-git.ts` | T3.1 (parallel with T3.2 or T3.3) |
| T3.5 Integration tests | implementer | `fleet_git_radar.rs`, `fleet_git_land.rs` (new), `common/fleet_git.rs` | T3.2, T3.3 |

### Phase 4: FG-9, FG-10 (after ADR-010 is accepted)

| Task | Worker | Owned files | After |
| --- | --- | --- | --- |
| T4.1 Proto | backend-dev | `fleetdeck-proto/src/fleet_git_move.rs` (new), `command.rs` (`MoveOut`, `MoveIn`, `MoveFinish`), `mutation.rs` (654, coordinator variants), `event.rs`, `fold.rs`, `session.rs`, `version.rs`, `lib.rs`; link `methods_table.rs`, `tests/method_table.rs`; ADR-004 rows; `web/src/gen/**` | ADR-010 |
| T4.2 Station move | backend-dev | `workspace/fleet_git/move_export.rs`, `move_import.rs` (new), the provider layouts named by ADR-010, `manager/move_session.rs` (new), `manager/move_tests.rs` (new), `store/schema_v31.rs` (new), `store/session_moves.rs` (new), `store/migrations.rs`, `link/stream.rs` (275), `link/intake_fleet_git.rs`, `manager/decide_fleet_git.rs` (the hold refusals) | T4.1 |
| T4.3 Station branches and coordinator tools | backend-dev | `workspace/fleet_git/remote_branches.rs` (new), `coordinator/git_tools.rs` (new), `coordinator/fleet_tools.rs` | T4.2 (it shares `intake_fleet_git.rs`; it runs after T4.2) |
| T4.4 Deck | backend-dev | `store/fleet_git_schema.rs`, `store/schema.rs`, `store/session_moves.rs` (new), `fleet_git_move.rs` (new), `http/move_routes.rs` (new), `http/project_branch_routes.rs` (new), `link/streams.rs`, `store/coordinator_git.rs` (new), `store/coordinator_ops.rs` (741, dispatch lines only), `store/coordinator_query_reads.rs`, `http/mod.rs`, `http/methods_table_rest.rs`, `tests/fleet_git_coordinator.rs` (new) | T4.1 (parallel with T4.2) |
| T4.5 Board | ux-dev | `web/src/session/OtherStations.tsx` (new), `session/BranchesView.tsx`, `session/MoveDialog.tsx` (new), `session/SessionView.tsx`, tests, `web/e2e/fleet-git.spec.ts`, `web/e2e/fake-deck-git.ts`, `web/e2e-real/fleet-git.spec.ts` (new) | T4.1 |
| T4.6 Integration tests | implementer | `fleet_git_move.rs` (new), `fleet_git_api.rs`, `common/fleet_git.rs` | T4.2, T4.3, T4.4 |

### Phase 5: documents and the final review

`docs/v2/operations.md` (every new setting, the hook decision of §2.3, the push credentials of a service (an SSH agent or a credential helper that the service user can reach; `fleetdeck service install` from a terminal), the Landlock note of §18, the Taildrive policy and the mount steps for an artifact share), `README.md` (the feature list), `docs/v2/internals.md` (the link methods and the tables), `docs/v2/BACKLOG.md` (B1 done), `docs/v2/PROGRESS.md` ("Fleet Git" section). Then the adversary PRE-PR REVIEW of `fleet-git` against `v2` (at most 3 rounds).

## 20. Decisions made in this draft (for the architect)

The owner spec does not fix these choices. Each line gives the choice and one reason. The architect accepts or changes each one.

1. **"DL-201 group" means the `[repo_groups]` grouping** (the first DL-201 entry, M3 6-1), not the Omarchy DL-201. Reason: only that entry defines a group for repositories; the log has two entries with the number.
2. **Slug:** consecutive `-` are kept, the slug is cut to 40 characters. Reason: the spec says "changed to `-`" and names no collapse; 40 keeps refs short.
3. **One slug for two stations of a project** → 409 `slug_taken` on add home and on a sync start. Reason: one namespace per station is the base of O10 and of the FG-2 acceptance.
4. **D/F conflict:** a namespace check after the fetch; a blocking `fd/<slug>` fails the start with `branch_namespace_blocked` and a rename hint. Reason: a silent other branch name would break the O10 push filter.
5. **Fleet Git applies to worktree starts only;** a start in place gets no `FleetGitStart`. Reason: O6 names a new session branch; a start in place makes none.
6. **An owner-chosen branch is never pushed.** Reason: O10 allows only `fd/<own-slug>/*` that the station created.
7. **Settings delivery:** a full `fleet_git.projects_set` per station (at link start and on change) plus a snapshot in each start; the loops read the newest settings. Reason: a change of `push` or `cleanup` must reach running sessions; the snapshot removes the start race.
8. **Fetch:** `--prune --no-tags --no-recurse-submodules --no-write-fetch-head` of `push_remote`. Reason: smallest write set; tags and submodules are not needed for a base.
9. **Default branch order:** setting, `refs/remotes/<remote>/HEAD`, `main`, `master`; none → `base_stale` with `no_default_branch`. Reason: works for clones that lack `origin/HEAD`.
10. **Git 2.45 floor** for every Fleet Git command (`git_too_old`). Reason: the existing branch view floor; `merge-tree --name-only` needs 2.38 anyway.
11. **Network Git runs under `confine-exec`** (fetch, push, land worktree and rebase). Reason: the same write boundary as DL-200; the known cost (credential stores outside the roots) is documented.
12. **`workspace.base_stale` comes after `session.started`,** and the summary gets `base_stale`. Reason: the event needs the session stream; the card needs a durable flag.
13. **`wip_remote` is a URL, `push_remote` is a remote name.** Reason: a WIP server needs no remote setup in each home; the URL rules of DL-200 apply.
14. **Push triggers:** `on_turn` = each `turn.completed` and the end; `on_exit` = the first end the station sees. Reason: the end catches commits of a last turn that failed.
15. **One push at a time per station.** Reason: bounded network and Git load; pushes are small.
16. **Timeouts:** push 60 s, `ls-remote` 20 s, fetch 30 s (O7), merge-tree 20 s per pair, rebase and land worktree 120 s. Reason: the DL-144 and provisioning limits.
17. **Lease recovery:** after a failure, `ls-remote`; a remote tip equal to `pending_oid` counts as pushed. Reason: a crash after a successful push must not leave a stale lease forever.
18. **Backoff detail:** a new trigger while `failing` keeps `attempts` and the next time; "Push now" moves only the time. Reason: no burst against a failing remote; the owner still has control.
19. **Push state `stopped`** (project gone, moved session, landed and diverged) shows no badge. Reason: it is a final state, not a failure.
20. **Badge texts:** "Not pushed" for `failing`, "Pushing…" after 60 s, "Pushed <oid7>" in the header only. Reason: the card shows only what needs the owner.
21. **Cleanup timing:** first run 5 minutes after the start, then every 15 minutes. Reason: O13 counts hours; 15 minutes is precise enough and cheap.
22. **"Merged" also counts a squash or rebase merge:** ancestor, or `merge-tree` of the branch into the default branch gives the default tree. Reason: forge merges are often squashes; the test is conservative.
23. **Cleanup calls the trash decision through `decide_cleanup`** (no new `CommandOrigin`); op id `cleanup-<session>-<ms>`. Reason: `CommandOrigin` has no `#[serde(other)]`, so a new value is not additive.
24. **`ended_at`** is the first `exited`/`lost` time that the station saw. Reason: the summary has no end time.
25. **Include patterns:** `*` and `?` inside one segment, no `**` or `[`; a hidden name needs a pattern that starts with `.`; at most 32 patterns and 200 matches; new folders mode 0700; an existing file is skipped, never overwritten. Reason: bounded work and no change of tracked files.
26. **A refused include file does not fail the start;** it is listed in `workspace.include_skipped` (at most 50 entries). Reason: the same rule as O8 (a start does not fail for a bootstrap problem).
27. **Env limits:** at most 32 names, values up to 4096 characters; also refused: `GIT_*` and every `childenv` scrubbed name or prefix; the provider instance `env` wins on a clash. Reason: `GIT_*` can redirect Git; the instance account must not be overridden.
28. **`Project.station_id`/`path` stay as a copy of the first home.** Reason: older boards and DL-176 code keep working (additive).
29. **`remote_url` comes from `WorkspaceEntry.origin` of the first home,** editable. Reason: the DL-201 catalog already reads it without a Git process.
30. **Clone folder name:** the project name with characters outside `[A-Za-z0-9._-]` changed to `-`, no leading `.`, 80 characters. Reason: the DL-200 plain-name rule.
31. **`clone_root` is made (mode 0700) only when its parent is an allowed root.** Reason: no recursive creation outside a known root.
32. **`project.ensure_home` answers at once; the deck polls `project.home_status` every 10 s.** Reason: a clone can take 15 minutes; a link request must not wait that long.
33. **A clone stopped by a restart is `failed` (`clone_interrupted`) and its folder stays.** Reason: DL-200 forbids a recursive delete.
34. **Remove home** exists on the deck only; it refuses the last home and a home with a live session; the folder stays. Reason: the owner needs a way back; files are never deleted.
35. **`project_of` looks at every home,** and a project with `repo` also matches by `summary.repo.origin` on any station. Reason: sessions on new homes must belong to the project.
36. **The deck attaches `FleetGitStart` when station and `cwd` equal a home of a sync project;** a start body may also name `project_id`. Reason: a start from the plain spawn form in a home must follow the project.
37. **`on_battery`:** Linux supplies rule, WSL always false, macOS `pmset`, read at most every 60 s; unknown is `None`. Reason: O19 names the sources; `None` must not count as "not on battery".
38. **Requirements accept `os:<os>` and `arch:<arch>`** besides labels. Reason: the automatic traits are otherwise not usable in `requires`.
39. **Placement score:** preferences, then `min(1 - load5/cores, mem_available/mem_total)`, then live sessions, then name; `idle` = no session working, verifying or queued. Reason: O20 order with one number for "free CPU and RAM".
40. **`spread`:** a station that already got a session of the batch sorts after the others. Reason: "different stations when possible" with a fallback.
41. **Placement is a deck route (`POST /api/placement`); the board still sends the starts.** Reason: DL-202 makes a batch a board expansion.
42. **The deck drives the radar** (every 5 minutes and 30 s after a push of the project; one run per project at a time). Reason: only the deck knows the live sessions of all stations.
43. **Radar limits:** 120 s per run, 100 files per pair; pair order: each branch against the default branch first, newest first. Reason: O21 caps pairs; a slow run must end.
44. **Only pushed branches take part in the radar.** Reason: the radar home sees only fetched refs; O22.
45. **`GET /api/radar` is new; `GET /api/conflicts` and the "Conflict" label stay.** Reason: A4 (unchanged suites) and an older board.
46. **Land allowed on `exited`, `lost`, or idle with no open request.** Reason: O23 says "ended or idle"; an open approval means work in progress.
47. **Land route scope `admin`, owner login only; cancel only a `queued` land.** Reason: a land pushes to remotes; a running land cleans up itself.
48. **Land temp worktree at `<parent of the home>/.fleetdeck-land/<home>--<12 chars>`,** deleted by recorded identity. Reason: Landlock allows writes only inside the roots; the DL-150 delete rule.
49. **Rebase options** (`autoStash=false`, `updateRefs=false`, `core.editor=true`, `--no-autosquash --no-fork-point`); a missing identity fails with `land_no_identity`. Reason: a non-interactive and predictable rebase; no invented committer.
50. **`check_cmd` runs with the DL-148 runner** (10 minutes, marker, restart cleanup); the output is the last 50 lines, at most 16 KiB. Reason: the spec names the DL-148 rules.
51. **PR title and body:** the session title (else the branch) and the digest (else a fixed line). Reason: the PR needs text without third-party content.
52. **Merge mode with `wip_remote`:** the branch goes to the WIP remote, the default branch to `push_remote`. Reason: the default branch must reach the real remote.
53. **After a land the local session branch is not changed;** auto-push stops with `landed` when the local branch diverges. Reason: no change under a session; no force push over a landed branch.
54. **"Send to session" text** (§11.6) and only for a live idle session. Reason: a fixed prompt without third-party text; an ended session needs a resume first.
55. **A land lost by the station (`land_lost`) after a deck restart fails.** Reason: the queue must not block a project forever.
56. **The deck drives the move** in five steps with a hold; an idle live session is stopped gracefully at the hold. Reason: the transcript must not change during the copy.
57. **The deck pipes the transcript stream in memory;** staging on the target under the data dir; at most 256 files. Reason: O29 mode and size rules; no copy on the deck disk.
58. **The target makes `fd/<target slug>/<callsign>` at the source's pushed commit;** the source ref stays on the remote. Reason: O10 (a station pushes only its own namespace).
59. **The target keeps the source callsign when it is free.** Reason: the owner recognizes the session.
60. **`moved` is `summary.moved_to` on an `exited` session** (no `SessionState` value); a moved session refuses every input. Reason: `SessionState` is not additive (§1.6).
61. **Branch-only first prompt** (§12.6) with the summary inside the untrusted fence. Reason: the summary can quote tool and file text (ADR-008 §4.3).
62. **Before ADR-010 is accepted, both providers are `BranchOnly`.** Reason: O27 requires the spike first.
63. **`project.branches` runs on the radar home, lists only three-component `fd/*/*` refs, at most 200, without a fetch;** `project.fetch` is a separate call. Reason: one station with all remote refs; a read stays fast.
64. **Coordinator tool names** `project_branches`, `project_fetch`, `land_session`, `move_session`; commit subjects are fenced. Reason: the ADR-008 snake_case style; Git text is third-party text.
65. **Drift hash framing** (§14.2), limits 10000 entries / 4 MiB / 64 MiB, recomputed every 10 minutes, `$HOME` of the station process. Reason: deterministic, content-free, bounded.
66. **Drift versions** come from the first provider instance of each kind. Reason: the probe already has them (`StationProvider.version`).
67. **The drift reference is a deck preference** (`fleet_git_prefs`); no reference → "Pick a reference station". Reason: O34 says the owner selects it; it is board state.
68. **Taildrive interval 10 minutes and at start** (O36); a non-zero exit is `unavailable` with "exit <n>" and no stderr text. Reason: `TailscaleCli` keeps stderr out of answers by design.
69. **Parser:** names per the CLI source (`[a-z0-9_() ]`), at most 64 shares, any refusal makes the whole report `unknown`; a path that does not exist gets `path_state = missing`. Reason: O36 "never a guess".
70. **Exposure also covers the config dirs of every provider instance,** and the member deny list by path component only (no scan of the share). Reason: an instance home is a provider home; a scan of a share is unbounded.
71. **Machine name** = first label of `Self.DNSName`; **tailnet name** = `CurrentTailnet.Name`; URL segments percent-encoded. Reason: these are the names of the Taildrive path; share names hold spaces.
72. **Artifact folders:** `<project folder>/<callsign>/` with mode 0700 for each missing level, `mkdirat` without following links; a resume sets the variables again. Reason: O38 and the B9 link rules.
73. **`FileRoot::Artifacts` is the one exception to the allowed-roots check,** for the recorded folder only; members see the link with the deny list. Reason: a mount can be outside the roots; O39 keeps the deny list.
74. **The deck refuses to save an exposed `artifact_share`** (400 `share_exposed`). Reason: earlier feedback than the start event.
75. **Five features, one per phase** (`fleet_git.v1`, `.homes.v1`, `.land.v1`, `.move.v1`, `.branches.v1`). Reason: each phase merges and tests alone; the deck gates each action.
76. **409 `fleet_git_unsupported`** for a station without the feature. Reason: the DL-148 `setup_not_supported` pattern.
77. **Error codes and texts** of every failure table. Reason: DL-163 needs a code and a sentence.
78. **Redaction:** the last stderr line, URL credentials and token prefixes masked, 300 characters. Reason: DL-163 and DL-164 scrub rules.
79. **Migrations:** station V28–V31, deck one migration per phase, SQL in new files. Reason: `schema.rs` files are near 800 lines.
80. **New method rows go into the ADR-004 §9.6 table** (the table test reads it); ADR-009 explains them. Reason: no change to the test harness.
81. **`session.land`, `MoveOut`, `MoveIn`, `MoveFinish` are receipt mutations; all other new methods are not.** Reason: they change state that a retry must not repeat; the others are idempotent reads or replace-all writes.
82. **Every new wire enum has `#[serde(other)] Unknown`.** Reason: a newer station must not break an older deck.
83. **Refactor tasks first** (T1.0, T2.0) and the new file names of §19. Reason: the 800-line rule.
84. **Test clock** `FLEETDECK_TEST_CLOCK_OFFSET_MS` (test builds only) for the cleanup age. Reason: A2 FG-3 needs a 24-hour age without waiting.
85. **`fleetdeck doctor` line:** a confined `ls-remote` of each sync project. Reason: shows the Landlock credential limit before a push fails.
