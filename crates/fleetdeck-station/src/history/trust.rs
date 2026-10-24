//! The trusted path, the supported filesystems and the per-record identity
//! rules of the native-session browser (B7, design §1b and §2.2).
//!
//! A path is trusted when no user other than the station user and root can
//! change any component of it between the station's check and the provider's
//! own open. That claim holds only on a local Linux filesystem that the
//! station knows (`supported_fs`) and on a mount that is not idmapped or
//! loop-backed (`Mounts::verifiable`). Everywhere else the station makes no
//! claim ([`PathCheck::Unverifiable`]).

use std::path::{Path, PathBuf};

use fleetdeck_proto::command::check_native_session_id;
use fleetdeck_proto::event::ProviderKind;
use serde_json::Value;

use super::locate::encode_cwd;
use super::parse::cut_chars;

/// The most bytes of `/proc/self/mountinfo` that are read.
const MOUNTINFO_MAX: u64 = 1024 * 1024;
/// The longest `cwd` of a transcript, in bytes.
const CWD_MAX: usize = 4096;
/// The longest `title`, in characters.
const TITLE_MAX: usize = 120;

/// Why a path check did not pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathCheck {
    /// A component fails the rule: `link`, `owner`, `writable`, `missing`
    /// or `io` (missing and io fail closed).
    Untrusted(&'static str),
    /// The station cannot verify the path: `fs_unsupported` or
    /// `mount_unverifiable`.
    Unverifiable(&'static str),
}

/// True when the filesystem of `fd` is on the allow-list: ext2/3/4, xfs,
/// btrfs, tmpfs, zfs or f2fs. Always false off Linux.
#[must_use]
pub fn supported_fs(fd: impl std::os::fd::AsFd) -> bool {
    #[cfg(target_os = "linux")]
    {
        const ALLOWED: [u64; 6] = [
            0xEF53,      // ext2, ext3, ext4
            0x5846_5342, // xfs
            0x9123_683E, // btrfs
            0x0102_1994, // tmpfs
            0x2FC1_2FC1, // zfs
            0xF2F5_2010, // f2fs
        ];
        rustix::fs::fstatfs(fd)
            .is_ok_and(|s| u64::try_from(s.f_type).is_ok_and(|t| ALLOWED.contains(&t)))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = fd;
        false
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Mount {
    major: u32,
    minor: u32,
    point: PathBuf,
    /// `idmapped` in the mount options or the super options.
    idmapped: bool,
    /// The source is a loop device.
    loop_backed: bool,
}

/// The mount table of the process, loaded once per request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mounts {
    mounts: Vec<Mount>,
}

/// Decodes the octal escapes (`\040`) of a mountinfo field.
fn unescape(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && i + 3 < bytes.len()
            && bytes[i + 1..i + 4]
                .iter()
                .all(|b| (b'0'..=b'7').contains(b))
        {
            let n = bytes[i + 1..i + 4]
                .iter()
                .fold(0_u32, |acc, b| acc * 8 + u32::from(b - b'0'));
            out.push(u8::try_from(n & 0xFF).unwrap_or(b'?'));
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_mount(line: &str) -> Option<Mount> {
    // `id parent major:minor root point options [optional...] - type source super`
    let fields: Vec<&str> = line.split(' ').collect();
    let dash = fields.iter().position(|f| *f == "-")?;
    if dash < 6 || fields.len() < dash + 4 {
        return None;
    }
    let (major, minor) = fields[2].split_once(':')?;
    let options = fields[5];
    let source = unescape(fields[dash + 2]);
    let super_options = fields[dash + 3];
    let has = |list: &str| list.split(',').any(|o| o == "idmapped");
    Some(Mount {
        major: major.parse().ok()?,
        minor: minor.parse().ok()?,
        point: PathBuf::from(unescape(fields[4])),
        idmapped: has(options) || has(super_options),
        loop_backed: source.starts_with("/dev/loop"),
    })
}

impl Mounts {
    /// Reads `/proc/self/mountinfo` (at most 1 MiB). A failure gives an empty
    /// table, so nothing is verifiable.
    #[must_use]
    pub fn load() -> Self {
        use std::io::Read as _;
        let Ok(file) = std::fs::File::open("/proc/self/mountinfo") else {
            return Self::default();
        };
        let mut bytes = Vec::new();
        if file.take(MOUNTINFO_MAX).read_to_end(&mut bytes).is_err() {
            return Self::default();
        }
        Self::parse(&String::from_utf8_lossy(&bytes))
    }

    /// Parses the text of a mountinfo file; a line that does not parse is
    /// left out.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        Self {
            mounts: text.lines().filter_map(parse_mount).collect(),
        }
    }

    /// True when the mount that holds `path` on device `dev` (the mount with
    /// the longest mount point that is a prefix of `path`) is neither idmapped
    /// nor loop-backed. False when no mount matches.
    #[must_use]
    pub fn verifiable(&self, dev: u64, path: &Path) -> bool {
        #[cfg(target_os = "linux")]
        {
            let (major, minor) = (rustix::fs::major(dev), rustix::fs::minor(dev));
            self.mounts
                .iter()
                .filter(|m| m.major == major && m.minor == minor && path.starts_with(&m.point))
                .max_by_key(|m| m.point.components().count())
                .is_some_and(|m| !m.idmapped && !m.loop_backed)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (dev, path);
            false
        }
    }
}

/// Checks `path` component by component from `/`, without resolving a link
/// (design §1b): a link component fails; each component is owned by
/// `station_uid` or root and not writable by group or others, except a
/// root-owned sticky directory whose next component is owned by the station
/// user. Every component must be on a supported filesystem and a verifiable
/// mount; the first component that is not ends the check as unverifiable (an
/// untrusted component before it still fails).
///
/// # Errors
/// [`PathCheck`].
pub fn trusted_path(path: &Path, station_uid: u32, mounts: &Mounts) -> Result<(), PathCheck> {
    #[cfg(target_os = "linux")]
    {
        linux::walk(path, station_uid, mounts)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (path, station_uid, mounts);
        Err(PathCheck::Unverifiable("fs_unsupported"))
    }
}

/// The most links that [`trusted_home`] follows.
const HOME_LINKS_MAX: usize = 8;

/// Resolves a provider home through trusted links only: the result is a path
/// that [`trusted_path`] accepts. A link that the station user or root owns,
/// in a trusted directory, cannot be changed by another user, so following
/// it keeps the §1b boundary (B7 PRE-PR r1). A link elsewhere fails.
///
/// # Errors
/// [`PathCheck`]: `Untrusted("link")` for more than 8 links or a target with
/// a `.` or `..` component, `Untrusted("owner")` for a link of another user,
/// the failure of the directory that holds a link, or any other failure of
/// [`trusted_path`].
pub fn trusted_home(path: &Path, station_uid: u32, mounts: &Mounts) -> Result<PathBuf, PathCheck> {
    use std::os::unix::fs::MetadataExt as _;
    use std::path::Component;

    let only_normal = |p: &Path| {
        p.is_absolute()
            && p.components()
                .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
    };
    if !only_normal(path) {
        return Err(PathCheck::Untrusted("io"));
    }
    let mut current = path.to_path_buf();
    for hop in 0..=HOME_LINKS_MAX {
        match trusted_path(&current, station_uid, mounts) {
            Ok(()) => return Ok(current),
            // Another link, and the eight hops are used.
            Err(PathCheck::Untrusted("link")) if hop == HOME_LINKS_MAX => {
                return Err(PathCheck::Untrusted("link"));
            }
            Err(PathCheck::Untrusted("link")) => {}
            Err(other) => return Err(other),
        }
        // The first prefix that is a link.
        let mut prefix = PathBuf::new();
        let mut found: Option<(PathBuf, PathBuf)> = None;
        let mut comps = current.components();
        while let Some(component) = comps.next() {
            prefix.push(component);
            let is_link = std::fs::symlink_metadata(&prefix)
                .map_err(|_| PathCheck::Untrusted("io"))?
                .file_type()
                .is_symlink();
            if is_link {
                found = Some((prefix.clone(), comps.as_path().to_path_buf()));
                break;
            }
        }
        let Some((link, rest)) = found else {
            return Err(PathCheck::Untrusted("io"));
        };
        let parent = link.parent().ok_or(PathCheck::Untrusted("io"))?;
        trusted_path(parent, station_uid, mounts)?;
        let meta = std::fs::symlink_metadata(&link).map_err(|_| PathCheck::Untrusted("io"))?;
        if meta.uid() != station_uid && meta.uid() != 0 {
            return Err(PathCheck::Untrusted("owner"));
        }
        let target = std::fs::read_link(&link).map_err(|_| PathCheck::Untrusted("io"))?;
        let target = if target.is_absolute() {
            target
        } else {
            parent.join(target)
        };
        if !only_normal(&target) {
            return Err(PathCheck::Untrusted("link"));
        }
        // No empty join: it would add a trailing slash.
        current = if rest.as_os_str().is_empty() {
            target
        } else {
            target.join(rest)
        };
    }
    Err(PathCheck::Untrusted("link"))
}

#[cfg(target_os = "linux")]
mod linux {
    use std::ffi::OsStr;
    use std::os::fd::OwnedFd;
    use std::path::{Component, Path, PathBuf};

    use rustix::fs::{AtFlags, FileType, Mode, OFlags, Stat};
    use rustix::io::Errno;

    use super::{Mounts, PathCheck, supported_fs};

    fn io_check(e: Errno) -> PathCheck {
        if e == Errno::NOENT {
            PathCheck::Untrusted("missing")
        } else {
            PathCheck::Untrusted("io")
        }
    }

    pub(super) fn walk(path: &Path, uid: u32, mounts: &Mounts) -> Result<(), PathCheck> {
        if !path.is_absolute() {
            return Err(PathCheck::Untrusted("io"));
        }
        let mut names: Vec<&OsStr> = Vec::new();
        for component in path.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(name) => names.push(name),
                _ => return Err(PathCheck::Untrusted("io")),
            }
        }
        let dir_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let file_flags =
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC;
        let mut cur: OwnedFd = rustix::fs::open("/", dir_flags, Mode::empty()).map_err(io_check)?;
        let mut so_far = PathBuf::from("/");
        let mut index = 0;
        loop {
            let stat = rustix::fs::fstat(&cur).map_err(io_check)?;
            // The first component that cannot be verified ends the check:
            // the station makes no claim about what lies beyond it.
            if let Some(why) = judge(&cur, &stat, names.get(index).copied(), uid, mounts, &so_far)?
            {
                return Err(PathCheck::Unverifiable(why));
            }
            let Some(name) = names.get(index) else { break };
            let child =
                rustix::fs::statat(&cur, *name, AtFlags::SYMLINK_NOFOLLOW).map_err(io_check)?;
            let kind = FileType::from_raw_mode(child.st_mode);
            if kind == FileType::Symlink {
                return Err(PathCheck::Untrusted("link"));
            }
            let flags = if kind == FileType::Directory {
                dir_flags
            } else {
                file_flags
            };
            cur = rustix::fs::openat(&cur, *name, flags, Mode::empty()).map_err(io_check)?;
            so_far.push(name);
            index += 1;
        }
        Ok(())
    }

    /// The rule for one component. `Ok(Some(why))` when it is unverifiable.
    fn judge(
        fd: &OwnedFd,
        stat: &Stat,
        next: Option<&OsStr>,
        uid: u32,
        mounts: &Mounts,
        so_far: &Path,
    ) -> Result<Option<&'static str>, PathCheck> {
        if !supported_fs(fd) {
            return Ok(Some("fs_unsupported"));
        }
        if !mounts.verifiable(stat.st_dev, so_far) {
            return Ok(Some("mount_unverifiable"));
        }
        if stat.st_uid != uid && stat.st_uid != 0 {
            return Err(PathCheck::Untrusted("owner"));
        }
        if stat.st_mode & 0o022 != 0 {
            let is_dir = FileType::from_raw_mode(stat.st_mode) == FileType::Directory;
            let sticky_root = stat.st_uid == 0 && is_dir && stat.st_mode & 0o1000 != 0;
            let child_is_ours = next.is_some_and(|name| {
                rustix::fs::statat(fd, name, AtFlags::SYMLINK_NOFOLLOW)
                    .is_ok_and(|s| s.st_uid == uid)
            });
            if !(sticky_root && child_is_ours) {
                return Err(PathCheck::Untrusted("writable"));
            }
        }
        Ok(None)
    }
}

