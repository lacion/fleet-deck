//! Location and trust tests of the session history.

use std::os::unix::fs::symlink;
use std::time::{Duration, Instant};

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::history::HistoryUnavailable;

use super::locate::{self, Trust, encode_cwd, resolve_home};
use super::tests_support::*;
use super::{Fail, Limits};
use crate::workspace::PrivatePaths;

fn trust<'a>(private: &'a PrivatePaths, limits: &'a Limits, uid: u32) -> Trust<'a> {
    Trust {
        private,
        uid,
        limits,
        deadline: Instant::now() + Duration::from_secs(30),
    }
}

fn my_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

fn good_lines() -> Vec<String> {
    vec![claude_user(1, "hello"), claude_assistant(2, "hi")]
}

fn expect(result: Result<locate::Opened, Fail>, reason: HistoryUnavailable) {
    match result {
        Err(Fail::Unavailable(got)) => assert_eq!(got, reason),
        other => panic!("expected {reason:?}, got {other:?}"),
    }
}

#[tokio::test]
async fn it_finds_the_claude_file_at_the_exact_path() {
    let rig = Rig::new(ProviderKind::Claude).await;
    let path = rig.write(NATIVE, &good_lines());
    let (private, limits) = (PrivatePaths::default(), Limits::default());
    let opened = locate::locate(
        &rig.home(),
        ProviderKind::Claude,
        &rig.cwd(),
        NATIVE,
        &trust(&private, &limits, my_uid()),
    )
    .unwrap();
    assert_eq!(opened.size, std::fs::metadata(&path).unwrap().len());
    assert_eq!(opened.home, rig.home());
}

#[test]
fn the_directory_name_replaces_every_non_alphanumeric_character() {
    assert_eq!(encode_cwd("/home/a.b/c_d e"), "-home-a-b-c-d-e");
}

#[test]
fn the_home_comes_from_the_launch_environment() {
    let mut env = ChildEnv::empty();
    env.set("HOME", "/h").unwrap();
    assert_eq!(
        resolve_home(&env, ProviderKind::Claude).unwrap().to_str(),
        Some("/h/.claude")
    );
    assert_eq!(
        resolve_home(&env, ProviderKind::Codex).unwrap().to_str(),
        Some("/h/.codex")
    );
    env.set("CLAUDE_CONFIG_DIR", "/custom/claude").unwrap();
    env.set("CODEX_HOME", "/custom/codex").unwrap();
    assert_eq!(
        resolve_home(&env, ProviderKind::Claude).unwrap().to_str(),
        Some("/custom/claude")
    );
    assert_eq!(
        resolve_home(&env, ProviderKind::Codex).unwrap().to_str(),
        Some("/custom/codex")
    );
    assert!(resolve_home(&ChildEnv::empty(), ProviderKind::Claude).is_none());
}

#[tokio::test]
async fn an_env_override_of_the_config_dir_moves_the_lookup() {
    let rig = Rig::new(ProviderKind::Claude).await;
    let custom = rig.root.join("elsewhere");
    let dir = custom.join("projects").join(encode_cwd(&rig.cwd()));
    std::fs::create_dir_all(&dir).unwrap();
    write_private(&dir.join(format!("{NATIVE}.jsonl")), &join(&good_lines()));
    let mut env = ChildEnv::empty();
    env.set("HOME", &rig.root).unwrap();
    env.set("CLAUDE_CONFIG_DIR", &custom).unwrap();
    let home = resolve_home(&env, ProviderKind::Claude).unwrap();
    let (private, limits) = (PrivatePaths::default(), Limits::default());
    let t = trust(&private, &limits, my_uid());
    assert!(locate::locate(&home, ProviderKind::Claude, &rig.cwd(), NATIVE, &t).is_ok());
    // The default home has no file.
    expect(
        locate::locate(&rig.home(), ProviderKind::Claude, &rig.cwd(), NATIVE, &t),
        HistoryUnavailable::NotFound,
    );
}

#[tokio::test]
async fn a_symlinked_file_and_a_symlinked_directory_are_not_trusted() {
    let rig = Rig::new(ProviderKind::Claude).await;
    let real = rig.write("other-native", &good_lines());
    let (private, limits) = (PrivatePaths::default(), Limits::default());
    let t = trust(&private, &limits, my_uid());
    // A link in place of the file.
    let link = real.parent().unwrap().join(format!("{NATIVE}.jsonl"));
    symlink(&real, &link).unwrap();
    expect(
        locate::locate(&rig.home(), ProviderKind::Claude, &rig.cwd(), NATIVE, &t),
        HistoryUnavailable::NotTrusted,
    );
    // A link in place of the project directory.
    let projects = rig.home().join("projects");
    let encoded = projects.join(encode_cwd(&rig.cwd()));
    let moved = projects.join("moved");
    std::fs::rename(&encoded, &moved).unwrap();
    symlink(&moved, &encoded).unwrap();
    expect(
        locate::locate(&rig.home(), ProviderKind::Claude, &rig.cwd(), NATIVE, &t),
        HistoryUnavailable::NotTrusted,
    );
}

