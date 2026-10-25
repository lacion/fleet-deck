//! Records the git commit, its time and its UTC date in the build (backlog B5,
//! DL-192), and whether this is a release build. A build without git still
//! works: the commit is `unknown`, the time is 0 and the date is `00000000`.

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

/// The UTC date `YYYYMMDD` of unix seconds (integer math, proleptic Gregorian
/// calendar); `00000000` for a time that is not positive.
fn utc_date(secs: i64) -> String {
    if secs <= 0 {
        return "00000000".to_owned();
    }
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = secs / 86_400 + 719_468;
    let era = z / 146_097;
    let day_of_era = z % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_part + 2) / 5 + 1;
    let month = if month_part < 10 {
        month_part + 3
    } else {
        month_part - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}{month:02}{day:02}")
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
    // A bad number gives the unknown time, as `parse_time` does in the crate.
    let date = utc_date(time.trim().parse::<i64>().unwrap_or(0));
    println!("cargo:rustc-env=FLEETDECK_BUILD_DATE={date}");
    // Only the value `1` makes a release build (DL-192).
    let release = u8::from(preset("FLEETDECK_RELEASE").as_deref() == Some("1"));
    println!("cargo:rustc-env=FLEETDECK_RELEASE={release}");
    watch(&dir);
}
