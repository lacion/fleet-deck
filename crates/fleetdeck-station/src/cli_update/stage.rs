//! Stage and bind (design §3.6, steps 1 to 3).
//!
//! Nothing here runs a byte of the new version before it is bound to the
//! signed digest of §3.5. This part holds the children of an attempt (no
//! shell, own process group, marker, log, fence), the native download and
//! its checksum, the Codex staging with npm or Bun, and the probe of a
//! staged copy.

use std::ffi::OsString;
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use fleetdeck_core::ChildEnv;
use fleetdeck_proto::cli_update::reason;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_providers::update_probe::{self, UpdateProbe};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use super::channel::native_binary_url;
use super::detect::Tool;
use super::fence::{MARKER_VAR, fence};
use super::staged::{Lockfile, verify_staged};
use super::verify::VerifiedPackage;
use crate::integrations::http;

/// The free space that a download or a staging needs on its file system.
pub const MIN_FREE: u64 = 1 << 30;
/// The time limit of the download and of the staging (10 min).
pub const STAGE_LIMIT: Duration = Duration::from_secs(600);
/// The largest native binary (512 MiB).
pub const MAX_BINARY: u64 = 512 * 1024 * 1024;
/// The most bytes of one log file.
pub const LOG_CAP: u64 = 256 * 1024;
/// The log files that stay.
pub const LOG_KEEP: usize = 20;
/// The time limit of one probe (30 s).
pub const PROBE_LIMIT: Duration = Duration::from_secs(30);

/// Why a step of an attempt failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// The attempt ends `Refused` with this reason.
    Refused(&'static str),
    /// The attempt ends `Failed` with this reason (nothing changed).
    Failed(&'static str),
    /// The fence found a process that stays (`process_left`).
    ProcessLeft,
}

impl Failure {
    #[must_use]
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Refused(r) | Self::Failed(r) => r,
            Self::ProcessLeft => reason::PROCESS_LEFT,
        }
    }
}

/// Where the process group of a child goes right after the spawn (the
/// journal row, `installer_pgid`).
#[async_trait]
pub trait PgidSink: Send + Sync {
    async fn record(&self, pgid: i32);
}

/// What the children of one attempt share.
pub struct Ctx<'a> {
    pub attempt_id: &'a str,
    /// The attempt id of the first attempt of the journal row.
    pub marker: &'a str,
    pub station_dir: &'a Path,
    pub sink: &'a dyn PgidSink,
    pub curl: &'a Path,
}

impl Ctx<'_> {
    fn base(&self) -> PathBuf {
        self.station_dir.join("cli-update")
    }

    /// The empty private folder where the probes of this attempt run.
    ///
    /// # Errors
    /// A file system error.
    pub fn probe_cwd(&self) -> io::Result<PathBuf> {
        let dir = self.base().join("probe-cwd").join(self.attempt_id);
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)?;
        Ok(dir)
    }
}

/// One child of an attempt.
pub struct ChildSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(OsString, OsString)>,
    pub cwd: Option<PathBuf>,
    /// Bytes for stdin (a curl config); `None` closes stdin.
    pub stdin: Option<Vec<u8>>,
    pub limit: Duration,
}

/// How a child ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildEnd {
    Exited(i32),
    TimedOut,
    NoStart,
}

async fn capped<R: AsyncRead + Unpin>(pipe: Option<R>) -> Vec<u8> {
    let mut keep = Vec::new();
    if let Some(mut p) = pipe {
        let _ = (&mut p).take(LOG_CAP).read_to_end(&mut keep).await;
        let _ = tokio::io::copy(&mut p, &mut tokio::io::sink()).await;
    }
    keep
}

