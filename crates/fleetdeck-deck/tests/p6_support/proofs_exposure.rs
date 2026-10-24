//! The exposure proofs of the P6 end-to-end test (proofs 23 to 26 and 30,
//! and the byte-serving test of O12-12): sentinel data, owner captures and
//! same-ID artifacts, and the bytes that a wall serves.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::json;

use super::board::raw_get;
use super::board::revoke;
use super::fake_desktop::{MONITOR_SENTINEL, WINDOW_SENTINEL};
use super::mcp::png_of;
use super::*;

/// The committed screenshot of DP-1 of B.
async fn shot(w: &World, id: &str) -> serde_json::Value {
    let args = json!({"request_id": id, "station": w.to_b(), "monitor": "DP-1"});
    w.mcp.call("screenshot", args).await.ok()
}

// ------------------------------------------------------------------ proof 23

/// Proofs 23, 25 and 26: sentinel texts in the raw desktop data of B (a
/// monitor description, a window title), in the sessions of an unrelated
/// station, in the notification and the URL and in the native identity of
/// the binding. None of them is in any P6 result, any audit row or any log
/// line of the process.
#[tokio::test]
async fn sentinel_data_never_reaches_a_result_an_audit_row_or_a_log_line() {
    const TITLE: &str = "SENTINEL-TITLE-77aa";
    const BODY: &str = "SENTINEL-BODY-31bc";
    const PATH: &str = "SENTINEL-URL-52de";
    const OTHER: &str = "SENTINEL-OTHER-STATION-9f10";
    let w = World::start().await;
    let gamma = w
        .deck()
        .store
        .pin_station(fleetdeck_deck::store::stations::NewStation {
            key: fleetdeck_core::identity::Identity::generate()
                .unwrap()
                .public_key(),
            name: "gamma".to_owned(),
            scopes: std::collections::BTreeSet::from([fleetdeck_proto::link::Scope::Admin]),
            dial_url: None,
        })
        .await
        .unwrap()
        .station_id;
    let mut card = crate::common::fleet::card("g-1");
    card.title = Some(OTHER.to_owned());
    w.deck()
        .store
        .apply_snapshot(gamma.clone(), "gamma-db".into(), 0, vec![card])
        .await
        .unwrap();

    let b = w.to_b();
    let mut seen = Vec::new();
    let mut take = |t: mcp::Tool| {
        seen.push(t.raw.clone());
        t
    };
    let info = take(w.mcp.call("desktop_info", json!({"station": b})).await).ok();
    assert_eq!(info["monitors"].as_array().unwrap().len(), 2, "{info}");
    let sent = json!({"request_id": "n1", "station": b, "title": TITLE, "body": BODY});
    take(w.mcp.call("notify", sent).await).ok();
    let url = format!("http://localhost:3000/{PATH}");
    let open = take(
        w.mcp
            .call(
                "preview_open",
                json!({"request_id": "p1", "station": b, "url": url}),
            )
            .await,
    )
    .ok();
    let handle = open["window_id"].as_str().unwrap().to_owned();
    // The fake desktop announced the window with the sentinel title.
    assert!(w.b.rig.logs().is_empty() || !w.b.rig.logs().contains(OTHER));
    take(
        w.mcp
            .call(
                "window_close",
                json!({"request_id": "c1", "station": b, "window_id": handle}),
            )
            .await,
    )
    .ok();
    take(
        w.mcp
            .call("desktop_action_status", json!({"request_id": "n1"}))
            .await,
    )
    .ok();
    // The sessions of an unrelated station: not a grant station.
    let denied = take(
        w.mcp
            .call("desktop_info", json!({"station": gamma.as_str()}))
            .await,
    );
    assert_eq!(denied.code(), "not_granted");

    let everything = format!(
        "{}\n{}\n{}",
        seen.join("\n"),
        w.audit_text().await,
        mcp::logged()
    );
    for sentinel in [
        TITLE,
        BODY,
        PATH,
        OTHER,
        MONITOR_SENTINEL,
        WINDOW_SENTINEL,
        NATIVE_SENTINEL,
    ] {
        assert!(!everything.contains(sentinel), "{sentinel} leaked");
    }
}

// ------------------------------------------------------------ proofs 24, 30

