//! The link side of the coordinator endpoint (M3 6-5, ADR-006 §6.1, §6.6):
//! an authorized tool call goes to the connected deck as `coordinator.call`
//! (a mutation with a new `command_id`, decided once by the deck) or
//! `coordinator.query` (a read, no receipt). The envelope targets the
//! database incarnation that the deck's `hello` named.
//!
//! With a store ([`LinkForward::with_outbox`], DL-218) a call goes through
//! the durable station coordinator outbox of §6.5 (`super::outbox`).
//! Without one, and for every query, a missing deck fails at once with
//! `deck_unreachable`.
//!
//! `ask_coordinator` of a binding without a grant (a worker, R2.3) is a
//! call with an empty grant id. It never goes through the outbox: a deck
//! that is not connected is `unavailable`, and the deck answers with the
//! receipt that this module turns into a tool result or an error.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use fleetdeck_link::control::Control;
use fleetdeck_proto::StationId;
use fleetdeck_proto::fleet_view::{FleetView, FleetViewRequest};
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::mutation::{
    CoordinatorBinding, CoordinatorCall, CoordinatorCallEnvelope, CoordinatorQuery,
    CoordinatorQueryEnvelope,
};
use fleetdeck_proto::orchestration::MAX_WAIT_IDS;
use fleetdeck_proto::version::feature;
use fleetdeck_proto::{CommandId, IncarnationId, ProviderInstanceId, Receipt, Reject};
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::Value;

use super::bindings::{Bindings, Caller};
use super::desktop_forward::{DesktopRequest, McpToolResult};
use super::ingress::{FLEET_VIEW_UNSERVED, Forward};
use super::tools::to_op;

/// How long a forwarded call may be retried (`retry_until`, §6.5).
pub const CALL_RETRY: Duration = Duration::from_secs(15 * 60);
/// The longest question that `ask_coordinator` takes, in characters.
pub const MAX_QUESTION_CHARS: usize = 4000;
/// The deadline of a query; `wait_task` waits at most [`WAIT_LIMIT`].
pub const QUERY_LIMIT: Duration = Duration::from_secs(25);
pub const WAIT_LIMIT: Duration = Duration::from_secs(55);
/// The deadline of one `fleet.view` request (MCP P3 §3.1).
pub const FLEET_VIEW_LIMIT: Duration = Duration::from_secs(10);

/// The request path of a deck link (a [`Control`]; tests use a fake).
#[async_trait]
pub trait Transport: Send + Sync {
    /// Sends one request and waits for its outcome.
    async fn request(
        &self,
        method: &str,
        command_id: Option<&CommandId>,
        params: Value,
    ) -> Result<Outcome, String>;

    /// Whether the link negotiated `feature`; a fake has none by default.
    fn has_feature(&self, _feature: &str) -> bool {
        false
    }
}

#[async_trait]
impl Transport for Control {
    async fn request(
        &self,
        method: &str,
        command_id: Option<&CommandId>,
        params: Value,
    ) -> Result<Outcome, String> {
        Control::request(self, method, command_id, params)
            .await
            .map_err(|e| e.to_string())
    }

    fn has_feature(&self, feature: &str) -> bool {
        self.params().negotiated.has(feature)
    }
}

/// The deck link that carries coordinator traffic, when one is connected.
#[derive(Clone)]
pub(super) struct DeckLink {
    pub(super) transport: Arc<dyn Transport>,
    pub(super) incarnation: IncarnationId,
    serial: u64,
    /// The highest station sequence that the deck acknowledged on this
    /// link; a new link starts at 0 (the lifecycle fence of ADR-006 §6.5).
    acked: Arc<std::sync::atomic::AtomicU64>,
}

/// The clock of the envelopes (Unix ms).
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// Forwards tool calls over the current deck link.
#[derive(Clone)]
pub struct LinkForward {
    current: Arc<Mutex<Option<DeckLink>>>,
    /// The incarnation of the last deck link; an offline intake targets it.
    pub(super) last_incarnation: Arc<Mutex<Option<IncarnationId>>>,
    serial: Arc<std::sync::atomic::AtomicU64>,
    pub(super) clock: Clock,
    pub(super) outbox: Option<super::outbox::CallOutbox>,
    /// The bindings of the station: a deck link is also handed to them for
    /// `coordinator.authority_check` (ADR-008 §6.2).
    bindings: Option<Bindings>,
}

