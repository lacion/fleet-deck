use proptest::prelude::*;

use super::*;

fn pair() -> (Mux, Mux) {
    (Mux::new(Side::Dialer), Mux::new(Side::Listener))
}

fn pump(to: &mut Mux, frames: Vec<MuxFrame>) -> Vec<MuxEvent> {
    frames
        .into_iter()
        .filter_map(|f| to.receive(MuxFrame::decode(&f.encode()).unwrap()).unwrap())
        .collect()
}

fn data(stream: u32, len: usize) -> MuxFrame {
    MuxFrame::Data {
        stream,
        bytes: vec![7; len],
    }
}

fn open(stream: u32) -> MuxFrame {
    MuxFrame::Open {
        stream,
        token: "t".into(),
    }
}

/// Opens `stream` at `l` (the listener), as the dialer would.
fn opened(l: &mut Mux, stream: u32) {
    assert!(matches!(
        l.receive(open(stream)),
        Ok(Some(MuxEvent::Opened { .. }))
    ));
}

#[test]
fn frames_round_trip_and_an_empty_data_frame_is_a_protocol_error() {
    for f in [
        open(1),
        data(3, 3),
        MuxFrame::Credit {
            stream: 2,
            amount: 7,
        },
        MuxFrame::Close { stream: 2 },
        MuxFrame::Reset {
            stream: 5,
            reason: "cancelled".into(),
        },
    ] {
        assert_eq!(MuxFrame::decode(&f.encode()).unwrap(), f);
    }
    let empty = MuxFrame::decode(&[2, 0, 0, 0, 1]).unwrap_err();
    assert_eq!(
        (empty.clone(), empty.reason()),
        (MuxError::Malformed("empty data frame"), "protocol_error")
    );
    assert!(MuxFrame::decode(&[2, 0, 0, 0]).is_err());
    assert!(MuxFrame::decode(&[9, 0, 0, 0, 1]).is_err());
    assert!(MuxFrame::decode(&[3, 0, 0, 0, 1, 0]).is_err());
    let mut big = vec![2, 0, 0, 0, 1];
    big.extend(vec![0; MAX_CHUNK + 1]);
    assert!(MuxFrame::decode(&big).is_err());
}

#[test]
fn a_sender_stops_at_the_window_and_continues_after_a_grant() {
    let (mut d, mut l) = pair();
    let (s, frame) = d.open("tok").unwrap();
    assert_eq!(s, 1);
    assert_eq!(
        pump(&mut l, vec![frame]),
        vec![MuxEvent::Opened {
            stream: 1,
            token: "tok".into()
        }]
    );
    let bytes = vec![7u8; 600 * 1024];
    let (frames, taken) = d.send(s, &bytes).unwrap();
    assert_eq!(taken, WINDOW as usize);
    assert!(
        frames
            .iter()
            .all(|f| matches!(f, MuxFrame::Data { bytes, .. } if bytes.len() <= MAX_CHUNK))
    );
    assert_eq!(pump(&mut l, frames).len(), 4);
    assert_eq!(l.buffered(), u64::from(WINDOW));
    assert_eq!(
        d.send(s, &bytes[taken..]).unwrap().1,
        0,
        "no credit, nothing taken"
    );
    assert_eq!(
        l.consumed(s, 1000),
        None,
        "less than half a window gives no credit"
    );
    let credit = l.consumed(s, taken).unwrap();
    assert_eq!(
        credit,
        MuxFrame::Credit {
            stream: 1,
            amount: WINDOW
        }
    );
    assert_eq!(
        l.buffered(),
        0,
        "consumed beyond the buffer counts the buffer only"
    );
    d.receive(credit).unwrap();
    assert_eq!(d.send_credit(s), u64::from(WINDOW));
}

#[test]
fn violations_end_the_link_with_their_reason() {
    let mut l = Mux::new(Side::Listener);
    let err = |l: &mut Mux, f: MuxFrame| l.receive(f).unwrap_err();
    assert_eq!(
        err(&mut l, open(2)),
        MuxError::BadStreamId(2),
        "the dialer uses odd ids"
    );
    opened(&mut l, 3);
    assert_eq!(
        err(&mut l, open(3)),
        MuxError::BadStreamId(3),
        "an id is never reused"
    );
    assert_eq!(
        err(&mut l, open(1)),
        MuxError::BadStreamId(1),
        "ids only grow"
    );
    assert_eq!(err(&mut l, data(9, 1)), MuxError::UnknownStream(9));
    for _ in 0..4 {
        l.receive(data(3, MAX_CHUNK)).unwrap();
    }
    let e = err(&mut l, data(3, 1));
    assert!(matches!(e, MuxError::Window { .. }));
    assert_eq!(e.reason(), "flow_control_violation");
    l.receive(MuxFrame::Close { stream: 3 }).unwrap();
    assert_eq!(err(&mut l, data(3, 1)), MuxError::DataAfterClose(3));
    let e = err(
        &mut l,
        MuxFrame::Credit {
            stream: 3,
            amount: u32::MAX,
        },
    );
    assert_eq!(
        (e.clone(), e.reason()),
        (MuxError::CreditOverflow(3), "protocol_error")
    );
}

