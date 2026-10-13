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

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use fleetdeck_link::control::Control;
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::mutation::{
    CoordinatorBinding, CoordinatorCall, CoordinatorCallEnvelope, CoordinatorQuery,
    CoordinatorQueryEnvelope,
};
use fleetdeck_proto::orchestration::MAX_WAIT_IDS;
use fleetdeck_proto::{CommandId, IncarnationId, ProviderInstanceId};
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::Value;

use super::bindings::Caller;
use super::ingress::Forward;
use super::tools::to_op;

/// How long a forwarded call may be retried (`retry_until`, §6.5).
pub const CALL_RETRY: Duration = Duration::from_secs(15 * 60);
/// The deadline of a query; `wait_task` waits at most [`WAIT_LIMIT`].
pub const QUERY_LIMIT: Duration = Duration::from_secs(25);
pub const WAIT_LIMIT: Duration = Duration::from_secs(55);

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

fn new_id(prefix: &str) -> Result<String, String> {
    let mut bytes = [0u8; 16];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| "internal: no randomness".to_owned())?;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("{prefix}{hex}"))
}

fn binding(caller: &Caller) -> CoordinatorBinding {
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
        other => Err(format!("invalid_params: no read tool `{other}`")),
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
        }
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

#[async_trait]
impl Forward for LinkForward {
    async fn call(&self, caller: Caller, tool: &str, args: Value) -> Result<Value, String> {
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

    #[tokio::test]
    async fn without_a_deck_a_call_fails_at_once_as_unreachable() {
        let f = LinkForward::default();
        let r = f.call(caller(), "list_sessions", json!({})).await;
        assert!(r.unwrap_err().starts_with("deck_unreachable"));
    }
}
