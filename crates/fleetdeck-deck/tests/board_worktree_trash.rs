//! The worktree trash over a real link on loopback (DL-150): the owner
//! moves a worktree to the trash, restores it and deletes it; a token, a
//! missing CSRF header and a wrong confirmation name are refused; every call
//! is audited.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use common::board::{BoardClient, Http, board_with_node};
use fleetdeck_core::identity::Identity;
use fleetdeck_core::{SharedClock, SharedIds, SystemClock, UlidSource};
use fleetdeck_deck::store::AuditResult;
use fleetdeck_deck::{DeckNode, DeckStore};
use fleetdeck_link::reconnect::BackoffPolicy;
use fleetdeck_proto::Event;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::link::Scope;
use fleetdeck_station::link::StationNode;
use fleetdeck_station::manager::{Adapters, Policy, SessionManager};
use fleetdeck_station::store::{NewEvent, NewSession, OfflineApprovalPolicy, Store, StoreConfig};
use serde_json::{Value, json};

const T: Duration = Duration::from_secs(15);

fn fast() -> BackoffPolicy {
    BackoffPolicy {
        base: Duration::from_millis(50),
        cap: Duration::from_millis(200),
        stable: Duration::from_secs(60),
    }
}

fn clock_ids() -> (SharedClock, SharedIds) {
    let clock: SharedClock = Arc::new(SystemClock);
    let ids: SharedIds = Arc::new(UlidSource::new(Arc::clone(&clock)));
    (clock, ids)
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
}

/// A repository with the worktree `repo--fd-x`; returns the worktree.
fn repo(root: &Path) -> PathBuf {
    let main = root.join("repo");
    std::fs::create_dir_all(&main).unwrap();
    git(&main, &["init", "-q", "-b", "main"]);
    std::fs::write(main.join("a.txt"), "one\n").unwrap();
    git(&main, &["add", "."]);
    git(&main, &["commit", "-q", "-m", "init"]);
    let wt = root.join("repo--fd-x");
    git(
        &main,
        &["worktree", "add", "-q", "-b", "fd/x", wt.to_str().unwrap()],
    );
    wt
}

/// A station whose store holds one ended session in `worktree`.
async fn station(dir: &Path, root: PathBuf, worktree: &Path) -> Arc<StationNode> {
    let (clock, ids) = clock_ids();
    let store = Store::open(
        &dir.join("station.db"),
        clock,
        Arc::clone(&ids),
        StoreConfig::default(),
    )
    .await
    .unwrap();
    store
        .create_session(NewSession {
            session_id: "wt-session".into(),
            provider: ProviderKind::Claude,
            provider_instance: "claude".into(),
            cwd: worktree.display().to_string(),
            worktree: None,
            branch: None,
            mode: None,
            title: Some("Fix the parser".to_owned()),
            offline_approval_policy: OfflineApprovalPolicy::Wait,
        })
        .await
        .unwrap();
    let events: Vec<NewEvent> = vec![
        Event::SessionNamed {
            callsign: "x".to_owned(),
            worktree: Some(worktree.display().to_string()),
        }
        .into(),
        Event::SessionEnded {
            reason: "process_exit".into(),
            exit_code: Some(0),
        }
        .into(),
    ];
    store.append("wt-session".into(), events).await.unwrap();
    let policy = Policy::new(vec![root], 2, OfflineApprovalPolicy::Wait, BTreeMap::new());
    let manager = SessionManager::start(
        store,
        policy,
        Adapters::new(),
        ids,
        Arc::new(fleetdeck_station::workspace::PrivatePaths::default()),
    )
    .await
    .unwrap();
    StationNode::start(
        Arc::new(manager),
        Arc::new(Identity::generate().unwrap()),
        "alpha".to_owned(),
        &[],
        fast(),
    )
    .await
    .unwrap()
}

async fn deck(dir: &Path) -> (Arc<DeckNode>, BoardClient) {
    let (clock, ids) = clock_ids();
    let store = DeckStore::open(&dir.join("deck.db"), clock, ids)
        .await
        .unwrap();
    let identity = Arc::new(Identity::generate().unwrap());
    let node = DeckNode::start(store, identity, &["127.0.0.1:0".parse().unwrap()], fast())
        .await
        .unwrap();
    let board = board_with_node(Arc::clone(&node)).await;
    (node, board)
}

