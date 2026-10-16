# B9: a verified launch directory for every start

- **Status:** Draft, revision 1 (2026-10-16). High risk (the allowed-roots boundary): an adversary DESIGN REVIEW before code, a PRE-PR REVIEW before the merge.
- **Backlog:** B9 (from the B7 review).
- **Related:** `Policy::workspace` (`manager/policy.rs`), `canonical_workspace` (`providers/adapter.rs`), `workspace/files.rs` (`open_root`, `pinned_dir`), the shim launch (`host/shim/launch.rs`, `server_proc.rs`), DL-189 and B7 (other-user trust of transcripts).

## 1. The gap

The station checks a session folder by path: `Policy::workspace(cwd)` canonicalizes it (links followed) and tests `starts_with` against the canonical allowed roots. The decide stores the canonical string in the effect. After that, the path string is resolved again by name in several places before and after the process starts (fact sheet: `start_refusal`, Git `current_dir`, `canonical_workspace` in the drivers, `Command::current_dir` in the direct host and in the shim, Codex `thread/start { cwd }`).

A process of the station user that replaces a folder component with a link between the check and a later resolution can move the launch outside the allowed roots. The allowed roots limit what a deck command can choose; the local user is trusted, but an agent of another session runs as that user and can be steered by its input.

## 2. Threat model

- **In scope:** a process of the station user (for example an agent of another session) that swaps a component of the checked path for a link, or renames a directory and puts a link in its place, while a start, a resume, a setup command, a `!` command, a precheck or a terminal shell is being launched.
- **Out of scope:** (a) what a running provider does with paths after its start (its tools resolve paths all the time; Codex resolves `thread/start.cwd` itself); the station checks the identity once more after `thread/start` (§4.4) and does not go further; (b) the provider's own transcript lookup at a resume: a process of the station user can write the transcript file itself, so a link swap gives it no new power; other users are covered by DL-189/B7 (`trusted_home`, `trusted_path`, the import pin); (c) a component replaced by a real directory at the same path: the launch stays at a path inside the roots.

## 3. Design

### 3.1 The launch identity

`workspace/launch_dir.rs` (new):

- `pub struct LaunchDir { fd: OwnedFd, path: PathBuf, dev: u64, ino: u64 }`.
- `LaunchDir::open(path: &Path) -> Result<LaunchDir, LaunchDirError>`: `path` must be absolute with only normal components; `files::open_root(path)` (an `openat` walk from `/` with `O_NOFOLLOW | O_DIRECTORY` at every component: a link anywhere fails); `fstat` gives `(dev, ino)`.
- `LaunchDir::open_expect(path, (dev, ino))`: `open`, then the identity must match, else `LaunchDirError::Changed`.
- `LaunchDir::cwd(&self) -> PathBuf`: on Linux `files::pinned_dir(&self.fd)` (`/proc/self/fd/<n>`; the child resolves it before `exec` while it holds the inherited descriptor); elsewhere the path itself, after a second `open_expect` right before the spawn (a fail-closed recheck; a window of a few system calls remains and is documented).
- The text of a refusal: "The session folder changed: a link is in its path." (`launch_dir_changed`).

Because the walk follows no link and the path is the canonical string that `Policy::workspace` accepted inside the roots, the opened directory is the one at that path, inside the roots.

### 3.2 Where the identity is taken

The station takes it at the last station step before each launch, on the stored canonical `spec.cwd`:

1. **Provider start and resume** (`SessionTarget::start`, both hosts, after `start_refusal` and before `target_spawn` / `durable_launch` / `resume_durable`): `LaunchDir::open(spec.cwd)`; a failure fails the start with the text (no launch). The identity travels to the host (§3.3).
2. **Setup command** (`workspace/setup.rs` `run_held`), **`!` command** (`manager/shell_run.rs` `shell_spawn`), **precheck** (`workspace/precheck.rs` `run`), **terminal shell** (`shell/mod.rs` spawn): each opens a `LaunchDir` on its checked path right before the spawn and spawns with `current_dir(launch.cwd())` (portable-pty: `cmd.cwd(launch.cwd())`), keeping the `LaunchDir` alive until the spawn returned.

### 3.3 The hosts

- **Transport of the identity:** `fleetdeck_proto::command::SessionSpec` gains a station-local field `#[serde(skip)] #[ts(skip)] pub launch_identity: Option<(u64, u64)>`; it never travels on the wire; the station sets it right before the launch (§3.2.1). The providers' `ProcessSpec` gains `pub cwd_identity: Option<(u64, u64)>`; the drivers (Claude and Codex, direct and durable launch) copy it from the spec.
- **Direct host** (`DirectChildHost::spawn`): with `cwd_identity`, `LaunchDir::open_expect(&spec.cwd, identity)` (the driver's `canonical_workspace` result) and `current_dir(launch.cwd())`; the `LaunchDir` lives until `spawn` returned. A mismatch is `ProviderError::Workspace` with the text. If the driver's canonicalization followed a new link, the path or the identity differs and the launch fails.
- **Shim host:** `LaunchSpec` gains `#[serde(default)] pub cwd_identity: Option<(u64, u64)>` (the shim is the station's own executable, so the launching shim always knows the field). `server_proc::spawn_provider`: with an identity, `LaunchDir::open_expect(spec.cwd(), identity)` in the shim process and `current_dir(launch.cwd())` for `shim-exec`; the shim keeps the `LaunchDir` until the spawn returned. A mismatch fails the launch (`LaunchReply::Failed` with the text). `exec_provider` keeps its cwd (inherited).
- **No identity** (old callers, tests): today's behavior.

### 3.4 Codex `thread/start`

Codex receives `cwd` as a string and resolves it itself. After the thread opened (`session.started`), the station runs `LaunchDir::open_expect(spec.cwd, identity)` once more; a mismatch stops the session (`StopMode::Kill`) and fails the start with the text. A swap after this point is out of scope (§2a).

## 4. Tests

- `launch_dir` unit tests (Linux): open of a plain dir; a link at the last and at an inner component fails; `open_expect` with another identity fails; `cwd()` names the directory after the original path was renamed and replaced by a link (the child `pwd -P` prints the original directory).
- Direct host: a spawn whose path was swapped for a link after the identity was taken fails; one without a swap starts in the folder.
- Shim host (`fleetdeck-cli/tests`): the same through a real shim.
- Station: a start whose folder is swapped between decide and dispatch fails with `launch_dir_changed` and launches nothing; a `!` command and a setup command after a swap run nowhere (Linux) or are refused.
- Codex: a swap between the spawn and `session.started` stops the session.

## 5. Non-Linux

On macOS the identity check and the path recheck run (no `/proc/self/fd`); the window between the recheck and the kernel's `chdir` in the child remains and is documented in `SECURITY-CHECKLIST.md`.
