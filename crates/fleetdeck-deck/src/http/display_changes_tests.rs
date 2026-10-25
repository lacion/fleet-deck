//! The answer rules of the display change route (R13 §5D item 14, R11).

use axum::http::StatusCode;
use fleetdeck_proto::display_changes::{
    DisplayChanges, DisplayFileChange, MAX_DISPLAY_CHANGES, MAX_DISPLAY_PATCH_FILE,
};
use fleetdeck_proto::event::DiffSummary;
use fleetdeck_proto::files::DiffScope;

use super::{finish, size, station_failure};

fn file(path: &str, patch: Option<String>) -> DisplayFileChange {
    DisplayFileChange {
        path: Some(path.to_owned()),
        hidden: false,
        added: Some(1),
        removed: Some(0),
        binary: false,
        patch,
        too_large: false,
    }
}

fn answer(files: Vec<DisplayFileChange>) -> DisplayChanges {
    DisplayChanges {
        session_id: "s1".into(),
        scope: DiffScope::Worktree,
        truncated: false,
        summary: DiffSummary {
            files_changed: 0,
            insertions: 0,
            deletions: 0,
        },
        files,
    }
}

#[test]
fn a_hidden_file_keeps_only_its_marker() {
    let mut hidden = file("secret/.env", Some("x".to_owned()));
    hidden.hidden = true;
    hidden.too_large = true;
    let out = finish(answer(vec![hidden]), true);
    let f = &out.files[0];
    assert!(f.hidden && f.path.is_none() && f.patch.is_none());
    assert!(f.added.is_none() && f.removed.is_none() && !f.too_large && !f.binary);
}

#[test]
fn a_patch_that_was_not_asked_for_is_removed() {
    let out = finish(answer(vec![file("a", Some("patch".to_owned()))]), false);
    assert!(out.files[0].patch.is_none() && !out.files[0].too_large);
}

#[test]
fn a_section_above_64_kib_is_left_out_and_marked() {
    let big = "x".repeat(usize::try_from(MAX_DISPLAY_PATCH_FILE).unwrap() + 1);
    let edge = "y".repeat(usize::try_from(MAX_DISPLAY_PATCH_FILE).unwrap());
    let out = finish(
        answer(vec![file("big", Some(big)), file("edge", Some(edge))]),
        true,
    );
    assert!(out.files[0].patch.is_none() && out.files[0].too_large);
    assert!(out.files[1].patch.is_some() && !out.files[1].too_large);
}

#[test]
fn an_answer_above_one_mib_loses_patches_from_the_end_and_then_files() {
    let limit = usize::try_from(MAX_DISPLAY_CHANGES).unwrap();
    // 30 files with a 60 KiB patch each: about 1.8 MiB.
    let part = "z".repeat(60 * 1024);
    let files: Vec<_> = (0..30)
        .map(|i| file(&format!("f{i}"), Some(part.clone())))
        .collect();
    let out = finish(answer(files), true);
    assert!(size(&out) <= limit, "{}", size(&out));
    // The first files keep their patch, the last ones are marked.
    assert!(out.files[0].patch.is_some());
    let last = out.files.last().unwrap();
    assert!(last.patch.is_none() && last.too_large);
    assert!(!out.truncated);
    // Statistics alone above the limit: the last files go and `truncated`.
    let long = "p".repeat(4000);
    let files: Vec<_> = (0..400)
        .map(|i| file(&format!("{long}{i}"), None))
        .collect();
    let before = files.len();
    let out = finish(answer(files), false);
    assert!(size(&out) <= limit, "{}", size(&out));
    assert!(out.truncated && out.files.len() < before);
    assert_eq!(
        out.files[0].path.as_deref(),
        Some(format!("{long}0").as_str())
    );
}

#[test]
fn a_small_answer_is_not_changed() {
    let files = vec![file("a", Some("p".to_owned())), file("b", None)];
    let out = finish(answer(files.clone()), true);
    assert_eq!(out.files, files);
    assert!(!out.truncated);
}

#[test]
fn station_codes_map_to_fixed_answers() {
    let f = station_failure("no_base");
    assert_eq!((f.status, f.code), (StatusCode::CONFLICT, "no_base"));
    let f = station_failure("feature_not_negotiated");
    assert_eq!(
        (f.status, f.code),
        (StatusCode::CONFLICT, "changes_unsupported")
    );
    let f = station_failure("anything else");
    assert_eq!(
        (f.status, f.code),
        (StatusCode::BAD_GATEWAY, "station_error")
    );
    let f = station_failure("timeout");
    assert_eq!((f.status, f.code), (StatusCode::GATEWAY_TIMEOUT, "timeout"));
    let f = station_failure("busy");
    assert_eq!(f.status, StatusCode::TOO_MANY_REQUESTS);
}