#[test]
fn late_frames_of_finished_streams_and_resets_of_unknown_streams_are_ignored() {
    let (mut d, mut l) = pair();
    let (s, frame) = d.open("t").unwrap();
    pump(&mut l, vec![frame]);
    // The listener resets; the dialer's frames already in flight arrive late.
    let reset = l.reset(s, "cancelled").unwrap();
    for late in [
        data(s, 5),
        MuxFrame::Close { stream: s },
        MuxFrame::Credit {
            stream: s,
            amount: 1,
        },
    ] {
        assert_eq!(l.receive(late), Ok(None));
    }
    assert_eq!(
        pump(&mut d, vec![reset]),
        vec![MuxEvent::Reset {
            stream: s,
            reason: "cancelled".into()
        }]
    );
    assert_eq!(
        l.receive(MuxFrame::Reset {
            stream: 77,
            reason: "x".into()
        }),
        Ok(None)
    );
    assert_eq!(
        l.receive(MuxFrame::Reset {
            stream: s,
            reason: "x".into()
        }),
        Ok(None)
    );
    // Own ids keep growing after the stream ended.
    assert_eq!(d.open("t").unwrap().0, 3);
    assert_eq!(l.reset(99, "x"), None);
}

#[test]
fn at_most_64_streams_are_open_on_one_link_both_sides_together() {
    let (mut d, mut l) = pair();
    for _ in 0..40 {
        let (_, f) = d.open("t").unwrap();
        pump(&mut l, vec![f]);
    }
    for _ in 0..24 {
        let (_, f) = l.open("t").unwrap();
        pump(&mut d, vec![f]);
    }
    assert_eq!((d.open_streams(), l.open_streams()), (64, 64));
    assert_eq!(d.open("t").unwrap_err(), StreamError::TooManyStreams);
    assert_eq!(l.open("t").unwrap_err(), StreamError::TooManyStreams);

    // A crossing open at a full link is refused with a reset, not the link.
    let (s, f) = d.open_after_close_for_test(&mut l);
    let theirs = l.open("t").unwrap().1;
    let ours = pump(&mut l, vec![f]);
    assert_eq!(ours.len(), 1, "the listener had room when ours arrived");
    let refused = d.receive(theirs).unwrap();
    assert!(matches!(
        refused,
        Some(MuxEvent::Refused { reset: MuxFrame::Reset { ref reason, .. } }) if reason == "flow_control_violation"
    ));
    let _ = s;

    // A peer that opens more than 64 streams itself ends the link.
    let mut l = Mux::new(Side::Listener);
    for i in 0..64 {
        opened(&mut l, 2 * i + 1);
    }
    let e = l.receive(open(129)).unwrap_err();
    assert_eq!(
        (e.clone(), e.reason()),
        (MuxError::TooManyStreams, "flow_control_violation")
    );
}

impl Mux {
    /// Test helper: closes one of this dialer's streams on both sides, then
    /// opens a new one; returns its id and open frame (not delivered).
    fn open_after_close_for_test(&mut self, l: &mut Mux) -> (u32, MuxFrame) {
        let first = *self.streams.keys().next().unwrap();
        l.receive(self.close(first).unwrap()).unwrap();
        self.receive(l.close(first).unwrap()).unwrap();
        self.open("t").unwrap()
    }
}

#[test]
fn unconsumed_bytes_over_4_mib_end_the_link_and_a_reset_does_not_free_them() {
    let mut l = Mux::new(Side::Listener);
    for i in 0..17 {
        opened(&mut l, 2 * i + 1);
    }
    for i in 0..16 {
        for _ in 0..4 {
            l.receive(data(2 * i + 1, MAX_CHUNK)).unwrap();
        }
    }
    assert_eq!(l.buffered(), MAX_BUFFERED);
    // A reset stream keeps its unconsumed bytes in the budget.
    l.reset(1, "cancelled").unwrap();
    let e = l.receive(data(33, 1)).unwrap_err();
    assert_eq!(
        (e.clone(), e.reason()),
        (
            MuxError::Buffered(MAX_BUFFERED + 1),
            "flow_control_violation"
        )
    );
    // Consuming the reset stream's bytes frees the budget.
    assert_eq!(l.consumed(1, MAX_CHUNK), None);
    assert_eq!(l.buffered(), MAX_BUFFERED - MAX_CHUNK as u64);
    l.receive(data(33, 1)).unwrap();
}

#[test]
fn both_halves_must_close_before_the_stream_is_gone() {
    let (mut d, mut l) = pair();
    let (s, f) = d.open("p").unwrap();
    l.receive(f).unwrap();
    l.receive(d.close(s).unwrap()).unwrap();
    assert_eq!(d.close(s), Err(StreamError::Closed(s)));
    assert_eq!(d.send(s, b"x"), Err(StreamError::Closed(s)));
    assert_eq!(l.open_streams(), 1);
    d.receive(l.close(s).unwrap()).unwrap();
    assert_eq!((d.open_streams(), l.open_streams()), (0, 0));
}

proptest! {
    #[test]
    fn decode_never_panics(buf in proptest::collection::vec(any::<u8>(), 0..80)) {
        let _ = MuxFrame::decode(&buf);
    }

    #[test]
    fn receive_never_panics(frames in proptest::collection::vec(proptest::collection::vec(any::<u8>(), 5..12), 0..30)) {
        let mut l = Mux::new(Side::Listener);
        for f in frames {
            if let Ok(f) = MuxFrame::decode(&f) {
                let _ = l.receive(f);
            }
        }
    }
}
