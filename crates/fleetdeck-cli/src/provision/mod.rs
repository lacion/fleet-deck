//! The provisioning bootstrap of `fleetdeck provision-bootstrap` (P8, design
//! `omarchy-p8.md` 3): a durable state machine over an [`effects::Effects`]
//! trait. Task T3a is the pure logic; task T3b writes the real effects and
//! wires the hidden command.

pub mod effects;
pub mod real;
pub mod release;
pub mod retry;
pub mod run;
pub mod state;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod release_tests;
#[cfg(test)]
mod run_tests;
#[cfg(test)]
mod run_tests_flow;
#[cfg(test)]
mod run_tests_steps;
#[cfg(test)]
mod state_tests;

use std::fmt::Write as _;

/// The 64 lowercase hex digits of the SHA-256 of `bytes`.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    let mut out = String::with_capacity(64);
    for byte in digest.as_ref() {
        // Writing to a `String` cannot fail.
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_is_the_lowercase_digest() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn the_logic_never_writes_to_stdout() {
        let needle = concat!("print", "ln!(");
        let other = concat!("print", "!(");
        for (name, text) in [
            ("mod.rs", include_str!("mod.rs")),
            ("effects.rs", include_str!("effects.rs")),
            ("release.rs", include_str!("release.rs")),
            ("retry.rs", include_str!("retry.rs")),
            ("run.rs", include_str!("run.rs")),
            ("state.rs", include_str!("state.rs")),
        ] {
            // `eprint` is also forbidden: stderr goes through `progress`.
            let hits = text
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .filter(|l| l.contains(needle) || l.contains(other))
                .filter(|l| !l.contains("concat!"))
                .count();
            assert_eq!(hits, 0, "{name}");
        }
    }
}
