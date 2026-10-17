use std::path::{Path, PathBuf};

use fleetdeck_proto::fleet_git::SharePathState;

use super::*;

/// The table of the CLI: widths from the longest field of each column, the
/// header, the dash line and one row per share, each with 4 spaces between
/// the columns.
fn table(rows: &[(&str, &str, &str)]) -> Vec<u8> {
    let w = |f: fn(&(&str, &str, &str)) -> usize, min: usize| {
        rows.iter().map(f).max().unwrap_or(0).max(min)
    };
    let a = w(|r| r.0.chars().count(), 4);
    let b = w(|r| r.1.chars().count(), 4);
    let c = w(|r| r.2.chars().count(), 2);
    let mut out = format!(
        "{:<a$}    {:<b$}    as\n{}    {}    {}\n",
        "name",
        "path",
        "-".repeat(a),
        "-".repeat(b),
        "-".repeat(c)
    );
    for (name, path, user) in rows {
        out.push_str(&format!("{name:<a$}    {path:<b$}    {user}\n"));
    }
    out.into_bytes()
}

fn refused(bytes: &[u8]) {
    assert!(
        matches!(parse_drive_list(bytes), Err(TailscaleError::Parse(_))),
        "expected a refusal for {:?}",
        String::from_utf8_lossy(bytes)
    );
}

#[test]
fn a_table_with_no_share_gives_an_empty_list() {
    assert_eq!(parse_drive_list(&table(&[])).unwrap(), []);
}

#[test]
fn one_and_three_shares_are_read() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("out");
    std::fs::create_dir(&out).unwrap();
    let out = out.to_string_lossy().into_owned();
    let one = parse_drive_list(&table(&[("out", &out, "alice")])).unwrap();
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].name, "out");
    assert_eq!(one[0].path, out);
    assert_eq!(one[0].candidates, std::slice::from_ref(&out));
    assert_eq!(one[0].as_user.as_deref(), Some("alice"));
    assert_eq!(one[0].path_state, SharePathState::Ok);
    assert!(one[0].exposed.is_empty());
    // The same path length in every row: no row is padded.
    let gone = tmp.path().join("nop").to_string_lossy().into_owned();
    assert_eq!(gone.len(), out.len());
    let three = parse_drive_list(&table(&[
        ("a", &out, "x"),
        ("bb", &gone, "yy"),
        ("ccc", &out, ""),
    ]))
    .unwrap();
    let names: Vec<&str> = three.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["a", "bb", "ccc"]);
    assert_eq!(three[1].path_state, SharePathState::Missing);
    assert_eq!(three[2].as_user, None, "a blank `as` is None");
}

#[test]
fn a_name_with_spaces_and_parentheses_and_a_non_ascii_path_are_read() {
    let tmp = tempfile::tempdir().unwrap();
    let dir: PathBuf = tmp.path().join("exportación");
    std::fs::create_dir(&dir).unwrap();
    let dir = dir.to_string_lossy().into_owned();
    let shares = parse_drive_list(&table(&[("my share (1)", &dir, "bob")])).unwrap();
    assert_eq!(shares[0].name, "my share (1)");
    assert_eq!(shares[0].path, dir);
    assert_eq!(shares[0].path_state, SharePathState::Ok);
}

#[test]
fn each_malformed_table_is_refused() {
    let good = table(&[("out", "/tmp", "alice")]);
    let text = String::from_utf8(good.clone()).unwrap();
    // A wrong header, a wrong dash line, a short row.
    refused(text.replacen("name", "NAME", 1).as_bytes());
    refused(text.replacen("----", "--", 1).as_bytes());
    refused(b"name    path    as\n----    ----    --\nout\n");
    // A tab, a carriage return, no final new line, not UTF-8.
    refused(text.replace("alice", "al\tice").as_bytes());
    refused(text.replace('\n', "\r\n").as_bytes());
    refused(text.trim_end().as_bytes());
    refused(&[0xff, 0xfe, b'\n']);
    // A duplicate name, an upper-case name, a relative path.
    refused(&table(&[("out", "/tmp", "a"), ("out", "/tmp", "b")]));
    refused(&table(&[("Out", "/tmp", "a")]));
    refused(&table(&[("out", "tmp/x", "a")]));
    // An `as` user with a space.
    refused(&table(&[("out", "/tmp", "a b")]));
}