// ------------------------------------------------------------- identity

/// A record that contradicts the identity of its transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Contradiction;

/// What the identity rules read from a transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Found {
    pub cwd: String,
    pub title: Option<String>,
    pub parent: Option<String>,
}

/// The per-record identity rules of design §2.2, fed with every record of
/// the head window and the tail window.
///
/// - Claude: a `sessionId` must name the file stem; every `cwd` must be the
///   same; the project directory name equals `encode_cwd(cwd)`; some record
///   names the session.
/// - Codex: the first record is `session_meta` with `payload.id` equal to the
///   id suffix and a `cwd`; a later `session_meta` or `turn_context` that
///   names an id or a `cwd` must agree.
pub(super) struct Identity {
    kind: ProviderKind,
    id: String,
    dir_name: Option<String>,
    cwd: Option<String>,
    named: bool,
    started: bool,
    title: Option<String>,
    parent: Option<String>,
}

impl Identity {
    /// `dir_name` is the project directory of a Claude transcript.
    pub(super) fn new(kind: ProviderKind, id: &str, dir_name: Option<&str>) -> Self {
        Self {
            kind,
            id: id.to_owned(),
            dir_name: dir_name.map(str::to_owned),
            cwd: None,
            named: false,
            started: false,
            title: None,
            parent: None,
        }
    }

