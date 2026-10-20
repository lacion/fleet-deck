//! The deck-local pass that a send and an authority check repeat (R13 P6
//! §6.2 step 10, decision O12-9 item 8), the switches, and the tool classes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::identity::Identity;
use fleetdeck_link::Epochs;
use fleetdeck_link::hello::LocalHello;
use fleetdeck_link::node::NodeConfig;
use fleetdeck_link::tls::Role;
use fleetdeck_testkit::Det;

use super::*;
use crate::live::{DeckLive, LiveCell};
use crate::peers::DeckPeers;
use crate::store::{DeckStore, NewGrant};

fn alpha() -> StationId {
    StationId::from("alpha")
}

fn live(desktop_control: bool) -> DeckLive {
    DeckLive {
        name: "deck".into(),
        conflict_mail: false,
        fleet_control: true,
        desktop_control,
        tailscale_cert: false,
        tailscale_bin: None,
        knobs: crate::knobs::DeckKnobs::default(),
        worker_grant: fleetdeck_core::config_worker_grant::WorkerGrantConfig {
            enabled: false,
            ..fleetdeck_core::config_worker_grant::WorkerGrantConfig::default()
        },
    }
}

struct Rig {
    det: Det,
    link: DeckLink,
    grant_id: String,
}

/// A deck link with a bound `desktop` grant of `coord-1` on `alpha`, fleet
/// control on, desktop control on, and `alpha` linked with every desktop
/// feature.
async fn rig(classes: &[GrantClass]) -> Rig {
    let det = Det::new();
    let store = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    let peers = DeckPeers::load(store.clone()).await.unwrap();
    let node = NodeConfig::new(
        Arc::new(Identity::generate().unwrap()),
        LocalHello::new(Role::Deck, store.incarnation().clone()),
        Arc::new(Epochs::in_memory()),
    )
    .with_store(Arc::clone(&peers));
    let link = DeckLink::new(store.clone(), peers, node);
    link.desktop_live.set(LiveCell::new(live(true))).ok();
    store.set_fleet_control(true);
    let mut grant = NewGrant::new(alpha(), "coord-1".into());
    grant.classes = classes.iter().copied().collect();
    let grant = store.create_grant(grant).await.unwrap();
    let bound = store
        .bind_grant(CoordinatorBinding {
            binding_id: "b-1".into(),
            grant_id: grant.grant_id.clone(),
            generation: 1,
            provider_instance: "claude".into(),
            native_id: "n-1".into(),
        })
        .await
        .unwrap();
    assert!(bound);
    store.set_link_features(
        &alpha(),
        1,
        target_features(true)
            .iter()
            .map(|f| (*f).to_owned())
            .collect(),
    );
    Rig {
        det,
        link,
        grant_id: grant.grant_id,
    }
}

fn held<'a>(rig: &'a Rig, station: &'a StationId, class: OpClass) -> Held<'a> {
    Held {
        grant_id: &rig.grant_id,
        binding_id: "b-1",
        generation: 1,
        caller: station,
        target: station,
        class,
        tool: None,
    }
}

#[tokio::test]
async fn a_good_grant_passes_and_returns_the_grant_as_it_is_now() {
    let r = rig(&[GrantClass::Desktop]).await;
    let station = alpha();
    let grant = local_pass(&r.link, &held(&r, &station, OpClass::Desktop))
        .await
        .expect("a pass");
    assert_eq!(grant.grant_id, r.grant_id);
    assert_eq!(grant.binding.map(|b| b.generation), Some(1));
}

#[tokio::test]
async fn a_revoked_an_expired_and_an_unknown_grant_are_not_granted() {
    let r = rig(&[GrantClass::Desktop]).await;
    let station = alpha();
    let mut unknown = held(&r, &station, OpClass::Desktop);
    unknown.grant_id = "no-such-grant";
    assert_eq!(
        local_pass(&r.link, &unknown).await.err(),
        Some("not_granted")
    );
    r.det.advance(Duration::from_secs(13 * 3600));
    assert_eq!(
        local_pass(&r.link, &held(&r, &station, OpClass::Desktop))
            .await
            .err(),
        Some("not_granted")
    );
    let r = rig(&[GrantClass::Desktop]).await;
    r.link
        .store
        .revoke_grant(r.grant_id.clone(), "test".into())
        .await
        .unwrap();
    assert_eq!(
        local_pass(&r.link, &held(&r, &station, OpClass::Desktop))
            .await
            .err(),
        Some("not_granted")
    );
}

