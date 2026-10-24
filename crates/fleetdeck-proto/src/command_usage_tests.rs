use super::*;
use serde_json::json;

#[test]
fn usage_recount_has_its_method_and_session() {
    let value = json!({"op": "usage_recount", "session_id": "s1"});
    let command: SessionCommand = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(command.method(), "session.usage_recount");
    assert_eq!(command.session_id().map(SessionId::as_str), Some("s1"));
    assert_eq!(serde_json::to_value(&command).unwrap(), value);
}
