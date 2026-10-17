//! The timeline of a wall display over loopback HTTPS (R13 P2b, §5D items 7,
//! 16 and 17, R9): the display event projection on every send path, the
//! subscription rules, the effective scope and the people features. Sentinel
//! tests: an owner event with a sentinel in every forbidden field never
//! reaches a display frame.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod displays_support;
mod share_support;

use std::collections::BTreeSet;
use std::time::Duration;

use common::board::{BoardClient, Ws};
use displays_support::*;
use fleetdeck_core::identity::Identity;
use fleetdeck_deck::store::displays::DisplayChange;
use fleetdeck_deck::store::stations::NewStation;
use fleetdeck_proto::event::{
    ApprovalOption, Event, EventBody, ExpiredReason, ProviderRefs, RawRef, RequestKind, ResolvedBy,
    ResponseChannel, RunOrigin, ToolKind, ToolStatus, TurnStatus,
};
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::session::RepoInfo;
use fleetdeck_proto::{EventEnvelope, SessionSummary};
use serde_json::{Value, json};
use share_support::*;

fn env(seq: u64, session: &str, event: Event) -> EventEnvelope {
    EventEnvelope {
        station_seq: seq,
        session_id: session.into(),
        session_seq: seq,
        at: 1_000 + seq as i64,
        proto_version: 1,
        // A station path: a display never gets it.
        raw_ref: Some(RawRef {
            file: format!("/home/{SECRET}/log.jsonl"),
            offset: 7,
        }),
        event: EventBody::Known(event),
    }
}

fn refs_with_secret() -> ProviderRefs {
    let mut m = ProviderRefs::new();
    m.insert("ref".to_owned(), json!(SECRET));
    m
}

fn run_started(seq: u64, origin: RunOrigin, summary: &str) -> EventEnvelope {
    env(
        seq,
        "s1",
        Event::RunStarted {
            run_id: format!("r{seq}").into(),
            input_summary: summary.to_owned(),
            origin,
            author: Some(format!("member:{SECRET}")),
            input_id: Some(format!("in-{SECRET}").into()),
            input_text: Some(format!("full prompt {SECRET}")),
        },
    )
}

fn message(seq: u64, session: &str, text: &str) -> EventEnvelope {
    env(
        seq,
        session,
        Event::MessageCompleted {
            turn_id: "t1".into(),
            parent_node_id: None,
            text: text.to_owned(),
        },
    )
}

fn tool_started(seq: u64, node: &str, title: &str) -> EventEnvelope {
    env(
        seq,
        "s1",
        Event::ToolStarted {
            node_id: node.into(),
            parent_node_id: None,
            tool_kind: ToolKind::Shell,
            title: title.to_owned(),
            input_summary: format!("curl {SECRET}"),
            files: vec![format!("/{SECRET}/f")],
            provider_refs: refs_with_secret(),
        },
    )
}

fn tool_completed(seq: u64, node: &str, status: ToolStatus) -> EventEnvelope {
    env(
        seq,
        "s1",
        Event::ToolCompleted {
            node_id: node.into(),
            status,
            files: vec![format!("/{SECRET}/f")],
            exit_code: Some(3),
            provider_refs: refs_with_secret(),
        },
    )
}