#[tokio::test]
async fn a_hard_link_is_not_trusted() {
    let rig = Rig::new(ProviderKind::Claude).await;
    let path = rig.write(NATIVE, &good_lines());
    std::fs::hard_link(&path, rig.root.join("second-name")).unwrap();
    let (private, limits) = (PrivatePaths::default(), Limits::default());
    expect(
        locate::locate(
            &rig.home(),
            ProviderKind::Claude,
            &rig.cwd(),
            NATIVE,
            &trust(&private, &limits, my_uid()),
        ),
        HistoryUnavailable::NotTrusted,
    );
}

#[tokio::test]
async fn another_owner_is_not_trusted() {
    let rig = Rig::new(ProviderKind::Claude).await;
    rig.write(NATIVE, &good_lines());
    let (private, limits) = (PrivatePaths::default(), Limits::default());
    expect(
        locate::locate(
            &rig.home(),
            ProviderKind::Claude,
            &rig.cwd(),
            NATIVE,
            &trust(&private, &limits, my_uid().wrapping_add(1)),
        ),
        HistoryUnavailable::NotTrusted,
    );
}

#[tokio::test]
async fn a_private_path_file_is_not_trusted() {
    let rig = Rig::new(ProviderKind::Claude).await;
    let path = rig.write(NATIVE, &good_lines());
    let private = PrivatePaths::default();
    private.register(&path);
    let limits = Limits::default();
    expect(
        locate::locate(
            &rig.home(),
            ProviderKind::Claude,
            &rig.cwd(),
            NATIVE,
            &trust(&private, &limits, my_uid()),
        ),
        HistoryUnavailable::NotTrusted,
    );
}

#[tokio::test]
async fn the_first_record_that_names_a_session_must_name_this_one() {
    let rig = Rig::new(ProviderKind::Claude).await;
    let other = claude_user(1, "x").replace(NATIVE, "99999999-0000-0000-0000-000000000000");
    rig.write(NATIVE, &[other, claude_user(2, "y")]);
    let (private, limits) = (PrivatePaths::default(), Limits::default());
    let t = trust(&private, &limits, my_uid());
    expect(
        locate::locate(&rig.home(), ProviderKind::Claude, &rig.cwd(), NATIVE, &t),
        HistoryUnavailable::NotTrusted,
    );
    // No record names a session in the first records.
    rig.write(NATIVE, &[r#"{"type":"mode"}"#.to_owned()]);
    expect(
        locate::locate(&rig.home(), ProviderKind::Claude, &rig.cwd(), NATIVE, &t),
        HistoryUnavailable::NotTrusted,
    );
}

#[tokio::test]
async fn a_file_over_the_size_limit_is_too_large() {
    let rig = Rig::new(ProviderKind::Claude).await;
    rig.write(NATIVE, &good_lines());
    let (private, limits) = (
        PrivatePaths::default(),
        Limits {
            file_max: 10,
            ..Limits::default()
        },
    );
    expect(
        locate::locate(
            &rig.home(),
            ProviderKind::Claude,
            &rig.cwd(),
            NATIVE,
            &trust(&private, &limits, my_uid()),
        ),
        HistoryUnavailable::TooLarge,
    );
}

fn codex_lines() -> Vec<String> {
    vec![codex_meta(NATIVE), codex_message(1, "user", "hello")]
}

#[tokio::test]
async fn codex_zero_one_and_two_matches() {
    let rig = Rig::new(ProviderKind::Codex).await;
    let (private, limits) = (PrivatePaths::default(), Limits::default());
    let t = trust(&private, &limits, my_uid());
    let find = || locate::locate(&rig.home(), ProviderKind::Codex, "/w", NATIVE, &t);
    std::fs::create_dir_all(rig.home().join("sessions/2026/10/14")).unwrap();
    expect(find(), HistoryUnavailable::NotFound);
    rig.write(NATIVE, &codex_lines());
    assert!(find().is_ok());
    // The same thread in the archive: two matches.
    let archived = rig.home().join("archived_sessions");
    std::fs::create_dir_all(&archived).unwrap();
    write_private(
        &archived.join(format!("rollout-2026-10-01T00-00-00-{NATIVE}.jsonl")),
        &join(&codex_lines()),
    );
    expect(find(), HistoryUnavailable::Ambiguous);
}

#[tokio::test]
async fn codex_first_record_must_be_this_threads_session_meta() {
    let rig = Rig::new(ProviderKind::Codex).await;
    rig.write(
        NATIVE,
        &[codex_meta("another-thread"), codex_message(1, "user", "x")],
    );
    let (private, limits) = (PrivatePaths::default(), Limits::default());
    expect(
        locate::locate(
            &rig.home(),
            ProviderKind::Codex,
            "/w",
            NATIVE,
            &trust(&private, &limits, my_uid()),
        ),
        HistoryUnavailable::NotTrusted,
    );
}

#[tokio::test]
async fn a_codex_home_with_more_than_5000_entries_is_busy() {
    let rig = Rig::new(ProviderKind::Codex).await;
    let day = rig.home().join("sessions/2026/10/14");
    std::fs::create_dir_all(&day).unwrap();
    for n in 0..5001 {
        std::fs::write(day.join(format!("f{n}")), b"").unwrap();
    }
    let (private, limits) = (PrivatePaths::default(), Limits::default());
    expect(
        locate::locate(
            &rig.home(),
            ProviderKind::Codex,
            "/w",
            NATIVE,
            &trust(&private, &limits, my_uid()),
        ),
        HistoryUnavailable::Busy,
    );
}
