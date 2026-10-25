//! M3 goal rule "modules stay under 800 lines": `tools/module-size.sh`
//! (run by `make check`) passes on this tree and fails, naming the files,
//! when the limit is lower than the largest module.

#![allow(clippy::expect_used)]

use std::path::PathBuf;
use std::process::{Command, Output};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn run(limit: Option<&str>) -> Output {
    let mut cmd = Command::new("sh");
    cmd.arg("tools/module-size.sh").current_dir(root());
    match limit {
        Some(limit) => cmd.env("FD_MODULE_LIMIT", limit),
        None => cmd.env_remove("FD_MODULE_LIMIT"),
    };
    cmd.output().expect("run tools/module-size.sh")
}

/// The line count of every module that the script checks, from the tree.
fn largest_module() -> (usize, String) {
    fn walk(dir: &std::path::Path, out: &mut Vec<(usize, String)>) {
        for entry in std::fs::read_dir(dir).expect("read dir").flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                if name != "target" && name != "node_modules" {
                    walk(&path, out);
                }
            } else if [".rs", ".ts", ".tsx"].iter().any(|ext| name.ends_with(ext)) {
                let text = std::fs::read_to_string(&path).expect("read module");
                out.push((text.lines().count(), path.display().to_string()));
            }
        }
    }
    let mut all = Vec::new();
    walk(&root().join("crates"), &mut all);
    walk(&root().join("web/src"), &mut all);
    all.into_iter().max().expect("modules exist")
}

#[test]
fn every_module_has_fewer_than_800_lines() {
    let out = run(None);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let (lines, path) = largest_module();
    assert!(lines < 800, "{path} has {lines} lines");
}

#[test]
fn the_module_size_check_fails_and_names_the_largest_module_below_its_size() {
    let (lines, path) = largest_module();
    let out = run(Some(&lines.to_string()));
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let file = path.rsplit('/').next().expect("file name");
    assert!(
        stdout.contains(&format!("{lines} ")) && stdout.contains(file),
        "{stdout}"
    );
}