/// Every owner event that carries a secret, and the kept and dropped ones.
fn owner_events() -> Vec<EventEnvelope> {
    vec![
        run_started(
            1,
            RunOrigin::Board,
            &format!("  Fix the build\nline two {SECRET}"),
        ),
        run_started(2, RunOrigin::Shell, &format!("rm -rf {SECRET}")),
        run_started(3, RunOrigin::Mail, &format!("mail {SECRET}")),
        run_started(4, RunOrigin::Coordinator, &format!("coord {SECRET}")),
        env(
            5,
            "s1",
            Event::TurnStarted {
                run_id: "r1".into(),
                turn_id: "t1".into(),
                provider_refs: refs_with_secret(),
            },
        ),
        tool_started(6, "n1", "Bash"),
        env(
            7,
            "s1",
            Event::ToolOutput {
                node_id: "n1".into(),
                chunk: format!("out {SECRET}"),
                truncated: false,
            },
        ),
        tool_completed(8, "n1", ToolStatus::Ok),
        env(
            9,
            "s1",
            Event::FileChanged {
                path: format!("/{SECRET}/x"),
                change: fleetdeck_proto::event::FileChange::Modified,
                node_id: None,
            },
        ),
        env(
            10,
            "s1",
            Event::ApprovalRequested {
                request_id: "q1".into(),
                node_id: Some("n1".into()),
                request_kind: RequestKind::Command,
                title: format!("Run {SECRET}"),
                details: format!("details {SECRET}"),
                input: Some(json!({ "command": SECRET })),
                options: vec![ApprovalOption {
                    option_id: format!("opt-{SECRET}"),
                    label: format!("Allow {SECRET}"),
                    kind: None,
                }],
                response: ResponseChannel::RpcReply,
                provider_refs: refs_with_secret(),
            },
        ),
        env(
            11,
            "s1",
            Event::ApprovalResolved {
                request_id: "q1".into(),
                option_id: format!("opt-{SECRET}"),
                by: ResolvedBy::Board,
                answered_by: Some(format!("member:{SECRET}")),
            },
        ),
        env(
            12,
            "s1",
            Event::PlanProposed {
                plan_id: "p1".into(),
                turn_id: None,
                markdown: format!("plan {SECRET}"),
            },
        ),
        env(
            13,
            "s1",
            Event::SessionNotice {
                code: "x".to_owned(),
                text: format!("notice {SECRET}"),
            },
        ),
        EventEnvelope {
            station_seq: 14,
            session_id: "s1".into(),
            session_seq: 14,
            at: 1_014,
            proto_version: 1,
            raw_ref: None,
            event: EventBody::Unknown {
                kind: "future.thing".to_owned(),
                payload: serde_json::from_value(json!({
                    "kind": "future.thing",
                    "text": SECRET,
                }))
                .unwrap(),
            },
        },
        message(15, "s1", "All done"),
        env(
            16,
            "s1",
            Event::TurnCompleted {
                turn_id: "t1".into(),
                status: TurnStatus::Done,
                usage: None,
            },
        ),
        env(
            17,
            "s1",
            Event::RunCompleted {
                run_id: "r1".into(),
                status: TurnStatus::Done,
                background_pending: false,
            },
        ),
        tool_started(18, "n2", &"A".repeat(60)),
        tool_completed(19, "n-unknown", ToolStatus::Failed),
        env(
            20,
            "s1",
            Event::ApprovalExpired {
                request_id: "q-unknown".into(),
                reason: ExpiredReason::TurnEnded,
            },
        ),
    ]
}

fn summary(id: &str, root: &str) -> SessionSummary {
    let mut s = card(id);
    s.repo = Some(RepoInfo {
        root: root.to_owned(),
        origin: Some(format!("github.com/{SECRET}")),
    });
    s
}

async fn display_ws(c: &BoardClient, cookie: &str) -> Ws {
    let t = c
        .post("/api/ws/ticket", &[("Cookie", cookie)], json!({}))
        .await;
    let ticket = t.body["ticket"].as_str().unwrap().to_owned();
    let mut ws = c
        .ws(&format!("/api/ws?ticket={ticket}"), &[])
        .await
        .expect("the display socket opens");
    assert_eq!(ws.next().await.unwrap()["type"], "hello");
    assert_eq!(ws.next().await.unwrap()["type"], "snapshot");
    ws
}

/// No timeline frame within `wait`; a board delta in between is fine.
async fn no_timeline_frame(ws: &mut Ws, wait: Duration) {
    let end = tokio::time::Instant::now() + wait;
    while let Ok(m) = tokio::time::timeout_at(end, ws.next()).await {
        let m = m.expect("an open socket");
        assert!(
            m["type"] != "display_timeline_events" && m["type"] != "timeline_events",
            "unexpected frame: {m}"
        );
    }
}

