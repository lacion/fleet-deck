//! The fake station and the helpers of the review tests (DL-179): the
//! station answers `files.session_diff`, `files.diff`, `settings.get` and
//! `session.follow_up` and records every request. Like the real station it
//! removes the files of the deny list from a diff when the deck marks the call
//! as made for a member.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::common::board::{BoardClient, Http, board};
use crate::common::*;
use fleetdeck_deck::http::review_anchor_cache::clear_station;
use fleetdeck_link::LiveLink;
use fleetdeck_link::control::Inbound;
use fleetdeck_proto::link::{Outcome, WireError};
use fleetdeck_proto::member_deny::MemberDeny;
use fleetdeck_proto::{CommandId, SessionSummary};
use serde_json::{Value, json};

pub const BASE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const FROM: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
pub const TO: &str = "cccccccccccccccccccccccccccccccccccccccc";
pub const OTHER: &str = "dddddddddddddddddddddddddddddddddddddddd";

pub const PATCH: &str = "\
diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,4 @@
 fn one() {}
-fn two() {}
+fn two() { 2 }
+fn three() {}
 fn four() {}
diff --git a/.env b/.env
index 3333333..4444444 100644
--- a/.env
+++ b/.env
@@ -1 +1 @@
-TOKEN=old
+TOKEN=brand-new-secret
diff --git a/secrets.txt b/renamed.txt
similarity index 90%
rename from secrets.txt
rename to renamed.txt
--- a/secrets.txt
+++ b/renamed.txt
@@ -1,2 +1,2 @@
 keep
-password=hunter2
+password=hunter3
";

pub const TURN_PATCH: &str = "\
diff --git a/src/turn.rs b/src/turn.rs
--- a/src/turn.rs
+++ b/src/turn.rs
@@ -1 +1,2 @@
 old
+new line of the turn
";

/// How the fake station answers a follow-up.
#[derive(Clone, Copy, PartialEq)]
pub enum Follow {
    Accept,
    Reject,
    /// The receipt is lost: the station answers nothing.
    Silent,
}

pub struct Cfg {
    pub patch: String,
    pub base: String,
    /// `files.session_diff` answers with this error instead.
    pub session_error: Option<&'static str>,
    pub turns: HashMap<(String, String), String>,
    /// The `files.diff` selector is refused as an old station would.
    pub refuse_selector: bool,
    /// The `member_deny` setting; `None`: `settings.get` fails.
    pub deny: Option<Vec<String>>,
    pub follow: Follow,
}

pub type Log = Arc<Mutex<Vec<(String, Value)>>>;

pub struct Fake {
    pub cfg: Arc<Mutex<Cfg>>,
    pub log: Log,
    pub station: fleetdeck_proto::StationId,
}

impl Fake {
    pub fn set(&self, f: impl FnOnce(&mut Cfg)) {
        f(&mut self.cfg.lock().unwrap());
        clear_station(&self.station);
    }

    pub fn calls(&self, method: &str) -> Vec<Value> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, p)| p.clone())
            .collect()
    }

    /// The `(command_id, text, author)` of every follow-up the station got.
    pub fn follow_ups(&self) -> Vec<(String, String, Option<String>)> {
        self.calls("session.follow_up")
            .into_iter()
            .map(|p| {
                (
                    p["command_id"].as_str().unwrap().to_owned(),
                    p["command"]["input"]["text"].as_str().unwrap().to_owned(),
                    p["command"]["input"]["author"].as_str().map(str::to_owned),
                )
            })
            .collect()
    }
}

pub fn summary_of(patch: &str) -> Value {
    json!({ "files_changed": patch.matches("diff --git").count(), "insertions": 0, "deletions": 0 })
}

