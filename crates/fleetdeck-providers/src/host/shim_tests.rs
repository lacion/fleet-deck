//! Unit tests of the shim core (ADR-005 §9). The numbers are the ADR test
//! numbers. These tests cover the parts without a process or a socket; the
//! real-process parts are in `crates/fleetdeck-cli/tests/shim_*.rs`.

use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::AsyncWrite;

use super::identity::{IdentityCheck, check_identity, start_time_of_stat};
use super::output::OutputBuffer;
use super::proof::{AttachDecision, AttachKey, decide_attach, nonce, peer_allowed};
use super::settle::{EffectResult, Facts, SHIM_NOT_RECEIVED, SHIM_WRITE_FAILED, settle};
use super::wire::{
    self, FromShim, GapReason, ProviderIdentity, Refusal, StatusAnswer, ToShim, WireError,
    WriteState,
};
use super::writes::{
    Admission, CAPACITY, MAX_PENDING_BYTES, MAX_PENDING_WRITES, MAX_RECORDS, SUPERSEDED,
    WriteTable, write_tracked,
};

// ---- wire ----

#[tokio::test]
async fn frames_round_trip_and_a_truncated_frame_is_never_returned() {
    let header = ToShim::Write {
        generation: 3,
        write_id: "e1:0".into(),
        write_seq: 7,
        sent_at: 1,
    };
    let frame = wire::encode(&header, b"{\"x\":1}\n").unwrap();
    let mut r = &frame[..];
    let (got, payload): (ToShim, Vec<u8>) = wire::read_frame(&mut r).await.unwrap().unwrap();
    assert_eq!(got, header);
    assert_eq!(payload, b"{\"x\":1}\n");
    assert!(
        wire::read_frame::<_, ToShim>(&mut r)
            .await
            .unwrap()
            .is_none()
    );
    // Every cut inside the frame is `Truncated`, never a frame.
    for cut in 1..frame.len() {
        let mut r = &frame[..cut];
        assert!(
            matches!(
                wire::read_frame::<_, ToShim>(&mut r).await,
                Err(WireError::Truncated)
            ),
            "cut {cut}"
        );
    }
}

#[tokio::test]
async fn an_oversized_frame_is_refused_before_allocation() {
    let big = vec![0u8; wire::MAX_PAYLOAD + 1];
    assert!(matches!(
        wire::encode(&ToShim::Kill { generation: 1 }, &big),
        Err(WireError::TooLarge(_))
    ));
    let mut r: &[u8] = &u32::MAX.to_be_bytes();
    assert!(matches!(
        wire::read_frame::<_, ToShim>(&mut r).await,
        Err(WireError::TooLarge(_))
    ));
}

#[test]
fn unknown_fields_and_unknown_frames_are_refused() {
    let extra = r#"{"type":"kill","generation":1,"force":true}"#;
    assert!(serde_json::from_str::<ToShim>(extra).is_err());
    let unknown = r#"{"type":"format_disk","generation":1}"#;
    assert!(serde_json::from_str::<ToShim>(unknown).is_err());
}

#[test]
fn the_protocol_choice_is_the_highest_common_version() {
    assert_eq!(wire::pick_protocol(&[1, 2, 9]), Some(wire::PROTOCOL));
    assert_eq!(wire::pick_protocol(&[0, 9]), None);
}

#[test]
fn hex_round_trips_and_refuses_bad_text() {
    assert_eq!(
        wire::unhex(&wire::hex(&[0, 1, 254, 255])).unwrap(),
        vec![0, 1, 254, 255]
    );
    assert!(wire::unhex("abc").is_none());
    assert!(wire::unhex("zz").is_none());
}

#[test]
fn a_status_answer_frame_nests_the_answer_and_the_state() {
    let frame = FromShim::WriteStatus {
        write_id: "e1:0".into(),
        write_seq: 4,
        answer: StatusAnswer::Known {
            state: WriteState::Partial {
                bytes_done: 3,
                error: "broken pipe".into(),
            },
        },
    };
    let json = serde_json::to_value(&frame).unwrap();
    assert_eq!(json["answer"]["answer"], "known");
    assert_eq!(json["answer"]["state"]["state"], "partial");
    assert_eq!(serde_json::from_value::<FromShim>(json).unwrap(), frame);
}

// ---- 8, 9: handshake ----

const INC: &str = "inc-1";

fn proof(key: &AttachKey, nonce: &[u8], inc: &str, generation: u64) -> String {
    wire::hex(&key.proof(nonce, inc, generation))
}