fn subscribe(station: &str, session: &str) -> Value {
    json!({ "type": "subscribe", "station_id": station, "session_id": session,
            "incarnation": null, "after": null })
}

/// The items of a `display_timeline_events` frame by `station_seq`.
fn items(frame: &Value) -> Vec<(u64, Value)> {
    frame["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["station_seq"].as_u64().unwrap(), e["item"].clone()))
        .collect()
}

fn item_at(list: &[(u64, Value)], seq: u64) -> &Value {
    &list.iter().find(|(s, _)| *s == seq).unwrap().1
}

#[tokio::test]
async fn a_display_gets_only_display_events_on_replay_and_live_and_never_a_sentinel() {
    let (f, c, _h, st) = setup_deck().await;
    f.store
        .apply_snapshot(f.station.clone(), f.inc.clone(), 0, vec![card("s1")])
        .await
        .unwrap();
    f.store
        .apply_events(f.station.clone(), f.inc.clone(), owner_events())
        .await
        .unwrap();
    let (_, cookie) = display_login(&c, "Wall", &[&st]).await;
    let mut ws = display_ws(&c, &cookie).await;
    ws.send(subscribe(&st, "s1")).await;
    let start = ws.next_of("timeline_start").await;
    assert_eq!(start["reset"], true);
    // The replay is one display frame; no owner `timeline_events`.
    let frame = ws.next().await.unwrap();
    assert_eq!(frame["type"], "display_timeline_events", "{frame}");
    let text = frame.to_string();
    assert!(!text.contains(SECRET), "{text}");
    assert!(
        !text.contains("raw_ref") && !text.contains("/log.jsonl"),
        "{text}"
    );
    // The highest examined seq, also past the dropped events.
    assert_eq!(frame["high_water"], 20);
    let got = items(&frame);
    let kept: Vec<u64> = got.iter().map(|(s, _)| *s).collect();
    assert_eq!(kept, vec![1, 2, 3, 4, 5, 6, 8, 10, 11, 15, 16, 17, 18, 19]);
    // The prompt: the first line for Board; the fixed text for the others.
    assert_eq!(item_at(&got, 1)["summary"], "Fix the build");
    for seq in [2, 3, 4] {
        assert_eq!(item_at(&got, seq)["summary"], "Run started");
    }
    assert_eq!(item_at(&got, 5)["item"], "turn_started");
    // Tools: the safe name, the status from the completion.
    assert_eq!(item_at(&got, 6)["name"], "Bash");
    assert!(item_at(&got, 6)["status"].is_null());
    assert_eq!(item_at(&got, 8)["name"], "Bash");
    assert_eq!(item_at(&got, 8)["status"], "ok");
    assert_eq!(
        item_at(&got, 18)["name"].as_str().unwrap().chars().count(),
        40
    );
    assert_eq!(item_at(&got, 19)["name"], "Tool");
    assert_eq!(item_at(&got, 19)["status"], "failed");
    // A request: its kind and the correlated tool name, no title or details.
    assert_eq!(
        item_at(&got, 10),
        &json!({ "item": "request", "request_id": "q1", "kind": "command",
                 "tool_name": "Bash", "state": "opened" })
    );
    assert_eq!(item_at(&got, 11)["state"], "resolved");
    assert_eq!(item_at(&got, 11)["tool_name"], "Bash");
    // An answer to an unknown request is dropped (seq 20).
    assert!(!kept.contains(&20));
    assert_eq!(item_at(&got, 15)["text"], "All done");

    // Live: a batch that only holds dropped events sends no frame ...
    f.store
        .apply_events(
            f.station.clone(),
            f.inc.clone(),
            vec![
                env(
                    21,
                    "s1",
                    Event::MessageDelta {
                        turn_id: "t1".into(),
                        parent_node_id: None,
                        text: format!("stream {SECRET}"),
                    },
                ),
                env(
                    22,
                    "s1",
                    Event::ReasoningDelta {
                        turn_id: "t1".into(),
                        text: format!("think {SECRET}"),
                    },
                ),
            ],
        )
        .await
        .unwrap();
    no_timeline_frame(&mut ws, Duration::from_millis(500)).await;
    // ... and the next kept event carries the high-water mark past them.
    f.store
        .apply_events(
            f.station.clone(),
            f.inc.clone(),
            vec![message(23, "s1", "Live message")],
        )
        .await
        .unwrap();
    let live = ws.next_of("display_timeline_events").await;
    assert!(!live.to_string().contains(SECRET), "{live}");
    assert_eq!(live["high_water"], 23);
    assert_eq!(items(&live).len(), 1);
    assert_eq!(items(&live)[0].1["text"], "Live message");

    // A client that follows the cursor gets nothing replayed.
    ws.send(
        json!({ "type": "subscribe", "station_id": st, "session_id": "s1",
                    "incarnation": start["incarnation"], "after": 23 }),
    )
    .await;
    assert_eq!(ws.next_of("timeline_start").await["reset"], false);
    no_timeline_frame(&mut ws, Duration::from_millis(400)).await;
    // A replay from the start of a stored timeline of only dropped events
    // still sends a frame with the high-water mark.
    ws.send(
        json!({ "type": "subscribe", "station_id": st, "session_id": "s1",
                    "incarnation": start["incarnation"], "after": 19 }),
    )
    .await;
    ws.next_of("timeline_start").await;
    let replay = ws.next_of("display_timeline_events").await;
    assert_eq!(replay["high_water"], 23);
    assert!(!replay.to_string().contains(SECRET));
}

#[tokio::test]
async fn a_display_gets_no_people_frame_and_cannot_see_a_session_outside_its_scope() {
    let (f, c, h, st) = setup_deck().await;
    f.store
        .apply_snapshot(f.station.clone(), f.inc.clone(), 0, vec![card("s1")])
        .await
        .unwrap();
    let (_, cookie) = display_login(&c, "Wall", &[&st]).await;
    let mut ws = display_ws(&c, &cookie).await;
    ws.send(subscribe(&st, "s1")).await;
    ws.next_of("timeline_start").await;
    // A session that does not exist and a station outside the ceiling look
    // the same.
    ws.send(subscribe(&st, "nope")).await;
    assert_eq!(ws.next_of("error").await["code"], "unknown_session");
    ws.send(subscribe("not-a-station", "s1")).await;
    assert_eq!(ws.next_of("error").await["code"], "unknown_session");
    // Presence stays refused.
    ws.send(json!({ "type": "presence", "station_id": st, "session_id": "s1" }))
        .await;
    assert_eq!(ws.next_of("error").await["code"], "display_read_only");
    // People chat on a subscribed session never reaches the display.
    let r = post(
        &c,
        &h,
        &format!("/api/sessions/{st}/s1/chat"),
        json!({ "text": "hi" }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    ws.quiet(Duration::from_millis(600)).await;
}

#[tokio::test]
async fn a_display_holds_at_most_32_subscriptions() {
    let (f, c, _h, st) = setup_deck().await;
    let cards: Vec<_> = (0..34).map(|i| card(&format!("c{i}"))).collect();
    f.store
        .apply_snapshot(f.station.clone(), f.inc.clone(), 0, cards)
        .await
        .unwrap();
    let (_, cookie) = display_login(&c, "Wall", &[&st]).await;
    let mut ws = display_ws(&c, &cookie).await;
    for i in 0..32 {
        ws.send(subscribe(&st, &format!("c{i}"))).await;
        ws.next_of("timeline_start").await;
    }
    ws.send(subscribe(&st, "c32")).await;
    assert_eq!(ws.next_of("error").await["code"], "too_many_subscriptions");
    // An unsubscribe frees a place.
    ws.send(json!({ "type": "unsubscribe", "station_id": st, "session_id": "c0" }))
        .await;
    ws.send(subscribe(&st, "c32")).await;
    ws.next_of("timeline_start").await;
}

#[tokio::test]
async fn the_directive_filter_narrows_cards_and_subscriptions_and_a_ceiling_change_ends_them() {
    let (f, c, h, st) = setup_deck().await;
    f.store
        .apply_snapshot(
            f.station.clone(),
            f.inc.clone(),
            0,
            vec![summary("s1", "/srv/a/alpha"), summary("s2", "/srv/b/beta")],
        )
        .await
        .unwrap();
    let (display, cookie) = display_login(&c, "Wall", &[&st]).await;
    let mut ws = display_ws(&c, &cookie).await;
    ws.send(subscribe(&st, "s1")).await;
    ws.next_of("timeline_start").await;
    ws.send(subscribe(&st, "s2")).await;
    ws.next_of("timeline_start").await;

    // The owner narrows the display to the repository `alpha`.
    let r = post(
        &c,
        &h,
        &format!("/api/displays/{display}/directive"),
        json!({ "mode": "mission", "filter": { "stations": [st], "repo": "alpha" } }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    // No board change happens, and the socket still takes the new scope: the
    // card of `beta` goes.
    let d = ws.next_of("delta").await;
    assert_eq!(d["removed"].as_array().unwrap().len(), 1, "{d}");
    assert_eq!(d["removed"][0]["session_id"], "s2");
    let r = get_with(&c, "/api/sessions", &cookie).await;
    let cards = r.body.as_array().unwrap();
    assert_eq!(cards.len(), 1, "{}", r.body);
    assert_eq!(cards[0]["summary"]["session_id"], "s1");
    // The subscription of `beta` ended: a live event reaches no one; the
    // `alpha` one still reaches the display.
    f.store
        .apply_events(
            f.station.clone(),
            f.inc.clone(),
            vec![
                message(1, "s2", "beta text"),
                message(2, "s1", "alpha text"),
            ],
        )
        .await
        .unwrap();
    let live = ws.next_of("display_timeline_events").await;
    assert_eq!(live["session_id"], "s1");
    assert!(!live.to_string().contains("beta text"));
    // A new subscription to `beta` is refused.
    ws.send(subscribe(&st, "s2")).await;
    assert_eq!(ws.next_of("error").await["code"], "unknown_session");

    // The ceiling moves to another station: the card and the subscription go
    // at once, and a live event of `alpha` reaches no one.
    let other = c
        .store
        .pin_station(NewStation {
            key: Identity::generate().unwrap().public_key(),
            name: "other".to_owned(),
            scopes: BTreeSet::from([Scope::Read]),
            dial_url: None,
        })
        .await
        .unwrap();
    c.store
        .display_update(
            display.clone(),
            DisplayChange {
                ceiling: Some(vec![other.station_id.clone()]),
                ..DisplayChange::default()
            },
        )
        .await
        .unwrap();
    let d = ws.next_of("delta").await;
    assert_eq!(d["removed"][0]["session_id"], "s1", "{d}");
    f.store
        .apply_events(
            f.station.clone(),
            f.inc.clone(),
            vec![message(3, "s1", "alpha again")],
        )
        .await
        .unwrap();
    ws.quiet(Duration::from_millis(600)).await;
}

#[tokio::test]
async fn a_revoked_display_stops_receiving_timeline_frames() {
    let (f, c, h, st) = setup_deck().await;
    f.store
        .apply_snapshot(f.station.clone(), f.inc.clone(), 0, vec![card("s1")])
        .await
        .unwrap();
    let (display, cookie) = display_login(&c, "Wall", &[&st]).await;
    let mut ws = display_ws(&c, &cookie).await;
    ws.send(subscribe(&st, "s1")).await;
    ws.next_of("timeline_start").await;
    let r = post(
        &c,
        &h,
        &format!("/api/displays/{display}/revoke"),
        json!({}),
    )
    .await;
    assert_eq!(r.status, 204, "{}", r.body);
    f.store
        .apply_events(
            f.station.clone(),
            f.inc.clone(),
            vec![message(1, "s1", "after the revoke")],
        )
        .await
        .unwrap();
    loop {
        match ws.next().await {
            Ok(m) => assert_ne!(m["type"], "display_timeline_events", "{m}"),
            Err(code) => {
                assert_eq!(code, Some(4401));
                break;
            }
        }
    }
}
