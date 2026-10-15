//! The hash of an executable file (DL-194): the shim proves its build to
//! the station before it gets the session token.

use std::io::Read as _;
use std::path::{Path, PathBuf};

/// The largest executable that is hashed.
const MAX_EXE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// The running program's own executable object: `/proc/self/exe` on Linux
/// (it names the executed object even after its path was replaced), else
/// `std::env::current_exe()`.
///
/// # Errors
/// `current_exe()` fails (not Linux).
pub fn own_exe() -> std::io::Result<PathBuf> {
    if cfg!(target_os = "linux") {
        Ok(PathBuf::from("/proc/self/exe"))
    } else {
        std::env::current_exe()
    }
}

/// The lower-case hex SHA-256 of the file at `path`, read in 64 KiB chunks.
///
/// # Errors
/// The file cannot be read, or it is over 2 GiB.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = vec![0u8; 64 * 1024];
    let mut total: u64 = 0;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > MAX_EXE_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "the executable is too large to hash",
            ));
        }
        context.update(&buf[..n]);
    }
    Ok(super::wire::hex(context.finish().as_ref()))
}
