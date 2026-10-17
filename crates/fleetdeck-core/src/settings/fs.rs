//! The file operations of a save, behind a small trait so that a test can
//! fail one call (DL-172, design 1.3 and T1).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

/// The calls that change or sync files. Reads go straight to `std::fs`.
pub trait Fs: Send + Sync {
    /// Creates a new file with `O_CREAT|O_EXCL` and mode 0600.
    fn create_excl_0600(&self, path: &Path) -> io::Result<File>;
    /// `path` names the open file. The real file system ignores it; a test
    /// file system uses it to choose the call that fails.
    fn write(&self, file: &mut File, path: &Path, bytes: &[u8]) -> io::Result<()>;
    fn fsync(&self, file: &File, path: &Path) -> io::Result<()>;
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// Syncs a directory, so that a created, renamed or removed entry is
    /// durable.
    fn fsync_dir(&self, dir: &Path) -> io::Result<()>;
    fn remove(&self, path: &Path) -> io::Result<()>;
    /// Creates one directory with mode 0700 (the parent exists).
    fn mkdir(&self, path: &Path) -> io::Result<()>;
}

/// The real file system.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealFs;

impl Fs for RealFs {
    fn create_excl_0600(&self, path: &Path) -> io::Result<File> {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
    }

    fn write(&self, file: &mut File, _path: &Path, bytes: &[u8]) -> io::Result<()> {
        file.write_all(bytes)
    }

    fn fsync(&self, file: &File, _path: &Path) -> io::Result<()> {
        file.sync_all()
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }

    fn fsync_dir(&self, dir: &Path) -> io::Result<()> {
        File::open(dir)?.sync_all()
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)
    }

    fn mkdir(&self, path: &Path) -> io::Result<()> {
        fs::DirBuilder::new().mode(0o700).create(path)
    }
}
