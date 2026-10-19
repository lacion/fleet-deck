use std::io;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::Path;

use fleetdeck_proto::desktop_tools::PostCheck;

use super::files::{
    RunState, RunStateKind, Spec, SpecError, check_dir, ensure_dir, output_path, read_output,
    read_spec, read_state, spec_path, state_path, targets, write_output, write_spec, write_state,
};
use super::test_support::{ID, private_dir, spec};

type Change = fn(&mut Spec);

fn mode(path: &Path, bits: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(bits)).unwrap();
}

fn put(dir: &Path, id: &str, bytes: &[u8], bits: u32) {
    let path = spec_path(dir, id);
    std::fs::write(&path, bytes).unwrap();
    mode(&path, bits);
}

fn json(spec: &Spec) -> Vec<u8> {
    serde_json::to_vec(spec).unwrap()
}

#[test]
fn maintenance_spec_round_trips() {
    let dir = private_dir();
    let want = spec(ID);
    write_spec(dir.path(), &want).unwrap();
    assert_eq!(read_spec(dir.path(), ID).unwrap(), want);
    let found = std::fs::metadata(spec_path(dir.path(), ID))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(found & 0o777, 0o600);
}

#[test]
fn maintenance_spec_written_twice_is_refused_and_leaves_no_temp_file() {
    let dir = private_dir();
    let first = spec(ID);
    write_spec(dir.path(), &first).unwrap();
    let mut second = spec(ID);
    second.cols = 120;
    let err = write_spec(dir.path(), &second).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(read_spec(dir.path(), ID).unwrap(), first);
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, [format!("{ID}.spec.json")]);
}

#[test]
fn maintenance_spec_that_is_a_link_is_refused() {
    let dir = private_dir();
    let real = dir.path().join("real.json");
    std::fs::write(&real, json(&spec(ID))).unwrap();
    mode(&real, 0o600);
    symlink(&real, spec_path(dir.path(), ID)).unwrap();
    assert_eq!(read_spec(dir.path(), ID), Err(SpecError::Unsafe));
}

#[test]
fn maintenance_spec_with_the_mode_0644_is_refused() {
    let dir = private_dir();
    put(dir.path(), ID, &json(&spec(ID)), 0o644);
    assert_eq!(read_spec(dir.path(), ID), Err(SpecError::Unsafe));
}