#[tokio::test]
async fn another_binding_id_generation_caller_target_or_class_is_refused() {
    let r = rig(&[GrantClass::Desktop]).await;
    let station = alpha();
    let other = StationId::from("beta");
    let mut stale_id = held(&r, &station, OpClass::Desktop);
    stale_id.binding_id = "b-2";
    let mut stale_generation = held(&r, &station, OpClass::Desktop);
    stale_generation.generation = 2;
    for stale in [stale_id, stale_generation] {
        assert_eq!(
            local_pass(&r.link, &stale).await.err(),
            Some("binding_stale")
        );
    }
    let mut wrong_caller = held(&r, &station, OpClass::Desktop);
    wrong_caller.caller = &other;
    assert_eq!(
        local_pass(&r.link, &wrong_caller).await.err(),
        Some("not_granted")
    );
    // A target outside the grant, and a class that the grant lacks.
    let mut outside = held(&r, &station, OpClass::Desktop);
    outside.target = &other;
    assert_eq!(
        local_pass(&r.link, &outside).await.err(),
        Some("not_granted")
    );
    assert_eq!(
        local_pass(&r.link, &held(&r, &station, OpClass::Capture))
            .await
            .err(),
        Some("not_granted")
    );
}

#[tokio::test]
async fn the_switches_are_checked_first_and_an_empty_cell_is_off() {
    let r = rig(&[GrantClass::Desktop]).await;
    let station = alpha();
    let h = held(&r, &station, OpClass::Desktop);
    r.link.store.set_fleet_control(false);
    assert_eq!(
        local_pass(&r.link, &h).await.err(),
        Some("fleet_control_off")
    );
    r.link.store.set_fleet_control(true);
    r.link
        .desktop_live
        .get()
        .expect("a cell")
        .publish(live(false));
    assert_eq!(
        local_pass(&r.link, &h).await.err(),
        Some("desktop_control_off")
    );
    assert_eq!(switches(&r.link), Err("desktop_control_off"));
    // A deck link whose cell was never set counts as off.
    let bare = rig(&[GrantClass::Desktop]).await;
    let empty = DeckLink::new(
        bare.link.store.clone(),
        Arc::clone(&bare.link.peers),
        bare.link.node.clone(),
    );
    assert_eq!(switches(&empty), Err("desktop_control_off"));
}

#[tokio::test]
async fn the_target_link_needs_its_features_and_a_missing_link_is_unreachable() {
    let r = rig(&[GrantClass::Desktop, GrantClass::Capture]).await;
    let station = alpha();
    // Every feature of both sets: both classes pass.
    for class in [OpClass::Desktop, OpClass::Capture] {
        assert!(
            local_pass(&r.link, &held(&r, &station, class))
                .await
                .is_ok()
        );
    }
    // A link that lacks the capture features: the desktop class still passes.
    let desktop_only: Vec<String> = target_features(false)
        .iter()
        .map(|f| (*f).to_owned())
        .collect();
    r.link.store.clear_link_features(&station, 1);
    r.link.store.set_link_features(&station, 1, desktop_only);
    assert!(
        local_pass(&r.link, &held(&r, &station, OpClass::Desktop))
            .await
            .is_ok()
    );
    assert_eq!(
        local_pass(&r.link, &held(&r, &station, OpClass::Capture))
            .await
            .err(),
        Some("desktop_unsupported")
    );
    // A second live link without any feature: nothing passes.
    r.link.store.set_link_features(&station, 2, Vec::new());
    assert_eq!(
        local_pass(&r.link, &held(&r, &station, OpClass::Desktop))
            .await
            .err(),
        Some("desktop_unsupported")
    );
    // No link at all.
    r.link.store.clear_link_features(&station, 1);
    r.link.store.clear_link_features(&station, 2);
    assert_eq!(
        local_pass(&r.link, &held(&r, &station, OpClass::Desktop))
            .await
            .err(),
        Some("station_unreachable")
    );
}

#[test]
fn the_class_of_each_tool_and_the_callsign_grammar() {
    use Tool::*;
    for tool in [
        WallShow,
        Notify,
        PreviewOpen,
        WindowClose,
        DesktopActionStatus,
    ] {
        assert_eq!(tool.class(), Some(OpClass::Desktop));
    }
    for tool in [Screenshot, CaptureStatus, ReadCaptureImage] {
        assert_eq!(tool.class(), Some(OpClass::Capture));
    }
    assert_eq!(DesktopInfo.class(), None);
    assert_eq!(Notify.op_tool(), Some(OpTool::Notify));
    assert_eq!(Screenshot.op_tool(), Some(OpTool::Screenshot));
    assert_eq!(DesktopInfo.op_tool(), None);
    assert_eq!(DesktopActionStatus.op_tool(), None);
    assert!(callsign_ok("swift-fox"));
    assert!(callsign_ok(&"a".repeat(32)));
    for bad in ["", "Swift", "swift fox", "swift_fox", "é", &"a".repeat(33)] {
        assert!(!callsign_ok(bad), "{bad:?}");
    }
}

