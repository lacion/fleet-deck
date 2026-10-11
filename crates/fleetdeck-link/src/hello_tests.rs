use super::*;

struct Two {
    deck: Identity,
    station: Identity,
}

fn two() -> Two {
    Two {
        deck: Identity::generate().unwrap(),
        station: Identity::generate().unwrap(),
    }
}

fn inc(n: u8) -> IncarnationId {
    IncarnationId::from(format!("01J00000000000000000000IN{n}"))
}

/// The deck dials (epoch 1), the station listens.
fn hellos(t: &Two) -> (LocalHello, Hello, LocalHello, Hello) {
    let dl = LocalHello::new(Role::Deck, inc(1));
    let sl = LocalHello::new(Role::Station, inc(2));
    let attempt = Attempt::new(1).unwrap();
    let dh = dl.dialer_hello(&t.deck, &attempt, new_nonce().unwrap(), Channel::Control);
    let sh = sl.listener_hello(&t.station, new_nonce().unwrap(), Channel::Control);
    (dl, dh, sl, sh)
}

/// The deck's view: the peer (station) is the listener.
fn at_deck(
    dl: &LocalHello,
    dh: &Hello,
    sh: &Hello,
    key: &PeerKey,
    c: Channel,
) -> Result<Negotiated, HandshakeError> {
    negotiate(dl, dh, sh, key, c, Side::Listener)
}

#[test]
fn both_sides_negotiate_the_same_result() {
    let t = two();
    let (dl, dh, sl, sh) = hellos(&t);
    let a = at_deck(&dl, &dh, &sh, &t.station.public_key(), Channel::Control).unwrap();
    let b = negotiate(
        &sl,
        &sh,
        &dh,
        &t.deck.public_key(),
        Channel::Control,
        Side::Dialer,
    )
    .unwrap();
    assert_eq!(a, b);
    assert_eq!(a.version, 1);
    assert!(a.has(feature::DATA_STREAMS));
    assert_eq!(dh.incarnation, inc(1));
    assert_eq!(sh.incarnation, inc(2));
}

#[test]
fn the_highest_common_version_wins() {
    let t = two();
    let (mut dl, mut dh, mut sl, mut sh) = hellos(&t);
    (dl.proto_min, dl.proto_max, dh.proto_min, dh.proto_max) = (1, 3, 1, 3);
    (sl.proto_min, sl.proto_max, sh.proto_min, sh.proto_max) = (2, 5, 2, 5);
    assert_eq!(
        at_deck(&dl, &dh, &sh, &t.station.public_key(), Channel::Control)
            .unwrap()
            .version,
        3
    );
    assert_eq!(
        negotiate(
            &sl,
            &sh,
            &dh,
            &t.deck.public_key(),
            Channel::Control,
            Side::Dialer
        )
        .unwrap()
        .version,
        3
    );
}

#[test]
fn no_common_version_is_a_clear_error() {
    let t = two();
    let (mut dl, dh, _, mut sh) = hellos(&t);
    (dl.proto_min, dl.proto_max) = (2, 3);
    (sh.proto_min, sh.proto_max) = (1, 1);
    let e = at_deck(&dl, &dh, &sh, &t.station.public_key(), Channel::Control).unwrap_err();
    assert_eq!(e.reason(), "version_mismatch");
    assert_eq!(
        e.to_string(),
        "no common link protocol version: this node speaks 2..=3, the peer speaks 1..=1"
    );
    let HandshakeFrame::Close {
        reason,
        detail,
        min_epoch,
    } = e.close_frame()
    else {
        panic!()
    };
    assert_eq!(reason, "version_mismatch");
    assert!(detail.contains("2..=3"));
    assert_eq!(min_epoch, None);
}

#[test]
fn features_are_the_intersection_and_required_ones_are_enforced() {
    let t = two();
    let (mut dl, dh, _, mut sh) = hellos(&t);
    sh.features = vec![feature::CONTROL.into(), "future.v9".into()];
    let n = at_deck(&dl, &dh, &sh, &t.station.public_key(), Channel::Control).unwrap();
    assert_eq!(n.features, vec![feature::CONTROL.to_owned()]);
    dl.required.push(feature::DATA_STREAMS.into());
    let e = at_deck(&dl, &dh, &sh, &t.station.public_key(), Channel::Control).unwrap_err();
    assert_eq!(
        e,
        HandshakeError::MissingFeature(feature::DATA_STREAMS.into())
    );
}

#[test]
fn unknown_hello_fields_are_ignored() {
    let t = two();
    let (_, dh, _, _) = hellos(&t);
    let mut v = serde_json::to_value(HandshakeFrame::Hello(dh.clone())).unwrap();
    v["resume_token"] = "x".into();
    v["features"]
        .as_array_mut()
        .unwrap()
        .push("future.v9".into());
    let parsed: HandshakeFrame = serde_json::from_value(v).unwrap();
    let HandshakeFrame::Hello(h) = parsed else {
        panic!()
    };
    assert_eq!(h.node_id, dh.node_id);
}

