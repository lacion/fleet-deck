//! `fleetdeck provision-import --source <absolute-path>` (P8 plan D14,
//! design `omarchy-p8.md` 4): copies the provisioning file from a supported
//! medium into `<config home>/fleetdeck/provision.toml`.
//!
//! The command reads only the one given file, through one descriptor, and
//! never runs anything from the medium. The destination is created through
//! [`fleetdeck_core::private_file::write_new_shared_parent`]: mode 0600,
//! atomic, never over a different file. Source modes are ignored.
//!
//! Every refusal is a [`Refused`] (`<code>: <text>` on stderr, exit 1).
//! Success prints one line on stderr and nothing on stdout.

use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::Path;

use fleetdeck_core::paths::Env;
use fleetdeck_core::private_file::{self, PrivateWriteError};
use fleetdeck_core::provision::{self, MAX_BYTES};
use rustix::fs::{Mode, OFlags, open};
use rustix::io::Errno;
use rustix::process::geteuid;

use crate::tailnet_join::{Refused, check_name, parse_target};

/// The prefix of the temporary leaf next to the destination.
const TEMP_PREFIX: &str = ".provision-import.";

const INVALID_ARGUMENTS: &str = "The import arguments are not valid.";
const UNSAFE_SOURCE: &str = "The source is a link, not a regular file, or larger than 16 KiB.";
const SOURCE_UNREADABLE: &str = "The source file could not be read.";
const UNSUPPORTED_MEDIUM: &str = "The source file system is not supported for provisioning media.";
const FILE_INVALID: &str = "The provisioning file is not valid.";
const OUTPUT_EXISTS: &str = "A different provisioning file already exists.";
const UNSAFE_OUTPUT: &str = "The destination folder is not private or contains a link.";
const WRITE_FAILED: &str = "The provisioning file could not be written.";

/// The file system types of a supported medium: iso9660, ext2/3/4, xfs,
/// btrfs and f2fs. FAT, exFAT, NTFS through FUSE, tmpfs and every other
/// type are refused.
const MEDIA: [i64; 5] = [
    0x9660,      // iso9660
    0xEF53,      // ext2, ext3, ext4
    0x5846_5342, // xfs
    0x9123_683E, // btrfs
    0xF2F5_2010, // f2fs
];

/// What a successful import did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Imported,
    AlreadyImported,
}

/// True when `f_type` (the `statfs` magic number) is a supported medium.
#[must_use]
pub fn medium_ok(f_type: i64) -> bool {
    MEDIA.contains(&f_type)
}

fn refused(code: &str, detail: &str) -> Refused {
    Refused {
        code: code.to_owned(),
        detail: detail.to_owned(),
    }
}

/// The `statfs` type of the file system of the open file. Only Linux is
/// known: another system gives 0, which is not a supported medium.
fn real_fs_type(file: &File) -> io::Result<i64> {
    #[cfg(target_os = "linux")]
    {
        let stat = rustix::fs::fstatfs(file).map_err(io::Error::from)?;
        // A 32-bit `f_type` can be signed: keep its 32 bits. The width of
        // `f_type` depends on the target: it is `i64` on 64-bit Linux.
        #[allow(clippy::useless_conversion)]
        i64::try_from(stat.f_type)
            .map(|t| t & 0xFFFF_FFFF)
            .map_err(|_| io::Error::other("unknown file system type"))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = file;
        Ok(0)
    }
}

/// `fleetdeck provision-import`.
///
/// # Errors
/// A [`Refused`] with the code of the failed rule.
pub fn run(source: &Path, env: &Env) -> anyhow::Result<()> {
    let config_home =
        crate::omarchy::config_home(env).map_err(|_| refused("write_failed", WRITE_FAILED))?;
    match import(source, &config_home, &real_fs_type)? {
        Outcome::Imported => eprintln!("Provisioning file imported."),
        Outcome::AlreadyImported => eprintln!("Provisioning file already imported."),
    }
    Ok(())
}

/// The import with a given file system type check (a test gives its own).
///
/// # Errors
/// A [`Refused`] with the code of the failed rule.
pub fn import(
    source: &Path,
    config_home: &Path,
    fs_type: &dyn Fn(&File) -> io::Result<i64>,
) -> Result<Outcome, Refused> {
    if !source.is_absolute() {
        return Err(refused("invalid_request", INVALID_ARGUMENTS));
    }
    let bytes = read_source(source, fs_type)?;
    check_content(&bytes)?;
    publish(config_home, &bytes)
}