#[test]
fn an_attach_with_another_incarnation_or_a_bad_proof_is_refused() {
    let key = AttachKey::generate().unwrap();
    let other = AttachKey::generate().unwrap();
    let n = nonce().unwrap();
    let decide =
        |inc: &str, generation: u64, p: &str| decide_attach(&key, &n, INC, 1, inc, generation, p);
    assert_eq!(
        decide(INC, 2, &proof(&key, &n, INC, 2)),
        AttachDecision::Accept
    );
    // Another key, another nonce, another generation in the proof, junk.
    assert_eq!(
        decide(INC, 2, &proof(&other, &n, INC, 2)),
        AttachDecision::Refuse(Refusal::BadProof)
    );
    let n2 = nonce().unwrap();
    assert_eq!(
        decide(INC, 2, &proof(&key, &n2, INC, 2)),
        AttachDecision::Refuse(Refusal::BadProof)
    );
    assert_eq!(
        decide(INC, 2, &proof(&key, &n, INC, 3)),
        AttachDecision::Refuse(Refusal::BadProof)
    );
    assert_eq!(
        decide(INC, 2, "zz"),
        AttachDecision::Refuse(Refusal::BadProof)
    );
    // A valid proof of another station.
    assert_eq!(
        decide("inc-2", 2, &proof(&key, &n, "inc-2", 2)),
        AttachDecision::Refuse(Refusal::ForeignStation)
    );
    // The same or a lower generation.
    for generation in [0, 1] {
        assert_eq!(
            decide(INC, generation, &proof(&key, &n, INC, generation)),
            AttachDecision::Refuse(Refusal::StaleOwner)
        );
    }
    // The length prefixes keep the parts apart.
    assert_ne!(key.proof(b"ab", "c", 1), key.proof(b"a", "bc", 1));
    assert!(AttachKey::from_bytes(&[0; 31]).is_none());
    assert_eq!(format!("{key:?}"), "AttachKey(<redacted>)");
}

#[test]
fn a_peer_with_another_uid_is_refused() {
    assert!(peer_allowed(Some(1000), 1000));
    assert!(!peer_allowed(Some(0), 1000));
    assert!(!peer_allowed(Some(1001), 1000));
    assert!(!peer_allowed(None, 1000));
}

#[test]
fn a_stale_generation_frame_is_recognized_by_its_generation() {
    // ADR-005 §3 "Stale writers": every station frame carries a generation.
    let frames = [
        ToShim::Ack {
            generation: 4,
            cursor: 0,
        },
        ToShim::Write {
            generation: 4,
            write_id: "w".into(),
            write_seq: 1,
            sent_at: 0,
        },
        ToShim::Stop {
            generation: 4,
            grace_ms: 0,
            term_grace_ms: 0,
        },
        ToShim::Kill { generation: 4 },
        ToShim::AckExit { generation: 4 },
    ];
    assert!(frames.iter().all(|f| f.generation() == 4));
    assert!(frames[2].is_control() && frames[3].is_control() && !frames[1].is_control());
}

// ---- 10: identity ----

#[test]
fn a_reused_pid_with_another_start_time_or_boot_id_is_lost_not_reattached() {
    let stored = ProviderIdentity {
        pid: 42,
        start_time: 1000,
        boot_id: "b1".into(),
    };
    let same = stored.clone();
    let reused = ProviderIdentity {
        start_time: 2000,
        ..stored.clone()
    };
    let rebooted = ProviderIdentity {
        boot_id: "b2".into(),
        ..stored.clone()
    };
    assert_eq!(
        check_identity(Some(&stored), Some(&same)),
        IdentityCheck::Same
    );
    assert_eq!(
        check_identity(Some(&stored), Some(&reused)),
        IdentityCheck::Mismatch
    );
    assert_eq!(
        check_identity(Some(&stored), Some(&rebooted)),
        IdentityCheck::Mismatch
    );
    assert_eq!(check_identity(None, Some(&same)), IdentityCheck::Mismatch);
    assert_eq!(check_identity(Some(&stored), None), IdentityCheck::Missing);
}

#[test]
fn the_start_time_is_field_22_also_with_a_hostile_command_name() {
    let stat = "77 (a) b) c) S 1 77 77 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 987654 1000 10";
    assert_eq!(start_time_of_stat(stat), Some(987_654));
    assert_eq!(start_time_of_stat("77 (x"), None);
}