impl Default for LinkForward {
    fn default() -> Self {
        Self::with_clock(Arc::new(now_ms))
    }
}

impl std::fmt::Debug for LinkForward {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinkForward")
            .field("connected", &self.link().is_some())
            .finish()
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn ms(d: Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

pub(super) fn new_id(prefix: &str) -> Result<String, String> {
    let mut bytes = [0u8; 16];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| "internal: no randomness".to_owned())?;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("{prefix}{hex}"))
}

pub(super) fn binding(caller: &Caller) -> CoordinatorBinding {
    CoordinatorBinding {
        binding_id: caller.binding_id.clone(),
        grant_id: caller.grant_id.clone(),
        generation: caller.generation,
        provider_instance: ProviderInstanceId(caller.provider_instance.clone()),
        native_id: caller.native_id.clone(),
    }
}

/// The query of a read tool.
fn query(tool: &str, args: &Value) -> Result<(CoordinatorQuery, Duration), String> {
    match tool {
        "list_sessions" => Ok((CoordinatorQuery::ListSessions, QUERY_LIMIT)),
        "wait_task" => {
            let task_id = args
                .get("task_id")
                .and_then(Value::as_str)
                .ok_or_else(|| "invalid_params: `task_id` is required".to_owned())?;
            Ok((
                CoordinatorQuery::WaitTask {
                    task_id: task_id.to_owned(),
                },
                WAIT_LIMIT,
            ))
        }
        "wait_tasks" => {
            let ids: Option<Vec<String>> = args
                .get("task_ids")
                .and_then(Value::as_array)
                .map(|a| a.iter().map(|v| v.as_str().map(str::to_owned)).collect())
                .and_then(|ids: Vec<Option<String>>| ids.into_iter().collect());
            let ids = ids
                .filter(|ids| !ids.is_empty() && ids.len() <= MAX_WAIT_IDS)
                .ok_or_else(|| {
                    format!("invalid_params: `task_ids` has 1 to {MAX_WAIT_IDS} task ids")
                })?;
            Ok((CoordinatorQuery::WaitTasks { task_ids: ids }, WAIT_LIMIT))
        }
        "command_status" => {
            let command_id = args
                .get("command_id")
                .and_then(Value::as_str)
                .ok_or_else(|| "invalid_params: `command_id` is required".to_owned())?;
            Ok((
                CoordinatorQuery::CommandStatus {
                    command_id: CommandId::from(command_id),
                },
                QUERY_LIMIT,
            ))
        }
        other => super::fleet_tools::query(other, args).map(|q| (q, QUERY_LIMIT)),
    }
}

impl LinkForward {
    /// A forwarder whose envelopes use `clock` (tests use the deck's).
    #[must_use]
    pub fn with_clock(clock: Clock) -> Self {
        Self {
            current: Arc::default(),
            last_incarnation: Arc::default(),
            serial: Arc::default(),
            clock,
            outbox: None,
            bindings: None,
        }
    }

    /// This forwarder with the station bindings: every deck link that it
    /// connects is set on them, and cleared on disconnect.
    #[must_use]
    pub fn with_bindings(mut self, bindings: Bindings) -> Self {
        self.bindings = Some(bindings);
        self
    }

    /// A deck link came up: coordinator traffic goes to it. Returns the
    /// serial that [`Self::disconnect`] takes.
    pub fn connect(&self, control: Control, incarnation: IncarnationId) -> u64 {
        self.connect_transport(Arc::new(control), incarnation)
    }

