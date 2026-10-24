//! The data channel on real links over loopback (ADR-004 §2.4, §2.7, §9.2):
//! a stream opens only with a token from the control channel; the two
//! channels share one lifetime; dropping the data handles keeps the link.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use common::{T, eventually, link, listen, next_request, paired};
use fleetdeck_link::{DataEvent, DataLink, LiveLink};
use fleetdeck_proto::link::Outcome;
use serde_json::json;

async fn event(d: &mut DataLink) -> DataEvent {
    tokio::time::timeout(T, d.events.recv())
        .await
        .expect("no data event")
        .expect("the data channel ended")
}

fn data(l: &mut LiveLink) -> &mut DataLink {
    l.data
        .as_mut()
        .expect("both sides negotiated data_streams.v1")
}

/// The deck asks for an artifact; the station answers with a stream token.
async fn artifact_token(at_deck: &LiveLink, at_station: &mut LiveLink) -> String {
    let c = at_deck.control.clone();
    let call = tokio::spawn(async move {
        c.request("artifact.open", None, json!({ "path": "log.txt" }))
            .await
    });
    let r = next_request(at_station).await;
    assert_eq!(r.method, "artifact.open");
    let token = data(at_station)
        .handle
        .issue_token("artifact:log.txt")
        .unwrap();
    let answer = json!({ "token": token.as_str() });
    at_station
        .control
        .respond(r.id, Outcome::Ok(answer))
        .await
        .unwrap();
    match call.await.unwrap().unwrap() {
        Outcome::Ok(v) => v["token"].as_str().unwrap().to_owned(),
        Outcome::Err(e) => panic!("artifact.open failed: {e:?}"),
    }
}

#[tokio::test]
async fn an_artifact_stream_opens_with_a_token_from_the_control_channel_and_the_token_works_once() {
    for station_dials in [true, false] {
        let (deck, station) = paired();
        let (mut at_deck, mut at_station) = if station_dials {
            let mut l = listen(&deck).await;
            let (s, d) = link(&station, &deck, &mut l).await;
            (d, s)
        } else {
            let mut l = listen(&station).await;
            link(&deck, &station, &mut l).await
        };
        let token = artifact_token(&at_deck, &mut at_station).await;
        let s = data(&mut at_deck).handle.open(&token).await.unwrap();
        assert_eq!(
            event(data(&mut at_station)).await,
            DataEvent::Opened {
                stream: s,
                purpose: "artifact:log.txt".into()
            }
        );
        let body: Vec<u8> = (0..1 << 20).map(|i: u32| (i % 251) as u8).collect();
        let station_data = data(&mut at_station).handle.clone();
        let sent = body.clone();
        let writer = tokio::spawn(async move {
            station_data.write(s, sent).await?;
            station_data.close(s).await
        });
        let mut got = Vec::new();
        loop {
            match event(data(&mut at_deck)).await {
                DataEvent::Data { stream, bytes } if stream == s => {
                    data(&mut at_deck)
                        .handle
                        .consumed(s, bytes.len())
                        .await
                        .unwrap();
                    got.extend(bytes);
                }
                DataEvent::Closed { stream } if stream == s => break,
                other => panic!("unexpected {other:?}"),
            }
        }
        writer.await.unwrap().unwrap();
        assert!(got == body, "the deck got the artifact unchanged");

        // A used token opens nothing: the station resets the stream, and its
        // application never sees it.
        let again = data(&mut at_deck).handle.open(&token).await.unwrap();
        assert_eq!(
            event(data(&mut at_deck)).await,
            DataEvent::Reset {
                stream: again,
                reason: "bad_stream_token".into()
            }
        );
        assert!(data(&mut at_station).events.try_recv().is_err());
        assert!(at_deck.control.ended().is_none() && at_station.control.ended().is_none());
    }
}

#[tokio::test]
async fn a_closed_control_channel_ends_the_data_channel_with_the_same_reason() {
    let (deck, station) = paired();
    let mut l = listen(&station).await;
    let (mut at_deck, mut at_station) = link(&deck, &station, &mut l).await;
    at_deck.control.close("shutdown", "the deck stops");
    for l in [&mut at_deck, &mut at_station] {
        let c = tokio::time::timeout(T, l.control.closed()).await.unwrap();
        assert_eq!(c.reason, "shutdown");
        let d = tokio::time::timeout(T, data(l).handle.closed())
            .await
            .unwrap();
        assert!(["shutdown", "eof"].contains(&d.reason.as_str()), "{d:?}");
        let e = data(l).handle.open("x").await.unwrap_err();
        assert!(
            e.to_string().contains("shutdown") || e.to_string().contains("eof"),
            "{e}"
        );
    }
    let (dk, st) = (deck.config.table.clone(), station.config.table.clone());
    eventually("both tables are empty", || dk.is_empty() && st.is_empty()).await;
}

#[tokio::test]
async fn a_lost_data_channel_closes_the_link_with_data_channel_lost_on_both_sides() {
    let (deck, station) = paired();
    let mut l = listen(&deck).await;
    let (mut at_station, mut at_deck) = link(&station, &deck, &mut l).await;
    data(&mut at_station)
        .handle
        .fail_for_test("cable cut")
        .await;
    for l in [&at_station, &at_deck] {
        let c = tokio::time::timeout(T, l.control.closed()).await.unwrap();
        assert_eq!(c.reason, "data_channel_lost", "{c:?}");
    }
    assert!(data(&mut at_deck).handle.ended().is_some());
    let (dk, st) = (deck.config.table.clone(), station.config.table.clone());
    eventually("both tables are empty", || dk.is_empty() && st.is_empty()).await;
}

#[tokio::test]
async fn dropping_the_data_handles_keeps_the_link_up() {
    let (deck, station) = paired();
    let mut l = listen(&station).await;
    let (mut at_deck, mut at_station) = link(&deck, &station, &mut l).await;
    drop(at_deck.data.take());
    drop(at_station.data.take());
    tokio::time::sleep(Duration::from_millis(800)).await;
    let c = at_deck.control.clone();
    let call = tokio::spawn(async move { c.request("session.snapshot", None, json!({})).await });
    let r = next_request(&mut at_station).await;
    at_station
        .control
        .respond(r.id, Outcome::Ok(json!([])))
        .await
        .unwrap();
    assert_eq!(call.await.unwrap().unwrap(), Outcome::Ok(json!([])));
    assert!(at_deck.control.ended().is_none() && at_station.control.ended().is_none());
}

#[tokio::test]
async fn a_superseded_link_ends_its_data_channel_and_fails_its_pending_writes() {
    let (deck, station) = paired();
    let mut l = listen(&deck).await;
    let (mut old_s, mut old_d) = link(&station, &deck, &mut l).await;
    let token = artifact_token(&old_d, &mut old_s).await;
    let s = data(&mut old_d).handle.open(&token).await.unwrap();
    assert!(matches!(
        event(data(&mut old_s)).await,
        DataEvent::Opened { .. }
    ));
    // Nobody reads at the deck: the station's write waits for credit.
    let w = {
        let h = data(&mut old_s).handle.clone();
        tokio::spawn(async move { h.write(s, vec![7; 512 * 1024]).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!w.is_finished());
    let (_new_s, _new_d) = link(&station, &deck, &mut l).await;
    let e = tokio::time::timeout(T, w)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(e.to_string().contains("superseded"), "{e}");
    for l in [&mut old_s, &mut old_d] {
        let d = tokio::time::timeout(T, data(l).handle.closed())
            .await
            .unwrap();
        assert!(["superseded", "eof"].contains(&d.reason.as_str()), "{d:?}");
    }
}
