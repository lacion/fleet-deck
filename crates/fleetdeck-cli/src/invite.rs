//! Reads a pairing invitation (G12): from a prompt that does not echo, from
//! stdin (`--stdin`), or from `--invite-file` with mode 0600. Never from
//! argv: `clap` has no argument for it. The text stays in a `Secret` and
//! no error message contains it.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use anyhow::{Context, bail};
use fleetdeck_core::secret::Secret;

use crate::cli::InviteInput;

/// The start of every invitation text (`fleetdeck_link::pairing::PREFIX`).
pub const PREFIX: &str = "fdinv1:";

/// An invitation is about 160 bytes; more is not an invitation.
pub const MAX_BYTES: u64 = 4096;

/// Reads the invitation from the source that `input` selects.
pub fn read(input: &InviteInput) -> anyhow::Result<Secret<String>> {
    if let Some(path) = &input.invite_file {
        return from_file(path);
    }
    if input.stdin {
        return from_reader(io::stdin().lock());
    }
    from_prompt()
}

/// The file must be a regular file that only its owner can read or write.
pub fn from_file(path: &Path) -> anyhow::Result<Secret<String>> {
    let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let meta = file
        .metadata()
        .with_context(|| format!("cannot read the metadata of {}", path.display()))?;
    if !meta.is_file() {
        bail!("{} is not a regular file", path.display());
    }
    // Exactly 0600 (review round 18): no execute bit, no group or other
    // bits, owner read and write.
    if meta.mode() & 0o7777 != 0o600 {
        bail!(
            "{} has mode {:o}: an invitation file must have mode 0600 (chmod 600 {})",
            path.display(),
            meta.mode() & 0o7777,
            path.display()
        );
    }
    from_reader(file)
}

/// The first line of `r`, without surrounding white space, at most
/// [`MAX_BYTES`].
pub fn from_reader(r: impl Read) -> anyhow::Result<Secret<String>> {
    let mut line = String::new();
    BufReader::new(r.take(MAX_BYTES))
        .read_line(&mut line)
        .context("cannot read the invitation")?;
    let line = Secret::new(line);
    let text = line.expose().trim();
    if text.is_empty() {
        bail!("no invitation was given");
    }
    if text.len() as u64 >= MAX_BYTES - 1 {
        bail!("the input is too long to be an invitation");
    }
    Ok(Secret::new(text.to_owned()))
}

/// Restores the terminal mode when dropped.
struct EchoOff<'a> {
    fd: std::os::fd::BorrowedFd<'a>,
    saved: rustix::termios::Termios,
}

impl Drop for EchoOff<'_> {
    fn drop(&mut self) {
        let _ =
            rustix::termios::tcsetattr(self.fd, rustix::termios::OptionalActions::Now, &self.saved);
    }
}

fn echo_off(fd: std::os::fd::BorrowedFd<'_>) -> io::Result<EchoOff<'_>> {
    let saved = rustix::termios::tcgetattr(fd)?;
    let mut quiet = saved.clone();
    quiet.local_modes &= !rustix::termios::LocalModes::ECHO;
    rustix::termios::tcsetattr(fd, rustix::termios::OptionalActions::Now, &quiet)?;
    Ok(EchoOff { fd, saved })
}

fn from_prompt() -> anyhow::Result<Secret<String>> {
    let stdin = io::stdin();
    let fd = stdin.as_fd();
    if !rustix::termios::isatty(fd) {
        bail!("standard input is not a terminal: use --stdin or --invite-file");
    }
    eprint!("Paste the invitation (input is hidden): ");
    io::stderr().flush().ok();
    let text = {
        let _quiet = echo_off(fd).context("cannot turn off the terminal echo")?;
        from_reader(stdin.lock())
    };
    eprintln!();
    text
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    const INV: &str = "fdinv1:wss://127.0.0.1:7443#aa.bb.cc.1";

    #[test]
    fn the_first_line_is_read_and_trimmed() {
        let s = from_reader(format!("  {INV}  \nrest\n").as_bytes()).unwrap();
        assert_eq!(s.expose(), INV);
        assert_eq!(format!("{s:?}"), "Secret(<redacted>)");
    }

    #[test]
    fn empty_and_huge_input_is_refused_without_echoing_it() {
        assert!(from_reader("\n".as_bytes()).is_err());
        let huge = "x".repeat(10_000);
        let e = from_reader(huge.as_bytes()).unwrap_err().to_string();
        assert!(!e.contains("xxxx"), "{e}");
    }

    #[test]
    fn an_invitation_file_must_have_mode_0600() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inv.txt");
        fs::write(&path, format!("{INV}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let e = format!("{:#}", from_file(&path).unwrap_err());
        assert!(e.contains("mode 644"), "{e}");
        assert!(
            !e.contains("fdinv1"),
            "the error never shows the invitation"
        );
        for mode in [0o700, 0o400, 0o640, 0o4600] {
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            let e = format!("{:#}", from_file(&path).unwrap_err());
            assert!(e.contains(&format!("mode {mode:o}")), "{e}");
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(from_file(&path).unwrap().expose(), INV);
        assert!(from_file(dir.path()).is_err(), "a directory is refused");
    }
}
