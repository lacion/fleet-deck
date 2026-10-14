//! Tailscale on WSL (tailnet design sections 14 and 15): the host facts that
//! the deck reads, and the checks for the Windows program.
//!
//! Under WSL the shared resolver ([`crate::tailscale::TailscaleCli::locate`])
//! uses `tailscale.exe` of Windows when no other program is set or found and
//! [`windows_program_ok`] passes: the mount of the drive C: is the plain
//! WSL mount of that drive (read from `/proc/self/mountinfo`), no mount
//! redirects a component of the path, and no component is a symbolic link.

use std::path::{Path, PathBuf};

use crate::local_addrs;

/// The Tailscale program of Windows, as WSL sees it.
pub const WINDOWS_PROGRAM: &str = "/mnt/c/Program Files/Tailscale/tailscale.exe";

const OSRELEASE: &str = "/proc/sys/kernel/osrelease";
const INTEROP: &str = "/proc/sys/fs/binfmt_misc/WSLInterop";
const MOUNTINFO: &str = "/proc/self/mountinfo";
/// The WSL mount of the drive C:.
const C_DRIVE: &str = "/mnt/c";
/// The mount points that must not exist: a mount there could redirect a
/// component of [`WINDOWS_PROGRAM`].
const REDIRECTS: [&str; 4] = [
    "/mnt",
    "/mnt/c/Program Files",
    "/mnt/c/Program Files/Tailscale",
    WINDOWS_PROGRAM,
];

/// The facts about this machine that the tailnet routes and the automatic
/// start use. The tests give their own facts.
#[derive(Debug, Clone, Default)]
pub struct HostFacts {
    /// The process runs in WSL.
    pub wsl: bool,
    /// `ip_nonlocal_bind` is on: a bind does not prove a local address.
    pub nonlocal_bind: bool,
}

/// True when the kernel text names WSL (`microsoft` or `wsl`, in any case)
/// and WSL interop is registered. One of the two alone is not WSL.
#[must_use]
pub fn wsl_from(osrelease: &str, interop: bool) -> bool {
    let kernel = osrelease.to_ascii_lowercase();
    interop && (kernel.contains("microsoft") || kernel.contains("wsl"))
}

/// True when this process runs in WSL: the kernel says so and the interop
/// file exists. An environment variable alone never counts.
#[must_use]
pub fn is_wsl() -> bool {
    let kernel = std::fs::read_to_string(OSRELEASE).unwrap_or_default();
    wsl_from(&kernel, Path::new(INTEROP).exists())
}

/// The real facts of this machine.
#[must_use]
pub fn detect() -> HostFacts {
    HostFacts {
        wsl: is_wsl(),
        nonlocal_bind: local_addrs::nonlocal_bind(),
    }
}

/// One line of `/proc/self/mountinfo`. The root, the mount point and the
/// source are decoded (octal escapes); the super options are raw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountEntry {
    pub mount_point: String,
    pub root: String,
    pub fstype: String,
    pub source: String,
    pub super_options: String,
}

/// Decodes the octal escapes (`\040`, `\134`, ...) of a mountinfo field. A
/// backslash that does not start three octal digits of a byte, or a result
/// that is not UTF-8, is `None`.
fn unescape(field: &str) -> Option<String> {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        let digits = bytes.get(i + 1..i + 4)?;
        if !digits.iter().all(|c| (b'0'..=b'7').contains(c)) {
            return None;
        }
        let value = digits
            .iter()
            .fold(0_u32, |n, c| n * 8 + u32::from(c - b'0'));
        out.push(u8::try_from(value).ok()?);
        i += 4;
    }
    String::from_utf8(out).ok()
}

