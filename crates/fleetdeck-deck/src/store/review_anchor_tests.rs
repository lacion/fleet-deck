use fleetdeck_proto::review::ReviewSide::{New, Old};

use super::*;

const MODIFIED: &str = "\
diff --git a/src/a.rs b/src/a.rs
index 1111111..2222222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,4 +1,5 @@ fn main() {
 one
-two
+TWO
+two and a half
 three
 four
@@ -20,2 +21,2 @@
 twenty
-twenty-one
+TWENTY-ONE
\\ No newline at end of file
";

fn parse(patch: &str) -> ParsedDiff {
    ParsedDiff::parse(patch)
}

#[test]
fn the_new_side_counts_context_and_added_lines_and_the_old_side_context_and_removed_lines() {
    let d = parse(MODIFIED);
    // New side: one(1) TWO(2) two and a half(3) three(4) four(5).
    assert_eq!(
        d.anchor(New, "src/a.rs", 2, 3).unwrap().text,
        "TWO\ntwo and a half"
    );
    assert_eq!(d.anchor(New, "src/a.rs", 4, 4).unwrap().text, "three");
    // Old side: one(1) two(2) three(3) four(4); the added lines have no number.
    assert_eq!(d.anchor(Old, "src/a.rs", 2, 2).unwrap().text, "two");
    assert_eq!(
        d.anchor(Old, "src/a.rs", 1, 4).unwrap().text,
        "one\ntwo\nthree\nfour"
    );
    // The second hunk starts at 21 on the new side and 20 on the old side.
    assert_eq!(
        d.anchor(New, "src/a.rs", 21, 22).unwrap().text,
        "twenty\nTWENTY-ONE"
    );
    assert_eq!(
        d.anchor(Old, "src/a.rs", 21, 21).unwrap().text,
        "twenty-one"
    );
    // The marker `\ No newline at end of file` is not a line.
    assert_eq!(
        d.anchor(New, "src/a.rs", 22, 23),
        Err(AnchorError::NotInDiff)
    );
    // The anchor has no trailing newline and names the same path twice only
    // when a rename gives another one.
    let a = d.anchor(New, "src/a.rs", 4, 5).unwrap();
    assert_eq!(a.text, "three\nfour");
    assert_eq!((a.path.as_str(), a.other_path), ("src/a.rs", None));
}

#[test]
fn a_range_over_two_hunks_or_over_a_line_that_no_hunk_shows_is_not_in_the_diff() {
    let d = parse(MODIFIED);
    for (side, a, b) in [
        (New, 5, 21),
        (New, 6, 6),
        (New, 1, 6),
        (New, 100, 100),
        (Old, 4, 5),
        (New, 0, 1),
        (New, 3, 2),
    ] {
        assert_eq!(
            d.anchor(side, "src/a.rs", a, b),
            Err(AnchorError::NotInDiff),
            "{side:?} {a}-{b}"
        );
    }
    assert_eq!(
        d.anchor(New, "src/other.rs", 1, 1),
        Err(AnchorError::NotInDiff)
    );
}

#[test]
fn a_removed_line_that_looks_like_a_header_stays_a_line_of_its_hunk() {
    let patch = "\
diff --git a/x b/x
--- a/x
+++ b/x
@@ -1,3 +1 @@
 keep
--- not a header
-+++ not a header
";
    let d = parse(patch);
    assert_eq!(
        d.anchor(Old, "x", 1, 3).unwrap().text,
        "keep\n-- not a header\n+++ not a header"
    );
    assert_eq!(d.anchor(New, "x", 1, 1).unwrap().text, "keep");
}

#[test]
fn a_rename_names_the_other_path_and_each_side_has_its_own_path() {
    let patch = "\
diff --git a/old/name.rs b/new/name.rs
similarity index 90%
rename from old/name.rs
rename to new/name.rs
--- a/old/name.rs
+++ b/new/name.rs
@@ -1,2 +1,2 @@
 same
-before
+after
";
    let d = parse(patch);
    let new = d.anchor(New, "new/name.rs", 2, 2).unwrap();
    assert_eq!(
        (
            new.text.as_str(),
            new.path.as_str(),
            new.other_path.as_deref()
        ),
        ("after", "new/name.rs", Some("old/name.rs"))
    );
    let old = d.anchor(Old, "old/name.rs", 2, 2).unwrap();
    assert_eq!(
        (
            old.text.as_str(),
            old.path.as_str(),
            old.other_path.as_deref()
        ),
        ("before", "old/name.rs", Some("new/name.rs"))
    );
    // The old path does not exist on the new side, nor the new one on the old.
    assert_eq!(
        d.anchor(New, "old/name.rs", 1, 1),
        Err(AnchorError::NotInDiff)
    );
    assert_eq!(
        d.anchor(Old, "new/name.rs", 1, 1),
        Err(AnchorError::NotInDiff)
    );
}

#[test]
fn a_copy_uses_the_copy_lines_and_a_pure_rename_or_a_binary_file_has_no_lines() {
    let patch = "\
diff --git a/src.rs b/copy.rs
similarity index 100%
copy from src.rs
copy to copy.rs
diff --git a/from.rs b/to.rs
similarity index 100%
rename from from.rs
rename to to.rs
diff --git a/pic.png b/pic.png
index 1..2 100644
Binary files a/pic.png and b/pic.png differ
";
    let d = parse(patch);
    for (side, path) in [
        (New, "copy.rs"),
        (Old, "src.rs"),
        (New, "to.rs"),
        (Old, "from.rs"),
        (New, "pic.png"),
        (Old, "pic.png"),
    ] {
        assert_eq!(
            d.anchor(side, path, 1, 1),
            Err(AnchorError::NotInDiff),
            "{path}"
        );
    }
}

#[test]
fn a_new_file_has_no_old_side_and_a_deleted_file_has_no_new_side() {
    let patch = "\
diff --git a/added.rs b/added.rs
new file mode 100644
--- /dev/null
+++ b/added.rs
@@ -0,0 +1,2 @@
+first
+second
diff --git a/gone.rs b/gone.rs
deleted file mode 100644
--- a/gone.rs
+++ /dev/null
@@ -1,2 +0,0 @@
-a
-b
";
    let d = parse(patch);
    assert_eq!(
        d.anchor(New, "added.rs", 1, 2).unwrap().text,
        "first\nsecond"
    );
    assert_eq!(d.anchor(Old, "added.rs", 1, 1), Err(AnchorError::NotInDiff));
    assert_eq!(d.anchor(Old, "gone.rs", 1, 2).unwrap().text, "a\nb");
    assert_eq!(d.anchor(New, "gone.rs", 1, 1), Err(AnchorError::NotInDiff));
}

#[test]
fn paths_with_spaces_and_quoted_paths_are_read_from_the_header() {
    let patch = "\
diff --git a/my dir/my file.txt b/my dir/my file.txt
--- a/my dir/my file.txt
+++ b/my dir/my file.txt
@@ -1 +1 @@
-x
+y
diff --git \"a/caf\\303\\251.txt\" \"b/caf\\303\\251.txt\"
--- \"a/caf\\303\\251.txt\"
+++ \"b/caf\\303\\251.txt\"
@@ -1 +1 @@
-p
+q
diff --git a/a b/c.txt b/a b/c.txt
--- a/a b/c.txt
+++ b/a b/c.txt
@@ -1 +1 @@
-m
+n
";
    let d = parse(patch);
    assert_eq!(d.anchor(New, "my dir/my file.txt", 1, 1).unwrap().text, "y");
    assert_eq!(d.anchor(New, "café.txt", 1, 1).unwrap().text, "q");
    assert_eq!(d.anchor(New, "a b/c.txt", 1, 1).unwrap().text, "n");
}

#[test]
fn a_cut_patch_loses_its_partial_last_line_and_nothing_past_the_cut_is_in_the_diff() {
    // Cut in the middle of the line `four`: its text is not complete.
    let cut = format!(
        "{}\n[diff truncated]\n",
        &MODIFIED[..MODIFIED.find(" four").unwrap() + 3]
    );
    let d = parse(&cut);
    assert_eq!(d.anchor(New, "src/a.rs", 4, 4).unwrap().text, "three");
    assert_eq!(d.anchor(New, "src/a.rs", 5, 5), Err(AnchorError::NotInDiff));
    assert_eq!(
        d.anchor(New, "src/a.rs", 21, 21),
        Err(AnchorError::NotInDiff)
    );
    // Cut at a line boundary: the last line is complete.
    let at = MODIFIED.find(" four").unwrap();
    let cut = format!("{}{}", &MODIFIED[..at + 6], CUT_MARKER);
    let d = parse(&cut);
    assert_eq!(d.anchor(New, "src/a.rs", 5, 5).unwrap().text, "four");
    assert_eq!(
        d.anchor(New, "src/a.rs", 21, 21),
        Err(AnchorError::NotInDiff)
    );
}

#[test]
fn an_anchor_over_4_kib_is_too_large() {
    let line = "x".repeat(1000);
    let mut patch = String::from("diff --git a/big b/big\n--- a/big\n+++ b/big\n@@ -0,0 +1,6 @@\n");
    for _ in 0..6 {
        patch.push_str(&format!("+{line}\n"));
    }
    let d = parse(&patch);
    assert!(d.anchor(New, "big", 1, 4).is_ok());
    assert_eq!(d.anchor(New, "big", 1, 5), Err(AnchorError::TooLarge));
}

#[test]
fn an_untracked_file_patch_of_the_station_has_lines_and_the_no_newline_marker() {
    let patch = "diff --git a/n.txt b/n.txt\nnew file mode 100644\n--- /dev/null\n+++ b/n.txt\n@@ -0,0 +1,2 @@\n+a\n+b\n\\ No newline at end of file\n";
    let d = parse(patch);
    assert_eq!(d.anchor(New, "n.txt", 1, 2).unwrap().text, "a\nb");
    assert_eq!(d.anchor(New, "n.txt", 3, 3), Err(AnchorError::NotInDiff));
}