/// Runs one child: no shell, fixed argv, own process group, the marker in
/// its environment, output into the attempt log, then the fence.
///
/// # Errors
/// [`Failure::ProcessLeft`] when the fence finds a process.
pub async fn run_child(ctx: &Ctx<'_>, spec: ChildSpec) -> Result<ChildEnd, Failure> {
    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args)
        .env_clear()
        .envs(
            spec.env
                .iter()
                .filter(|(k, _)| k != MARKER_VAR)
                .map(|(k, v)| (k, v)),
        )
        .env(MARKER_VAR, ctx.marker)
        .stdin(if spec.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true);
    if let Some(cwd) = &spec.cwd {
        cmd.current_dir(cwd);
    }
    let Ok(mut child) = cmd.spawn() else {
        return Ok(ChildEnd::NoStart);
    };
    let pgid = child.id().and_then(|p| i32::try_from(p).ok());
    if let Some(pgid) = pgid {
        ctx.sink.record(pgid).await;
    }
    if let (Some(bytes), Some(mut pipe)) = (spec.stdin, child.stdin.take()) {
        tokio::spawn(async move {
            let _ = pipe.write_all(&bytes).await;
            let _ = pipe.shutdown().await;
        });
    }
    let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
    let work = async {
        let (out, err) = tokio::join!(capped(stdout), capped(stderr));
        (out, err, child.wait().await)
    };
    let end = match tokio::time::timeout(spec.limit, work).await {
        Ok((out, err, status)) => {
            append_log(ctx, &out, &err);
            status.map_or(ChildEnd::NoStart, |s| {
                ChildEnd::Exited(s.code().unwrap_or(-1))
            })
        }
        Err(_) => {
            if let Some(g) = pgid.and_then(rustix::process::Pid::from_raw) {
                let _ = rustix::process::kill_process_group(g, rustix::process::Signal::KILL);
            }
            let _ = child.wait().await;
            ChildEnd::TimedOut
        }
    };
    run_fence(ctx, pgid).await?;
    Ok(end)
}

/// The fence of §3.6.3 in a blocking task.
///
/// # Errors
/// [`Failure::ProcessLeft`].
pub async fn run_fence(ctx: &Ctx<'_>, pgid: Option<i32>) -> Result<(), Failure> {
    let marker = ctx.marker.to_owned();
    match tokio::task::spawn_blocking(move || fence(&marker, pgid)).await {
        Ok(Ok(())) => Ok(()),
        _ => Err(Failure::ProcessLeft),
    }
}

/// Appends the output of a child to `<station dir>/cli-update/logs/<id>.log`
/// (0600, at most [`LOG_CAP`] bytes) and keeps the newest [`LOG_KEEP`] files.
fn append_log(ctx: &Ctx<'_>, out: &[u8], err: &[u8]) {
    use std::io::Write as _;
    let dir = ctx.base().join("logs");
    let write = || -> io::Result<()> {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)?;
        let path = dir.join(format!("{}.log", ctx.attempt_id));
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&path)?;
        let mut room = LOG_CAP.saturating_sub(file.metadata()?.len());
        for part in [out, err] {
            let n = usize::try_from(room).unwrap_or(usize::MAX).min(part.len());
            file.write_all(&part[..n])?;
            room -= n as u64;
        }
        prune_logs(&dir, LOG_KEEP)
    };
    if let Err(e) = write() {
        tracing::warn!(error = %e, "the update log is not written");
    }
}

/// Removes the oldest files of `dir` until `keep` stay.
///
/// # Errors
/// A file system error.
pub fn prune_logs(dir: &Path, keep: usize) -> io::Result<()> {
    let mut files: Vec<_> = std::fs::read_dir(dir)?
        .flatten()
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    files.sort();
    let extra = files.len().saturating_sub(keep);
    for (_, path) in files.into_iter().take(extra) {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

/// The free bytes of the file system of `path`.
///
/// # Errors
/// The `statvfs` error.
pub fn free_bytes(path: &Path) -> io::Result<u64> {
    let s = rustix::fs::statvfs(path).map_err(io::Error::from)?;
    Ok(s.f_bavail.saturating_mul(s.f_frsize))
}

/// The SHA-256 of a file as lower-case hex, streamed.
///
/// # Errors
/// A read error.
pub fn sha256_file(path: &Path) -> io::Result<String> {
    use std::io::Read as _;
    let mut f = std::fs::File::open(path)?;
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        ctx.update(&buf[..n]);
    }
    Ok(super::tree_digest::hex(ctx.finish().as_ref()))
}

/// True when both paths are on one device.
///
/// # Errors
/// A `stat` error.
pub fn same_device(a: &Path, b: &Path) -> io::Result<bool> {
    Ok(std::fs::metadata(a)?.dev() == std::fs::metadata(b)?.dev())
}

/// A rename that does not replace `to`.
///
/// # Errors
/// The rename error (`AlreadyExists` when `to` exists).
pub fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use rustix::fs::{CWD, RenameFlags, renameat_with};
        renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE).map_err(io::Error::from)
    }
    #[cfg(not(target_os = "linux"))]
    {
        std::fs::hard_link(from, to)?;
        std::fs::remove_file(from)
    }
}

/// The probe environment: the instance environment plus
/// `DISABLE_AUTOUPDATER=1` and the marker (§3.6 "Environments").
///
/// `None` when [`ChildEnv`] refuses the marker (a NUL byte).
#[must_use]
pub fn probe_env(base: &ChildEnv, marker: &str) -> Option<ChildEnv> {
    let mut env = base.clone();
    env.set("DISABLE_AUTOUPDATER", "1").ok()?;
    env.set(MARKER_VAR, marker).ok()?;
    Some(env)
}