fn parse_line(line: &str) -> Option<MountEntry> {
    // The optional fields end at ` - `; the part before it has no spaces
    // inside a field (they are escaped), so the first ` - ` is the end.
    let (head, tail) = line.split_once(" - ")?;
    let fields: Vec<&str> = head.split(' ').collect();
    if fields.len() < 6 || fields.iter().any(|f| f.is_empty()) || !fields[2].contains(':') {
        return None;
    }
    let mut rest = tail.splitn(3, ' ');
    let (fstype, source, super_options) = (rest.next()?, rest.next()?, rest.next()?);
    if fstype.is_empty() || source.is_empty() {
        return None;
    }
    Some(MountEntry {
        root: unescape(fields[3])?,
        mount_point: unescape(fields[4])?,
        fstype: fstype.to_owned(),
        source: unescape(source)?,
        super_options: super_options.to_owned(),
    })
}

/// Parses the text of `/proc/self/mountinfo`. A line that cannot be parsed
/// makes the whole answer `None`.
#[must_use]
pub fn parse_mountinfo(text: &str) -> Option<Vec<MountEntry>> {
    text.lines().map(parse_line).collect()
}

/// True when the 9p super options name the drive C: itself: exactly one
/// `aname=` item, and in its value (split at `;`) exactly one `path=` item
/// whose value is `C:\` (case-insensitive, raw: nothing is decoded).
fn path_is_c_root(super_options: &str) -> bool {
    let mut aname = super_options
        .split(',')
        .filter_map(|item| item.strip_prefix("aname="));
    let (Some(value), None) = (aname.next(), aname.next()) else {
        return false;
    };
    let mut paths = value
        .split(';')
        .filter_map(|item| item.strip_prefix("path="));
    matches!((paths.next(), paths.next()), (Some(p), None) if p.eq_ignore_ascii_case("C:\\"))
}

/// True when `/mnt/c` is the WSL mount of the root of the drive C:: exactly
/// one entry for it (no stacked mount), root `/`, file system `9p` or
/// `drvfs`, source `C:\`, and for `9p` the option `path=C:\`; and no mount
/// on `/mnt` or on a component of [`WINDOWS_PROGRAM`] below `/mnt/c`.
#[must_use]
pub fn c_drive_mount_ok(entries: &[MountEntry]) -> bool {
    let mut on_c = entries.iter().filter(|e| e.mount_point == C_DRIVE);
    let (Some(c), None) = (on_c.next(), on_c.next()) else {
        return false;
    };
    let kind_ok = match c.fstype.as_str() {
        "9p" => path_is_c_root(&c.super_options),
        "drvfs" => true,
        _ => false,
    };
    kind_ok
        && c.root == "/"
        && c.source.eq_ignore_ascii_case("C:\\")
        && !entries
            .iter()
            .any(|e| REDIRECTS.contains(&e.mount_point.as_str()))
}

/// True when `root/Program Files` and `root/Program Files/Tailscale` are
/// directories and `root/Program Files/Tailscale/tailscale.exe` is a regular
/// file. Each one is checked by `symlink_metadata`: no component is a
/// symbolic link (WSL shows a Windows junction or link as a symbolic link).
#[must_use]
pub fn windows_tree_ok(root: &Path) -> bool {
    let files = root.join("Program Files");
    let dir = files.join("Tailscale");
    let exe = dir.join("tailscale.exe");
    let kind = |p: &Path| std::fs::symlink_metadata(p).map(|m| m.file_type());
    kind(&files).is_ok_and(|t| t.is_dir())
        && kind(&dir).is_ok_and(|t| t.is_dir())
        && kind(&exe).is_ok_and(|t| t.is_file())
}

/// The Windows program of Tailscale, when this is WSL and every check of
/// design section 15.1 passes. Nothing is run. `None` when `mountinfo`
/// cannot be read or parsed.
#[must_use]
pub fn windows_program_ok() -> Option<PathBuf> {
    let text = std::fs::read_to_string(MOUNTINFO).ok();
    program_from(is_wsl(), text.as_deref(), Path::new(C_DRIVE))
}

