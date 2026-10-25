//! MCP P4b §4.1: the bindings that a set does not rewrite, the target binding
//! id, and the snapshot projection of §5.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

#[tokio::test]
async fn a_dead_held_fenced_or_ended_binding_is_not_rewritten() {
    for shim in [false, true] {
        let wide = give("g-1", &["read", "mail"]);

        // A revoked entry stays dead; the record is written.
        let mut rig = Rig::new(shim).await;
        let (_, digest) = rig.run(&fleet(BindingState::Revoked), None).await;
        accepted(&rig.set(1, "g-1", Some(&wide)).await);
        assert_eq!(rig.memory(&digest).state, BindingState::Revoked);
        assert_eq!(rig.memory(&digest).grant_id, "");
        assert_eq!(rig.record().await.unwrap().state, RecordState::Attached);

        // A held digest.
        let mut rig = Rig::new(shim).await;
        let row = shim.then_some(RowState::Active);
        let (_, digest) = rig.run(&fleet(BindingState::Active), row).await;
        rig.bindings().hold([digest.clone()]);
        accepted(&rig.set(1, "g-1", Some(&wide)).await);
        assert_eq!(rig.memory(&digest).grant_id, "");
        if shim {
            assert_eq!(rig.row(&digest).await.grant_id, "");
        }
        assert_eq!(rig.record().await.unwrap().seq, 1);
        assert!(rig.events().await.is_empty());

        // A shim row that ended: memory is not rewritten either.
        if shim {
            let mut rig = Rig::new(true).await;
            let (_, digest) = rig
                .run(&fleet(BindingState::Active), Some(RowState::Ended))
                .await;
            accepted(&rig.set(1, "g-1", Some(&wide)).await);
            assert_eq!(rig.memory(&digest).grant_id, "");
            assert_eq!(rig.row(&digest).await.grant_id, "");

            // A fenced digest, and an unreadable fence (fail closed).
            let mut rig = Rig::new(true).await;
            let (_, digest) = rig
                .run(&fleet(BindingState::Active), Some(RowState::Active))
                .await;
            let fence = rig.fence.clone().unwrap();
            fence
                .add(&[("shim1".to_owned(), digest.clone())], "revoke")
                .unwrap();
            accepted(&rig.set(1, "g-1", Some(&wide)).await);
            assert_eq!(rig.memory(&digest).grant_id, "");
            assert_eq!(rig.row(&digest).await.grant_id, "");
            assert!(rig.events().await.is_empty());

            let mut rig = Rig::new(true).await;
            let (_, digest) = rig
                .run(&fleet(BindingState::Active), Some(RowState::Active))
                .await;
            std::fs::write(rig._tmp.path().join("run/bindings.fence"), "junk\n").unwrap();
            accepted(&rig.set(1, "g-1", Some(&wide)).await);
            assert_eq!(rig.memory(&digest).grant_id, "");
            assert_eq!(rig.row(&digest).await.grant_id, "");
        }

        // An end of another grant than the live one rewrites nothing.
        let mut rig = Rig::new(shim).await;
        let live = binding("s1:g-9", "g-9", BindingState::Active, 3);
        let row = shim.then_some(RowState::Active);
        let (_, digest) = rig.run(&live, row).await;
        accepted(&rig.set(1, "g-1", None).await);
        assert_eq!(rig.memory(&digest).grant_id, "g-9");
        assert_eq!(rig.memory(&digest).state, BindingState::Active);
        assert!(rig.events().await.is_empty());
    }
}

