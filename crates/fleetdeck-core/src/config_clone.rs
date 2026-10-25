//! The host and organization rules of `clone_host` and `clone_org` in
//! `deck.toml`.

/// At least two labels of `[a-z0-9]([a-z0-9-]*[a-z0-9])?`, 1 to 253 bytes.
pub(super) fn valid_clone_host(host: &str) -> bool {
    let edge = |c: u8| c.is_ascii_lowercase() || c.is_ascii_digit();
    let label = |l: &str| {
        l.bytes().next().is_some_and(edge)
            && l.bytes().next_back().is_some_and(edge)
            && l.bytes().all(|c| edge(c) || c == b'-')
    };
    host.len() <= 253 && host.contains('.') && host.split('.').all(label)
}

/// At most 200 bytes of `[A-Za-z0-9._-]+` segments that a `/` separates.
pub(super) fn valid_clone_org(org: &str) -> bool {
    let name = |c: u8| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-');
    org.len() <= 200 && org.split('/').all(|s| !s.is_empty() && s.bytes().all(name))
}
