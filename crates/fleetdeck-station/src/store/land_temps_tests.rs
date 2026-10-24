//! The temporary worktree rows of a land (design §11.6, V30).

use fleetdeck_testkit::Det;

use super::{LandTempState, finish, get, kept, open, reserve, set_present};
use crate::store::call;
use crate::store::git_state::tests::store;

#[tokio::test]
async fn a_row_is_reserved_then_present_then_removed() {
    let det = Det::new();
    let store = store(&det).await;
    call(&store.inner.db, |conn| {
        reserve(
            conn,
            "land-1",
            1,
            "/r",
            "/r/.fleetdeck-land/land-1abcde-1",
            10,
        )?;
        let row = get(conn, "land-1", 1)?.unwrap();
        assert_eq!(row.state, LandTempState::Reserved);
        assert_eq!((row.dev, row.ino, row.admin_dir), (None, None, None));
        assert!(set_present(
            conn,
            "land-1",
            1,
            (7, 9),
            "/h/.git/worktrees/w",
            11
        )?);
        // A second call finds no `reserved` row.
        assert!(!set_present(conn, "land-1", 1, (1, 1), "/x", 12)?);
        let row = get(conn, "land-1", 1)?.unwrap();
        assert_eq!(row.state, LandTempState::Present);
        assert_eq!((row.dev, row.ino), (Some(7), Some(9)));
        assert_eq!(row.admin_dir.as_deref(), Some("/h/.git/worktrees/w"));
        assert_eq!(open(conn)?.len(), 1);
        assert!(finish(conn, "land-1", 1, LandTempState::Removed, None, 13)?);
        assert!(open(conn)?.is_empty());
        // A finished row does not change again.
        assert!(!finish(
            conn,
            "land-1",
            1,
            LandTempState::Kept,
            Some("x"),
            14
        )?);
        assert_eq!(
            get(conn, "land-1", 1)?.unwrap().state,
            LandTempState::Removed
        );
        Ok(())
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_kept_row_has_its_reason_and_the_attempts_are_separate_rows() {
    let det = Det::new();
    let store = store(&det).await;
    call(&store.inner.db, |conn| {
        reserve(conn, "land-1", 1, "/r", "/r/.fleetdeck-land/a-1", 10)?;
        reserve(conn, "land-1", 2, "/r", "/r/.fleetdeck-land/a-2", 11)?;
        // One row for each (land, attempt).
        assert!(reserve(conn, "land-1", 2, "/r", "/r/.fleetdeck-land/a-2", 12).is_err());
        assert!(finish(
            conn,
            "land-1",
            1,
            LandTempState::Kept,
            Some("temp_changed"),
            20
        )?);
        let rows = kept(conn)?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].code.as_deref(), Some("temp_changed"));
        assert_eq!(open(conn)?.len(), 1);
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(store.land_temps_kept().await.unwrap().len(), 1);
    assert_eq!(store.land_temps_open().await.unwrap().len(), 1);
}