/// §3.6.1 on `program`, then the fence. `env` comes from [`probe_env`].
///
/// # Errors
/// [`Failure::ProcessLeft`]; `Failed(install_failed)` when the probe folder
/// cannot be made.
pub async fn probe_checked(
    ctx: &Ctx<'_>,
    kind: ProviderKind,
    program: &Path,
    env: &ChildEnv,
) -> Result<UpdateProbe, Failure> {
    let cwd = ctx
        .probe_cwd()
        .map_err(|_| Failure::Failed(reason::INSTALL_FAILED))?;
    let probe = update_probe::check(kind, program, env, &cwd, PROBE_LIMIT).await;
    run_fence(ctx, None).await?;
    Ok(probe)
}

/// §3.6 S4: `version == target` and the handshake passes.
///
/// # Errors
/// `Refused(version_mismatch)`, `Refused(handshake_failed)`, `ProcessLeft`.
pub async fn probe_staged(
    ctx: &Ctx<'_>,
    kind: ProviderKind,
    program: &Path,
    env: &ChildEnv,
    target: &str,
) -> Result<(), Failure> {
    let probe = probe_checked(ctx, kind, program, env).await?;
    if probe.version.as_deref() != Some(target) {
        return Err(Failure::Refused(reason::VERSION_MISMATCH));
    }
    probe
        .handshake
        .map_err(|_| Failure::Refused(reason::HANDSHAKE_FAILED))
}

// ---- Native Claude ------------------------------------------------------

/// The file of S1: `<versions>/.fd-stage-<attempt_id>`.
#[must_use]
pub fn native_stage_file(versions: &Path, attempt_id: &str) -> PathBuf {
    versions.join(format!(".fd-stage-{attempt_id}"))
}

fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The curl config of the download of S1.
#[must_use]
pub fn download_config(url: &str, output: &Path) -> String {
    format!(
        "proto = \"=https\"\nno-location\nmax-time = {}\nmax-filesize = {MAX_BINARY}\n\
         url = {}\noutput = {}\ncreate-file-mode = 0600\n",
        STAGE_LIMIT.as_secs(),
        quote(url),
        quote(&output.to_string_lossy()),
    )
}

/// §3.6 native S1: the space check and the download into
/// [`native_stage_file`]. Returns the path of the file.
///
/// # Errors
/// `Refused(disk_full)`, `Failed(channel_unreachable)`, `Failed(timeout)`,
/// `ProcessLeft`.
pub async fn download_native(
    ctx: &Ctx<'_>,
    versions: &Path,
    target: &str,
    platform: &str,
) -> Result<PathBuf, Failure> {
    if free_bytes(versions).map_or(true, |f| f < MIN_FREE) {
        return Err(Failure::Refused(reason::DISK_FULL));
    }
    let url = native_binary_url(target, platform).ok_or(Failure::Refused(reason::NOT_A_VERSION))?;
    let file = native_stage_file(versions, ctx.attempt_id);
    let _ = std::fs::remove_file(&file);
    let spec = ChildSpec {
        program: ctx.curl.to_path_buf(),
        args: http::ARGS.map(str::to_owned).to_vec(),
        env: crate::integrations::exec::curl_env(),
        cwd: None,
        stdin: Some(download_config(&url, &file).into_bytes()),
        limit: STAGE_LIMIT,
    };
    match run_child(ctx, spec).await? {
        ChildEnd::Exited(0) => Ok(file),
        ChildEnd::TimedOut => Err(Failure::Failed(reason::TIMEOUT)),
        _ => Err(Failure::Failed(reason::CHANNEL_UNREACHABLE)),
    }
}

/// Where S3 put the binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placed {
    /// `<versions>/<target>`.
    pub path: PathBuf,
    /// True when S3 created it (S4 removes it on a failure).
    pub created: bool,
}