/// Proofs 23, 24, 25 and 30: an owner capture of B and an artifact with the
/// same ID on another station stay apart from the capture of the coordinator.
/// The image read needs the station ID, the coordinator provenance and the
/// grant; a guess of any other ID answers `unknown_artifact`. The sentinel
/// bytes of the owner capture reach no result, audit row or log line.
#[tokio::test]
async fn an_owner_capture_and_a_same_id_artifact_are_not_readable_as_a_coordinator_capture() {
    use super::board::{enable_desktop, owner_capture};
    use fleetdeck_proto::artifacts::{Artifact, ArtifactKind};

    const OWNER: &str = "SENTINEL-OWNER-CAPTURE-6b2d";
    let w = World::start().await;
    w.b.grim(&png_of(3_000, b"coordinator pixels "));
    let mine = shot(&w, "s1").await;
    let id = mine["artifact"]["artifact_id"].as_str().unwrap().to_owned();
    let mut seen = Vec::new();

    // The same artifact ID on station A (an ordinary image of the caller).
    let twin = Artifact {
        artifact_id: id.clone(),
        station_id: String::new(),
        session_id: Some(CALLER.into()),
        title: "twin".to_owned(),
        kind: ArtifactKind::Image,
        latest_version: 1,
        created_at: 1,
        updated_at: 1,
        author: "agent".to_owned(),
    };
    w.deck()
        .store
        .note_artifact(w.a_id.clone(), &twin)
        .await
        .unwrap();
    let read = |station: &StationIdText, artifact: &str| json!({"artifact": {"station_id": station.0, "artifact_id": artifact, "version": 1}});
    let on_b = w
        .mcp
        .call(
            "read_capture_image",
            read(&StationIdText(w.b_id.as_str()), &id),
        )
        .await;
    assert!(
        !on_b.is_error,
        "{}",
        on_b.raw.chars().take(300).collect::<String>()
    );
    let on_a = w
        .mcp
        .call(
            "read_capture_image",
            read(&StationIdText(w.a_id.as_str()), &id),
        )
        .await;
    assert_eq!(on_a.code(), "unknown_artifact");
    seen.push(on_a.raw);

    // An owner capture of the same monitor: not readable by the coordinator.
    let (c, h) = w.deck().board().await;
    enable_desktop(&c);
    w.b.grim(&png_of(3_000, OWNER.as_bytes()));
    let owned = owner_capture(&c, &h, "beta", "DP-1").await;
    assert_eq!(owned.status, 200, "{}", owned.body);
    let owner_id = owned.body["capture"]["published"]["artifact"]["artifact_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(owner_id, id);
    let theirs = w
        .mcp
        .call(
            "read_capture_image",
            read(&StationIdText(w.b_id.as_str()), &owner_id),
        )
        .await;
    assert_eq!(theirs.code(), "unknown_artifact");
    assert!(theirs.content.iter().all(|b| b["type"] == "text"));
    seen.push(theirs.raw);

    // Guesses: an artifact ID, a request ID and a capture status.
    let guess = w
        .mcp
        .call(
            "read_capture_image",
            read(
                &StationIdText(w.b_id.as_str()),
                "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            ),
        )
        .await;
    assert_eq!(guess.code(), "unknown_artifact");
    let status = w
        .mcp
        .call("capture_status", json!({"request_id": "never-sent"}))
        .await
        .ok();
    assert_eq!(status["state"], "unknown");
    seen.push(status.to_string());
    let everything = format!(
        "{}\n{}\n{}",
        seen.join("\n"),
        w.audit_text().await,
        mcp::logged()
    );
    assert!(!everything.contains(OWNER));
    assert!(
        !everything.contains(&owner_id),
        "the owner artifact ID leaked"
    );
}

struct StationIdText<'a>(&'a str);

// -------------------------------------------------------------- O12-12 (T5)

/// The open item of O12-12: a valid coordinator directive that shows an
/// artifact of the caller on a wall serves the bytes of that artifact to the
/// display, after both live policy checks (the caller station A and the
/// target station B, over the real links).
#[tokio::test]
async fn a_valid_coordinator_artifact_directive_serves_its_bytes_to_the_display() {
    use fleetdeck_deck::store::displays::NewDisplay;
    use fleetdeck_proto::artifacts::ArtifactKind;
    use fleetdeck_proto::display::DisplayBinding;
    use fleetdeck_station::artifacts::service::ContentPublish;

    const PAGE: &str = "<p>a page of the coordinator</p>";
    let w = World::start().await;
    // A real artifact of the caller session on A (its bytes are on A).
    let published =
        w.a.manager
            .artifacts()
            .expect("the artifact service of A")
            .publish_content(
                ContentPublish {
                    session_id: Some(CALLER.into()),
                    name: "page.html".to_owned(),
                    content: PAGE.to_owned(),
                    title: Some("Report".to_owned()),
                    artifact_id: None,
                    author: Some("agent".to_owned()),
                },
                None,
            )
            .await
            .unwrap();
    assert_eq!(published.artifact.kind, ArtifactKind::Html);
    let id = published.artifact.artifact_id.clone();
    // The deck indexes it, as it does from the station event.
    w.deck()
        .store
        .note_artifact(w.a_id.clone(), &published.artifact)
        .await
        .unwrap();
    // An owner-paired wall of B; its ceiling holds both stations.
    let store = &w.deck().store;
    let display = store
        .create_display(NewDisplay {
            name: "Wall".to_owned(),
            ceiling: vec![w.a_id.clone(), w.b_id.clone()],
        })
        .await
        .unwrap()
        .display_id;
    store
        .display_set_binding(
            display.clone(),
            Some(DisplayBinding {
                station_id: w.b_id.clone(),
                monitor: "DP-1".to_owned(),
            }),
        )
        .await
        .unwrap();
    let show = json!({"request_id": "w1", "station": w.to_b(), "display_id": display,
        "content": {"kind": "artifact", "artifact": {
            "station_id": w.a_id.as_str(), "artifact_id": id, "version": 1}}});
    let shown = w.mcp.call("wall_show", show).await.ok();
    assert_eq!(shown, json!({"generation": 1}));

    // The display logs in, asks for its directive and then for the bytes.
    let (secret, _) = store.create_display_session(display.clone()).await.unwrap();
    let cookie = format!("fd_display={}", secret.expose());
    let (c, owner) = w.deck().board().await;
    let got = c
        .get(
            "/api/displays/self/directive",
            &[("Cookie", cookie.as_str())],
        )
        .await;
    assert_eq!(got.status, 200, "{}", got.body);
    assert_eq!(got.body["directive"]["mode"], "artifact", "{}", got.body);
    let path = format!("/api/displays/self/artifact/{}/{id}/1/_", w.a_id);
    let (status, bytes) = raw_get(&c, &path, &[("Cookie", cookie.as_str())]).await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(bytes, PAGE.as_bytes());

    // After the grant ends, the same route serves nothing.
    revoke(&c, &owner, &w.grant_id).await;
    let (status, _) = raw_get(&c, &path, &[("Cookie", cookie.as_str())]).await;
    assert_eq!(status, 404);
}
