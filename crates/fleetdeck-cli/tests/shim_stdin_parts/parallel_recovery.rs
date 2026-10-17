//! Recovery with several shims at the same time. Split from
//! `shim_stdin.rs` to keep each module under 800 lines.

use super::*;

#[tokio::test]
async fn recovery_waits_for_blocked_writes_of_several_shims_in_parallel() {
    let rig = Rig::new().await;
    let first = rig.launch("exec sleep 30").await;
    let second = rig.launch("exec sleep 30").await;
    let effects = rig.claimed_all(&[1, 2]).await;
    let tags = [
        rig.tag(&first, &effects[0]).await,
        rig.tag(&second, &effects[1]).await,
    ];
    let shims = [first.record.shim_id.clone(), second.record.shim_id.clone()];
    drop((first, second));
    // Each shim gets a frame that blocks on its full pipe; the station dies
    // before any answer.
    for (shim_id, tag) in shims.iter().zip(&tags) {
        let (mut raw, generation) = rig.raw_of(shim_id).await;
        raw.send(&write_frame(generation, tag), &vec![b'y'; 512 * 1024])
            .await
            .unwrap();
        drop(raw);
    }
    let started = std::time::Instant::now();
    let all = recover_shims(&rig.store, &rig.host(), AckMode::OnDelivery, IN_FLIGHT)
        .await
        .unwrap();
    let took = started.elapsed();
    assert_eq!(all.len(), 2);
    assert!(
        all.iter()
            .all(|(_, outcome)| matches!(outcome, Recovered::Reattached(_))),
        "{all:?}"
    );
    assert!(
        took >= IN_FLIGHT,
        "each write had its bounded wait: {took:?}"
    );
    assert!(
        took < IN_FLIGHT * 2 - Duration::from_millis(300),
        "the waits ran one after the other: {took:?}"
    );
    for effect in &effects {
        assert_eq!(
            rig.effect(effect).await,
            (
                EffectState::OutcomeUnknown,
                Some("shim_write_unknown".to_owned())
            )
        );
    }
}
