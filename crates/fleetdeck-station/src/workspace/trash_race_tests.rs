//! The races of the worktree trash (DL-150, pre-PR review): the identity
//! check sits in the call that deletes or renames, and a shell open that is
//! pending or admitted before a hold cannot spawn in a held folder.

use fleetdeck_proto::shell::ShellChange;

use super::super::{Checked, delete_admin_checked, delete_checked, move_checked};
use super::*;
use crate::manager::trash::OPENING;

fn identity_of(path: &Path) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::symlink_metadata(path).unwrap();
    (m.dev(), m.ino())
}

/// The identity check sits in the call that deletes or renames: a folder
/// that was replaced after the claim is never deleted or moved.
#[tokio::test]
async fn a_folder_replaced_after_the_claim_is_neither_deleted_nor_moved() {
    let mut r = rig().await;
    let (_, row) = r.trashed().await;
    let trash = PathBuf::from(&row.trash_path);
    let rec = (row.dev, row.ino);
    assert_eq!(identity_of(&trash), rec);
    // The honest writer moves the folder aside and puts another one there.
    std::fs::rename(&trash, r.base.join("aside")).unwrap();
    std::fs::create_dir(&trash).unwrap();
    std::fs::write(trash.join("other.txt"), "x").unwrap();
    assert_eq!(delete_checked(&row.trash_path, rec), Err(Checked::Changed));
    assert!(trash.join("other.txt").is_file());
    assert!(r.base.join("aside/work.txt").is_file());
    // The same for the move out of the trash and into it.
    let back = r.wt.display().to_string();
    assert_eq!(
        move_checked(&row.trash_path, &back, rec),
        Err(Checked::Changed)
    );
    assert!(!r.wt.exists());
    assert_eq!(
        move_checked(&r.base.join("aside").display().to_string(), &back, rec),
        Ok(())
    );
    assert!(r.wt.join("work.txt").is_file());
    // A destination that is taken stops the move.
    assert_eq!(
        move_checked(&back, &row.trash_path, rec),
        Err(Checked::Changed)
    );
    assert!(r.wt.join("work.txt").is_file());
    // A folder that is gone counts as deleted.
    assert_eq!(delete_checked("/nonexistent/fd-trash-test", rec), Ok(()));
    // The Git record of another worktree is not deleted.
    let admin = r.main.join(".git/worktrees/repo--fd-x");
    assert!(admin.is_dir());
    assert_eq!(
        delete_admin_checked(
            &admin.display().to_string(),
            &r.main.join(".git").canonicalize().unwrap(),
            "/somewhere/else"
        ),
        Err(Checked::Changed)
    );
    assert!(admin.is_dir());
}

fn queue_open(r: &Rig, effect: &str, cwd: &Path) -> impl std::future::Future<Output = ()> {
    let change = ShellChange::Open {
        shell_id: format!("sh-{effect}"),
        cwd: cwd.display().to_string(),
        cols: 80,
        rows: 24,
    };
    let now = r.det.now_ms();
    let (policy, store, id) = (r.policy.clone(), r.store.clone(), effect.to_owned());
    let envelope = CommandEnvelope {
        command_id: CommandId::from(format!("01JQ{id}")),
        target_incarnation: None,
        retry_until: i64::try_from(now).unwrap() + RETRY_WINDOW_MS,
        origin: CommandOrigin::Board,
        command: SessionCommand::Rename {
            session_id: SESSION.into(),
            title: None,
        },
    };
    async move {
        let _ = &policy;
        let command_id = envelope.command_id.clone();
        // A raw `queued` shell effect, as `shell::decide` leaves it.
        let decided = store
            .decide(envelope, Caller::local(), move |d| {
                crate::store::shell_effects::enqueue(
                    d.connection(),
                    &id,
                    &command_id,
                    &change,
                    d.now(),
                )?;
                Ok(Default::default())
            })
            .await
            .unwrap();
        assert_eq!(decided.receipt.state, ReceiptState::Accepted, "{decided:?}");
    }
}

#[tokio::test]
async fn a_pending_shell_open_refuses_a_trash_and_a_purge_and_fails_their_effects() {
    let mut r = rig().await;
    // A shell open that is queued but not started (nothing in the list yet).
    queue_open(&r, "e1", &r.wt.clone()).await;
    let (code, message) = r.refused(Rig::trash_cmd()).await;
    assert_eq!(
        (code, message.as_str()),
        (RejectCode::InvalidState, OPENING)
    );
    // An open that is queued after the decision: effect step 0 fails.
    let mut fresh = rig().await;
    let op = fresh.ok(Rig::trash_cmd()).await;
    queue_open(&fresh, "e2", &wt_of(&fresh)).await;
    assert_eq!(
        fresh.run(&op, WorktreeAction::Trash).await,
        Dispatch::Confirmed
    );
    let row = fresh.row(&op).await;
    assert_eq!(
        (row.state.as_str(), row.phase.as_deref()),
        ("failed", Some("trash"))
    );
    assert_eq!(row.message.as_deref(), Some(OPENING));
    assert!(wt_of(&fresh).is_dir());
    // A purge: the decision and the effect.
    let mut p = rig().await;
    let (op, row) = p.trashed().await;
    let name = Path::new(&row.trash_path)
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    queue_open(&p, "e3", Path::new(&row.trash_path)).await;
    let (code, message) = p.refused(Rig::purge_cmd(&name)).await;
    assert_eq!(
        (code, message.as_str()),
        (RejectCode::InvalidState, OPENING)
    );
    assert_eq!(p.row(&op).await.state, "trashed");
}

fn wt_of(r: &Rig) -> PathBuf {
    r.wt.clone()
}

/// An open that was admitted before the hold began runs after it: the
/// shell effect fails with the custody text and spawns no PTY.
#[tokio::test]
async fn a_queued_shell_open_fails_when_a_row_holds_its_folder() {
    let r = rig().await;
    queue_open(&r, "e4", &r.wt.clone()).await;
    let now = r.det.now_ms();
    // The hold begins (the decision refuses while the open is pending, so
    // the row is stored the way a decision that ran earlier left it).
    let new = crate::store::NewWorktreeOp {
        op_id: "01JHOLD00000".into(),
        session_id: SESSION.into(),
        original_path: r.wt.display().to_string(),
        trash_path: r
            .base
            .join(".fleetdeck-trash/repo--fd-x--01jhold0000")
            .display()
            .to_string(),
        dev: 1,
        ino: 2,
    };
    r.store
        .decide(
            CommandEnvelope {
                command_id: CommandId::from("01JHOLDCMD"),
                target_incarnation: None,
                retry_until: i64::try_from(now).unwrap() + RETRY_WINDOW_MS,
                origin: CommandOrigin::Board,
                command: Rig::trash_cmd(),
            },
            Caller::local(),
            move |d| {
                d.insert_worktree_op(&new)?;
                Ok(Default::default())
            },
        )
        .await
        .unwrap();
    let shells = Arc::new(Shells::new(ShellConfig::default()));
    let ran = crate::shell::dispatch(&r.store, &shells, &r.policy)
        .await
        .unwrap();
    assert_eq!(ran, 1);
    assert!(shells.list().is_empty(), "no PTY was spawned");
    let state = r
        .store
        .shell_effect(CommandId::from("01JQe4"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.state, "failed");
    assert_eq!(state.detail.as_deref(), Some(HELD));
}