    /// [`Self::connect`] over any [`Transport`].
    pub fn connect_transport(
        &self,
        transport: Arc<dyn Transport>,
        incarnation: IncarnationId,
    ) -> u64 {
        *self
            .last_incarnation
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(incarnation.clone());
        let serial = self
            .serial
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        if let Some(bindings) = &self.bindings {
            bindings.set_deck(serial, Arc::clone(&transport));
        }
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) = Some(DeckLink {
            transport,
            incarnation,
            serial,
            acked: Arc::default(),
        });
        serial
    }

    /// The deck acknowledged the station event stream through `seq` on the
    /// link of `serial`.
    pub fn acknowledged(&self, serial: u64, seq: u64) {
        if let Some(link) = self.link().filter(|l| l.serial == serial) {
            link.acked
                .fetch_max(seq, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// True while the deck of `link` has not acknowledged the last
    /// coordinator lifecycle event (ADR-006 §6.5, 25e): after a (re)connect
    /// no call and no query leaves before that acknowledgment. Without an
    /// outbox (unit tests) there is no store and no fence.
    pub(super) async fn fenced(&self, link: &DeckLink) -> Result<bool, String> {
        let Some(outbox) = &self.outbox else {
            return Ok(false);
        };
        let watermark = outbox
            .store
            .coordinator_lifecycle_seq()
            .await
            .map_err(|e| format!("internal: {e}"))?;
        Ok(link.acked.load(std::sync::atomic::Ordering::Relaxed) < watermark)
    }

    /// The deck link of `serial` ended; a newer link stays.
    pub fn disconnect(&self, serial: u64) {
        if let Some(bindings) = &self.bindings {
            bindings.clear_deck(serial);
        }
        let mut current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
        if current.as_ref().is_some_and(|l| l.serial == serial) {
            *current = None;
        }
    }

    pub(super) fn link(&self) -> Option<DeckLink> {
        self.current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The envelope and method of one tool call.
    ///
    /// # Errors
    /// `invalid_params` for arguments that do not map to an operation.
    pub fn envelope(
        caller: &Caller,
        tool: &str,
        args: &Value,
        incarnation: &IncarnationId,
        now: i64,
    ) -> Result<(&'static str, Option<CommandId>, Value), String> {
        match to_op(tool, args)? {
            Some(op) => {
                let command_id = CommandId::from(new_id("coord-")?);
                let envelope = CoordinatorCallEnvelope {
                    command_id: command_id.clone(),
                    target_incarnation: incarnation.clone(),
                    retry_until: now + ms(CALL_RETRY),
                    call: CoordinatorCall {
                        caller: caller.session_id.clone(),
                        binding: binding(caller),
                        idempotency_key: None,
                        op,
                    },
                };
                let v = serde_json::to_value(envelope).map_err(|e| format!("internal: {e}"))?;
                Ok(("coordinator.call", Some(command_id), v))
            }
            None => {
                let (query, limit) = query(tool, args)?;
                let envelope = CoordinatorQueryEnvelope {
                    query_id: new_id("q-")?,
                    target_incarnation: incarnation.clone(),
                    caller: caller.session_id.clone(),
                    binding: binding(caller),
                    deadline: now + ms(limit),
                    query,
                };
                let v = serde_json::to_value(envelope).map_err(|e| format!("internal: {e}"))?;
                Ok(("coordinator.query", None, v))
            }
        }
    }
}

/// The tool error of a rejected receipt. `no_coordinator` and
/// `rate_limited` are named in the deck's message; any other rejection
/// is `<code>: <message>`.
fn rejection(reject: &Reject) -> String {
    let message = reject.message.as_str();
    if ["no_coordinator:", "rate_limited:"]
        .iter()
        .any(|p| message.starts_with(p))
    {
        return message.to_owned();
    }
    let code = serde_json::to_value(reject.code)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "rejected".to_owned());
    format!("{code}: {message}")
}

impl LinkForward {
    /// `ask_coordinator` of a worker without a grant (R2.3): one direct
    /// call to the deck, never queued.
    async fn ask_without_grant(&self, caller: Caller, args: Value) -> Result<Value, String> {
        let question = args.get("question").and_then(Value::as_str);
        if question.is_none_or(|q| q.trim().is_empty() || q.chars().count() > MAX_QUESTION_CHARS) {
            return Err(format!(
                "invalid_params: `question` has 1 to {MAX_QUESTION_CHARS} characters"
            ));
        }
        let Some(link) = self.link() else {
            return Err("unavailable: the deck is not connected".to_owned());
        };
        let reply = match self
            .call_direct(&link, caller, "ask_coordinator", args)
            .await
        {
            Err(e) if e.starts_with("deck_unreachable") => {
                return Err("unavailable: the deck is not connected".to_owned());
            }
            other => other?,
        };
        let receipt: Receipt = serde_json::from_value(reply.clone())
            .map_err(|e| format!("internal: bad receipt: {e}"))?;
        match receipt.reject {
            Some(reject) => Err(rejection(&reject)),
            None => Ok(reply),
        }
    }

    /// One call over `link` without the outbox and without the lifecycle
    /// fence.
    async fn call_direct(
        &self,
        link: &DeckLink,
        caller: Caller,
        tool: &str,
        args: Value,
    ) -> Result<Value, String> {
        let (method, command_id, params) =
            Self::envelope(&caller, tool, &args, &link.incarnation, (self.clock)())?;
        match link
            .transport
            .request(method, command_id.as_ref(), params)
            .await
        {
            Ok(Outcome::Ok(v)) => Ok(v),
            Ok(Outcome::Err(e)) => Err(format!("{}: {}", e.code, e.message)),
            Err(e) => Err(format!("deck_unreachable: {e}")),
        }
    }
}

#[async_trait]
impl Forward for LinkForward {
    async fn call_desktop(&self, caller: Caller, request: DesktopRequest) -> McpToolResult {
        self.desktop(caller, request).await
    }

    async fn call(&self, caller: Caller, tool: &str, args: Value) -> Result<Value, String> {
        if caller.grant_id.is_empty() {
            return match tool {
                "ask_coordinator" => self.ask_without_grant(caller, args).await,
                other => Err(format!("forbidden: no such tool `{other}`")),
            };
        }
        if let Some(outbox) = &self.outbox
            && to_op(tool, &without_key(&args))?.is_some()
        {
            return self.durable_call(outbox, caller, tool, args).await;
        }
        let Some(link) = self.link() else {
            return Err("deck_unreachable: no deck is connected".to_owned());
        };
        if self.fenced(&link).await? {
            return Err(
                "deck_syncing: the deck has not acknowledged the coordinator \
                        lifecycle yet; call again"
                    .to_owned(),
            );
        }
        self.call_direct(&link, caller, tool, args).await
    }

    async fn fleet_view(
        &self,
        caller: Caller,
        view: FleetView,
        cursor: Option<String>,
        station: Option<StationId>,
    ) -> Result<Value, String> {
        let Some(link) = self.link() else {
            return Err("unavailable: the deck is not connected".to_owned());
        };
        // A link without the feature gets no request at all.
        if !link.transport.has_feature(feature::FLEET_VIEW) {
            return Err(FLEET_VIEW_UNSERVED.to_owned());
        }
        let params = serde_json::to_value(FleetViewRequest {
            caller: caller.session_id,
            view,
            cursor,
            station,
        })
        .map_err(|e| format!("internal: {e}"))?;
        // No outbox and no lifecycle fence: a read with a fixed deadline.
        let sent = link.transport.request("fleet.view", None, params);
        match tokio::time::timeout(FLEET_VIEW_LIMIT, sent).await {
            Err(_) => Err("deck_unreachable: the fleet view timed out".to_owned()),
            Ok(Err(e)) => Err(format!("deck_unreachable: {e}")),
            Ok(Ok(Outcome::Err(e))) => Err(format!("{}: {}", e.code, e.message)),
            Ok(Ok(Outcome::Ok(v))) => Ok(v),
        }
    }
}

/// The arguments without the outbox parameter `idempotency_key`.
pub(super) fn without_key(args: &Value) -> Value {
    let mut args = args.clone();
    if let Some(obj) = args.as_object_mut() {
        obj.remove("idempotency_key");
    }
    args
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use serde_json::json;

    fn caller() -> Caller {
        Caller {
            binding_id: "b-1".to_owned(),
            grant_id: "g-1".to_owned(),
            session_id: "coord-1".into(),
            provider_instance: "claude".to_owned(),
            native_id: "native-1".to_owned(),
            generation: 2,
            input_any: false,
        }
    }

    #[test]
    fn a_mutating_tool_becomes_a_call_envelope_and_a_read_tool_a_query_of_the_deck_incarnation() {
        let inc: IncarnationId = "deck-inc".to_owned().into();
        let args = json!({"to": {"station": "st", "session_id": "s1"}, "text": "hi"});
        let (method, id, v) =
            LinkForward::envelope(&caller(), "send_mail", &args, &inc, 1_000).unwrap();
        assert_eq!(method, "coordinator.call");
        let env: CoordinatorCallEnvelope = serde_json::from_value(v).unwrap();
        assert_eq!(Some(env.command_id.clone()), id);
        assert!(env.command_id.as_str().starts_with("coord-"));
        assert_eq!(env.target_incarnation, inc);
        assert_eq!(env.retry_until, 901_000);
        assert_eq!(env.call.caller.as_str(), "coord-1");
        assert_eq!(env.call.binding.generation, 2);
        assert_eq!(env.call.binding.native_id, "native-1");

        let (method, id, v) = LinkForward::envelope(
            &caller(),
            "wait_task",
            &json!({"task_id": "t1"}),
            &inc,
            1_000,
        )
        .unwrap();
        assert_eq!(method, "coordinator.query");
        assert!(id.is_none());
        let q: CoordinatorQueryEnvelope = serde_json::from_value(v).unwrap();
        assert_eq!(
            q.query,
            CoordinatorQuery::WaitTask {
                task_id: "t1".into()
            }
        );
        assert_eq!(q.deadline, 56_000);
        assert_eq!(q.target_incarnation, inc);
        // Two calls never share a command id.
        let a = LinkForward::envelope(&caller(), "send_mail", &args, &inc, 1)
            .unwrap()
            .1;
        let b = LinkForward::envelope(&caller(), "send_mail", &args, &inc, 1)
            .unwrap()
            .1;
        assert_ne!(a, b);
        // A missing task id is refused before anything is sent.
        assert!(LinkForward::envelope(&caller(), "wait_task", &json!({}), &inc, 1).is_err());

        // `wait_tasks` is one long-poll query over its task ids.
        let (method, _, v) = LinkForward::envelope(
            &caller(),
            "wait_tasks",
            &json!({"task_ids": ["t1", "t2"]}),
            &inc,
            1_000,
        )
        .unwrap();
        assert_eq!(method, "coordinator.query");
        let q: CoordinatorQueryEnvelope = serde_json::from_value(v).unwrap();
        assert_eq!(
            q.query,
            CoordinatorQuery::WaitTasks {
                task_ids: vec!["t1".into(), "t2".into()]
            }
        );
        assert_eq!(q.deadline, 56_000);
        for bad in [json!({}), json!({"task_ids": []}), json!({"task_ids": [1]})] {
            assert!(LinkForward::envelope(&caller(), "wait_tasks", &bad, &inc, 1).is_err());
        }
    }

    #[test]
    fn command_status_is_a_query_with_the_given_command_id() {
        let inc: IncarnationId = "deck-inc".to_owned().into();
        let (method, id, v) = LinkForward::envelope(
            &caller(),
            "command_status",
            &json!({"command_id": "coord-abc"}),
            &inc,
            1_000,
        )
        .unwrap();
        assert_eq!(method, "coordinator.query");
        assert!(id.is_none());
        let q: CoordinatorQueryEnvelope = serde_json::from_value(v).unwrap();
        assert_eq!(
            q.query,
            CoordinatorQuery::CommandStatus {
                command_id: "coord-abc".into()
            }
        );
        assert_eq!(q.deadline, 26_000);
        for bad in [json!({}), json!({"command_id": 1})] {
            let e = LinkForward::envelope(&caller(), "command_status", &bad, &inc, 1).unwrap_err();
            assert_eq!(e, "invalid_params: `command_id` is required");
        }
    }

    #[tokio::test]
    async fn without_a_deck_a_call_fails_at_once_as_unreachable() {
        let f = LinkForward::default();
        let r = f.call(caller(), "list_sessions", json!({})).await;
        assert!(r.unwrap_err().starts_with("deck_unreachable"));
    }

    struct Deck {
        sent: Mutex<Vec<Value>>,
        reply: Mutex<Reject>,
        accept: bool,
    }

    #[async_trait]
    impl Transport for Deck {
        async fn request(
            &self,
            method: &str,
            command_id: Option<&CommandId>,
            params: Value,
        ) -> Result<Outcome, String> {
            assert_eq!(method, "coordinator.call");
            self.sent.lock().unwrap().push(params);
            let id = command_id.cloned().unwrap();
            let receipt = if self.accept {
                Receipt::accepted(id, 0, fleetdeck_proto::command::CommandResult::default(), 1)
            } else {
                Receipt::rejected(id, self.reply.lock().unwrap().clone(), 1)
            };
            Ok(Outcome::Ok(serde_json::to_value(receipt).unwrap()))
        }
    }

    fn worker() -> Caller {
        Caller {
            binding_id: "w-1".to_owned(),
            grant_id: String::new(),
            session_id: "worker-1".into(),
            provider_instance: "claude".to_owned(),
            native_id: "native-w".to_owned(),
            generation: 1,
            input_any: false,
        }
    }

    fn deck(accept: bool, code: fleetdeck_proto::RejectCode, message: &str) -> Arc<Deck> {
        Arc::new(Deck {
            sent: Mutex::default(),
            reply: Mutex::new(Reject::new(code, message)),
            accept,
        })
    }

    #[tokio::test]
    async fn a_worker_without_a_grant_asks_the_deck_with_an_empty_grant_id() {
        let f = LinkForward::default();
        let d = deck(true, fleetdeck_proto::RejectCode::Forbidden, "");
        f.connect_transport(d.clone(), "deck-inc".to_owned().into());
        let q = json!({"question": "which branch?"});
        let r = f.call(worker(), "ask_coordinator", q).await.unwrap();
        assert_eq!(r["state"], "accepted");
        let sent = d.sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 1);
        let env: CoordinatorCallEnvelope = serde_json::from_value(sent[0].clone()).unwrap();
        assert_eq!(env.call.caller.as_str(), "worker-1");
        assert!(env.call.binding.grant_id.is_empty());
        assert_eq!(
            env.call.op,
            fleetdeck_proto::mutation::CoordinatorOp::AskCoordinator {
                question: "which branch?".to_owned()
            }
        );
        // The question is checked before anything is sent.
        for bad in [
            json!({}),
            json!({"question": " "}),
            json!({"question": "x".repeat(4001)}),
        ] {
            let e = f.call(worker(), "ask_coordinator", bad).await.unwrap_err();
            assert!(e.starts_with("invalid_params"), "{e}");
        }
        // A worker has no other tool.
        let e = f
            .call(worker(), "list_sessions", json!({}))
            .await
            .unwrap_err();
        assert!(e.starts_with("forbidden"), "{e}");
        assert_eq!(d.sent.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_rejected_question_names_its_reason_and_a_missing_deck_is_unavailable() {
        use fleetdeck_proto::RejectCode;
        let q = json!({"question": "which branch?"});
        let f = LinkForward::default();
        let e = f
            .call(worker(), "ask_coordinator", q.clone())
            .await
            .unwrap_err();
        assert_eq!(e, "unavailable: the deck is not connected");
        for (code, message, expected) in [
            (
                RejectCode::InvalidState,
                "no_coordinator: no open task of this session has a coordinator",
                "no_coordinator: no open task of this session has a coordinator",
            ),
            (
                RejectCode::InvalidState,
                "rate_limited: at most 10 questions per session per hour",
                "rate_limited: at most 10 questions per session per hour",
            ),
            (
                RejectCode::Forbidden,
                "the caller has no coordinator grant",
                "forbidden: the caller has no coordinator grant",
            ),
        ] {
            let f = LinkForward::default();
            f.connect_transport(deck(false, code, message), "deck-inc".to_owned().into());
            let e = f
                .call(worker(), "ask_coordinator", q.clone())
                .await
                .unwrap_err();
            assert_eq!(e, expected);
        }
    }
}
