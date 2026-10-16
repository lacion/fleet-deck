//! The pin of `session.import` (B7, design `native-sessions.md` §2.4).

use fleetdeck_proto::command::SessionCommand;
use serde_json::json;

fn base() -> serde_json::Value {
    json!({
        "op": "import",
        "spec": {"provider_instance": "claude", "cwd": "/w", "mode": null, "model": null, "title": null},
        "native_session_id": "abc",
        "prompt": null,
    })
}

#[test]
fn an_import_without_a_pin_has_none_and_writes_none() {
    let plain: SessionCommand = serde_json::from_value(base()).unwrap();
    assert!(matches!(
        plain,
        SessionCommand::Import {
            transcript: None,
            ..
        }
    ));
    assert!(
        serde_json::to_value(&plain)
            .unwrap()
            .get("transcript")
            .is_none()
    );
}

#[test]
fn an_import_pin_round_trips() {
    let mut with = base();
    with["transcript"] = json!({"home": "/h/.claude", "dev": 7, "ino": 9});
    let pinned: SessionCommand = serde_json::from_value(with.clone()).unwrap();
    let SessionCommand::Import {
        transcript: Some(pin),
        ..
    } = &pinned
    else {
        panic!("no pin: {pinned:?}");
    };
    assert_eq!((pin.home.as_str(), pin.dev, pin.ino), ("/h/.claude", 7, 9));
    assert_eq!(serde_json::to_value(&pinned).unwrap(), with);
}
