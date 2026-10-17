//! The root thread identity of a Codex app-server (DL-194 §9.9).
//!
//! The thread-open answer states the first root identity. A later
//! `thread/started` is classified by its own metadata: the vendored 0.160.0
//! schema marks a sub-agent thread with `parentThreadId` or with a
//! `source` of `{"subAgent": ...}`. A thread that is not in `child_threads`
//! is not proof of a root.

use serde_json::Value;

use super::str_at;

/// The native id of the thread of a `thread/started` notification, when that
/// thread is a root thread; `None` for a sub-agent thread or a notification
/// without a thread id.
pub(super) fn root_thread_id(params: &Value) -> Option<String> {
    let thread = params.get("thread")?;
    let has_parent = thread
        .get("parentThreadId")
        .is_some_and(|parent| !parent.is_null());
    let sub_agent = thread
        .get("source")
        .and_then(Value::as_object)
        .is_some_and(|source| source.contains_key("subAgent"));
    if has_parent || sub_agent {
        return None;
    }
    str_at(thread, "id").map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::root_thread_id;

    #[test]
    fn a_plain_thread_is_a_root() {
        let p = json!({"thread": {"id": "t-1", "source": "appServer", "parentThreadId": null}});
        assert_eq!(root_thread_id(&p).as_deref(), Some("t-1"));
        let p = json!({"thread": {"id": "t-2", "source": {"custom": "x"}}});
        assert_eq!(root_thread_id(&p).as_deref(), Some("t-2"));
    }

    #[test]
    fn a_thread_with_a_parent_is_not_a_root() {
        let p = json!({"thread": {"id": "t-3", "parentThreadId": "t-1"}});
        assert_eq!(root_thread_id(&p), None);
    }

    #[test]
    fn a_sub_agent_source_is_not_a_root() {
        let p = json!({"thread": {"id": "t-4",
            "source": {"subAgent": {"thread_spawn": {"depth": 1, "parent_thread_id": "t-1"}}}}});
        assert_eq!(root_thread_id(&p), None);
    }

    #[test]
    fn a_notification_without_a_thread_id_has_no_identity() {
        assert_eq!(root_thread_id(&json!({"thread": {}})), None);
        assert_eq!(root_thread_id(&json!({})), None);
    }
}