#[test]
fn the_peer_identity_role_channel_and_nonce_are_checked() {
    let t = two();
    let (dl, dh, sl, sh) = hellos(&t);
    let sk = t.station.public_key();
    // node_id must be the key that TLS proved
    let other = Identity::generate().unwrap().public_key();
    assert_eq!(
        at_deck(&dl, &dh, &sh, &other, Channel::Control)
            .unwrap_err()
            .reason(),
        "node_id_mismatch"
    );
    // a deck never links to a deck
    let fake = LocalHello::new(Role::Deck, inc(3)).listener_hello(
        &t.station,
        new_nonce().unwrap(),
        Channel::Control,
    );
    assert_eq!(
        at_deck(&dl, &dh, &fake, &sk, Channel::Control)
            .unwrap_err()
            .reason(),
        "role_mismatch"
    );
    // the Hello channel must match the WebSocket path
    assert_eq!(
        at_deck(&dl, &dh, &sh, &sk, Channel::Data)
            .unwrap_err()
            .reason(),
        "channel_mismatch"
    );
    // an echoed nonce is a reflection
    let mut echo = sh.clone();
    echo.nonce = dh.nonce.clone();
    assert_eq!(
        at_deck(&dl, &dh, &echo, &sk, Channel::Control)
            .unwrap_err()
            .reason(),
        "bad_hello"
    );
    let mut bad = sh.clone();
    bad.nonce = "00".into();
    assert!(at_deck(&dl, &dh, &bad, &sk, Channel::Control).is_err());
    let mut inverted = sh;
    (inverted.proto_min, inverted.proto_max) = (3, 1);
    assert!(at_deck(&dl, &dh, &inverted, &sk, Channel::Control).is_err());
    let _ = sl;
}

#[test]
fn signatures_bind_the_tls_session_the_hellos_and_the_side() {
    let t = two();
    let (_, dh, _, sh) = hellos(&t);
    let dtext = serde_json::to_string(&HandshakeFrame::Hello(dh)).unwrap();
    let stext = serde_json::to_string(&HandshakeFrame::Hello(sh)).unwrap();
    let (dk, sk) = (t.station.public_key(), t.deck.public_key());
    let tr = transcript(&[1; 32], &dtext, &stext, &dk, &sk);
    let HandshakeFrame::Auth { signature } = sign(&t.station, &tr, Side::Dialer) else {
        panic!()
    };
    assert!(verify(&dk, &tr, Side::Dialer, &signature).is_ok());
    // another TLS session (exporter) → the signature does not verify
    let other_session = transcript(&[2; 32], &dtext, &stext, &dk, &sk);
    assert!(verify(&dk, &other_session, Side::Dialer, &signature).is_err());
    // a changed Hello → no
    let changed = transcript(
        &[1; 32],
        &dtext.replace("\"link_epoch\":1", "\"link_epoch\":2"),
        &stext,
        &dk,
        &sk,
    );
    assert_ne!(changed, tr);
    assert!(verify(&dk, &changed, Side::Dialer, &signature).is_err());
    // reflected as the other side → no
    assert!(verify(&dk, &tr, Side::Listener, &signature).is_err());
    // signed by another key → no
    assert!(verify(&sk, &tr, Side::Dialer, &signature).is_err());
    assert!(verify(&dk, &tr, Side::Dialer, "zz").is_err());
}

#[test]
fn channel_paths_round_trip() {
    for c in [Channel::Control, Channel::Data, Channel::Pair] {
        assert_eq!(Channel::from_path(c.path()), Some(c));
    }
    assert_eq!(Channel::from_path("/"), None);
}

#[test]
fn a_dialer_sends_an_epoch_and_an_attempt_and_a_listener_sends_neither() {
    let t = two();
    let (dl, dh, sl, sh) = hellos(&t);
    let (dk, sk) = (t.deck.public_key(), t.station.public_key());
    assert_eq!(dh.link_epoch, 1);
    assert!(dh.attempt_id.as_deref().is_some_and(|a| a.len() == 32));
    assert_eq!((sh.link_epoch, sh.attempt_id.as_deref()), (0, None));
    // The station checks the dialing deck.
    let mut no_attempt = dh.clone();
    no_attempt.attempt_id = None;
    let e = negotiate(&sl, &sh, &no_attempt, &dk, Channel::Control, Side::Dialer).unwrap_err();
    assert_eq!(e.reason(), "bad_hello");
    let mut epoch_zero = dh.clone();
    epoch_zero.link_epoch = 0;
    assert!(negotiate(&sl, &sh, &epoch_zero, &dk, Channel::Control, Side::Dialer).is_err());
    let mut short = dh.clone();
    short.attempt_id = Some("abc".into());
    assert!(negotiate(&sl, &sh, &short, &dk, Channel::Control, Side::Dialer).is_err());
    // The deck checks the listening station.
    let mut with_attempt = sh.clone();
    with_attempt.attempt_id = dh.attempt_id.clone();
    assert!(at_deck(&dl, &dh, &with_attempt, &sk, Channel::Control).is_err());
    let mut with_epoch = sh;
    with_epoch.link_epoch = 4;
    assert!(at_deck(&dl, &dh, &with_epoch, &sk, Channel::Control).is_err());
    // Two attempts never share an id.
    let (a, b) = (Attempt::new(1).unwrap(), Attempt::new(1).unwrap());
    assert_ne!(a.attempt_id, b.attempt_id);
}

#[test]
fn a_stale_epoch_close_names_the_lowest_accepted_epoch() {
    let e = HandshakeError::StaleEpoch {
        got: 3,
        min_epoch: 8,
    };
    assert_eq!(e.reason(), "stale_link_epoch");
    let frame = e.close_frame();
    let text = serde_json::to_value(&frame).unwrap();
    assert_eq!(text["min_epoch"], 8);
    let back: HandshakeFrame = serde_json::from_value(text).unwrap();
    assert_eq!(back, frame);
    // An older peer without `min_epoch` still parses.
    let old: HandshakeFrame =
        serde_json::from_str(r#"{"type":"close","reason":"x","detail":"y"}"#).unwrap();
    assert!(matches!(
        old,
        HandshakeFrame::Close {
            min_epoch: None,
            ..
        }
    ));
}
