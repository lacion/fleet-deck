//! The tree digest of a folder (design §3.6, step A1).
//!
//! SHA-256 over the sorted list of `(relative path, kind, mode, SHA-256 of
//! the content or the link text)` of every entry of the folder. Links are
//! not followed. The rollback copy of an npm or Bun install is checked with
//! it (§3.6.2).

use std::fs;
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use ring::digest::{Context, SHA256};

/// One entry of the sorted list.
struct Entry {
    rel: Vec<u8>,
    kind: u8,
    mode: u32,
    content: [u8; 32],
}

/// The tree digest of `root` as 64 lower-case hex digits. `root` itself may
/// be a link: then the digest covers the link text only.
///
/// # Errors
/// Any read error of the folder.
pub fn tree_digest(root: &Path) -> io::Result<String> {
    let mut entries = Vec::new();
    let meta = fs::symlink_metadata(root)?;
    if meta.file_type().is_dir() {
        walk(root, Path::new(""), &mut entries)?;
    } else {
        entries.push(entry_of(root, Path::new(""), &meta)?);
    }
    entries.sort_by(|a, b| a.rel.cmp(&b.rel));
    let mut ctx = Context::new(&SHA256);
    for e in &entries {
        ctx.update(&(e.rel.len() as u64).to_be_bytes());
        ctx.update(&e.rel);
        ctx.update(&[e.kind]);
        ctx.update(&e.mode.to_be_bytes());
        ctx.update(&e.content);
    }
    Ok(hex(ctx.finish().as_ref()))
}

/// Lower-case hex of `bytes`.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

fn walk(dir: &Path, rel: &Path, out: &mut Vec<Entry>) -> io::Result<()> {
    for item in fs::read_dir(dir)? {
        let item = item?;
        let path: PathBuf = item.path();
        let rel_path = rel.join(item.file_name());
        let meta = fs::symlink_metadata(&path)?;
        out.push(entry_of(&path, &rel_path, &meta)?);
        if meta.file_type().is_dir() {
            walk(&path, &rel_path, out)?;
        }
    }
    Ok(())
}

fn entry_of(path: &Path, rel: &Path, meta: &fs::Metadata) -> io::Result<Entry> {
    let ft = meta.file_type();
    let (kind, content) = if ft.is_symlink() {
        let text = fs::read_link(path)?;
        (b'l', sha(text.as_os_str().as_bytes()))
    } else if ft.is_dir() {
        (b'd', [0u8; 32])
    } else if ft.is_file() {
        (b'f', sha_file(path)?)
    } else {
        (b'o', [0u8; 32])
    };
    let mode = if ft.is_symlink() {
        0
    } else {
        meta.permissions().mode() & 0o7777
    };
    Ok(Entry {
        rel: rel.as_os_str().as_bytes().to_vec(),
        kind,
        mode,
        content,
    })
}

fn sha(bytes: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(ring::digest::digest(&SHA256, bytes).as_ref());
    out
}

fn sha_file(path: &Path) -> io::Result<[u8; 32]> {
    let mut f = fs::File::open(path)?;
    let mut ctx = Context::new(&SHA256);
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        ctx.update(&buf[..n]);
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(ctx.finish().as_ref());
    Ok(out)
}

#[cfg(test)]
#[path = "tree_digest_tests.rs"]
mod tests;
