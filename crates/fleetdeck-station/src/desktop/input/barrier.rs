//! The board-window barrier (R13 P7 §7.2): a window is refused as a target of
//! input when any available evidence identifies Fleet Deck. The evidence:
//!
//! - the station record is a wall (`Kind::Wall`);
//! - the current or initial class is `fleetdeck-wall`, `fleetdeck-wall-*`,
//!   `fleetdeck-board` or `fleetdeck-board-*`;
//! - the current or initial title contains `fleet deck` or `fleetdeck`
//!   (ASCII case-insensitive);
//! - a title contains the host of a known deck origin as a whole token;
//! - the trusted launch URL of the preview named a known deck origin
//!   ([`Marks`]; only that classification is kept, never the URL).
//!
//! The barrier is best effort. A title is content of the page, and the class
//! of a browser does not name its origin: a false negative stays possible, and
//! the barrier never claims that every board window is excluded (§7.2).

use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};

use crate::desktop::events::Kind;
use crate::desktop::{Fail, code};
use crate::store::Store;

/// The most marked handles the station keeps; the oldest goes first.
const MAX_MARKS: usize = 1024;

/// The preview handles whose launch URL named a known deck origin (§7.2: the
/// minimal board-origin classification of the preview provenance).
#[derive(Debug, Default)]
pub struct Marks {
    ids: Mutex<VecDeque<String>>,
}

impl Marks {
    /// Marks the handle `window_id`.
    pub fn mark(&self, window_id: &str) {
        let mut ids = self.ids.lock().unwrap_or_else(PoisonError::into_inner);
        if ids.iter().any(|id| id == window_id) {
            return;
        }
        if ids.len() >= MAX_MARKS {
            ids.pop_front();
        }
        ids.push_back(window_id.to_owned());
    }

    /// Whether the handle `window_id` is marked.
    #[must_use]
    pub fn is_marked(&self, window_id: &str) -> bool {
        self.ids
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|id| id == window_id)
    }
}

/// The names of a window that the barrier reads.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Names<'a> {
    pub class: &'a str,
    pub initial_class: &'a str,
    pub title: &'a str,
    pub initial_title: &'a str,
}

/// `fleetdeck-wall`, `fleetdeck-wall-*`, `fleetdeck-board` or
/// `fleetdeck-board-*`.
fn board_class(class: &str) -> bool {
    ["fleetdeck-wall", "fleetdeck-board"].iter().any(|base| {
        class == *base
            || class
                .strip_prefix(base)
                .is_some_and(|rest| rest.starts_with('-'))
    })
}

/// A character of a host name.
fn host_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '-')
}

/// Whether `title` contains `host` as a whole token. `host` is lower case.
/// The characters before the match must not belong to a host name; after it
/// a port (`:8443`) and a trailing dot are allowed, but another label
/// (`.evil`) or another character of a name is not.
fn has_host_token(title: &str, host: &str) -> bool {
    if host.is_empty() {
        return false;
    }
    // An IPv6 literal appears in brackets in a title; its brackets end it.
    if host.contains(':') {
        return title.to_ascii_lowercase().contains(&format!("[{host}]"));
    }
    let lower = title.to_ascii_lowercase();
    let mut from = 0;
    while let Some(found) = lower[from..].find(host) {
        let start = from + found;
        let end = start + host.len();
        let before_ok = lower[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !host_char(c));
        let after = &lower[end..];
        let mut chars = after.chars();
        let after_ok = match chars.next() {
            None => true,
            // A port follows the host.
            Some(':') => true,
            Some('.') => !chars.next().is_some_and(host_char),
            Some(c) => !host_char(c),
        };
        if before_ok && after_ok {
            return true;
        }
        from = start + 1;
        while !lower.is_char_boundary(from) {
            from += 1;
        }
    }
    false
}

fn board_title(title: &str, deck_hosts: &[String]) -> bool {
    let lower = title.to_ascii_lowercase();
    lower.contains("fleet deck")
        || lower.contains("fleetdeck")
        || deck_hosts.iter().any(|host| has_host_token(title, host))
}

/// Whether the window of a record of kind `kind` with the names `names` is a
/// Fleet Deck board or wall window, by any of the rules above. `deck_hosts`
/// are the lower case host names of the known deck origins.
#[must_use]
pub(crate) fn is_board(kind: Kind, names: &Names<'_>, deck_hosts: &[String]) -> bool {
    kind == Kind::Wall
        || board_class(names.class)
        || board_class(names.initial_class)
        || board_title(names.title, deck_hosts)
        || board_title(names.initial_title, deck_hosts)
}

/// The lower case host of a URL: the part after `://` up to `/`, `?` or `#`,
/// without user info and port; a bracketed IPv6 address keeps its brackets
/// out. `None` when there is no host.
#[must_use]
pub(crate) fn host_of_url(url: &str) -> Option<String> {
    let after = &url[url.find("://")? + 3..];
    let end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let authority = &after[..end];
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next().unwrap_or_default()
    } else {
        authority.split(':').next().unwrap_or_default()
    };
    let host = host.to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

/// The lower case hosts of the deck origins that this station knows from its
/// pairing records (the address that it dials), for the board barrier (R13
/// P7 §7.2). A station that is dialed by the deck knows none.
///
/// # Errors
/// `failed` when the pairing records cannot be read.
pub(crate) async fn deck_hosts(store: &Store) -> Result<Vec<String>, Fail> {
    let keys = store.paired_keys().await.map_err(|e| {
        tracing::warn!(error = %e, "the paired decks cannot be read");
        Fail::new(code::FAILED, "The station could not read the deck origins.")
    })?;
    let mut hosts: Vec<String> = keys
        .iter()
        .filter_map(|key| key.dial_url.as_deref())
        .filter_map(host_of_url)
        .collect();
    hosts.sort();
    hosts.dedup();
    Ok(hosts)
}

/// Whether the host of `url` is one of the known deck hosts (§7.2: the
/// launch URL of a preview; any route and any port).
#[must_use]
pub(crate) fn is_deck_origin(url: &str, deck_hosts: &[String]) -> bool {
    host_of_url(url).is_some_and(|host| deck_hosts.contains(&host))
}

#[cfg(test)]
#[path = "barrier_tests.rs"]
mod tests;