#[tokio::test]
async fn the_target_binding_id_is_freed_of_dead_entries_and_refused_for_a_live_one() {
    for shim in [false, true] {
        let wide = give("g-1", &["read"]);

        // A dead entry under the target id is removed; the live one is rewritten.
        let mut rig = Rig::new(shim).await;
        let (_, dead) = rig
            .run(&binding("s1:g-1", "g-1", BindingState::Revoked, 2), None)
            .await;
        let row = shim.then_some(RowState::Active);
        let (_, digest) = rig.run(&fleet(BindingState::Active), row).await;
        accepted(&rig.set(1, "g-1", Some(&wide)).await);
        assert!(
            !rig.bindings().has_digest(&dead),
            "the dead entry is removed"
        );
        assert_eq!(rig.memory(&digest).grant_id, "g-1");

        // Another live entry under the target id: no rewrite.
        let mut rig = Rig::new(shim).await;
        let mut other = binding("s1:g-1", "g-1", BindingState::Active, 2);
        other.session_id = "s2".into();
        rig.run(&other, None).await;
        let row = shim.then_some(RowState::Active);
        let (_, digest) = rig.run(&fleet(BindingState::Active), row).await;
        accepted(&rig.set(1, "g-1", Some(&wide)).await);
        assert_eq!(rig.memory(&digest).grant_id, "");
        if shim {
            assert_eq!(rig.row(&digest).await.grant_id, "");
        }
        assert!(rig.events().await.is_empty());
    }
}

#[test]
fn the_projection_follows_the_record() {
    let bindings = Bindings::default();
    let add = |id: &str, session: &str, grant: &str, state: BindingState| {
        let (_, digest) = mint().unwrap();
        let mut b = binding(id, grant, state, 4);
        b.session_id = session.into();
        bindings.insert(digest, b);
    };
    let record =
        |session: &str, grant: &str, state: RecordState, reason: Option<&str>| GrantRecord {
            session_id: session.into(),
            grant_id: grant.to_owned(),
            seq: 3,
            spec: None,
            generation: 6,
            state,
            ended_reason: reason.map(str::to_owned),
            updated_at: 1,
        };
    // An attached record with a dead credential is left out; one with a live
    // credential reports it; a grant without a record is as today.
    add("a:g-a", "a", "g-a", BindingState::Revoked);
    add("b:g-b", "b", "g-b", BindingState::Active);
    add("c:g-c", "c", "g-c", BindingState::Expired);
    // The fleet-only binding is never projected.
    add("a:fleet", "a", "", BindingState::Active);
    // An ended record that memory holds is reported from memory.
    add("f:g-f", "f", "g-f", BindingState::Revoked);
    let records = [
        record("a", "g-a", RecordState::Attached, None),
        record("b", "g-b", RecordState::Attached, None),
        record("d", "g-d", RecordState::Ended, Some("revoked")),
        record("e", "g-e", RecordState::Ended, Some("expired")),
        record("f", "g-f", RecordState::Ended, Some("revoked")),
    ];
    let got: Vec<_> = bindings
        .projection_with(&records)
        .into_iter()
        .map(|b| (b.binding_id, b.state, b.generation))
        .collect();
    assert_eq!(
        got,
        [
            ("b:g-b".to_owned(), Phase::Active, 4),
            ("c:g-c".to_owned(), Phase::Expired, 4),
            ("d:g-d".to_owned(), Phase::Revoked, 6),
            ("e:g-e".to_owned(), Phase::Expired, 6),
            ("f:g-f".to_owned(), Phase::Revoked, 4),
        ]
    );
    let synthetic = bindings
        .projection_with(&records)
        .into_iter()
        .find(|b| b.binding_id == "d:g-d")
        .unwrap();
    assert_eq!(synthetic.grant_id, "g-d");
    assert_eq!(synthetic.session_id.as_str(), "d");
    assert_eq!(synthetic.provider_instance, "");
    assert_eq!(synthetic.native_id, None);
    // Without records the projection is as before.
    let plain: Vec<_> = bindings
        .projection()
        .into_iter()
        .map(|b| b.binding_id)
        .collect();
    assert_eq!(plain, ["a:g-a", "b:g-b", "c:g-c", "f:g-f"]);
    assert_eq!(bindings.projection(), bindings.projection_with(&[]));
}
