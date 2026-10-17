//! The request bodies of the display routes that carry a managed launch or a
//! binding (R13 §5E item 4, §6D items 1 and 2).

use fleetdeck_proto::StationId;
use serde::Deserialize;

use crate::store::displays_pairing::ExpectedLaunch;

/// The most bytes of the body of `POST /api/displays/pair`.
pub const MAX_PAIR_BODY: usize = 1024;

/// The optional body of `POST /api/displays/pair`: the non-secret display
/// id and operation revision of a managed `wall.open` launch. The code and
/// the pending cookie stay the authorization.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PairBody {
    expected_display_id: Option<String>,
    operation_revision: Option<i64>,
}

/// A display id as the deck makes it: 26 characters of a ULID.
#[must_use]
pub fn is_display_id(id: &str) -> bool {
    id.len() == 26 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Parses the body of the pairing start. An empty body is no launch.
///
/// # Errors
/// A message for a body that is too long, not JSON, has an unknown field, a
/// display id that is not 26 letters or digits, a negative revision, or only
/// one of the two fields.
pub fn parse_pair_body(bytes: &[u8]) -> Result<Option<ExpectedLaunch>, &'static str> {
    if bytes.is_empty() {
        return Ok(None);
    }
    if bytes.len() > MAX_PAIR_BODY {
        return Err("the body is too long");
    }
    let body: PairBody = serde_json::from_slice(bytes).map_err(|_| "the body is not valid")?;
    match (body.expected_display_id, body.operation_revision) {
        (None, None) => Ok(None),
        (Some(id), Some(op)) if is_display_id(&id) && op >= 0 => Ok(Some(ExpectedLaunch {
            display_id: id,
            operation_revision: op,
        })),
        _ => Err("expected_display_id and operation_revision go together and must be valid"),
    }
}

/// The binding of the settings body: a station id and a monitor name.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BindingBody {
    pub station_id: StationId,
    pub monitor: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pair_body_is_optional_and_strict() {
        assert_eq!(parse_pair_body(b"").unwrap(), None);
        assert_eq!(parse_pair_body(b"{}").unwrap(), None);
        let id = "01J0000000000000000000ABCD";
        let ok = format!(r#"{{"expected_display_id":"{id}","operation_revision":3}}"#);
        let got = parse_pair_body(ok.as_bytes()).unwrap().unwrap();
        assert_eq!((got.display_id.as_str(), got.operation_revision), (id, 3));
        for bad in [
            r#"{"expected_display_id":"short","operation_revision":1}"#.to_owned(),
            format!(r#"{{"expected_display_id":"{id}"}}"#),
            r#"{"operation_revision":1}"#.to_owned(),
            format!(r#"{{"expected_display_id":"{id}","operation_revision":-1}}"#),
            format!(r#"{{"expected_display_id":"{id}","operation_revision":1,"x":1}}"#),
            "not json".to_owned(),
            format!("{{\"expected_display_id\":\"{}\"}}", "a".repeat(2000)),
        ] {
            assert!(parse_pair_body(bad.as_bytes()).is_err(), "{bad}");
        }
    }
}