/// The checks of [`windows_program_ok`] on given inputs: `wsl`, the text of
/// `mountinfo` (`None`: it could not be read) and the directory `root` that
/// stands for `/mnt/c`.
fn program_from(wsl: bool, mountinfo: Option<&str>, root: &Path) -> Option<PathBuf> {
    if !wsl {
        return None;
    }
    let entries = parse_mountinfo(mountinfo?)?;
    (c_drive_mount_ok(&entries) && windows_tree_ok(root))
        .then(|| root.join("Program Files/Tailscale/tailscale.exe"))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;
    use crate::tailscale::{ProgramKind, TailscaleCli};

    /// The entry of the owner's machine (WSL 2, mirrored networking).
    const OWNER: &str = "131 80 0:70 / /mnt/c rw,noatime - 9p C:\\134 rw,aname=drvfs;path=C:\\;uid=1000;gid=1000;symlinkroot=/mnt/,cache=5,access=client,msize=65536,trans=fd,rfd=6,wfd=6";
    const ROOT: &str = "80 0 8:16 / / rw,relatime - ext4 /dev/sdb rw,discard\n";

    fn ok(text: &str) -> bool {
        c_drive_mount_ok(&parse_mountinfo(text).expect("parses"))
    }

    #[test]
    fn wsl_needs_the_kernel_text_and_the_interop_file() {
        assert!(wsl_from("5.15.167.4-microsoft-standard-WSL2", true));
        assert!(wsl_from("6.6.87.2-Microsoft-standard-WSL2", true));
        assert!(wsl_from("4.4.0-19041-Microsoft", true));
        assert!(wsl_from("5.10.0-wsl", true));
        // The kernel text alone, or the interop file alone, is not WSL.
        assert!(!wsl_from("5.15.167.4-microsoft-standard-WSL2", false));
        assert!(!wsl_from("6.8.0-45-generic", true));
        assert!(!wsl_from("", true));
        assert!(!wsl_from("", false));
    }

    #[test]
    fn the_default_facts_are_a_plain_machine() {
        let facts = HostFacts::default();
        assert!(!facts.wsl && !facts.nonlocal_bind);
    }

    #[test]
    fn the_owner_entry_is_parsed_per_field_and_accepted() {
        let entries = parse_mountinfo(&format!("{ROOT}{OWNER}\n")).unwrap();
        assert_eq!(entries.len(), 2);
        let c = &entries[1];
        assert_eq!(c.mount_point, "/mnt/c");
        assert_eq!(c.root, "/");
        assert_eq!(c.fstype, "9p");
        // The source is decoded: `C:\134` is `C:\`.
        assert_eq!(c.source, "C:\\");
        // The super options are raw: the 9p `path` value stays as printed.
        assert!(
            c.super_options
                .starts_with("rw,aname=drvfs;path=C:\\;uid=1000")
        );
        assert!(c_drive_mount_ok(&entries));
        assert!(ok(OWNER));
    }

    #[test]
    fn escapes_are_decoded_in_the_root_the_mount_point_and_the_source_only() {
        let line = r"7 1 0:5 /a\040b /mnt/x\011y rw - 9p src\040dir rw,aname=a\040b";
        let e = &parse_mountinfo(line).unwrap()[0];
        assert_eq!(e.root, "/a b");
        assert_eq!(e.mount_point, "/mnt/x\ty");
        assert_eq!(e.source, "src dir");
        assert_eq!(e.super_options, r"rw,aname=a\040b");
        // Optional fields before ` - ` are skipped.
        let line = "7 1 0:5 / /mnt/z rw shared:1 master:2 - ext4 /dev/x rw";
        assert_eq!(parse_mountinfo(line).unwrap()[0].fstype, "ext4");
    }

    #[test]
    fn a_line_that_cannot_be_parsed_makes_the_whole_text_none() {
        for bad in [
            "garbage",
            "1 2 3 / /mnt/c rw - 9p C:\\134",
            "1 2 0:70 / /mnt/c rw 9p C:\\134 rw",
            "1 2 0:70 / /mnt/c rw - 9p C:\\ rw,aname=drvfs;path=C:\\",
            "1 2 0:70 /\\9 /mnt/c rw - 9p C:\\134 rw",
            "1 2 0:70 /\\777 /mnt/c rw - 9p C:\\134 rw",
            "",
        ] {
            let text = format!("{OWNER}\n{bad}\n");
            assert!(parse_mountinfo(&text).is_none(), "{bad:?}");
        }
        // No text at all has no entries and no `/mnt/c`.
        assert_eq!(parse_mountinfo(""), Some(Vec::new()));
        assert!(!ok(""));
    }

    #[test]
    fn the_drive_mount_checks() {
        // drvfs is accepted without the 9p options.
        assert!(ok("1 2 0:70 / /mnt/c rw - drvfs C:\\134 rw,uid=1000"));
        // A wrong drive, a subtree and a wrong file system.
        assert!(!ok(&OWNER.replace("C:\\134", "D:\\134")));
        assert!(!ok(&OWNER.replace("0:70 / /mnt/c", "0:70 /Users /mnt/c")));
        assert!(!ok(&OWNER.replace("- 9p", "- ext4")));
        // `path=C:\134` with the source `C:\134`: a subtree that decodes to
        // `C:\` only if the options were decoded.
        assert!(!ok(
            "1 2 0:70 / /mnt/c rw - 9p C:\\134 rw,aname=drvfs;path=C:\\134;uid=1000"
        ));
        // A subtree `path`.
        assert!(!ok(&OWNER.replace("path=C:\\;", "path=C:\\Users\\x;")));
        // Two `path=` items, and no `path=` item.
        assert!(!ok(&OWNER.replace("path=C:\\;", "path=C:\\;path=C:\\;")));
        assert!(!ok(&OWNER.replace("path=C:\\;", "")));
        // Two `aname=` items.
        assert!(!ok(
            &OWNER.replace("rw,aname", "aname=drvfs;path=C:\\,rw,aname")
        ));
        // A second `/mnt/c` entry (stacked).
        assert!(!ok(&format!("{OWNER}\n{OWNER}")));
        // No `/mnt/c` entry.
        assert!(!ok(ROOT));
    }

    #[test]
    fn a_mount_that_redirects_a_component_is_refused() {
        for point in [
            "/mnt",
            "/mnt/c/Program\\040Files",
            "/mnt/c/Program\\040Files/Tailscale",
            "/mnt/c/Program\\040Files/Tailscale/tailscale.exe",
        ] {
            let text = format!("{OWNER}\n9 1 8:1 /x {point} rw - ext4 /dev/sda1 rw");
            assert!(!ok(&text), "{point}");
        }
        // A mount elsewhere below `/mnt/c` does not matter.
        let other = "9 1 8:1 /x /mnt/c/Users/other rw - ext4 /dev/sda1 rw";
        assert!(ok(&format!("{OWNER}\n{other}")));
        let drive = "9 1 8:1 /x /mnt/d rw - ext4 /dev/sda1 rw";
        assert!(ok(&format!("{OWNER}\n{drive}")));
    }

    fn tree(root: &Path) {
        std::fs::create_dir_all(root.join("Program Files/Tailscale")).unwrap();
        std::fs::write(root.join("Program Files/Tailscale/tailscale.exe"), b"x").unwrap();
    }

    #[test]
    fn the_tree_needs_real_directories_and_a_regular_file() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!windows_tree_ok(tmp.path()));
        tree(tmp.path());
        assert!(windows_tree_ok(tmp.path()));
        // A missing file, and a directory in place of the file.
        let exe = tmp.path().join("Program Files/Tailscale/tailscale.exe");
        std::fs::remove_file(&exe).unwrap();
        assert!(!windows_tree_ok(tmp.path()));
        std::fs::create_dir(&exe).unwrap();
        assert!(!windows_tree_ok(tmp.path()));
    }

    #[test]
    fn a_symbolic_link_at_any_component_fails_the_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        tree(&real);
        // `Program Files` is a link.
        let root = tmp.path().join("c1");
        std::fs::create_dir(&root).unwrap();
        symlink(real.join("Program Files"), root.join("Program Files")).unwrap();
        assert!(!windows_tree_ok(&root));
        // `Tailscale` is a link.
        let root = tmp.path().join("c2");
        std::fs::create_dir_all(root.join("Program Files")).unwrap();
        symlink(
            real.join("Program Files/Tailscale"),
            root.join("Program Files/Tailscale"),
        )
        .unwrap();
        assert!(!windows_tree_ok(&root));
        // `tailscale.exe` is a link.
        let root = tmp.path().join("c3");
        std::fs::create_dir_all(root.join("Program Files/Tailscale")).unwrap();
        symlink(
            real.join("Program Files/Tailscale/tailscale.exe"),
            root.join("Program Files/Tailscale/tailscale.exe"),
        )
        .unwrap();
        assert!(!windows_tree_ok(&root));
    }

    #[test]
    fn the_program_needs_wsl_a_readable_mountinfo_and_every_check() {
        let tmp = tempfile::tempdir().unwrap();
        tree(tmp.path());
        let exe = tmp.path().join("Program Files/Tailscale/tailscale.exe");
        assert_eq!(program_from(true, Some(OWNER), tmp.path()), Some(exe));
        // Not WSL, an unreadable mountinfo, an unparseable one, a wrong mount.
        assert_eq!(program_from(false, Some(OWNER), tmp.path()), None);
        assert_eq!(program_from(true, None, tmp.path()), None);
        assert_eq!(program_from(true, Some("garbage"), tmp.path()), None);
        assert_eq!(program_from(true, Some(ROOT), tmp.path()), None);
        // A good mount and a tree that is missing.
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(program_from(true, Some(OWNER), empty.path()), None);
    }

    #[test]
    fn locate_prefers_the_setting_then_path_then_the_windows_program() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("tailscale");
        std::fs::write(&bin, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let path_var = std::env::join_paths([tmp.path()]).unwrap();
        let windows = || Some(PathBuf::from(WINDOWS_PROGRAM));
        let set = Path::new("/opt/set/tailscale");
        let cli = TailscaleCli::locate_with(Some(set), Some(&path_var), None, windows);
        assert_eq!((cli.program(), cli.kind()), (set, ProgramKind::Configured));
        let cli = TailscaleCli::locate_with(None, Some(&path_var), None, windows);
        assert_eq!(
            (cli.program(), cli.kind()),
            (bin.as_path(), ProgramKind::Path)
        );
        // The macOS app comes before the Windows program.
        let app = tmp.path().join("app");
        std::fs::write(&app, b"x").unwrap();
        let cli = TailscaleCli::locate_with(None, None, Some(&app), windows);
        assert_eq!(
            (cli.program(), cli.kind()),
            (app.as_path(), ProgramKind::MacosApp)
        );
        // Nothing else: the Windows program, only when the checks gave it.
        let cli = TailscaleCli::locate_with(None, None, None, windows);
        assert_eq!(
            (cli.program(), cli.kind()),
            (Path::new(WINDOWS_PROGRAM), ProgramKind::Windows)
        );
        // Not WSL (or a failed check): the bare name, looked up on PATH.
        let cli = TailscaleCli::locate_with(None, None, None, || None);
        assert_eq!(
            (cli.program(), cli.kind()),
            (Path::new("tailscale"), ProgramKind::Path)
        );
        // The setting wins without running the Windows checks.
        let _ =
            TailscaleCli::locate_with(Some(set), None, None, || panic!("the Windows checks ran"));
        assert_eq!(ProgramKind::Windows.as_str(), "windows");
        assert_eq!(ProgramKind::MacosApp.as_str(), "macos_app");
    }
}