/// §3.6 native S2 and S3. S2 binds the staged file to `expected_sha256`
/// (nothing has run it). S3 sets mode 0755 and renames it to
/// `<versions>/<target>` without replace; an existing file must have the
/// same checksum.
///
/// # Errors
/// `Refused(checksum_mismatch)` (the staged file is removed),
/// `Failed(install_failed)` for a rename error.
pub fn bind_native(
    staged: &Path,
    versions: &Path,
    target: &str,
    expected_sha256: &str,
) -> Result<Placed, Failure> {
    let refuse = |staged: &Path| {
        let _ = std::fs::remove_file(staged);
        Failure::Refused(reason::CHECKSUM_MISMATCH)
    };
    match sha256_file(staged) {
        Ok(sum) if sum == expected_sha256 => {}
        _ => return Err(refuse(staged)),
    }
    let io_failed = |_: io::Error| Failure::Failed(reason::INSTALL_FAILED);
    std::fs::set_permissions(staged, std::fs::Permissions::from_mode(0o755)).map_err(io_failed)?;
    let dest = versions.join(target);
    let same = |dest: &Path| sha256_file(dest).is_ok_and(|s| s == expected_sha256);
    if std::fs::symlink_metadata(&dest).is_ok() {
        return if same(&dest) {
            let _ = std::fs::remove_file(staged);
            Ok(Placed {
                path: dest,
                created: false,
            })
        } else {
            Err(refuse(staged))
        };
    }
    match rename_noreplace(staged, &dest) {
        Ok(()) => Ok(Placed {
            path: dest,
            created: true,
        }),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && same(&dest) => {
            let _ = std::fs::remove_file(staged);
            Ok(Placed {
                path: dest,
                created: false,
            })
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(refuse(staged)),
        Err(e) => Err(io_failed(e)),
    }
}

// ---- Codex by npm or Bun ------------------------------------------------

/// `R`: `<parent of modules>/.fd-cli-update/<attempt_id>`. `None` for a tool
/// that is not npm or Bun.
#[must_use]
pub fn codex_stage_root(tool: &Tool, attempt_id: &str) -> Option<PathBuf> {
    let modules = match tool {
        Tool::Npm { modules, .. } | Tool::Bun { modules, .. } => modules,
        _ => return None,
    };
    Some(modules.parent()?.join(".fd-cli-update").join(attempt_id))
}

/// The staged global folder `SM`.
#[must_use]
pub fn staged_modules(tool: &Tool, root: &Path) -> Option<PathBuf> {
    match tool {
        Tool::Npm { .. } => Some(root.join("p/lib/node_modules")),
        Tool::Bun { .. } => Some(root.join("b/install/global/node_modules")),
        _ => None,
    }
}

const REGISTRY: &str = "https://registry.npmjs.org/";

/// The argv of the npm staging (§3.6 Codex S2).
#[must_use]
pub fn npm_argv(root: &Path, target: &str) -> Vec<String> {
    let r = root.to_string_lossy();
    let rc = format!("{r}/empty.npmrc");
    vec![
        "install".into(),
        "--global".into(),
        format!("--prefix={r}/p"),
        format!("--userconfig={rc}"),
        format!("--globalconfig={rc}"),
        format!("--cache={r}/cache"),
        format!("--registry={REGISTRY}"),
        format!("--@openai:registry={REGISTRY}"),
        "--ignore-scripts".into(),
        "--no-fund".into(),
        "--no-audit".into(),
        "--no-update-notifier".into(),
        format!("@openai/codex@{target}"),
    ]
}

/// The argv of the Bun staging (§3.6 Codex S2).
#[must_use]
pub fn bun_argv(target: &str) -> Vec<String> {
    vec![
        "add".into(),
        "--global".into(),
        "--ignore-scripts".into(),
        format!("--registry={REGISTRY}"),
        format!("@openai/codex@{target}"),
    ]
}

/// The names of the station environment that a staging child may get: the
/// proxy and CA names of `curl_env()` and `NODE_EXTRA_CA_CERTS`.
const PASS: &[&str] = &[
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "CURL_CA_BUNDLE",
    "NODE_EXTRA_CA_CERTS",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
];

/// The environment of the staging child: only these names (§3.6
/// "Environments"). `station` is the station process environment; only the
/// [`PASS`] names are taken from it, and only `PATH` from `instance`.
#[must_use]
pub fn staging_env(
    root: &Path,
    tool_folder: &Path,
    instance: &ChildEnv,
    station: &[(OsString, OsString)],
    bun: bool,
    marker: &str,
) -> Vec<(OsString, OsString)> {
    let r = root.to_string_lossy();
    let mut path = tool_folder.as_os_str().to_owned();
    if let Some(p) = instance.get("PATH") {
        path.push(":");
        path.push(p);
    }
    let mut env: Vec<(OsString, OsString)> = vec![
        ("PATH".into(), path),
        ("HOME".into(), format!("{r}/home").into()),
        ("XDG_CONFIG_HOME".into(), format!("{r}/xdg").into()),
        ("XDG_CACHE_HOME".into(), format!("{r}/cache").into()),
        ("LANG".into(), "C".into()),
        (MARKER_VAR.into(), marker.into()),
    ];
    if bun {
        env.push(("BUN_INSTALL".into(), format!("{r}/b").into()));
        env.push((
            "BUN_INSTALL_GLOBAL_DIR".into(),
            format!("{r}/b/install/global").into(),
        ));
        env.push(("BUN_INSTALL_BIN".into(), format!("{r}/b/bin").into()));
        env.push(("BUN_INSTALL_CACHE_DIR".into(), format!("{r}/cache").into()));
    }
    env.extend(
        station
            .iter()
            .filter(|(k, _)| k.to_str().is_some_and(|k| PASS.contains(&k)))
            .cloned(),
    );
    env
}

/// §3.6 Codex S1 and S2: the checks and the staging into `root`. Returns
/// `SM`. `station` is the station process environment.
///
/// # Errors
/// `Refused(unsupported_layout)` (two devices, or not npm or Bun),
/// `Refused(disk_full)`, `Failed(install_failed)`, `Failed(timeout)`,
/// `ProcessLeft`.
pub async fn stage_codex(
    ctx: &Ctx<'_>,
    tool: &Tool,
    root: &Path,
    target: &str,
    instance: &ChildEnv,
    station: &[(OsString, OsString)],
) -> Result<PathBuf, Failure> {
    let (program, modules, bun) = match tool {
        Tool::Npm { npm, modules, .. } => (npm, modules, false),
        Tool::Bun { bun, modules, .. } => (bun, modules, true),
        _ => return Err(Failure::Refused(reason::UNSUPPORTED_LAYOUT)),
    };
    let layout = Failure::Refused(reason::UNSUPPORTED_LAYOUT);
    let sm = staged_modules(tool, root).ok_or(layout)?;
    let mk = |p: &Path| {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(p)
    };
    let prepare = || -> io::Result<()> {
        mk(root)?;
        for d in ["home", "xdg", "cache", "cwd"] {
            mk(&root.join(d))?;
        }
        std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(root.join("empty.npmrc"))
            .map(drop)
    };
    prepare().map_err(|_| Failure::Failed(reason::INSTALL_FAILED))?;
    if !same_device(root, modules).unwrap_or(false) {
        return Err(layout);
    }
    if free_bytes(modules).map_or(true, |f| f < MIN_FREE) {
        return Err(Failure::Refused(reason::DISK_FULL));
    }
    let folder = program.parent().ok_or(layout)?;
    let spec = ChildSpec {
        program: program.clone(),
        args: if bun {
            bun_argv(target)
        } else {
            npm_argv(root, target)
        },
        env: staging_env(root, folder, instance, station, bun, ctx.marker),
        cwd: Some(root.join("cwd")),
        stdin: None,
        limit: STAGE_LIMIT,
    };
    match run_child(ctx, spec).await? {
        ChildEnd::Exited(0) => Ok(sm),
        ChildEnd::TimedOut => Err(Failure::Failed(reason::TIMEOUT)),
        _ => Err(Failure::Failed(reason::INSTALL_FAILED)),
    }
}

/// §3.6 Codex S3: binds the staged folder to the verified set (T6). Returns
/// `moved`, the sorted top-level packages of `SM`.
///
/// # Errors
/// `Refused` with `tool_too_old`, `unverified_package`, `integrity_mismatch`
/// or `unsupported_layout`.
pub fn bind_codex(
    tool: &Tool,
    root: &Path,
    set: &[VerifiedPackage],
) -> Result<Vec<String>, Failure> {
    let layout = Failure::Refused(reason::UNSUPPORTED_LAYOUT);
    let (modules, lock) = match tool {
        Tool::Npm { modules, .. } => (
            modules,
            Lockfile::Npm(root.join("p/lib/node_modules/.package-lock.json")),
        ),
        Tool::Bun { modules, .. } => (
            modules,
            Lockfile::Bun(root.join("b/install/global/bun.lock")),
        ),
        _ => return Err(layout),
    };
    let sm = staged_modules(tool, root).ok_or(layout)?;
    let text =
        std::fs::read_to_string(modules.join("@openai/codex/package.json")).map_err(|_| layout)?;
    let doc: serde_json::Value = serde_json::from_str(&text).map_err(|_| layout)?;
    let bin = doc.get("bin").cloned().unwrap_or(serde_json::Value::Null);
    verify_staged(&sm, lock, set, &bin).map_err(Failure::Refused)
}

/// The entry point of the staged Codex (§3.6 Codex S4).
#[must_use]
pub fn staged_entry(sm: &Path) -> PathBuf {
    sm.join("@openai/codex/bin/codex.js")
}

#[cfg(test)]
#[path = "stage_tests.rs"]
mod tests;
