//! A task that outlives the await of its caller (pre-PR review finding 2).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicBool, Ordering};

use super::*;

#[tokio::test]
async fn settle_waits_for_a_task_whose_await_a_time_limit_dropped() {
    let m = Mutations::new();
    let done = Arc::new(AtomicBool::new(false));
    let d = Arc::clone(&done);
    let late = m.run(move || {
        std::thread::sleep(Duration::from_millis(400));
        d.store(true, Ordering::SeqCst);
    });
    // The limit drops the await; the blocking task goes on.
    assert!(
        tokio::time::timeout(Duration::from_millis(50), late)
            .await
            .is_err()
    );
    assert!(!done.load(Ordering::SeqCst));
    assert!(m.settle(Duration::from_secs(10)).await);
    // No change of the task can come after `settle` returned.
    assert!(done.load(Ordering::SeqCst));
}

#[tokio::test]
async fn settle_is_false_while_a_task_runs_past_the_limit() {
    let m = Arc::new(Mutations::new());
    let m2 = Arc::clone(&m);
    let task = tokio::spawn(async move {
        m2.run(|| std::thread::sleep(Duration::from_millis(600)))
            .await
            .unwrap();
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!m.settle(Duration::from_millis(50)).await);
    task.await.unwrap();
    assert!(m.settle(Duration::from_millis(50)).await);
}

#[tokio::test]
async fn settle_is_true_at_once_without_a_task() {
    let m = Mutations::new();
    assert!(m.settle(Duration::from_millis(10)).await);
    assert_eq!(m.run(|| 7).await.unwrap(), 7);
    assert!(m.settle(Duration::from_millis(10)).await);
}
