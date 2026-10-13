//! The Git access check before a clone (DL-144): `workspace.preflight`.
//!
//! The station reads the remote's branch list with its own Git credentials,
//! without a prompt, so the board can show a hint before a clone fails.

use std::path::{Path, PathBuf};
use std::time::Duration;

use fleetdeck_proto::link::Preflight;

use crate::workspace::provision::{self, Refused};
use crate::workspace::{Git, GitError, GitOptions};

/// The time limit of `git ls-remote`.
pub const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(20);
/// The most characters of the Git error text in an answer.
pub const MESSAGE_CHARS: usize = 2000;

/// Why a check gave no answer about the remote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreflightError {
    /// The URL is refused (the same rules as a clone URL).
    Refused(Refused),
    /// `git` did not start.
    Spawn(String),
}

/// Checks that the station can read `url`. A failed check is an answer
/// (`ok: false`), not an error.
///
/// # Errors
/// [`PreflightError`]: a refused URL, or a `git` that does not start.
pub async fn run(
    url: &str,
    canonical_inside: impl Fn(&Path) -> Option<PathBuf>,
    program: &Path,
) -> Result<Preflight, PreflightError> {
    provision::check_url(url, canonical_inside).map_err(PreflightError::Refused)?;
    // Like a clone, the check needs the owner's credential helpers and SSH
    // settings.
    let git = Git::new(
        std::env::temp_dir(),
        GitOptions {
            program: program.to_path_buf(),
            timeout: PREFLIGHT_TIMEOUT,
            isolated: false,
        },
    );
    let result = git
        .status(
            &[
                "-c",
                "protocol.ext.allow=never",
                "-c",
                "core.sshCommand=ssh -o BatchMode=yes -o ConnectTimeout=10",
                "ls-remote",
                "--exit-code",
                "--heads",
                "--",
                url,
            ],
            &[],
            None,
        )
        .await;
    match result {
        // Exit code 2: no matching refs, an empty repository.
        Ok((0 | 2, _, _)) => Ok(Preflight {
            ok: true,
            message: String::new(),
            hint: None,
        }),
        Ok((_, _, stderr)) => Ok(failed(&stderr, false)),
        Err(e @ GitError::Timeout { .. }) => Ok(failed(&e.to_string(), true)),
        Err(e) => Err(PreflightError::Spawn(provision::scrub(&e.to_string()))),
    }
}

fn failed(stderr: &str, timeout: bool) -> Preflight {
    let scrubbed = provision::scrub(stderr);
    let skip = scrubbed.chars().count().saturating_sub(MESSAGE_CHARS);
    Preflight {
        ok: false,
        message: scrubbed.chars().skip(skip).collect(),
        hint: hint(stderr, timeout).map(str::to_owned),
    }
}

/// What the owner can do, from the Git error text (the first match).
#[must_use]
pub fn hint(stderr: &str, timeout: bool) -> Option<&'static str> {
    let has = |needle: &str| stderr.contains(needle);
    if has("Permission denied (publickey)") {
        Some(
            "The station's SSH key has no access. Add the station user's SSH key to the Git host, or use an https:// URL.",
        )
    } else if has("could not read Username")
        || has("Authentication failed")
        || has("terminal prompts disabled")
    {
        Some(
            "The station has no credentials for this https URL. Use an SSH URL, or set up a credential helper for the station user.",
        )
    } else if has("Repository not found")
        || has("not found")
        || has("does not appear to be a git repository")
    {
        Some("The repository does not exist, or the station has no access to it.")
    } else if has("Could not resolve host") {
        Some("The station cannot resolve the host name.")
    } else if timeout {
        Some("The Git host did not answer within 20 s.")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .current_dir(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {out:?}");
    }

    fn inside(root: &Path) -> impl Fn(&Path) -> Option<PathBuf> + '_ {
        move |p| {
            let c = p.canonicalize().ok()?;
            c.starts_with(root).then_some(c)
        }
    }

    fn file_url(dir: &Path) -> String {
        format!("file://{}", dir.display())
    }

    #[tokio::test]
    async fn a_local_repository_is_ok_empty_or_not() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let empty = root.join("empty.git");
        std::fs::create_dir(&empty).unwrap();
        git(&empty, &["init", "-q", "--bare"]);
        let full = root.join("full");
        std::fs::create_dir(&full).unwrap();
        git(&full, &["init", "-q"]);
        std::fs::write(full.join("a.txt"), "one\n").unwrap();
        git(&full, &["add", "a.txt"]);
        git(&full, &["commit", "-qm", "one"]);
        for dir in [&empty, &full] {
            let got = run(&file_url(dir), inside(&root), Path::new("git"))
                .await
                .unwrap();
            assert_eq!(
                got,
                Preflight {
                    ok: true,
                    message: String::new(),
                    hint: None
                },
                "{}",
                dir.display()
            );
        }
    }

    #[tokio::test]
    async fn a_directory_that_is_not_a_repository_is_not_ok_and_has_a_hint() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let plain = root.join("plain");
        std::fs::create_dir(&plain).unwrap();
        let got = run(&file_url(&plain), inside(&root), Path::new("git"))
            .await
            .unwrap();
        assert!(!got.ok);
        assert!(!got.message.is_empty());
        assert!(got.hint.unwrap().contains("does not exist"));
    }

    #[tokio::test]
    async fn urls_with_credentials_or_outside_the_roots_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        for url in [
            "https://u:p@h/r".to_owned(),
            "https://tok@h/r".to_owned(),
            "ext::sh%20-c%20x".to_owned(),
            "file:///etc".to_owned(),
        ] {
            let err = run(&url, inside(&root), Path::new("git"))
                .await
                .unwrap_err();
            assert!(
                matches!(&err, PreflightError::Refused(r) if r.forbidden),
                "{url}: {err:?}"
            );
        }
        let err = run("", inside(&root), Path::new("git")).await.unwrap_err();
        assert!(matches!(&err, PreflightError::Refused(r) if !r.forbidden));
    }

    #[test]
    fn hints_follow_the_error_text_first_match_first() {
        let h = |t: &str| hint(t, false).unwrap_or("");
        assert!(h("git@h: Permission denied (publickey).").contains("SSH key"));
        assert!(
            h("fatal: could not read Username for 'https://h': terminal prompts disabled")
                .contains("credential helper")
        );
        assert!(h("remote: Repository not found.").contains("does not exist"));
        assert!(h("fatal: Could not resolve host: h").contains("resolve"));
        assert_eq!(hint("something else", false), None);
        assert!(
            hint("git ls-remote did not end", true)
                .unwrap()
                .contains("20 s")
        );
    }

    #[test]
    fn the_message_is_scrubbed_and_cut_to_its_end() {
        let long = format!("{} fatal: https://u:tok@h/r failed", "x ".repeat(2000));
        let got = failed(&long, false);
        assert_eq!(got.message.chars().count(), MESSAGE_CHARS);
        assert!(!got.message.contains("tok"));
        assert!(got.message.ends_with("https://h/r failed"));
    }
}
