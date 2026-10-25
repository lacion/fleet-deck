//! The pointer checks and the slot, the tool and the notification of the input
//! protocol (R13 P7 §5.3, §7.3, §8.1, §8.2, §13).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::Ordering;

use serde_json::json;

use super::rig::{Fx, Probe, calls, nothing, other_client, pair, refused};

// -------------------------------------------------- the pointer checks

#[tokio::test]
async fn a_point_outside_the_preview_is_unavailable_with_no_clamping() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    for (x, y) in [(800, 0), (0, 600), (800, 600), (32_767, 32_767)] {
        nothing(
            &fx,
            &probe,
            &fx.pointer(x, y),
            "input_destination_unavailable",
        )
        .await;
    }
    let e = refused(&fx, &probe, &fx.pointer(800, 10)).await;
    assert_eq!(
        e.message,
        "The pointer destination is not available inside this preview."
    );
}

#[tokio::test]
async fn a_point_over_another_window_a_layer_or_a_hidden_preview_is_unavailable() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    // Another mapped window over the point.
    let mut over = other_client();
    over["at"] = json!([150, 150]);
    over["size"] = json!([100, 100]);
    fx.set_clients(&[fx.preview_client(), over]);
    nothing(
        &fx,
        &probe,
        &fx.pointer(60, 60),
        "input_destination_unavailable",
    )
    .await;
    // A layer surface over the point.
    fx.set_clients(&[fx.preview_client()]);
    fx.write(
        "layers.json",
        &json!({"DP-1": {"levels": {"2": [{"x": 150, "y": 150, "w": 10, "h": 10}]}}}),
    );
    nothing(
        &fx,
        &probe,
        &fx.pointer(55, 55),
        "input_destination_unavailable",
    )
    .await;
    // A layer list that is malformed fails closed.
    fx.write_text("layers.json", "{\"DP-1\": 5}");
    nothing(
        &fx,
        &probe,
        &fx.pointer(1, 1),
        "input_destination_unavailable",
    )
    .await;
    fx.write_text("layers.json", "garbage");
    nothing(
        &fx,
        &probe,
        &fx.pointer(1, 1),
        "input_destination_unavailable",
    )
    .await;
    // A client list that is malformed fails closed for a pointer too.
    fx.write("layers.json", &json!({}));
    fx.write_text("clients.json", "[{\"address\":\"0x1\"}]");
    nothing(
        &fx,
        &probe,
        &fx.pointer(1, 1),
        "input_destination_unavailable",
    )
    .await;
    // A hidden preview.
    let mut hidden = fx.preview_client();
    hidden["hidden"] = json!(true);
    fx.set_clients(&[hidden]);
    nothing(
        &fx,
        &probe,
        &fx.pointer(1, 1),
        "input_destination_unavailable",
    )
    .await;
}

#[tokio::test]
async fn the_destination_is_checked_again_after_the_focus() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    let mut over = other_client();
    over["at"] = json!([150, 150]);
    over["size"] = json!([100, 100]);
    fx.write("clients_after.json", &json!([fx.preview_client(), over]));
    fx.write_text(
        "on_focus.sh",
        &format!(
            "/bin/cp {0}/clients_after.json {0}/clients.json",
            fx.root().display()
        ),
    );
    let e = refused(&fx, &probe, &fx.pointer(60, 60)).await;
    assert_eq!(e.code, "input_destination_unavailable");
    // The disclosure and the focus happened; the pointer did not move.
    assert!(fx.timeline().contains(&fx.focus_line()));
    assert!(!fx.timeline().iter().any(|l| l.contains("movecursor")));
}

// ------------------------------------------------- slot, tool, notify

#[tokio::test]
async fn a_held_slot_refuses_at_once_and_a_finished_call_frees_it() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    let guard = fx.desktop().input.try_take().unwrap();
    let started = std::time::Instant::now();
    let e = refused(&fx, &probe, &fx.key("a")).await;
    assert_eq!(e.code, "busy");
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    assert_eq!(calls(&fx), 0);
    assert_eq!(probe.claim.calls.load(Ordering::SeqCst), 0);
    drop(guard);
    fx.run(&fx.key("a"), &probe).await.unwrap();
    assert!(!fx.desktop().input.is_held());
    // A failing call frees the slot too.
    fx.flag("wtype_fail", true);
    assert!(fx.run(&fx.key("a"), &probe).await.is_err());
    assert!(!fx.desktop().input.is_held());
}

#[tokio::test]
async fn a_missing_wtype_refuses_keys_and_text_but_not_the_pointer() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    std::fs::remove_file(fx.root().join("bin/wtype")).unwrap();
    for action in [fx.key("a"), fx.text("x")] {
        let e = refused(&fx, &probe, &action).await;
        assert_eq!(
            pair(&e),
            (
                "input_tool_missing",
                "The keyboard input tool wtype is not installed on the target station.".into()
            )
        );
        assert_eq!(calls(&fx), 0);
        assert_eq!(probe.claim.calls.load(Ordering::SeqCst), 0);
    }
    fx.run(&fx.pointer(1, 1), &probe).await.unwrap();
}

#[tokio::test]
async fn a_notification_failure_prevents_the_focus_and_the_injection() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    fx.rig.fail("notify-send");
    let e = refused(&fx, &probe, &fx.key("a")).await;
    assert_eq!(e.code, "failed");
    assert_eq!(fx.timeline(), ["claim", "check", "notify-send"]);
    assert!(fx.calls_of("wtype").is_empty());
}

#[tokio::test]
async fn a_failed_focus_dispatch_is_not_retried_and_nothing_is_injected() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    fx.flag("focus_fail", true);
    let e = refused(&fx, &probe, &fx.key("a")).await;
    assert_eq!(e.code, "input_focus_unknown");
    let focuses = fx
        .timeline()
        .iter()
        .filter(|l| **l == fx.focus_line())
        .count();
    assert_eq!(focuses, 1);
    assert!(fx.calls_of("wtype").is_empty());
}