pub fn answer(
    cfg: &Cfg,
    method: &str,
    params: &Value,
    command_id: Option<CommandId>,
) -> Option<Outcome> {
    let member = params.get("member").is_some();
    let filter = |patch: &str| -> String {
        if !member {
            return patch.to_owned();
        }
        let extra = cfg.deny.clone().unwrap_or_default();
        MemberDeny::new(&extra).filter_patch(patch).patch
    };
    Some(match method {
        "files.session_diff" => match cfg.session_error {
            Some(code) => Outcome::Err(WireError::new(code, "no")),
            None => Outcome::Ok(json!({
                "session_id": params["session_id"], "base_oid": cfg.base,
                "summary": summary_of(&cfg.patch), "patch": filter(&cfg.patch),
                "truncated": false, "scope": "worktree",
            })),
        },
        "files.diff" => {
            if cfg.refuse_selector {
                return Some(Outcome::Err(WireError::new(
                    "protocol_error",
                    "params: unknown field `checkpoint`",
                )));
            }
            let pair = &params["checkpoint"];
            let key = (
                pair["from_commit"].as_str().unwrap_or_default().to_owned(),
                pair["to_commit"].as_str().unwrap_or_default().to_owned(),
            );
            match cfg.turns.get(&key) {
                Some(patch) => Outcome::Ok(json!({
                    "session_id": params["session_id"], "turn_id": "turn-1",
                    "from_commit": key.0, "to_commit": key.1,
                    "summary": summary_of(patch), "patch": filter(patch), "truncated": false,
                })),
                None => Outcome::Err(WireError::new("checkpoint_unavailable", "no row")),
            }
        }
        "settings.get" => match &cfg.deny {
            Some(list) => Outcome::Ok(json!({
                "values": { "member_deny": list }, "fields": [], "revision": "r", "overrides": [],
            })),
            None => Outcome::Err(WireError::new("internal", "cannot read")),
        },
        "session.follow_up" => {
            let id = command_id?;
            if cfg.follow == Follow::Silent {
                return None;
            }
            if cfg.follow == Follow::Accept {
                Outcome::Ok(json!({
                    "command_id": id, "state": "accepted", "station_seq": 1,
                    "result": { "session_id": "s1", "input_id": "in-1" },
                    "reject": null, "at": 0,
                }))
            } else {
                rejected(&id, fleetdeck_proto::RejectCode::UnknownSession)
            }
        }
        _ => return None,
    })
}

pub fn serve(
    mut station: LiveLink,
    cfg: Arc<Mutex<Cfg>>,
    log: Log,
) -> fleetdeck_link::control::Control {
    let control = station.control.clone();
    tokio::spawn(async move {
        loop {
            let req = match station.inbound.recv().await {
                Some(Inbound::Request(r)) => r,
                Some(Inbound::Frame(_)) => continue,
                None => break,
            };
            log.lock()
                .unwrap()
                .push((req.method.clone(), req.params.clone()));
            let out = {
                let cfg = cfg.lock().unwrap();
                answer(&cfg, &req.method, &req.params, req.command_id.clone())
            };
            if let Some(out) = out {
                let _ = station.control.respond(req.id, out).await;
            }
        }
    });
    control
}

pub type Headers = Vec<(String, String)>;