#[cfg(target_os = "linux")]
#[test]
fn the_identity_of_this_process_is_stable_and_another_pid_differs() {
    let me = super::identity::identity_of(std::process::id()).unwrap();
    assert_eq!(
        super::identity::identity_of(std::process::id()).unwrap(),
        me
    );
    assert!(!me.boot_id.is_empty());
    assert!(super::identity::identity_of(u32::MAX - 1).is_err());
}

// ---- 5, 6, 38: output ----

#[test]
fn a_full_buffer_pauses_the_provider_and_loses_no_byte() {
    let mut out = OutputBuffer::new(16);
    let stream: Vec<u8> = (0..64u8)
        .map(|i| if i % 5 == 4 { b'\n' } else { b'a' + i % 26 })
        .collect();
    let mut taken = 0;
    let mut station = Vec::new();
    let mut cursor = 0u64;
    while taken < stream.len() {
        let space = out.space();
        if space == 0 {
            // Paused: the station commits the complete lines and acks.
            let r = out.replay(cursor);
            assert_eq!(r.gap, None);
            let last_nl = r.bytes.iter().rposition(|b| *b == b'\n').unwrap();
            station.extend_from_slice(&r.bytes[..=last_nl]);
            cursor = r.offset + last_nl as u64 + 1;
            out.ack(cursor).unwrap();
            continue;
        }
        let chunk = &stream[taken..(taken + space.min(7)).min(stream.len())];
        let (offset, n) = out.push(chunk);
        assert_eq!(offset, taken as u64);
        assert_eq!(n, chunk.len());
        taken += n;
    }
    station.extend(out.replay(cursor).bytes);
    assert_eq!(station, stream);
    // More than the space is never stored.
    let mut full = OutputBuffer::new(4);
    assert_eq!(full.push(b"abcdef"), (0, 4));
    assert_eq!(full.space(), 0);
}

#[test]
fn an_ack_frees_bytes_and_a_replay_below_the_floor_reports_an_explicit_gap() {
    let mut out = OutputBuffer::new(1024);
    out.push(b"one\ntwo\nthree\n");
    out.ack(4).unwrap();
    out.ack(2).unwrap(); // A late ack changes nothing.
    assert_eq!(out.floor(), 4);
    let r = out.replay(8);
    assert_eq!(
        (r.gap, r.offset, r.bytes.as_slice()),
        (None, 8, &b"three\n"[..])
    );
    let r = out.replay(0);
    assert_eq!(r.gap, Some((0, 4, GapReason::BelowFloor)));
    assert_eq!((r.offset, r.bytes.as_slice()), (4, &b"two\nthree\n"[..]));
    assert!(out.ack(99).is_err());
    assert_eq!(out.replay(99).bytes, Vec::<u8>::new());
}

#[test]
fn a_line_of_the_buffer_size_without_a_newline_fails_the_session_with_an_explicit_gap_and_stops_the_provider()
 {
    let mut out = OutputBuffer::new(8);
    out.push(b"ok\nabc");
    assert_eq!(out.overlong_line(), None); // Not full yet.
    out.push(b"de");
    assert_eq!(out.overlong_line(), None); // Full, but a line is complete.
    out.ack(3).unwrap();
    assert_eq!(out.overlong_line(), None); // Space again.
    out.push(b"fgh");
    assert_eq!(out.space(), 0);
    assert_eq!(out.overlong_line(), Some((3, 11)));
}

// ---- 11, 30–33, 35, 42: writes ----

fn admit(t: &mut WriteTable, generation: u64, id: &str, seq: u64, bytes: &[u8]) -> Admission {
    t.admit(generation, id, seq, bytes.to_vec())
}

#[test]
fn a_duplicate_write_id_is_written_once() {
    let mut t = WriteTable::new();
    assert_eq!(admit(&mut t, 1, "e1:0", 1, b"hi\n"), Admission::Queued);
    assert_eq!(
        admit(&mut t, 1, "e1:0", 1, b"hi\n"),
        Admission::Duplicate(WriteState::Received)
    );
    let (id, bytes) = t.start_next().unwrap();
    assert_eq!((id.as_str(), bytes.as_slice()), ("e1:0", &b"hi\n"[..]));
    assert_eq!(t.start_next(), None, "one active write");
    assert_eq!(
        admit(&mut t, 1, "e1:0", 1, b"hi\n"),
        Admission::Duplicate(WriteState::Writing { bytes_done: 0 })
    );
    t.finish("e1:0", WriteState::Written);
    assert_eq!(
        admit(&mut t, 2, "e1:0", 1, b"hi\n"),
        Admission::Duplicate(WriteState::Written)
    );
    assert_eq!(t.start_next(), None, "nothing to write a second time");
    // Sequence reuse and identity/sequence mismatch: no record, no I/O.
    assert_eq!(admit(&mut t, 2, "e2:0", 1, b"x"), Admission::Mismatch);
    assert_eq!(admit(&mut t, 2, "e1:0", 9, b"x"), Admission::Mismatch);
    assert_eq!(t.state("e2:0"), None);
}

