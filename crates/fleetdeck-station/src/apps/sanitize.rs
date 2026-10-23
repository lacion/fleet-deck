//! The sanitizing of the app metadata that a server sends (B15, design
//! `mcp-apps.md` §4.3 item 6): `_meta.ui.csp` and `_meta.ui.prefersBorder` of
//! the resource contents item. `frameDomains`, `baseUriDomains` and
//! `_meta.ui.permissions` are ignored: phase 1 grants none.

use fleetdeck_proto::apps::AppCsp;
use serde_json::Value;

/// The longest origin that is kept.
pub const MAX_ORIGIN_CHARS: usize = 256;
/// The most origins of one list.
pub const MAX_ORIGINS: usize = 32;

/// Does `text` match
/// `^(https?|wss?)://(\*\.)?[a-z0-9-]+(\.[a-z0-9-]+)*(:[0-9]{1,5})?$`
/// (case-insensitive) and have at most [`MAX_ORIGIN_CHARS`] characters?
#[must_use]
pub fn is_origin(text: &str) -> bool {
    if text.chars().count() > MAX_ORIGIN_CHARS {
        return false;
    }
    let lower = text.to_ascii_lowercase();
    let Some(rest) = ["https://", "http://", "wss://", "ws://"]
        .iter()
        .find_map(|scheme| lower.strip_prefix(scheme))
    else {
        return false;
    };
    let rest = rest.strip_prefix("*.").unwrap_or(rest);
    let (host, port) = match rest.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (rest, None),
    };
    if port.is_some_and(|p| p.is_empty() || p.len() > 5 || !p.bytes().all(|b| b.is_ascii_digit())) {
        return false;
    }
    host.split('.').all(|label| {
        !label.is_empty()
            && label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    })
}

/// The origins of `list` that [`is_origin`] accepts: in order, without
/// duplicates, at most [`MAX_ORIGINS`].
fn origins(list: Option<&Value>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in list.and_then(Value::as_array).into_iter().flatten() {
        if out.len() == MAX_ORIGINS {
            break;
        }
        if let Some(text) = item.as_str()
            && is_origin(text)
            && !out.iter().any(|kept| kept == text)
        {
            out.push(text.to_owned());
        }
    }
    out
}

/// The CSP of an app from the `_meta` object of its resource contents item.
#[must_use]
pub fn csp(meta: Option<&Value>) -> AppCsp {
    let csp = meta.and_then(|m| m.get("ui")).and_then(|u| u.get("csp"));
    AppCsp {
        connect_domains: origins(csp.and_then(|c| c.get("connectDomains"))),
        resource_domains: origins(csp.and_then(|c| c.get("resourceDomains"))),
    }
}

/// `_meta.ui.prefersBorder`, when it is a boolean.
#[must_use]
pub fn prefers_border(meta: Option<&Value>) -> Option<bool> {
    meta.and_then(|m| m.get("ui"))
        .and_then(|u| u.get("prefersBorder"))
        .and_then(Value::as_bool)
}