pub fn refs(h: &Headers) -> Vec<(&str, &str)> {
    h.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

pub struct T {
    pub f: Fixture,
    pub fake: Fake,
    pub link: fleetdeck_link::control::Control,
    pub c: BoardClient,
    pub h: Headers,
}

impl T {
    pub fn st(&self) -> String {
        self.f.station.0.clone()
    }

    pub fn base(&self) -> String {
        format!("/api/sessions/{}/s1/review", self.st())
    }

    pub async fn get(&self, h: &Headers, path: &str) -> Http {
        self.c.get(path, &refs(h)).await
    }

    pub async fn post(&self, h: &Headers, path: &str, body: Value) -> Http {
        self.c.post(path, &refs(h), body).await
    }

    pub async fn patch(&self, h: &Headers, path: &str, body: Value) -> Http {
        self.c.req("PATCH", path, &refs(h), Some(body)).await
    }

    pub async fn delete(&self, h: &Headers, path: &str) -> Http {
        self.c.req("DELETE", path, &refs(h), None).await
    }

    /// A comment on `path` line `line` of the Since-spawn diff.
    pub async fn comment(
        &self,
        h: &Headers,
        path: &str,
        side: &str,
        line: u32,
        body: &str,
    ) -> Http {
        self.post(
            h,
            &format!("{}/comments", self.base()),
            json!({ "snapshot": format!("spawn:{BASE}"), "path": path, "side": side,
                    "line_start": line, "line_end": line, "body": body }),
        )
        .await
    }

    pub async fn send(&self, h: &Headers, key: &str, ids: &[&str]) -> Http {
        self.post(
            h,
            &format!("{}/send", self.base()),
            json!({ "send_key": key, "comment_ids": ids }),
        )
        .await
    }

    pub async fn again(&self, h: &Headers, review: &str, key: &str) -> Http {
        self.post(
            h,
            &format!("{}/reviews/{review}/send-again", self.base()),
            json!({ "attempt_key": key }),
        )
        .await
    }

    pub async fn list(&self, h: &Headers) -> Http {
        self.get(h, &self.base()).await
    }

    pub async fn wait_follow_ups(&self, n: usize) {
        let deadline = tokio::time::Instant::now() + T;
        while self.fake.calls("session.follow_up").len() < n {
            assert!(tokio::time::Instant::now() < deadline, "no follow-up");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    pub async fn review_state(&self, review: &str) -> String {
        let deadline = tokio::time::Instant::now() + T;
        loop {
            let r = self.list(&self.h).await;
            let state = r.body["reviews"]
                .as_array()
                .unwrap()
                .iter()
                .find(|x| x["review_id"] == review)
                .map(|x| x["delivery"].as_str().unwrap().to_owned())
                .unwrap_or_default();
            if state != "pending" || tokio::time::Instant::now() > deadline {
                return state;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

pub fn expect(r: &Http, status: u16, code: &str) {
    assert_eq!((r.status, r.code()), (status, code), "{}", r.body);
}

pub async fn owner(c: &BoardClient) -> Headers {
    let (cookie, csrf) = c.login().await;
    vec![
        ("Cookie".to_owned(), cookie),
        ("X-CSRF-Token".to_owned(), csrf),
    ]
}

pub async fn setup() -> T {
    let mut f = fixture().await;
    let station = f.connect().await;
    f.store
        .apply_snapshot(
            f.station.clone(),
            f.inc.clone(),
            0,
            vec![
                SessionSummary::new("s1".into()),
                SessionSummary::new("s2".into()),
            ],
        )
        .await
        .unwrap();
    let cfg = Arc::new(Mutex::new(Cfg {
        patch: PATCH.to_owned(),
        base: BASE.to_owned(),
        session_error: None,
        turns: HashMap::from([((FROM.to_owned(), TO.to_owned()), TURN_PATCH.to_owned())]),
        refuse_selector: false,
        deny: Some(Vec::new()),
        follow: Follow::Accept,
    }));
    let log = Log::default();
    let link = serve(station, Arc::clone(&cfg), Arc::clone(&log));
    let c = board(Arc::clone(&f.ctx)).await;
    let h = owner(&c).await;
    let station_id = f.station.clone();
    T {
        f,
        fake: Fake {
            cfg,
            log,
            station: station_id,
        },
        link,
        c,
        h,
    }
}

/// A person joins a share; returns the headers of the member.
pub async fn join(t: &T, share: Value, name: &str) -> Headers {
    let r = t.post(&t.h, "/api/shares", share).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let token = r.body["url"]
        .as_str()
        .unwrap()
        .split("#join=")
        .nth(1)
        .unwrap()
        .to_owned();
    let j =
        t.c.post(
            "/api/auth/join",
            &[],
            json!({ "token": token, "display_name": name }),
        )
        .await;
    assert_eq!(j.status, 200, "{}", j.body);
    let cookie = j
        .header("set-cookie")
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    vec![
        ("Cookie".to_owned(), cookie),
        (
            "X-CSRF-Token".to_owned(),
            j.body["csrf"].as_str().unwrap().to_owned(),
        ),
    ]
}

pub async fn member(t: &T, role: &str, name: &str) -> Headers {
    let share = json!({ "kind": "session", "target": format!("{}/s1", t.st()), "role": role });
    join(t, share, name).await
}

pub fn ids(list: &Http) -> Vec<String> {
    list.body["threads"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|t| t["comments"].as_array().unwrap().iter())
        .map(|c| c["comment_id"].as_str().unwrap().to_owned())
        .collect()
}

pub fn id(r: &Http) -> String {
    r.body["comment_id"].as_str().unwrap().to_owned()
}
