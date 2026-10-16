# B9: a verified launch directory for every start

- **Status:** Accepted, revision 4 (2026-10-16, owner decision DL-200 after design review round 3). High risk (the allowed-roots boundary): adversary DESIGN REVIEW round 1 FAIL (`/tmp/adv-b9-design-report.md`), round 2 FAIL (`/tmp/adv-b9-design-r2-report.md`: provisioning); round 3 FAIL (`/tmp/adv-b9-design-r3-report.md`: provisioning by descriptor); the owner chose Landlock for provisioning (DL-200), which this revision uses; a PRE-PR REVIEW before the merge.
- **Backlog:** B9 (from the B7 review).
- **Related:** `Policy::workspace` (`manager/policy.rs`), `canonical_workspace` (`providers/adapter.rs`), `workspace/files.rs` (`open_root`, `pinned_dir`), the shim launch (`host/shim/launch.rs`, `server_proc.rs`), DL-189 and B7 (other-user trust of transcripts).

## 1. The gap

The station checks a session folder by path: `Policy::workspace(cwd)` canonicalizes it (links followed) and tests `starts_with` against the canonical allowed roots. The decide stores the canonical string in the effect. After that, the path string is resolved again by name in several places before the process starts (Git in the start preparation, `canonical_workspace` in the drivers, `Command::current_dir` in the direct host and in the shim, the setup command, the `!` command, the precheck, the terminal shell).

A process that replaces a component of the checked path by a link between the check and a later resolution can make the station launch outside the allowed roots.

## 2. What the allowed roots are, and the threat model

- **The allowed roots are a launch policy of the station, not a sandbox.** They limit where a deck command can make the station start a process. They do not limit what a running provider or its tools do: Claude has no sandbox, a Codex agent with a write mode writes where its sandbox allows, and any agent can `cd` through a link. A separate OS user for agents is the only real boundary (DL-198 §12, a later owner decision).
- **B9 guarantee:** every process that the station itself starts for a session (provider start and resume on both hosts, the setup command, the `!` command, the precheck, the terminal shell) starts in exactly the directory object that the station checked inside the roots, or does not start. Git that the station runs to prepare a start runs in that object too.
- **In the threat model:** a process that can write inside the allowed roots and replaces a path component by a link, or renames a component inside the roots, during a launch.
- **Out of the threat model:**
  1. A process that can write **outside** the allowed roots (for example it renames an ancestor directory to a place outside the roots after the check). Such a process already starts what it wants where it wants; the roots give no boundary against it. A descriptor pins an object, not its ancestry (review round 1, finding 2): this is stated, not solved.
  2. What a provider does with paths after its process started: its tools resolve paths all the time. Codex receives the workspace path in `thread/start`/`thread/resume`/`thread/fork` and resolves it for every tool call and for a `!` command (`thread/shellCommand`); binding only its initialization would not change the guarantee (review round 1, findings 3 and 4). The station starts the app-server process in the pinned directory (§3.3) and checks the identity once more after `session.started` (§3.5) as a detection, not as a guarantee.
  3. The provider's own transcript lookup at a resume, for a process of the station user: it can write the transcript file itself; other users are covered by DL-189/B7.
  4. macOS (review round 1, finding 5): §6.

## 3. Design

### 3.1 The launch identity

`crates/fleetdeck-core/src/launch_dir.rs` (new; `fleetdeck-core` is below the station and the providers, so both use it):