/// A writer that accepts `limit` bytes, then fails, or blocks for ever.
struct Faulty {
    limit: usize,
    written: Vec<u8>,
    error: Option<std::io::ErrorKind>,
}

impl AsyncWrite for Faulty {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let room = self.limit - self.written.len();
        if room == 0 {
            return match self.error {
                Some(kind) => Poll::Ready(Err(kind.into())),
                None => Poll::Pending,
            };
        }
        let n = room.min(buf.len()).min(2);
        self.written.extend_from_slice(&buf[..n]);
        Poll::Ready(Ok(n))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

async fn run(t: &mut WriteTable, w: &mut Faulty) -> WriteState {
    let (id, bytes) = t.start_next().unwrap();
    let mut seen = Vec::new();
    let state = write_tracked(w, &bytes, |n| seen.push(n)).await;
    for n in seen {
        t.progress(&id, n);
    }
    t.finish(&id, state.clone());
    state
}

#[tokio::test]
async fn a_partial_write_is_outcome_unknown_blocks_input_and_is_never_resent() {
    let mut t = WriteTable::new();
    admit(&mut t, 1, "e1:0", 1, b"{\"long\":true}\n");
    let mut w = Faulty {
        limit: 5,
        written: Vec::new(),
        error: Some(std::io::ErrorKind::BrokenPipe),
    };
    let state = run(&mut t, &mut w).await;
    assert!(
        matches!(state, WriteState::Partial { bytes_done: 5, .. }),
        "{state:?}"
    );
    assert_eq!(w.written, b"{\"lon");
    // The duplicate is not written again.
    assert!(matches!(
        admit(&mut t, 1, "e1:0", 1, b"x"),
        Admission::Duplicate(WriteState::Partial { .. })
    ));
    assert_eq!(t.start_next(), None);
    let answer = t.status("e1:0", 1);
    assert_eq!(
        settle(facts(Some(&answer), true)),
        EffectResult::Unknown { block_input: true }
    );
}

#[tokio::test]
async fn a_write_that_fails_before_the_first_byte_is_retired_as_not_dispatched() {
    let mut t = WriteTable::new();
    admit(&mut t, 1, "e1:0", 1, b"hello\n");
    let mut w = Faulty {
        limit: 0,
        written: Vec::new(),
        error: Some(std::io::ErrorKind::BrokenPipe),
    };
    let state = run(&mut t, &mut w).await;
    assert!(
        matches!(state, WriteState::FailedUnwritten { .. }),
        "{state:?}"
    );
    assert!(w.written.is_empty());
    let answer = t.status("e1:0", 1);
    assert_eq!(
        settle(facts(Some(&answer), true)),
        EffectResult::Retired(SHIM_WRITE_FAILED)
    );
    // The gate is closed in this build: outcome_unknown.
    assert_eq!(
        settle(facts(Some(&answer), super::settle::RETIRE_ROWS_ENABLED)),
        EffectResult::Unknown { block_input: false }
    );
}

#[tokio::test]
async fn a_blocked_write_keeps_its_record_writing_and_never_blocks_the_table() {
    // ADR-005 test 41 (core part): a write blocked on a full pipe holds no
    // lock on the table; status and takeover answer at once.
    let mut t = WriteTable::new();
    admit(&mut t, 1, "e1:0", 1, b"abcdef");
    admit(&mut t, 1, "e2:0", 2, b"gh");
    let (id, bytes) = t.start_next().unwrap();
    let mut w = Faulty {
        limit: 4,
        written: Vec::new(),
        error: None,
    };
    let mut done = 0;
    let blocked = tokio::time::timeout(
        std::time::Duration::from_millis(50),
        write_tracked(&mut w, &bytes, |n| done = n),
    )
    .await;
    assert!(blocked.is_err(), "the write blocks");
    t.progress(&id, done);
    assert_eq!(
        t.status("e1:0", 1),
        StatusAnswer::Known {
            state: WriteState::Writing { bytes_done: 4 }
        }
    );
    assert_eq!(t.takeover(2), vec![("e2:0".to_owned(), 2)]);
    assert_eq!(
        t.state("e1:0"),
        Some(&WriteState::Writing { bytes_done: 4 })
    );
}

#[test]
fn an_evicted_write_record_is_forgotten_stays_outcome_unknown_and_a_resubmitted_frame_is_refused_without_io()
 {
    let mut t = WriteTable::new();
    for seq in 1..=(MAX_RECORDS as u64 + 3) {
        let id = format!("e{seq}:0");
        assert_eq!(admit(&mut t, 1, &id, seq, b"x\n"), Admission::Queued);
        let (id, _) = t.start_next().unwrap();
        t.finish(&id, WriteState::Written);
    }
    assert_eq!(t.evicted_through(), 3);
    assert_eq!(t.state("e1:0"), None);
    // The same identity again under the current generation: no I/O.
    assert_eq!(admit(&mut t, 2, "e1:0", 1, b"x\n"), Admission::Forgotten);
    assert_eq!(t.start_next(), None);
    let answer = t.status("e1:0", 1);
    assert_eq!(answer, StatusAnswer::Forgotten);
    assert_eq!(
        settle(facts(Some(&answer), true)),
        EffectResult::Unknown { block_input: false }
    );
    // A record that was not evicted still answers with its state.
    assert_eq!(
        t.status("e4:0", 4),
        StatusAnswer::Known {
            state: WriteState::Written
        }
    );
}

#[test]
fn a_complete_frame_of_the_old_generation_after_a_takeover_is_dropped_and_never_written() {
    let mut t = WriteTable::new();
    admit(&mut t, 1, "e1:0", 1, b"a\n");
    admit(&mut t, 1, "e2:0", 2, b"b\n");
    admit(&mut t, 1, "e3:0", 3, b"c\n");
    let (active, _) = t.start_next().unwrap();
    assert_eq!(active, "e1:0");
    // Generation 2 takes over: waiting writes of generation 1 are superseded.
    assert_eq!(
        t.takeover(2),
        vec![("e2:0".to_owned(), 2), ("e3:0".to_owned(), 3)]
    );
    let superseded = WriteState::FailedUnwritten {
        error: SUPERSEDED.to_owned(),
    };
    assert_eq!(t.state("e2:0"), Some(&superseded));
    assert_eq!(
        t.state("e1:0"),
        Some(&WriteState::Writing { bytes_done: 0 })
    );
    t.finish("e1:0", WriteState::Written);
    assert_eq!(t.start_next(), None, "no superseded write runs");
    // A frame that the shim never saw before the takeover (dropped by its
    // generation at the socket) has no record: not_received.
    assert_eq!(t.status("e4:0", 4), StatusAnswer::NotReceived);
}

#[test]
fn a_not_received_tombstone_refuses_every_later_frame_of_its_identity_and_a_released_slot_needs_a_new_effect()
 {
    let mut t = WriteTable::new();
    assert_eq!(t.status("e1:0", 1), StatusAnswer::NotReceived);
    assert_eq!(t.state("e1:0"), Some(&WriteState::ClosedUnwritten));
    // The late frame of the same identity is never written.
    assert_eq!(
        admit(&mut t, 2, "e1:0", 1, b"late\n"),
        Admission::Duplicate(WriteState::ClosedUnwritten)
    );
    assert_eq!(t.start_next(), None);
    // The same sequence with another identity: refused.
    assert_eq!(admit(&mut t, 2, "e9:0", 1, b"x\n"), Admission::Mismatch);
    // A new effect has a new identity and a new sequence.
    assert_eq!(admit(&mut t, 2, "e2:0", 2, b"again\n"), Admission::Queued);
    assert_eq!(
        t.status("e1:0", 1),
        StatusAnswer::Known {
            state: WriteState::ClosedUnwritten
        }
    );
    let answer = t.status("e1:0", 1);
    assert_eq!(
        settle(facts(Some(&answer), true)),
        EffectResult::Retired(SHIM_NOT_RECEIVED)
    );
}

#[test]
fn pending_writes_beyond_256_or_4_mib_are_failed_unwritten_and_an_oversized_input_gets_a_rejection_receipt()
 {
    let capacity = WriteState::FailedUnwritten {
        error: CAPACITY.to_owned(),
    };
    let mut t = WriteTable::new();
    for seq in 1..=MAX_PENDING_WRITES as u64 {
        assert_eq!(
            admit(&mut t, 1, &format!("e{seq}:0"), seq, b"x"),
            Admission::Queued
        );
    }
    assert_eq!(
        admit(&mut t, 1, "over:0", 999, b"x"),
        Admission::Refused(capacity.clone())
    );
    assert_eq!(t.state("over:0"), Some(&capacity));
    let mut t = WriteTable::new();
    let mib = vec![b'x'; 1024 * 1024];
    for seq in 1..=4u64 {
        assert_eq!(
            admit(&mut t, 1, &format!("e{seq}:0"), seq, &mib),
            Admission::Queued
        );
    }
    assert_eq!(MAX_PENDING_BYTES, 4 * mib.len());
    assert_eq!(
        admit(&mut t, 1, "e5:0", 5, b"x"),
        Admission::Refused(capacity)
    );
    // A running write frees its bytes from the pending total.
    t.start_next().unwrap();
    assert_eq!(admit(&mut t, 1, "e6:0", 6, b"x"), Admission::Queued);
    // An input above one frame never reaches the shim: the encoder refuses
    // it, and the station rejects it with a receipt before any claim.
    let oversized = vec![b'x'; wire::MAX_PAYLOAD + 1];
    assert!(
        wire::encode(
            &ToShim::Write {
                generation: 1,
                write_id: "big:0".into(),
                write_seq: 7,
                sent_at: 0
            },
            &oversized
        )
        .is_err()
    );
}

// ---- 12: the effect table ----

fn facts(answer: Option<&StatusAnswer>, retire_enabled: bool) -> Facts<'_> {
    Facts {
        answer,
        already_unknown: false,
        wait_over: false,
        live: false,
        retire_enabled,
    }
}