/// Posts until the deck has a live, synced link to `alpha`.
async fn act(c: &BoardClient, h: &[(&str, &str)], action: &str, body: Value) -> Http {
    let path = format!("/api/stations/alpha/worktrees/{action}");
    let deadline = tokio::time::Instant::now() + T;
    loop {
        let r = c.post(&path, h, body.clone()).await;
        let waiting = [(404, "unknown_station"), (409, "not_synced")];
        if !waiting.contains(&(r.status, r.code())) {
            return r;
        }
        assert!(tokio::time::Instant::now() < deadline, "{}", r.body);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The inventory row of the session, once `done` accepts it.
async fn inventory(c: &BoardClient, h: &[(&str, &str)], done: impl Fn(&Value) -> bool) -> Value {
    let deadline = tokio::time::Instant::now() + T;
    loop {
        let r = c.get("/api/stations/alpha/worktrees", h).await;
        if r.status == 200 && done(&r.body["worktrees"][0]) {
            return r.body["worktrees"][0].clone();
        }
        assert!(tokio::time::Instant::now() < deadline, "{}", r.body);
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

/// The stored command once the station answered it.
async fn receipt(c: &BoardClient, h: &[(&str, &str)], id: &str) -> Value {
    let deadline = tokio::time::Instant::now() + T;
    loop {
        let r = c.get(&format!("/api/commands/{id}"), h).await;
        if r.status == 200 && !r.body["receipt"].is_null() {
            return r.body["receipt"].clone();
        }
        assert!(tokio::time::Instant::now() < deadline, "{}", r.body);
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

#[tokio::test]
async fn the_owner_trashes_restores_and_purges_a_worktree_and_every_call_is_audited() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("projects");
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let wt = repo(&root);
    std::fs::create_dir_all(tmp.path().join("s")).unwrap();
    std::fs::create_dir_all(tmp.path().join("d")).unwrap();
    let (node, board) = deck(&tmp.path().join("d")).await;
    let st = station(&tmp.path().join("s"), root.clone(), &wt).await;
    st.join(
        &node
            .invite(fleetdeck_deck::knobs::DeckKnobs::default().invite)
            .unwrap(),
    )
    .await
    .unwrap();
    let (cookie, csrf) = board.login().await;
    let h = [("Cookie", cookie.as_str()), ("X-CSRF-Token", csrf.as_str())];
    let list_h = [("Cookie", cookie.as_str())];

    // Trash: the owner gets the command; the station accepts it.
    let r = act(
        &board,
        &h,
        "trash",
        json!({"session_id": "wt-session", "command_id": "trash-0001"}),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["command_id"], "trash-0001");
    assert_eq!(r.body["method"], "session.worktree_trash");
    assert_eq!(
        receipt(&board, &list_h, "trash-0001").await["state"],
        "accepted"
    );
    let row = inventory(&board, &list_h, |w| w["trash"]["state"] == "trashed").await;
    assert_eq!(row["trash"]["state"], "trashed");
    let trash_path = PathBuf::from(row["trash"]["trash_path"].as_str().unwrap());
    assert!(trash_path.is_dir());
    assert!(!wt.exists());

    // A resend of the same command id sends nothing new.
    let again = act(
        &board,
        &h,
        "trash",
        json!({"session_id": "wt-session", "command_id": "trash-0001"}),
    )
    .await;
    assert_eq!(again.status, 200, "{}", again.body);

    // Restore.
    let r = act(
        &board,
        &h,
        "restore",
        json!({"session_id": "wt-session", "command_id": "restore-0001"}),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    inventory(&board, &list_h, |w| {
        w["trash"].is_null() && w["verdict"] != "trashed"
    })
    .await;
    assert!(wt.join("a.txt").is_file());

    // A wrong confirmation name is refused by the station.
    let r = act(
        &board,
        &h,
        "trash",
        json!({"session_id": "wt-session", "command_id": "trash-0002"}),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let row = inventory(&board, &list_h, |w| w["trash"]["state"] == "trashed").await;
    let trash_path = PathBuf::from(row["trash"]["trash_path"].as_str().unwrap());
    let r = act(
        &board,
        &h,
        "purge",
        json!({"session_id": "wt-session", "command_id": "purge-0001", "confirm_name": "nope"}),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let rec = receipt(&board, &list_h, "purge-0001").await;
    assert_eq!(rec["state"], "rejected");
    assert_eq!(rec["reject"]["code"], "invalid_command");
    assert!(trash_path.is_dir());

    // The right name deletes the folder.
    let name = trash_path.file_name().unwrap().to_str().unwrap();
    let r = act(
        &board,
        &h,
        "purge",
        json!({"session_id": "wt-session", "command_id": "purge-0002", "confirm_name": name}),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    inventory(&board, &list_h, |w| {
        w["trash"].is_null() && w["verdict"] == "missing"
    })
    .await;
    assert!(!trash_path.exists());

    // A release of a row that is not failed is refused by the station.
    let r = act(
        &board,
        &h,
        "release",
        json!({"session_id": "wt-session", "command_id": "release-0001"}),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        receipt(&board, &list_h, "release-0001").await["state"],
        "rejected"
    );

    for action in [
        "worktree.trash",
        "worktree.restore",
        "worktree.purge",
        "worktree.release",
    ] {
        let ok = board.audited(action, AuditResult::Ok).await;
        assert!(!ok.is_empty(), "{action}");
    }
    let rows = board.store.audit_log(100).await.unwrap();
    let trash = rows
        .iter()
        .find(|e| e.action == "worktree.trash" && e.result == AuditResult::Ok)
        .unwrap();
    assert!(
        trash
            .target
            .as_deref()
            .is_some_and(|t| t == "alpha/wt-session"),
        "{trash:?}"
    );
    assert!(trash.command_id.is_some());
}

#[tokio::test]
async fn a_token_and_a_missing_csrf_header_are_refused_and_audited() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("projects");
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let wt = repo(&root);
    std::fs::create_dir_all(tmp.path().join("s")).unwrap();
    std::fs::create_dir_all(tmp.path().join("d")).unwrap();
    let (node, board) = deck(&tmp.path().join("d")).await;
    let st = station(&tmp.path().join("s"), root.clone(), &wt).await;
    st.join(
        &node
            .invite(fleetdeck_deck::knobs::DeckKnobs::default().invite)
            .unwrap(),
    )
    .await
    .unwrap();
    let (cookie, csrf) = board.login().await;
    let h = [("Cookie", cookie.as_str()), ("X-CSRF-Token", csrf.as_str())];
    // Wait until the deck knows the station.
    let r = act(
        &board,
        &h,
        "release",
        json!({"session_id": "wt-session", "command_id": "warmup-0001"}),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);

    let token = board
        .store
        .create_token(
            "ops".into(),
            BTreeSet::from([Scope::Read, Scope::Admin, Scope::Drive]),
            86_400_000,
        )
        .await
        .unwrap();
    let bearer = format!("Bearer {}", token.1.expose());
    for action in ["trash", "restore", "purge", "release"] {
        let r = board
            .post(
                &format!("/api/stations/alpha/worktrees/{action}"),
                &[("Authorization", bearer.as_str())],
                json!({"session_id": "wt-session", "command_id": format!("tok-{action}")}),
            )
            .await;
        assert_eq!((r.status, r.code()), (403, "owner_only"), "{}", r.body);
        let refused = board
            .audited(&format!("worktree.{action}"), AuditResult::Refused)
            .await;
        assert!(
            refused.contains(&Some("owner_only".to_owned())),
            "{action}: {refused:?}"
        );
    }
    // Nothing was sent for the token.
    let r = board
        .get("/api/commands/tok-trash", &[("Cookie", cookie.as_str())])
        .await;
    assert_eq!(r.status, 404, "{}", r.body);

    // The cookie login needs the CSRF header.
    let r = board
        .post(
            "/api/stations/alpha/worktrees/trash",
            &[("Cookie", cookie.as_str())],
            json!({"session_id": "wt-session", "command_id": "csrf-0001"}),
        )
        .await;
    assert_eq!((r.status, r.code()), (403, "csrf"), "{}", r.body);
    assert!(wt.is_dir());

    // A body with an unknown field, and confirm_name outside purge.
    let r = board
        .post(
            "/api/stations/alpha/worktrees/trash",
            &h,
            json!({"session_id": "wt-session", "command_id": "bad-0001", "force": true}),
        )
        .await;
    assert_eq!((r.status, r.code()), (400, "invalid_body"), "{}", r.body);
    let r = board
        .post(
            "/api/stations/alpha/worktrees/trash",
            &h,
            json!({"session_id": "wt-session", "command_id": "bad-0002", "confirm_name": "x"}),
        )
        .await;
    assert_eq!((r.status, r.code()), (400, "invalid_body"), "{}", r.body);
    assert!(wt.is_dir());
}