- `pub struct LaunchIdentity { pub root: PathBuf, pub rel: PathBuf, pub dev: u64, pub ino: u64 }` (`Serialize`/`Deserialize`): `root` is the canonical allowed root that contains the path (the longest matching root), `rel` the relative rest (normal components only; empty for the root itself).
- `pub struct LaunchDir { fd: OwnedFd, identity: LaunchIdentity }`.
- `LaunchDir::open(root, rel) -> Result<LaunchDir, LaunchDirError>`: `root_fd = files::open_root(root)` (the no-follow walk; roots are canonical and hold no link); on Linux `rustix::fs::openat2(root_fd, rel, O_RDONLY | O_DIRECTORY | O_CLOEXEC, RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS)` (one atomic kernel resolution: no link, no `..` escape); `ENOSYS` (a kernel before 5.6) → the `openat` no-follow walk of `files::descend` from `root_fd` (documented as weaker against a concurrent rename inside the roots); `fstat` gives `(dev, ino)`.
- `LaunchDir::reopen(identity) -> Result<LaunchDir, LaunchDirError>`: `open(identity.root, identity.rel)`, then `(dev, ino)` must equal, else `Changed`.
- `LaunchDir::cwd(&self) -> Result<PathBuf, LaunchDirError>`: Linux: `/proc/self/fd/<n>` (`files::pinned_dir`), and the station checks that the path is a directory (`metadata(..).is_dir()`); otherwise `Err(Unavailable)` (no fallback to the plain path; review round 1, finding 6). Other systems: `Err(Unsupported)` unless §6 applies.
- `LaunchDir::path(&self)`: `root.join(rel)` (for texts and for the provider's `cwd` string).
- Texts: `Changed` and any open failure: "The session folder changed: a link is in its path." (`launch_dir_changed`); `Unavailable`: "The station cannot hold the session folder open (no /proc)." (`launch_dir_unavailable`).

### 3.2 Where the identities are taken

At the station's **dispatch** of a start, resume or import (`target.rs` `dispatch`, before `start_refusal` → `base::planned` → `defer_start`), after `policy.workspace` of the existing directory:
- **A start in an existing folder, a resume, an import:** `LaunchDir::open(root, rel)` of the canonical `spec.cwd`; this identity is the session's launch identity.
- **A new worktree** (`spec.workspace` is a worktree plan): two preparation identities: the **source repository** (the folder that the decide checked before it replaced `spec.cwd` with the destination) and the **destination parent** (an existing directory inside the roots). The destination itself does not exist yet (`provision.rs` requires that); its identity is taken when the station creates it (§3.3.1).
- **A clone:** one preparation identity, the **destination parent**; the destination is created as in §3.3.1.
- A failure fails the start with the text. All identities are stored in the effect payload's spec (§3.4) so that the setup task, the provisioning and the launch use them; after a station restart they are reopened with `reopen`, never trusted without it.

### 3.3 The launches

1. **Start preparation (review round 1 finding 7; rounds 2 and 3, provisioning; DL-200):**
   - Git for the base (`workspace/base.rs`) runs with `Git::new(source.cwd()?)` (the pinned source repository, the `branches.rs` pattern).
   - **Provisioning is confined by Landlock (Linux):** every Git command of a new worktree or a clone (`workspace/provision.rs`, `clone_reuse.rs`) runs through a new hidden subcommand `fleetdeck confine-exec --write-root <root>… -- <git> <args>`. The helper opens each allowed root with `files::open_root` (roots are canonical and hold no link; renaming a root needs write access outside the roots, §2 item 1), builds a Landlock ruleset that handles every filesystem write right of the best available ABI (`landlock` crate, best effort), allows them `PathBeneath` each root descriptor and on `/dev/null`, calls `restrict_self`, and `exec`s Git. Reads are not restricted. Git keeps its path arguments (the planned destination, `git -C <source>`), so Git and its children can write only below the root objects, whatever a link swap does to a path: a redirected write fails with `EACCES` and the start fails.
   - **The station requires full enforcement:** the helper reports the Landlock status before `exec` (a status line on stderr that the station reads: `fully_enforced`, `partially_enforced`, `not_enforced`). `not_enforced` (no Landlock in the kernel) → the start goes on with **detection only** (below) and a warning in the station log; `fleetdeck doctor` shows a `warn` line "Landlock is not available: worktree and clone provisioning is checked after Git, not confined." `partially_enforced` counts as enforced for the write rights that the ABI knows.
   - **After Git:** the station opens the planned destination with `LaunchDir::open(root, rel)` (`openat2`, no link); this is the session's launch identity. A failure (a link in the path, a missing folder) fails the start with the text.
   - **Cleanup never deletes recursively** (round 3 finding 1): a failed or cancelled provisioning leaves the destination in place; the start's failure text names its path; the existing worktree inventory shows it to the owner.
   - **macOS:** the same confinement with Seatbelt (`sandbox-exec` with a profile that denies `file-write*` outside the roots) is part of the deferred macOS work (§6, DL-200); until then macOS provisioning is detection only.
2. **Setup command, `!` command (Claude), precheck, terminal shell:** each reopens the identity (or opens its checked path) right before its spawn and uses `current_dir(launch.cwd()?)`; portable-pty: `cmd.cwd(launch.cwd()?)` after the station's own `is_dir` check (portable-pty's HOME fallback cannot happen for a path that the station just checked; the `LaunchDir` stays alive until the spawn returned).
3. **Direct host:** `ProcessSpec` gains `pub launch: Option<LaunchIdentity>`. With it, the drivers do **not** call `canonical_workspace`: they use `identity.root.join(identity.rel)` as the path string (review finding 1), and `DirectChildHost::spawn` runs `LaunchDir::reopen(identity)` and `current_dir(launch.cwd()?)`, keeping it alive until `spawn` returned. Without an identity (tests, the local CLI), today's behavior.
4. **Shim host:** `LaunchSpec` gains `#[serde(default)] pub launch: Option<LaunchIdentity>` (`provider_from` copies it). The shim (`server_proc::spawn_provider`) runs `LaunchDir::reopen` and `current_dir(launch.cwd()?)` for `shim-exec`, keeping it alive until the spawn returned; a failure is `LaunchReply::Failed` with the text.
5. **Production launches need the identity:** `SessionTarget::start`, `start_durable` and `resume_durable` refuse a spec without `launch` (`launch_dir_missing`), so no station path reaches a provider launch without it. Restore and reattach start no new process and need none.

