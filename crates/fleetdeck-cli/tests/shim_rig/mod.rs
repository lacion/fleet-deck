//! The test rig of `shim_process.rs`: one shim with its directories,
//! files, key and launch spec, and helpers that read its frames.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use fleetdeck_providers::host::shim::client::{
    AttachResult, Attachment, Connection, Launched, launch,
};
use fleetdeck_providers::host::shim::journal::{ShimFiles, prepare_dir};
use fleetdeck_providers::host::shim::launch::{LaunchReply, LaunchSpec};
use fleetdeck_providers::host::shim::proof::AttachKey;
use fleetdeck_providers::host::shim::wire::{ExitState, FromShim, Refusal, ToShim, hex};

pub const WAIT: Duration = Duration::from_secs(10);
pub const INC: &str = "inc-1";

/// An echo provider: it records every stdin line in `$REC` and answers it.
pub const ECHO: &str =
    r#"while IFS= read -r l; do printf '%s\n' "$l" >> "$REC"; printf 'got:%s\n' "$l"; done"#;

pub fn exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_fleetdeck"))
}

pub fn shim_id() -> String {
    static N: AtomicU32 = AtomicU32::new(0);
    format!(
        "01SHIMTEST{:08}{:08}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    )
}

/// One shim under test: its directories, files, key and spec.
pub struct Rig {
    pub tmp: tempfile::TempDir,
    pub files: ShimFiles,
    pub key: AttachKey,
    pub spec: LaunchSpec,
}

impl Rig {
    pub fn new(script: &str) -> Self {
        Self::with(script, |_| {})
    }

    pub fn with(script: &str, edit: impl FnOnce(&mut LaunchSpec)) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let shims = tmp.path().join("shims");
        prepare_dir(&shims).unwrap();
        let id = shim_id();
        let files = ShimFiles::new(&shims, &id);
        files.create_journal(INC).unwrap();
        let key = AttachKey::generate().unwrap();
        files.write_key(&key).unwrap();
        let rec = tmp.path().join("rec");
        let mut spec = LaunchSpec {
            shim_id: id.clone(),
            incarnation: INC.to_owned(),
            shims_dir: shims,
            socket_path: tmp.path().join("run").join(format!("{id}.s")),
            buffer_bytes: 64 * 1024,
            exit_publish_after_ms: 60_000,
            exit_retry_ms: 60_000,
            program: b"/bin/sh".to_vec(),
            args: vec![b"-c".to_vec(), script.as_bytes().to_vec()],
            cwd: tmp.path().as_os_str().as_encoded_bytes().to_vec(),
            env: vec![
                (b"PATH".to_vec(), b"/usr/bin:/bin".to_vec()),
                (b"REC".to_vec(), rec.as_os_str().as_encoded_bytes().to_vec()),
            ],
        };
        edit(&mut spec);
        Self {
            tmp,
            files,
            key,
            spec,
        }
    }

    pub fn rec(&self) -> String {
        std::fs::read_to_string(self.tmp.path().join("rec")).unwrap_or_default()
    }

    pub async fn launch(&self) -> Launched {
        launch(&exe(), &self.spec, &self.key, WAIT).await.unwrap()
    }

    /// Launches and expects `ready`; returns the shim and the provider pid.
    pub async fn ready(&self) -> (Launched, u32) {
        let launched = self.launch().await;
        let LaunchReply::Ready { provider_identity } = &launched.reply else {
            panic!("{:?}", launched.reply);
        };
        let pid = provider_identity.pid;
        (launched, pid)
    }

    pub async fn connect(&self) -> Connection {
        Connection::open(&self.spec.socket_path, WAIT)
            .await
            .unwrap()
    }

    pub async fn attach(&self, generation: u64) -> (Attachment, FromShim) {
        let result = self
            .connect()
            .await
            .attach(&self.key, INC, generation, WAIT)
            .await
            .unwrap();
        match result {
            AttachResult::Attached { attachment, frame } => (attachment, frame),
            AttachResult::Refused(reason) => panic!("refused: {reason:?}"),
        }
    }

    pub async fn refused(&self, incarnation: &str, generation: u64, key: &AttachKey) -> Refusal {
        let conn = self.connect().await;
        let proof = hex(&key.proof(&conn.hello.nonce, incarnation, generation));
        match conn
            .attach_with_proof(incarnation, generation, proof, WAIT)
            .await
            .unwrap()
        {
            AttachResult::Refused(reason) => reason,
            AttachResult::Attached { frame, .. } => panic!("attached: {frame:?}"),
        }
    }
}

pub fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|s| {
            s.rfind(')')
                .map(|i| s[i + 1..].trim_start().starts_with('Z'))
        })
        == Some(false)
}

pub async fn wait_gone(pid: u32) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while alive(pid) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "pid {pid} still runs"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The stdout bytes and the other frames read until `pick` says stop.
#[derive(Default)]
pub struct Seen {
    /// The first stdout offset received.
    pub start: Option<u64>,
    pub bytes: Vec<u8>,
    pub frames: Vec<FromShim>,
}

impl Seen {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }

    /// The offset after the last complete line received.
    pub fn line_cursor(&self) -> u64 {
        let start = self.start.unwrap_or(0);
        let end = self
            .bytes
            .iter()
            .rposition(|b| *b == b'\n')
            .map_or(0, |i| i + 1);
        start + end as u64
    }
}

pub async fn read_until(att: &mut Attachment, mut stop: impl FnMut(&Seen) -> bool) -> Seen {
    let mut seen = Seen::default();
    let deadline = tokio::time::Instant::now() + WAIT;
    while !stop(&seen) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "stopped waiting; text {:?}, frames {:?}",
            seen.text(),
            seen.frames
        );
        let Some((frame, payload)) = att.recv(WAIT).await.unwrap() else {
            panic!(
                "the shim closed; text {:?}, frames {:?}",
                seen.text(),
                seen.frames
            );
        };
        match frame {
            FromShim::Output { offset, .. } => {
                let start = *seen.start.get_or_insert(offset);
                assert_eq!(
                    offset,
                    start + seen.bytes.len() as u64,
                    "offsets are contiguous"
                );
                seen.bytes.extend_from_slice(&payload);
            }
            other => seen.frames.push(other),
        }
    }
    seen
}

pub fn write(generation: u64, id: &str, seq: u64) -> ToShim {
    ToShim::Write {
        generation,
        write_id: id.to_owned(),
        write_seq: seq,
        sent_at: 0,
    }
}

pub fn exited(seen: &Seen) -> Option<&ExitState> {
    seen.frames.iter().find_map(|f| match f {
        FromShim::Exited { exit } => Some(exit),
        _ => None,
    })
}

pub async fn end(rig: &Rig, mut launched: Launched, generation: u64) {
    let (mut att, _) = rig.attach(generation).await;
    att.send(&ToShim::Kill { generation }, &[]).await.unwrap();
    read_until(&mut att, |s| exited(s).is_some()).await;
    att.send(&ToShim::AckExit { generation }, &[])
        .await
        .unwrap();
    let status = tokio::time::timeout(WAIT, launched.child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success(), "{status:?}");
}