#[test]
fn sixty_five_shares_are_refused_and_sixty_four_are_read() {
    let names: Vec<String> = (0..65).map(|i| format!("s{i}")).collect();
    let rows: Vec<(&str, &str, &str)> = names.iter().map(|n| (n.as_str(), "/tmp", "u")).collect();
    assert_eq!(parse_drive_list(&table(&rows[..64])).unwrap().len(), 64);
    refused(&table(&rows));
}

#[test]
fn a_padded_path_is_ambiguous_with_every_candidate_whatever_exists() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().join("exports");
    // `/exports/out ` is configured and missing; `/exports/out` exists.
    std::fs::create_dir(&base).unwrap();
    std::fs::create_dir(base.join("out")).unwrap();
    let out = base.join("out").to_string_lossy().into_owned();
    // The padding comes from a longer path in another row.
    let long = format!("{}/much/longer/path", base.to_string_lossy());
    let shares = parse_drive_list(&table(&[
        ("one", &format!("{out} "), "u"),
        ("two", &long, "u"),
    ]))
    .unwrap();
    let one = &shares[0];
    assert_eq!(one.path_state, SharePathState::Ambiguous);
    assert_eq!(one.path, out, "the shortest candidate");
    assert!(one.candidates.len() >= 2);
    assert_eq!(one.candidates[0], out);
    assert_eq!(one.candidates[1], format!("{out} "));
    // With no candidate existing it is `ambiguous` too.
    let none = parse_drive_list(&table(&[
        ("a", "/no/such/dir ", "u"),
        ("b", "/no/such/dir/longer", "u"),
    ]))
    .unwrap();
    assert_eq!(none[0].path_state, SharePathState::Ambiguous);
    // `k = 0` and the path missing is `missing`.
    let missing = parse_drive_list(&table(&[("a", "/no/such/dir", "u")])).unwrap();
    assert_eq!(missing[0].path_state, SharePathState::Missing);
}

#[test]
fn the_source_tree_never_runs_a_drive_command_that_changes_shares() {
    // The grep test of design §15.1: no `"drive", "share" | "unshare" |
    // "rename"` argument list anywhere in the code.
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut hits = Vec::new();
    let mut stack = vec![crates];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            let name = entry.file_name();
            if path.is_dir() {
                if name != "target" && name != "node_modules" {
                    stack.push(path);
                }
            } else if path.extension().is_some_and(|e| e == "rs")
                && name != "tailscale_drive_tests.rs"
            {
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                if changes_shares(&text) {
                    hits.push(path);
                }
            }
        }
    }
    assert!(hits.is_empty(), "a drive command changes shares: {hits:?}");
    // The matcher itself finds the forbidden shapes.
    assert!(changes_shares(r#"run(&["drive", "share", "a"])"#));
    assert!(changes_shares("&[\"drive\" ,\n \"rename\"]"));
    assert!(!changes_shares(r#"run(&["drive", "list"])"#));
}

/// True when `text` has `"drive"`, a comma, and then `"share"`,
/// `"unshare"` or `"rename"` (the regex `"drive"\s*,\s*"(share|unshare|rename)"`).
fn changes_shares(text: &str) -> bool {
    let mut rest = text;
    while let Some(at) = rest.find("\"drive\"") {
        rest = &rest[at + "\"drive\"".len()..];
        let after = rest.trim_start();
        if let Some(after) = after.strip_prefix(',') {
            let after = after.trim_start();
            if ["share", "unshare", "rename"]
                .iter()
                .any(|w| after.starts_with(&format!("\"{w}\"")))
            {
                return true;
            }
        }
    }
    false
}