### 3.4 Transport

`fleetdeck_proto::command::SessionSpec` gains `#[serde(default, skip_serializing_if = "Option::is_none")] #[ts(skip)] pub launch: Option<LaunchIdentity>`: it is stored in the station's effect payload (so the setup task and a restart see it) and is **never** accepted from the deck: the station's decide clears it on every incoming command (`spec.launch = None`) before anything else. (`LaunchIdentity` moves to `fleetdeck-proto` if `fleetdeck-core` cannot be used by the proto crate; the type is plain data.)

### 3.5 Codex after the start

After `session.started`, the station reopens the identity once; a mismatch stops the session (`StopMode::Kill`) and fails the start with the text. This is a detection (§2, out of the threat model item 2).

## 4. Tests (Linux)

- `launch_dir`: open of a plain dir; a link at the last and at an inner component fails; `..` in `rel` is refused; `reopen` after the dir was renamed and replaced by a link fails; `cwd()` names the original object after its path was swapped (a child's `pwd -P` prints the original directory); `cwd()` when `/proc` is not mounted in a test namespace is skipped unless possible — at least a unit test of the `is_dir` refusal through an injected checker.
- Direct host and shim host: a spawn whose path was swapped for a link after the identity was taken fails; a plain one starts in the folder; a driver with an identity does not canonicalize (a swapped path is never followed).
- Station: a start whose folder is swapped between decide and dispatch fails with `launch_dir_changed` and runs no Git and launches nothing; `confine-exec`: a write below a root works, a write outside the roots (also through a link inside a root that points outside) fails with `EACCES`, the status line is printed; a worktree start and a clone start whose destination parent is replaced by a link to a folder outside the roots right before the Git step write nothing outside the roots and fail; a failed provisioning leaves its destination in place; a `!` command, a setup command and a precheck after a swap do not run; an incoming command with `spec.launch` set from the deck has it cleared; a production launch without an identity is refused.
- Codex: a swap between the spawn and `session.started` stops the session.

## 5. Docs

`SECURITY-CHECKLIST.md`: a row for B9 (what it guarantees, the out-of-model items of §2, the `ENOSYS` fallback). `operations.md`: the texts.

## 6. macOS

The owner deferred macOS work to a run on a Mac (shim parity step 4). On macOS this design keeps today's path behavior plus `LaunchDir::open`/`reopen` checks right before each spawn (the window between the check and the child's `chdir` remains). Step 4 adds the `libproc` crate; with it, the station checks the child's working directory identity right after the spawn (`proc_pidinfo` `PROC_PIDVNODEPATHINFO`) and kills a mismatch. This is recorded as a macOS item for step 4, not built here.