    fn note_cwd(&mut self, value: &Value) -> Result<(), Contradiction> {
        let cwd = value.as_str().ok_or(Contradiction)?;
        if !cwd.starts_with('/') || cwd.len() > CWD_MAX || cwd.contains('\0') {
            return Err(Contradiction);
        }
        match &self.cwd {
            Some(prev) if prev != cwd => Err(Contradiction),
            Some(_) => Ok(()),
            None => {
                self.cwd = Some(cwd.to_owned());
                Ok(())
            }
        }
    }

    /// Feeds one record line. A line that is not JSON is not a record.
    ///
    /// # Errors
    /// [`Contradiction`].
    pub(super) fn record(&mut self, line: &[u8]) -> Result<(), Contradiction> {
        let first = !std::mem::replace(&mut self.started, true);
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            return if first && self.kind == ProviderKind::Codex {
                Err(Contradiction)
            } else {
                Ok(())
            };
        };
        match self.kind {
            ProviderKind::Claude => {
                if let Some(session) = value.get("sessionId") {
                    if session.as_str() != Some(self.id.as_str()) {
                        return Err(Contradiction);
                    }
                    self.named = true;
                }
                if let Some(cwd) = value.get("cwd") {
                    self.note_cwd(cwd)?;
                }
                if value.get("type").and_then(Value::as_str) == Some("ai-title")
                    && let Some(title) = value.get("aiTitle").and_then(Value::as_str)
                {
                    self.title = Some(cut_chars(title, TITLE_MAX));
                }
                Ok(())
            }
            ProviderKind::Codex => self.codex(&value, first),
            ProviderKind::Unknown => Err(Contradiction),
        }
    }

    fn codex(&mut self, value: &Value, first: bool) -> Result<(), Contradiction> {
        let kind = value.get("type").and_then(Value::as_str);
        if first && kind != Some("session_meta") {
            return Err(Contradiction);
        }
        if !matches!(kind, Some("session_meta" | "turn_context")) {
            return Ok(());
        }
        let payload = value
            .get("payload")
            .and_then(Value::as_object)
            .ok_or(Contradiction)?;
        match payload.get("id") {
            Some(id) if id.as_str() == Some(self.id.as_str()) => {}
            Some(_) => return Err(Contradiction),
            None if first => return Err(Contradiction),
            None => {}
        }
        match payload.get("cwd") {
            Some(cwd) => self.note_cwd(cwd)?,
            None if first => return Err(Contradiction),
            None => {}
        }
        if first {
            match payload.get("parent_thread_id") {
                None | Some(Value::Null) => {}
                Some(Value::String(parent)) => {
                    check_native_session_id(parent).map_err(|_| Contradiction)?;
                    self.parent = Some(parent.clone());
                }
                Some(_) => return Err(Contradiction),
            }
        }
        Ok(())
    }

    /// The facts, once every record is fed.
    ///
    /// # Errors
    /// [`Contradiction`] for a missing `cwd`, a Claude transcript that names
    /// no session, or a project directory that is not `encode_cwd(cwd)`.
    pub(super) fn finish(self) -> Result<Found, Contradiction> {
        let cwd = self.cwd.ok_or(Contradiction)?;
        if self.kind == ProviderKind::Claude
            && (!self.named || self.dir_name.as_deref() != Some(encode_cwd(&cwd).as_str()))
        {
            return Err(Contradiction);
        }
        Ok(Found {
            cwd,
            title: self.title,
            parent: self.parent,
        })
    }
}

/// The native id of a Codex rollout file name
/// `rollout-YYYY-MM-DDThh-mm-ss-<id>.jsonl`, if it has the shape and the id
/// passes `check_native_session_id`.
pub(super) fn rollout_id(name: &str) -> Option<&str> {
    let rest = name.strip_prefix("rollout-")?.strip_suffix(".jsonl")?;
    let (stamp, id) = (rest.get(..19)?, rest.get(20..)?);
    let shape = b"dddd-dd-ddTdd-dd-dd";
    let stamp_ok = stamp.bytes().zip(shape).all(|(b, s)| match s {
        b'd' => b.is_ascii_digit(),
        other => b == *other,
    });
    if !stamp_ok || rest.as_bytes().get(19) != Some(&b'-') || check_native_session_id(id).is_err() {
        return None;
    }
    Some(id)
}
