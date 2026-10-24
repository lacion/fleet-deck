//! The SHA-256 of the running binary (R13 P8 T2, design `omarchy-p8.md`
//! 5.2): `station fingerprint` reports it so that bootstrap can compare it
//! with the trusted package.
//!
//! [`start`] reads the file behind `std::env::current_exe()` once, in a
//! blocking task, at station start. [`get`] waits for that result. A read
//! that fails gives `None` for the life of the process.

use std::fmt::Write as _;

use tokio::sync::OnceCell;

/// The size of one read.
const CHUNK: usize = 64 * 1024;

static HASH: OnceCell<Option<String>> = OnceCell::const_new();

/// The 64 lowercase hex digits of the SHA-256 of `reader`, read in 64 KiB
/// steps.
///
/// # Errors
/// A read error.
fn sha256_hex(mut reader: impl std::io::Read) -> std::io::Result<String> {
    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = vec![0u8; CHUNK];
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        context.update(&buffer[..read]);
    }
    let digest = context.finish();
    let mut out = String::with_capacity(64);
    for byte in digest.as_ref() {
        // Writing to a `String` cannot fail.
        let _ = write!(out, "{byte:02x}");
    }
    Ok(out)
}

fn compute() -> Option<String> {
    let result = std::env::current_exe()
        .and_then(std::fs::File::open)
        .and_then(sha256_hex);
    match result {
        Ok(hash) => Some(hash),
        Err(e) => {
            tracing::warn!(error = %e, "the hash of the running binary could not be read");
            None
        }
    }
}

async fn load() -> Option<String> {
    match tokio::task::spawn_blocking(compute).await {
        Ok(hash) => hash,
        Err(e) => {
            tracing::warn!(error = %e, "the binary hash task failed");
            None
        }
    }
}

/// Starts the hash of the running binary in the background. A second call
/// does nothing new.
pub fn start() {
    tokio::spawn(async {
        HASH.get_or_init(load).await;
    });
}

/// The hash of the running binary; waits for [`start`] (or does the work
/// itself when nobody started it). `None` when the file could not be read.
pub async fn get() -> Option<String> {
    HASH.get_or_init(load).await.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hash_of_known_bytes_is_the_sha256_in_lowercase_hex() {
        // SHA-256 of "abc" (FIPS 180-2 test vector).
        assert_eq!(
            sha256_hex(&b"abc"[..]).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // SHA-256 of nothing.
        assert_eq!(
            sha256_hex(&b""[..]).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn a_file_larger_than_one_chunk_hashes_like_the_whole_content() {
        let data = vec![0x5a_u8; CHUNK * 2 + 17];
        let whole = ring::digest::digest(&ring::digest::SHA256, &data);
        let expected: String = whole.as_ref().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(sha256_hex(&data[..]).unwrap(), expected);
    }

    #[test]
    fn a_read_error_is_an_error() {
        struct Broken;
        impl std::io::Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("read failed"))
            }
        }
        assert!(sha256_hex(Broken).is_err());
    }

    #[tokio::test]
    async fn get_gives_the_hash_of_the_running_binary() {
        let hash = get().await.unwrap();
        assert_eq!(hash.len(), 64);
        assert!(
            hash.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        let exe = std::fs::File::open(std::env::current_exe().unwrap()).unwrap();
        assert_eq!(hash, sha256_hex(exe).unwrap());
        // `start` after the result exists is harmless.
        start();
        assert_eq!(get().await.unwrap(), hash);
    }
}
