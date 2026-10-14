//! The worktree trash through the session manager (DL-150): the decision,
//! the effect worker, and the restart re-run of an interrupted move.

use std::process::Command;

use fleetdeck_proto::event::Event;

use super::*;
use crate::store::NewSession;
use crate::store::worktree_ops::{TRASHED, TRASHING};

const SESSION: &str = "wt-session";

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

struct Rig {
    det: Det,
    store: Store,
    policy: Policy,
    _root: tempfile::TempDir,
    base: PathBuf,
    wt: PathBuf,
}

async fn rig() -> Rig {
    let det = Det::new();
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let main = base.join("repo");
    std::fs::create_dir(&main).unwrap();
    git(&main, &["init", "-q", "-b", "main"]);
    std::fs::write(main.join("a.txt"), "one\n").unwrap();
    git(&main, &["add", "."]);
    git(&main, &["commit", "-q", "-m", "init"]);
    let wt = base.join("repo--fd-x");
    git(
        &main,
        &["worktree", "add", "-q", "-b", "fd/x", wt.to_str().unwrap()],
    );
    let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    store.recover(Vec::new()).await.unwrap();
    store
        .create_session(NewSession {
            session_id: SESSION.into(),
            provider: ProviderKind::Claude,
            provider_instance: "claude".into(),
            cwd: wt.display().to_string(),
            worktree: None,
            branch: None,
            mode: None,
            title: None,
            offline_approval_policy: OfflineApprovalPolicy::Wait,
        })
        .await
        .unwrap();
    store
        .append(
            SESSION.into(),
            vec![
                Event::SessionNamed {
                    callsign: "x".into(),
                    worktree: Some(wt.display().to_string()),
                }
                .into(),
                Event::SessionEnded {
                    reason: "process_exit".into(),
                    exit_code: Some(0),
                }
                .into(),
            ],
        )
        .await
        .unwrap();
    let policy = Policy::new(
        vec![base.clone()],
        4,
        OfflineApprovalPolicy::Wait,
        BTreeMap::from([("claude".into(), ProviderKind::Claude)]),
    );
    Rig {
        det,
        store,
        policy,
        _root: root,
        base,
        wt,
    }
}

fn envelope(
    det: &Det,
    id: &str,
    origin: CommandOrigin,
    command: SessionCommand,
) -> CommandEnvelope {
    CommandEnvelope {
        authority: None,
        command_id: CommandId::from(id),
        target_incarnation: None,
        retry_until: i64::try_from(det.now_ms()).unwrap() + RETRY_WINDOW_MS,
        origin,
        command,
    }
}

async fn state_of(store: &Store, op: &str, want: &str) {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let row = store.worktree_op(op.to_owned()).await.unwrap().unwrap();
        if row.state == want {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline, "{row:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn trash_command() -> SessionCommand {
    SessionCommand::WorktreeTrash {
        session_id: SESSION.into(),
    }
}

#[tokio::test]
async fn the_manager_trashes_and_restores_a_worktree_for_the_board_only() {
    let r = rig().await;
    let manager = SessionManager::start(
        r.store.clone(),
        r.policy.clone(),
        Adapters::new(),
        r.det.ids(),
        Arc::new(crate::workspace::PrivatePaths::default()),
    )
    .await
    .unwrap();
    // Any origin but the board is refused with no row.
    for (id, origin) in [
        ("01JWIRE1", CommandOrigin::Local),
        ("01JWIRE2", CommandOrigin::Coordinator),
    ] {
        let d = manager
            .submit(
                envelope(&r.det, id, origin, trash_command()),
                Caller::local(),
            )
            .await
            .unwrap();
        assert_eq!(d.receipt.reject.unwrap().code, RejectCode::Forbidden);
    }
    assert!(r.store.worktree_ops_open().await.unwrap().is_empty());
    let d = manager
        .submit(
            envelope(
                &r.det,
                "01JWIRE3AAAAAAA",
                CommandOrigin::Board,
                trash_command(),
            ),
            Caller::local(),
        )
        .await
        .unwrap();
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    state_of(&r.store, "01JWIRE3AAAAAAA", TRASHED).await;
    assert!(!r.wt.exists());
    assert!(
        r.base
            .join(".fleetdeck-trash/repo--fd-x--01jwire3aaaa")
            .is_dir()
    );
    // The effect settled: `Confirmed`, no session event.
    let effects = r.store.session_effects(SESSION.into()).await.unwrap();
    assert_eq!(effects.len(), 1);
    assert_eq!(effects[0].state, crate::store::EffectState::Confirmed);
    let d = manager
        .submit(
            envelope(
                &r.det,
                "01JWIRE4",
                CommandOrigin::Board,
                SessionCommand::WorktreeRestore {
                    session_id: SESSION.into(),
                },
            ),
            Caller::local(),
        )
        .await
        .unwrap();
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    state_of(&r.store, "01JWIRE3AAAAAAA", "restored").await;
    assert!(r.wt.join(".git").is_file());
    manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_station_start_runs_a_move_that_the_earlier_process_left_in_trashing() {
    let r = rig().await;
    // The decision of the earlier process: row `trashing` and one queued
    // effect; then the process dies after the rename, before the repair.
    let (policy, command) = (r.policy.clone(), trash_command());
    let d = r
        .store
        .decide(
            envelope(
                &r.det,
                "01JWIRE5BBBBBBB",
                CommandOrigin::Board,
                command.clone(),
            ),
            Caller::local(),
            move |d| crate::manager::trash::decide(d, &policy, CommandOrigin::Board, None, command),
        )
        .await
        .unwrap();
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    let row = r
        .store
        .worktree_op("01JWIRE5BBBBBBB".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, TRASHING);
    std::fs::create_dir_all(Path::new(&row.trash_path).parent().unwrap()).unwrap();
    std::fs::rename(&r.wt, &row.trash_path).unwrap();
    // The new process: recovery retires the old effect, the start queues a
    // new one, the worker runs it.
    let manager = SessionManager::start(
        r.store.clone(),
        r.policy.clone(),
        Adapters::new(),
        r.det.ids(),
        Arc::new(crate::workspace::PrivatePaths::default()),
    )
    .await
    .unwrap();
    state_of(&r.store, "01JWIRE5BBBBBBB", TRASHED).await;
    let states: Vec<_> = r
        .store
        .session_effects(SESSION.into())
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.state)
        .collect();
    assert!(states.contains(&crate::store::EffectState::RetiredNotDispatched));
    assert!(states.contains(&crate::store::EffectState::Confirmed));
    manager.shutdown(StopMode::Kill).await;
}