#[test]
fn maintenance_spec_over_16_kib_is_refused() {
    let dir = private_dir();
    let mut big = spec(ID);
    big.deck = "x".repeat(17 * 1024);
    put(dir.path(), ID, &json(&big), 0o600);
    assert_eq!(read_spec(dir.path(), ID), Err(SpecError::TooLarge));
    std::fs::remove_file(spec_path(dir.path(), ID)).unwrap();
    let err = write_spec(dir.path(), &big).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn maintenance_spec_with_another_id_is_refused() {
    let dir = private_dir();
    let other = "fedcba9876543210fedcba9876543210";
    put(dir.path(), ID, &json(&spec(other)), 0o600);
    assert_eq!(
        read_spec(dir.path(), ID),
        Err(SpecError::Invalid("maintenance_id"))
    );
}

#[test]
fn maintenance_read_with_a_bad_id_is_refused() {
    let dir = private_dir();
    for id in [
        "",
        "../etc/passwd",
        "ABCDEF0123456789ABCDEF0123456789",
        "abc",
    ] {
        assert_eq!(read_spec(dir.path(), id), Err(SpecError::Id));
        assert_eq!(read_state(dir.path(), id), Err(SpecError::Id));
        assert!(read_output(dir.path(), id).is_err());
    }
}

#[test]
fn maintenance_spec_with_an_unknown_field_is_refused() {
    let dir = private_dir();
    let mut value: serde_json::Value = serde_json::to_value(spec(ID)).unwrap();
    value["extra"] = serde_json::json!(1);
    put(dir.path(), ID, value.to_string().as_bytes(), 0o600);
    assert_eq!(read_spec(dir.path(), ID), Err(SpecError::Parse));
}

#[test]
fn maintenance_spec_with_a_tool_without_a_package_is_refused() {
    let dir = private_dir();
    for tools in [vec!["wlrctl".to_owned()], vec!["nope".to_owned()], vec![]] {
        let mut bad = spec(ID);
        bad.tools = tools;
        put(dir.path(), ID, &json(&bad), 0o600);
        assert_eq!(read_spec(dir.path(), ID), Err(SpecError::Invalid("tools")));
    }
}

#[test]
fn maintenance_spec_with_a_bad_field_is_refused() {
    let dir = private_dir();
    let cases: [(&str, Change); 7] = [
        ("v", |s| s.v = 2),
        ("request_id", |s| s.request_id = "x".into()),
        ("plan_id", |s| s.plan_id = "A".repeat(32)),
        ("station_token", |s| s.station_token = String::new()),
        ("runner_token", |s| s.runner_token = "g".repeat(32)),
        ("size", |s| s.cols = 0),
        ("conf_sha256", |s| s.conf_sha256 = "A".repeat(64)),
    ];
    for (field, change) in cases {
        let mut bad = spec(ID);
        change(&mut bad);
        put(dir.path(), ID, &json(&bad), 0o600);
        assert_eq!(
            read_spec(dir.path(), ID),
            Err(SpecError::Invalid(field)),
            "{field}"
        );
    }
}

#[test]
fn maintenance_spec_error_and_debug_text_hold_no_secret() {
    let dir = private_dir();
    let mut bad = spec(ID);
    bad.runner_token = "secret-token".into();
    put(dir.path(), ID, &json(&bad), 0o600);
    let text = read_spec(dir.path(), ID).unwrap_err().to_string();
    assert!(!text.contains("secret-token"));
    let good = spec(ID);
    let debug = format!("{good:?}");
    assert!(!debug.contains(&good.runner_token));
    assert!(!debug.contains(&good.station_token));
}

#[test]
fn maintenance_spec_missing_is_reported() {
    let dir = private_dir();
    assert_eq!(read_spec(dir.path(), ID), Err(SpecError::Missing));
}

#[test]
fn maintenance_targets_follow_the_order_of_the_tools() {
    let mut s = spec(ID);
    s.tools = vec!["jq".into(), "wtype".into(), "busctl".into()];
    assert_eq!(targets(&s), ["extra/jq", "extra/wtype", "core/systemd"]);
}

#[test]
fn maintenance_folder_checks() {
    let dir = private_dir();
    assert!(check_dir(dir.path()).is_ok());
    mode(dir.path(), 0o750);
    let kind = check_dir(dir.path()).unwrap_err().kind();
    assert_eq!(kind, io::ErrorKind::PermissionDenied);
    mode(dir.path(), 0o700);
    let link = dir.path().join("link");
    symlink(dir.path(), &link).unwrap();
    let kind = check_dir(&link).unwrap_err().kind();
    assert_eq!(kind, io::ErrorKind::PermissionDenied);
    let file = dir.path().join("file");
    std::fs::write(&file, b"x").unwrap();
    let kind = check_dir(&file).unwrap_err().kind();
    assert_eq!(kind, io::ErrorKind::PermissionDenied);
}

#[test]
fn maintenance_ensure_dir_creates_a_private_folder() {
    let dir = private_dir();
    let sub = dir.path().join("maintenance");
    ensure_dir(&sub).unwrap();
    let found = std::fs::metadata(&sub).unwrap().permissions().mode();
    assert_eq!(found & 0o777, 0o700);
    ensure_dir(&sub).unwrap();
    // A parent that is missing is not created.
    assert!(ensure_dir(&dir.path().join("a").join("b")).is_err());
    // An existing folder that is open to others is refused, not repaired.
    mode(&sub, 0o755);
    assert!(ensure_dir(&sub).is_err());
}

#[test]
fn maintenance_write_in_an_open_folder_is_refused() {
    let dir = private_dir();
    mode(dir.path(), 0o755);
    assert!(write_spec(dir.path(), &spec(ID)).is_err());
    assert_eq!(read_spec(dir.path(), ID), Err(SpecError::Folder));
}

#[test]
fn maintenance_state_round_trips_and_is_replaced() {
    let dir = private_dir();
    assert_eq!(read_state(dir.path(), ID), Ok(None));
    let running = RunState {
        v: 1,
        state: RunStateKind::Running,
        exit_code: None,
        post_check: None,
        at: 5,
    };
    write_state(dir.path(), ID, &running).unwrap();
    assert_eq!(read_state(dir.path(), ID), Ok(Some(running)));
    let exited = RunState {
        v: 1,
        state: RunStateKind::Exited,
        exit_code: Some(0),
        post_check: Some(PostCheck::TransactionChanged),
        at: 9,
    };
    write_state(dir.path(), ID, &exited).unwrap();
    assert_eq!(read_state(dir.path(), ID), Ok(Some(exited)));
    let text = std::fs::read_to_string(state_path(dir.path(), ID)).unwrap();
    assert!(text.contains(r#""state":"exited""#));
    assert!(text.contains(r#""post_check":"transaction_changed""#));
}

#[test]
fn maintenance_state_that_is_open_or_unknown_is_refused() {
    let dir = private_dir();
    let path = state_path(dir.path(), ID);
    let valid = r#"{"v":1,"state":"running","exit_code":null,"post_check":null,"at":1"#;
    std::fs::write(&path, format!("{valid}}}")).unwrap();
    mode(&path, 0o644);
    assert_eq!(read_state(dir.path(), ID), Err(SpecError::Unsafe));
    std::fs::write(&path, format!(r#"{valid},"x":1}}"#)).unwrap();
    mode(&path, 0o600);
    assert_eq!(read_state(dir.path(), ID), Err(SpecError::Parse));
    std::fs::write(&path, vec![b' '; 5000]).unwrap();
    assert_eq!(read_state(dir.path(), ID), Err(SpecError::TooLarge));
}

#[test]
fn maintenance_output_is_written_once_and_read_back() {
    let dir = private_dir();
    assert_eq!(read_output(dir.path(), ID).unwrap(), None);
    write_output(dir.path(), ID, b"hello \x1b[0m").unwrap();
    let back = read_output(dir.path(), ID).unwrap().unwrap();
    assert_eq!(back, b"hello \x1b[0m");
    let found = std::fs::metadata(output_path(dir.path(), ID))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(found & 0o777, 0o600);
    let err = write_output(dir.path(), ID, b"again").unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
    let back = read_output(dir.path(), ID).unwrap().unwrap();
    assert_eq!(back, b"hello \x1b[0m");
}

#[test]
fn maintenance_output_that_is_a_link_or_too_large_is_refused() {
    let dir = private_dir();
    let real = dir.path().join("real");
    std::fs::write(&real, b"x").unwrap();
    mode(&real, 0o600);
    symlink(&real, output_path(dir.path(), ID)).unwrap();
    let kind = read_output(dir.path(), ID).unwrap_err().kind();
    assert_eq!(kind, io::ErrorKind::PermissionDenied);
    std::fs::remove_file(output_path(dir.path(), ID)).unwrap();
    let err = write_output(dir.path(), ID, &vec![0_u8; 256 * 1024 + 1]).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    write_output(dir.path(), ID, &vec![0_u8; 256 * 1024]).unwrap();
}
