//! The content security policy of an MCP App document and the second
//! sanitizing of its domains (B15, design `mcp-apps.md` §4.7).

use fleetdeck_proto::apps::AppCsp;

/// The longest origin that is kept.
const MAX_ORIGIN_CHARS: usize = 256;
/// The most origins of one list.
const MAX_ORIGINS: usize = 32;

/// Does `text` match
/// `^(https?|wss?)://(\*\.)?[a-z0-9-]+(\.[a-z0-9-]+)*(:[0-9]{1,5})?$`
/// (case-insensitive) and have at most [`MAX_ORIGIN_CHARS`] characters? The
/// same rule as `is_origin` of the station (defense in depth).
pub(super) fn is_origin(text: &str) -> bool {
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
fn origins(list: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for text in list {
        if out.len() == MAX_ORIGINS {
            break;
        }
        if is_origin(text) && !out.contains(text) {
            out.push(text.clone());
        }
    }
    out
}

/// The CSP domains of `csp` after the second sanitizing.
pub(super) fn sanitize(csp: &AppCsp) -> AppCsp {
    AppCsp {
        connect_domains: origins(&csp.connect_domains),
        resource_domains: origins(&csp.resource_domains),
    }
}

/// The `Content-Security-Policy` header of an app document. There is no
/// `'self'` in a fetch directive: the app must not reach the deck API.
pub(super) fn header(csp: &AppCsp) -> String {
    let r = if csp.resource_domains.is_empty() {
        String::new()
    } else {
        format!(" {}", csp.resource_domains.join(" "))
    };
    let c = if csp.connect_domains.is_empty() {
        "'none'".to_owned()
    } else {
        csp.connect_domains.join(" ")
    };
    format!(
        "sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline'{r}; \
         style-src 'unsafe-inline'{r}; img-src data: blob:{r}; font-src data:{r}; \
         media-src data: blob:{r}; connect-src {c}; frame-src 'none'; object-src 'none'; \
         base-uri 'none'; form-action 'none'; frame-ancestors 'self'"
    )
}