/// Source rules 2 and 3: the open, the regular file, the size, the read,
/// and the file system type of the same descriptor.
fn read_source(
    source: &Path,
    fs_type: &dyn Fn(&File) -> io::Result<i64>,
) -> Result<Vec<u8>, Refused> {
    let unreadable = || refused("source_unreadable", SOURCE_UNREADABLE);
    let unsafe_source = || refused("unsafe_source", UNSAFE_SOURCE);
    let fd = match open(
        source,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::LOOP) => return Err(unsafe_source()),
        Err(_) => return Err(unreadable()),
    };
    let file = File::from(fd);
    let meta = file.metadata().map_err(|_| unreadable())?;
    if !meta.is_file() || meta.len() > MAX_BYTES as u64 {
        return Err(unsafe_source());
    }
    let bytes = read_limited(&file).map_err(|_| unreadable())?;
    if bytes.len() > MAX_BYTES {
        return Err(unsafe_source());
    }
    let f_type = fs_type(&file).map_err(|_| unreadable())?;
    if !medium_ok(f_type) {
        return Err(refused("unsupported_medium", UNSUPPORTED_MEDIUM));
    }
    Ok(bytes)
}

/// At most `MAX_BYTES + 1` bytes of the file.
fn read_limited(file: &File) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// `host:port` with brackets around an IPv6 address.
fn canonical(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// Source rule 4: the bytes are a provisioning file with a valid name and
/// a deck address in the canonical `host:port` form.
fn check_content(bytes: &[u8]) -> Result<(), Refused> {
    let invalid = || refused("file_invalid", FILE_INVALID);
    let file = provision::parse(bytes).map_err(|_| invalid())?;
    check_name(&file.name).map_err(|_| invalid())?;
    let (host, port) = parse_target(&file.deck).map_err(|_| invalid())?;
    if canonical(&host, port) != file.deck {
        return Err(invalid());
    }
    Ok(())
}

/// The destination rules: the private folder, the identical file, and the
/// no-replace write.
fn publish(config_home: &Path, bytes: &[u8]) -> Result<Outcome, Refused> {
    let write_failed = || refused("write_failed", WRITE_FAILED);
    let dir = config_home.join("fleetdeck");
    match fs::symlink_metadata(&dir) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&dir)
                .map_err(|_| write_failed())?;
        }
        Err(_) => return Err(write_failed()),
    }
    let meta = fs::symlink_metadata(&dir).map_err(|_| write_failed())?;
    if !meta.is_dir() || meta.uid() != geteuid().as_raw() || meta.mode() & 0o022 != 0 {
        return Err(refused("unsafe_output", UNSAFE_OUTPUT));
    }
    let dest = dir.join("provision.toml");
    match fs::symlink_metadata(&dest) {
        Ok(meta) => {
            return if meta.is_file() && same_bytes(&dest, bytes) {
                Ok(Outcome::AlreadyImported)
            } else {
                Err(refused("output_exists", OUTPUT_EXISTS))
            };
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(refused("output_exists", OUTPUT_EXISTS)),
    }
    match private_file::write_new_shared_parent(&dest, bytes, TEMP_PREFIX) {
        Ok(()) => Ok(Outcome::Imported),
        Err(PrivateWriteError::Exists) => Err(refused("output_exists", OUTPUT_EXISTS)),
        Err(PrivateWriteError::Unsafe) => Err(refused("unsafe_output", UNSAFE_OUTPUT)),
        Err(PrivateWriteError::Io(_)) => Err(write_failed()),
    }
}

/// True when the regular file `dest` has exactly `bytes`. A link, a file
/// over 16 KiB and a file that cannot be read are "different".
fn same_bytes(dest: &Path, bytes: &[u8]) -> bool {
    let Ok(fd) = open(
        dest,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    ) else {
        return false;
    };
    let file = File::from(fd);
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return false;
    }
    read_limited(&file).is_ok_and(|existing| existing == bytes)
}

#[cfg(test)]
#[path = "provision_import_tests.rs"]
mod tests;
