//! The render step of the coordinator diff (ADR-008 §4.4 step 5): one
//! `git diff --no-index` over two copies in a private temporary directory,
//! then the rewrite of its output so that no old-side byte is in it.

use std::path::Path;

use super::super::Git;
use super::super::files::FileError;
use super::internal;

/// The rewritten output of one `git diff --no-index`.
#[derive(Default)]
pub(super) struct Rendered {
    pub(super) text: String,
    pub(super) added: u64,
    pub(super) removed: u64,
}

/// Step 5: renders `old` and `new` with `git diff --no-index` in the private
/// temporary directory `temp`, then rewrites the output: the headers before
/// the first hunk are dropped, every `@@` header is rebuilt from its numbers,
/// every run of removed lines is one counted line, and the marker of a
/// missing final newline after a removed line is dropped.
pub(super) async fn render(
    git: &Git,
    temp: &Path,
    old: &[u8],
    new: &[u8],
) -> Result<Rendered, FileError> {
    std::fs::write(temp.join("a"), old).map_err(|e| internal(&e))?;
    std::fs::write(temp.join("b"), new).map_err(|e| internal(&e))?;
    let (status, out, stderr) = git
        .status(
            &[
                "diff",
                "--no-index",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                "--text",
                "-U3",
                "--",
                "a",
                "b",
            ],
            &[],
            None,
        )
        .await
        .map_err(|e| internal(&e))?;
    // Exit code 1: the files differ. 0: they do not.
    if status != 0 && status != 1 {
        return Err(internal(&format!(
            "git diff --no-index failed ({status}): {stderr}"
        )));
    }
    Ok(rewrite(&String::from_utf8_lossy(&out)))
}

/// The numbers of a hunk header `@@ -a[,b] +c[,d] @@ ...`.
fn hunk_numbers(line: &str) -> Option<[u64; 4]> {
    let rest = line.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let (new, _) = rest.split_once(" @@")?;
    let pair = |text: &str| -> Option<(u64, u64)> {
        match text.split_once(',') {
            Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
            None => Some((text.parse().ok()?, 1)),
        }
    };
    let (a, b) = pair(old)?;
    let (c, d) = pair(new)?;
    Some([a, b, c, d])
}

pub(super) fn rewrite(output: &str) -> Rendered {
    let mut out = Rendered::default();
    let mut in_hunk = false;
    let mut run = 0u64;
    let mut last_removed = false;
    let flush = |out: &mut Rendered, run: &mut u64| {
        if *run > 0 {
            out.text.push_str(&format!("-[{run} lines removed]\n"));
            out.removed += *run;
            *run = 0;
        }
    };
    // Not `str::lines`: a carriage return belongs to the line.
    for line in output.strip_suffix('\n').unwrap_or(output).split('\n') {
        if line.starts_with("@@ ") {
            flush(&mut out, &mut run);
            last_removed = false;
            let Some([a, b, c, d]) = hunk_numbers(line) else {
                // A header that does not parse has no safe form.
                in_hunk = false;
                continue;
            };
            in_hunk = true;
            out.text.push_str(&format!("@@ -{a},{b} +{c},{d} @@\n"));
            continue;
        }
        if !in_hunk {
            continue;
        }
        match line.as_bytes().first() {
            Some(b'-') => {
                run += 1;
                last_removed = true;
            }
            Some(b'\\') if last_removed => {}
            Some(b'+') => {
                flush(&mut out, &mut run);
                last_removed = false;
                out.added += 1;
                out.text.push_str(line);
                out.text.push('\n');
            }
            Some(_) | None => {
                flush(&mut out, &mut run);
                last_removed = false;
                out.text.push_str(line);
                out.text.push('\n');
            }
        }
    }
    flush(&mut out, &mut run);
    out
}
