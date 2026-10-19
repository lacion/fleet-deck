//! The maintenance runner (`board-gaps.md` §2.9, BG-1): a separate process,
//! `fleetdeck maintenance-run --id <id> --dir <dir>`, that reads its spec,
//! runs the fixed `sudo pacman` transaction in an echo-off PTY, serves one
//! authenticated client on a unix socket with a 256 KiB replay ring, and
//! records its exit, its output and a post-check.
//!
//! The runner never logs input bytes, output bytes or tokens. No value from
//! the wire reaches its argv: the packages come from the compiled mapping
//! (`crate::desktop_tools::mapping`).
//!
//! Files: `files.rs` (the folder, the spec, the state and the output),
//! `frame.rs` (the socket frames), `ring.rs` (the replay ring), `runner.rs`
//! (the start, the PTY and the exit), `client.rs` (the socket), `postcheck.rs`
//! (the versions after the exit).

mod client;
pub mod files;
pub mod frame;
pub mod postcheck;
pub mod ring;
pub mod runner;

#[cfg(test)]
mod files_tests;
#[cfg(test)]
mod frame_tests;
#[cfg(test)]
mod postcheck_tests;
#[cfg(test)]
mod ring_tests;
#[cfg(test)]
mod runner_tests;
#[cfg(test)]
mod test_support;