#[tokio::test]
async fn the_input_grant_rules_follow_the_tool_the_allowances_and_the_scope() {
    let r = rig(&[GrantClass::Desktop, GrantClass::Capture, GrantClass::Input]).await;
    let mut grant = r
        .link
        .store
        .grant(r.grant_id.clone())
        .await
        .unwrap()
        .unwrap();
    // No allowance: every input tool, `window_list` included.
    for tool in [
        Tool::KeyPress,
        Tool::TypeText,
        Tool::PointerMove,
        Tool::PointerClick,
        Tool::WindowList,
    ] {
        assert_eq!(
            input_grant(&grant, tool),
            Err("input_not_allowed"),
            "{tool:?}"
        );
    }
    grant.allow_input = true;
    grant.input_scope = Some(InputScope::OwnWindows);
    for tool in [Tool::KeyPress, Tool::TypeText, Tool::PointerMove] {
        assert_eq!(input_grant(&grant, tool), Ok(()), "{tool:?}");
    }
    // A click needs its allowance; `window_list` needs the broader scope.
    assert_eq!(
        input_grant(&grant, Tool::PointerClick),
        Err("input_click_not_allowed")
    );
    assert_eq!(
        input_grant(&grant, Tool::WindowList),
        Err("input_scope_not_allowed")
    );
    grant.allow_click = true;
    assert_eq!(input_grant(&grant, Tool::PointerClick), Ok(()));
    grant.input_scope = Some(InputScope::AnyWindow);
    for tool in [
        Tool::KeyPress,
        Tool::TypeText,
        Tool::PointerMove,
        Tool::PointerClick,
        Tool::WindowList,
    ] {
        assert_eq!(input_grant(&grant, tool), Ok(()), "{tool:?}");
    }
    // A scope that is `None` is not an allowance.
    grant.input_scope = None;
    assert_eq!(
        input_grant(&grant, Tool::KeyPress),
        Err("input_not_allowed")
    );
    // A missing class is `not_granted`, before everything else.
    grant.input_scope = Some(InputScope::AnyWindow);
    grant.classes.remove(&GrantClass::Capture);
    assert_eq!(input_grant(&grant, Tool::PointerClick), Err("not_granted"));
}

#[test]
fn the_input_features_follow_the_tool_and_the_scope() {
    let own = Some(InputScope::OwnWindows);
    let any = Some(InputScope::AnyWindow);
    assert_eq!(
        input_features(Tool::KeyPress, own),
        [feature::DESKTOP_INPUT_MCP]
    );
    assert_eq!(
        input_features(Tool::PointerClick, own),
        [feature::DESKTOP_INPUT_MCP, feature::DESKTOP_CLICK_MCP]
    );
    assert_eq!(
        input_features(Tool::TypeText, any),
        [feature::DESKTOP_INPUT_MCP, feature::DESKTOP_INPUT_ANY]
    );
    assert_eq!(
        input_features(Tool::PointerClick, any),
        [
            feature::DESKTOP_INPUT_MCP,
            feature::DESKTOP_CLICK_MCP,
            feature::DESKTOP_INPUT_ANY
        ]
    );
    assert_eq!(
        input_features(Tool::WindowList, any),
        [feature::DESKTOP_INPUT_MCP, feature::DESKTOP_INPUT_ANY]
    );
}

#[test]
fn the_tool_of_an_operation_row_round_trips_and_the_new_tools_have_their_class() {
    for tool in [
        OpTool::WallShow,
        OpTool::Notify,
        OpTool::PreviewOpen,
        OpTool::WindowClose,
        OpTool::Screenshot,
        OpTool::RecordMonitor,
        OpTool::KeyPress,
        OpTool::TypeText,
        OpTool::PointerMove,
        OpTool::PointerClick,
    ] {
        assert_eq!(Tool::of_op(tool).op_tool(), Some(tool));
    }
    assert_eq!(Tool::PointerClick.class(), Some(OpClass::Input));
    assert_eq!(Tool::WindowList.class(), Some(OpClass::Input));
    assert_eq!(Tool::WindowList.op_tool(), None);
}
