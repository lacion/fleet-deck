//! The land fences (design §11.2 M3, V30).

use fleetdeck_testkit::Det;

use super::{fenced_at, insert, is_fenced};
use crate::store::call;
use crate::store::git_state::tests::store;

#[tokio::test]
async fn a_fence_is_recorded_for_an_unknown_land_id_and_keeps_its_first_time() {
    let det = Det::new();
    let store = store(&det).await;
    assert!(!store.land_fenced("land-9".to_owned()).await.unwrap());
    call(&store.inner.db, |conn| insert(conn, "land-9", 100))
        .await
        .unwrap();
    call(&store.inner.db, |conn| insert(conn, "land-9", 200))
        .await
        .unwrap();
    assert!(store.land_fenced("land-9".to_owned()).await.unwrap());
    let at = call(&store.inner.db, |conn| fenced_at(conn, "land-9"))
        .await
        .unwrap();
    assert_eq!(at, Some(100));
    let other = call(&store.inner.db, |conn| is_fenced(conn, "land-8"))
        .await
        .unwrap();
    assert!(!other);
}
