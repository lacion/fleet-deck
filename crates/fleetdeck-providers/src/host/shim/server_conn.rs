//! The socket side of the shim: accept, handshake, takeover, frames.

use super::*;

impl Shim {
    pub(super) async fn accept(self: Arc<Self>, listener: UnixListener) {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            tokio::spawn(Arc::clone(&self).connection(stream));
        }
    }

    /// One connection: the peer check, the handshake, a takeover, then the
    /// frame loop (ADR-005 §3 steps 1–5).
    async fn connection(self: Arc<Self>, stream: tokio::net::UnixStream) {
        let own = rustix::process::getuid().as_raw();
        let peer = stream.peer_cred().ok().map(|c| c.uid());
        if !peer_allowed(peer, own) {
            return;
        }
        let (mut r, mut w) = stream.into_split();
        let Ok(nonce) = nonce() else {
            return;
        };
        let hello = FromShim::Hello {
            protocols: PROTOCOLS.to_vec(),
            shim_build: env!("CARGO_PKG_VERSION").to_owned(),
            shim_id: self.files.shim_id().to_owned(),
            nonce: hex(&nonce),
        };
        if write_frame(&mut w, &hello, &[]).await.is_err() {
            return;
        }
        let frame = tokio::time::timeout(ATTACH_WAIT, read_frame::<_, ToShim>(&mut r)).await;
        let Ok(Ok(Some((
            ToShim::Attach {
                protocol,
                incarnation,
                generation,
                proof,
            },
            _,
        )))) = frame
        else {
            return;
        };
        if pick_protocol(&[protocol]).is_none() {
            refuse(&mut w, Refusal::Incompatible).await;
            return;
        }
        let current = self.lock().generation;
        let decision = decide_attach(
            &self.key,
            &nonce,
            &self.spec.incarnation,
            current,
            &incarnation,
            generation,
            &proof,
        );
        if let AttachDecision::Refuse(reason) = decision {
            refuse(&mut w, reason).await;
            return;
        }
        let (tx, rx) = mpsc::channel(CONNECTION_QUEUE);
        let closed = Arc::new(watch::Sender::new(false));
        let attached = {
            let mut state = self.lock();
            if generation <= state.generation {
                None
            } else {
                state.generation = generation;
                state.writes.takeover(generation);
                if let Some(old) = state.conn.take() {
                    old.closed.send_replace(true);
                }
                state.conn = Some(Conn {
                    generation,
                    tx: tx.clone(),
                    sent: None,
                    closed: Arc::clone(&closed),
                });
                Some(FromShim::Attached {
                    generation,
                    provider_identity: Some(self.identity.clone()),
                    cursor_floor: state.buffer.floor(),
                    exit: state.exit.clone(),
                })
            }
        };
        let Some(attached) = attached else {
            refuse(&mut w, Refusal::StaleOwner).await;
            return;
        };
        if tx.send((attached, Vec::new())).await.is_err() {
            return;
        }
        tokio::spawn(write_frames(w, rx, Arc::clone(&closed)));
        let mut stop = closed.subscribe();
        loop {
            let frame = tokio::select! {
                frame = read_frame::<_, ToShim>(&mut r) => frame,
                _ = stop.wait_for(|c| *c) => break,
            };
            let Ok(Some((frame, payload))) = frame else {
                break;
            };
            if frame.generation() != self.lock().generation {
                // A stale owner: drop the frame and close (§3 step 5).
                break;
            }
            if !self.handle(frame, payload, &tx).await {
                break;
            }
        }
        let mut state = self.lock();
        if state
            .conn
            .as_ref()
            .is_some_and(|c| c.generation == generation)
        {
            state.conn = None;
        }
        drop(state);
        closed.send_replace(true);
    }

    /// One frame of the current attachment. False closes the connection.
    async fn handle(
        self: &Arc<Self>,
        frame: ToShim,
        payload: Vec<u8>,
        tx: &mpsc::Sender<Out>,
    ) -> bool {
        let answer = match frame {
            ToShim::Attach { .. } => return false,
            ToShim::Ack { cursor, .. } => {
                if self.lock().buffer.ack(cursor).is_err() {
                    return false;
                }
                self.acked.notify_one();
                None
            }
            ToShim::Replay { from, .. } => {
                let gap = {
                    let mut state = self.lock();
                    let floor = state.buffer.floor();
                    if let Some(conn) = state.conn.as_mut() {
                        conn.sent = Some(from.max(floor));
                    }
                    match state.overlong {
                        Some((start, end)) if from < end => {
                            Some((from.min(start), end, GapReason::OutputFrameTooLarge))
                        }
                        _ => (from < floor).then_some((from, floor, GapReason::BelowFloor)),
                    }
                };
                self.streamed.notify_one();
                gap.map(|(from, to, reason)| FromShim::Gap { from, to, reason })
            }
            ToShim::Write {
                generation,
                write_id,
                write_seq,
                ..
            } => {
                let admission = self
                    .lock()
                    .writes
                    .admit(generation, &write_id, write_seq, payload);
                match admission {
                    Admission::Queued => {
                        self.queued.notify_one();
                        None
                    }
                    Admission::Duplicate(state) | Admission::Refused(state) => {
                        Some(FromShim::WriteDone {
                            write_id,
                            write_seq,
                            state,
                        })
                    }
                    Admission::Forgotten => Some(FromShim::WriteStatus {
                        write_id,
                        write_seq,
                        answer: StatusAnswer::Forgotten,
                    }),
                    Admission::Mismatch => Some(FromShim::WriteStatus {
                        write_id,
                        write_seq,
                        answer: StatusAnswer::Mismatch,
                    }),
                }
            }
            ToShim::WriteStatus {
                write_id,
                write_seq,
                ..
            } => {
                let answer = self.lock().writes.status(&write_id, write_seq);
                Some(FromShim::WriteStatus {
                    write_id,
                    write_seq,
                    answer,
                })
            }
            ToShim::Stop {
                grace_ms,
                term_grace_ms,
                ..
            } => {
                self.lock().cause.get_or_insert("stopped");
                tokio::spawn(Arc::clone(self).stop(
                    Duration::from_millis(grace_ms),
                    Duration::from_millis(term_grace_ms),
                    "stop_forced",
                ));
                None
            }
            ToShim::Kill { .. } => {
                self.lock().cause = Some("killed");
                signal_group(self.pid(), rustix::process::Signal::KILL);
                None
            }
            ToShim::CloseStdin { .. } => {
                self.close_stdin.send_replace(true);
                None
            }
            ToShim::Interrupt { .. } => {
                signal_group(self.pid(), rustix::process::Signal::INT);
                None
            }
            ToShim::Status { .. } => {
                let state = self.lock();
                Some(FromShim::Status {
                    provider_identity: Some(self.identity.clone()),
                    acked: state.buffer.floor(),
                    end: state.buffer.end(),
                    pending_writes: u32::try_from(state.writes.pending()).unwrap_or(u32::MAX),
                })
            }
            ToShim::AckExit { .. } => {
                if self.lock().exit.is_none() {
                    return true;
                }
                self.files.remove_after_exit();
                self.finish();
                return false;
            }
        };
        match answer {
            Some(frame) => tx.send((frame, Vec::new())).await.is_ok(),
            None => true,
        }
    }
}

async fn refuse(w: &mut OwnedWriteHalf, reason: Refusal) {
    drop(write_frame(w, &FromShim::Refused { reason }, &[]).await);
    drop(w.shutdown().await);
}

/// The writer of one attachment. A takeover ends it at once, also inside
/// a frame that waits on a peer that does not read (the half-written frame
/// goes with the closed socket).
async fn write_frames(
    mut w: OwnedWriteHalf,
    mut rx: mpsc::Receiver<Out>,
    closed: Arc<watch::Sender<bool>>,
) {
    let mut stop = closed.subscribe();
    loop {
        let next = tokio::select! {
            next = rx.recv() => next,
            _ = stop.wait_for(|c| *c) => None,
        };
        let Some((frame, payload)) = next else {
            break;
        };
        let written = tokio::select! {
            written = write_frame(&mut w, &frame, &payload) => written.is_ok(),
            _ = stop.wait_for(|c| *c) => false,
        };
        if !written {
            break;
        }
    }
    closed.send_replace(true);
    drop(w.shutdown().await);
}
