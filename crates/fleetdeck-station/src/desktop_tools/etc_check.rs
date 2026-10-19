//! The root-owned configuration check of the install plan
//! (`board-gaps.md` §2.4, revision 4): `/etc`, `/etc/pacman.d`,
//! `/etc/pacman.conf` and `/etc/pacman.d/mirrorlist` must belong to root and
//! have no group or other write bit. The check needs no root. It returns the
//! SHA-256 of `/etc/pacman.conf`.

use std::fs::File;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use ring::digest::{SHA256, digest};

/// The most bytes of `pacman.conf` that the station reads.
const CONF_MAX: u64 = 1024 * 1024;

/// The root of the checked tree and the owner that its entries must have.
/// Production uses `/` and the UID 0; a test uses a temporary folder and its
/// own UID.
#[derive(Debug, Clone)]
pub struct EtcRoot {
    pub path: PathBuf,
    pub owner: u32,
}

impl EtcRoot {
    /// The real `/etc`, owned by root.
    #[must_use]
    pub fn system() -> Self {
        Self {
            path: PathBuf::from("/"),
            owner: 0,
        }
    }
}

/// Whether `rel` below the root exists, is not a link, has the owner and
/// has no group or other write bit.
fn checked(root: &EtcRoot, rel: &str) -> bool {
    std::fs::symlink_metadata(root.path.join(rel)).is_ok_and(|m| {
        !m.file_type().is_symlink() && m.uid() == root.owner && m.mode() & 0o022 == 0
    })
}

/// The SHA-256 of `etc/pacman.conf` as 64 lowercase hex characters, or
/// `None` when any check fails, the file is longer than 1 MiB, or it cannot
/// be read.
#[must_use]
pub fn conf_sha256(root: &EtcRoot) -> Option<String> {
    let all = [
        "etc",
        "etc/pacman.d",
        "etc/pacman.conf",
        "etc/pacman.d/mirrorlist",
    ];
    if !all.iter().all(|rel| checked(root, rel)) {
        return None;
    }
    let file = File::open(root.path.join("etc/pacman.conf")).ok()?;
    let mut bytes = Vec::new();
    file.take(CONF_MAX + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > CONF_MAX {
        return None;
    }
    Some(
        digest(&SHA256, &bytes)
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    )
}
