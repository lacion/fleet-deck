//! Records the git commit and its time in the build (backlog B5). A build
//! without git still works: the commit is `unknown` and the time is 0.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The trimmed stdout of `git <args>` in `dir`; `None` on any failure.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (!text.is_empty()).then_some(text)
}

/// A value that a packaged build set in the environment wins.
fn preset(name: &str) -> Option<String> {
    println!("cargo:rerun-if-env-changed={name}");
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// Reruns the script when the commit changes.
fn watch(dir: &Path) {
    let path_of = |name: &str| {
        let path = PathBuf::from(git(dir, &["rev-parse", "--git-path", name])?);
        Some(if path.is_absolute() {
            path
        } else {
            dir.join(path)
        })
    };
    let mut watched = Vec::new();
    if let Some(head) = path_of("HEAD") {
        watched.push(head);
    }
    if let Some(reference) = git(dir, &["symbolic-ref", "-q", "HEAD"]) {
        match path_of(&reference) {
            Some(file) if file.exists() => watched.push(file),
            // A packed ref has no file of its own: watch packed-refs, and the
            // folder where the next commit creates the loose ref.
            other => {
                watched.extend(path_of("packed-refs"));
                watched.extend(other.and_then(|f| f.parent().map(Path::to_path_buf)));
            }
        }
    }
    // A path that does not exist would rerun the script on every build.
    for path in watched.into_iter().filter(|p| p.exists()) {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let dir = std::env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_default();
    let commit = preset("FLEETDECK_BUILD_COMMIT")
        .or_else(|| git(&dir, &["rev-parse", "--short=10", "HEAD"]))
        .unwrap_or_else(|| "unknown".to_owned());
    let time = preset("FLEETDECK_BUILD_TIME")
        .or_else(|| git(&dir, &["show", "-s", "--format=%ct", "HEAD"]))
        .unwrap_or_else(|| "0".to_owned());
    println!("cargo:rustc-env=FLEETDECK_BUILD_COMMIT={commit}");
    println!("cargo:rustc-env=FLEETDECK_BUILD_TIME={time}");
    watch(&dir);
}