#[test]
fn write_status_maps_each_answer_to_the_effect_state_of_section_4_3() {
    let known = |state| StatusAnswer::Known { state };
    let unknown = EffectResult::Unknown { block_input: false };
    let cases = [
        (
            StatusAnswer::NotReceived,
            EffectResult::Retired(SHIM_NOT_RECEIVED),
        ),
        (
            known(WriteState::FailedUnwritten {
                error: "EPIPE".into(),
            }),
            EffectResult::Retired(SHIM_WRITE_FAILED),
        ),
        (
            known(WriteState::ClosedUnwritten),
            EffectResult::Retired(SHIM_NOT_RECEIVED),
        ),
        (known(WriteState::Written), unknown),
        (known(WriteState::Received), EffectResult::AskAgain),
        (
            known(WriteState::Writing { bytes_done: 3 }),
            EffectResult::AskAgain,
        ),
        (
            known(WriteState::Partial {
                bytes_done: 1,
                error: "EPIPE".into(),
            }),
            EffectResult::Unknown { block_input: true },
        ),
        (StatusAnswer::Forgotten, unknown),
        (StatusAnswer::Mismatch, unknown),
    ];
    for (answer, want) in &cases {
        assert_eq!(settle(facts(Some(answer), true)), *want, "{answer:?}");
        // An effect already outcome_unknown never becomes retired.
        let mut f = facts(Some(answer), true);
        f.already_unknown = true;
        assert!(
            !matches!(
                settle(f),
                EffectResult::Retired(_) | EffectResult::AskAgain | EffectResult::Sent
            ),
            "{answer:?}"
        );
        // With the gate closed, no answer retires an effect.
        assert!(
            !matches!(settle(facts(Some(answer), false)), EffectResult::Retired(_)),
            "{answer:?}"
        );
    }
    // No answer, a lost shim or an identity mismatch.
    assert_eq!(settle(facts(None, true)), unknown);
    // In flight after the bounded wait.
    let writing = known(WriteState::Writing { bytes_done: 0 });
    let mut f = facts(Some(&writing), true);
    f.wait_over = true;
    assert_eq!(settle(f), unknown);
    // Live write_done: written is Sent; nothing in flight asks again.
    let written = known(WriteState::Written);
    let mut f = facts(Some(&written), true);
    f.live = true;
    assert_eq!(settle(f), EffectResult::Sent);
    let mut f = facts(Some(&writing), true);
    f.live = true;
    assert_eq!(settle(f), unknown);
    // The gate of this build stays closed until tests 26–35 exist.
    const { assert!(!super::settle::RETIRE_ROWS_ENABLED) };
}
