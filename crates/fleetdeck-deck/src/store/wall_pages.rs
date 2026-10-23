//! The wall pages of a deck (B17 §3.2.2, §3.2.3): the allow-list of web
//! pages that the owner lets a wall show. The tables are made by the V48
//! migration ([`super::wall_pages_schema`]).
//!
//! - [`check_page`] validates a new entry (the first failure answers with
//!   its fixed text) and works out its origin;
//! - [`DeckStore::wall_page_add`] stores a checked entry (at most 50);
//! - [`DeckStore::wall_page_delete`] removes an entry and resets, in the
//!   same transaction, every display whose stored directive shows it.

use std::net::{Ipv4Addr, Ipv6Addr};

use fleetdeck_proto::desktop_text;
use fleetdeck_proto::wall_pages::{MAX_PAGE_LABEL, MAX_WALL_PAGES, NewWallPage, PageFit, WallPage};
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::display_directive_schema::invalidate_directive;
use super::{DeckStore, Result, call};

#[cfg(test)]
#[path = "wall_pages_tests.rs"]
mod tests;

/// The text of a refused label.
pub const LABEL_TEXT: &str = "Give a name of 1 to 60 characters.";
/// The text of a refused address.
pub const URL_TEXT: &str = "Give an http or https address.";
/// The text of a plain `http` address of a host that is not loopback.
pub const HTTP_TEXT: &str = "Use https for this address. A wall blocks plain http pages, except pages of its own machine (localhost).";
/// The text of the address of the board.
pub const BOARD_TEXT: &str =
    "This is the address of the board. A wall cannot show the board in a frame.";

/// A new entry that passed [`check_page`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedPage {
    /// Trimmed.
    pub label: String,
    pub url: String,
    pub origin: String,
    pub fit: PageFit,
}

/// Validates a new entry, in the order of §3.2.3. `is_board_origin` tells
/// whether an origin is one of the origins of the board. The error is the
/// text for the owner (`400 invalid_page`). The limit of 50 entries is
/// checked by [`DeckStore::wall_page_add`].
///
/// # Errors
/// The fixed text of the first check that fails.
pub fn check_page(
    new: NewWallPage,
    is_board_origin: impl Fn(&str) -> bool,
) -> std::result::Result<CheckedPage, &'static str> {
    let label = new.label.trim();
    if label.is_empty()
        || label.chars().count() > MAX_PAGE_LABEL
        || label.chars().any(char::is_control)
    {
        return Err(LABEL_TEXT);
    }
    let Some(url) = desktop_text::url(&new.url) else {
        return Err(URL_TEXT);
    };
    let (scheme, host, port) = split(url).ok_or(URL_TEXT)?;
    if scheme == "http" && !is_loopback_host(&host) {
        return Err(HTTP_TEXT);
    }
    let origin = origin_of(scheme, &host, port);
    if is_board_origin(&origin) {
        return Err(BOARD_TEXT);
    }
    Ok(CheckedPage {
        label: label.to_owned(),
        url: url.to_owned(),
        origin,
        fit: new.fit,
    })
}

/// The scheme (`http` or `https`, lower case), the host (lower case, with
/// the brackets of an IPv6 address) and the port of a URL that
/// [`desktop_text::url`] accepted.
fn split(url: &str) -> Option<(&'static str, String, Option<u16>)> {
    let (scheme, rest) = if let Some(rest) = strip_scheme(url, "https://") {
        ("https", rest)
    } else {
        ("http", strip_scheme(url, "http://")?)
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let (host, port) = if authority.starts_with('[') {
        let (address, tail) = authority.split_once(']')?;
        (format!("{address}]"), tail.strip_prefix(':'))
    } else {
        match authority.split_once(':') {
            Some((h, p)) => (h.to_owned(), Some(p)),
            None => (authority.to_owned(), None),
        }
    };
    let port = match port {
        Some(p) => Some(p.parse::<u16>().ok()?),
        None => None,
    };
    Some((scheme, host.to_ascii_lowercase(), port))
}

/// `url` without the prefix `prefix`, which is matched in any letter case
/// (`url` is ASCII: [`desktop_text::url`] checked it).
fn strip_scheme<'a>(url: &'a str, prefix: &str) -> Option<&'a str> {
    url.get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .map(|_| &url[prefix.len()..])
}

/// `scheme://host[:port]`: the port only when it is not the default.
fn origin_of(scheme: &str, host: &str, port: Option<u16>) -> String {
    let default = if scheme == "https" { 443 } else { 80 };
    match port {
        Some(p) if p != default => format!("{scheme}://{host}:{p}"),
        _ => format!("{scheme}://{host}"),
    }
}

/// The hosts that a browser trusts as secure over plain `http`: `localhost`,
/// a name that ends in `.localhost`, an IPv4 address in 127.0.0.0/8 and
/// `[::1]`.
fn is_loopback_host(host: &str) -> bool {
    if host == "localhost" || host.ends_with(".localhost") {
        return true;
    }
    if let Some(inner) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        return inner
            .parse::<Ipv6Addr>()
            .is_ok_and(|a| a == Ipv6Addr::LOCALHOST);
    }
    host.parse::<Ipv4Addr>().is_ok_and(|a| a.is_loopback())
}

/// The host of an origin, without the scheme and the port (for an audit
/// row).
#[must_use]
pub fn origin_host(origin: &str) -> &str {
    let rest = origin.split_once("://").map_or(origin, |(_, r)| r);
    if rest.starts_with('[') {
        return rest
            .split_once(']')
            .map_or(rest, |(h, _)| &rest[..=h.len()]);
    }
    rest.split(':').next().unwrap_or(rest)
}

fn fit_text(fit: PageFit) -> &'static str {
    match fit {
        PageFit::Screen => "screen",
        PageFit::Long => "long",
    }
}

const COLUMNS: &str = "page_id, label, url, origin, fit, created_at";

fn row(r: &Row<'_>) -> rusqlite::Result<WallPage> {
    let fit = match r.get::<_, String>(4)?.as_str() {
        "screen" => PageFit::Screen,
        "long" => PageFit::Long,
        _ => {
            return Err(rusqlite::Error::InvalidColumnType(
                4,
                "fit".to_owned(),
                rusqlite::types::Type::Text,
            ));
        }
    };
    Ok(WallPage {
        page_id: r.get(0)?,
        label: r.get(1)?,
        url: r.get(2)?,
        origin: r.get(3)?,
        fit,
        created_at: r.get(5)?,
    })
}

/// The entry `page_id`, if it exists.
///
/// # Errors
/// A database error.
pub fn find(conn: &Connection, page_id: &str) -> Result<Option<WallPage>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM wall_pages WHERE page_id = ?1"),
            params![page_id],
            row,
        )
        .optional()?)
}

impl DeckStore {
    /// The entries, oldest first.
    ///
    /// # Errors
    /// A database error.
    pub async fn wall_pages(&self) -> Result<Vec<WallPage>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM wall_pages ORDER BY created_at, page_id"
            ))?;
            let rows = stmt.query_map([], row)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    /// Stores a checked entry with a new id. `None`: the deck holds 50
    /// entries already.
    ///
    /// # Errors
    /// A database error.
    pub async fn wall_page_add(&self, new: CheckedPage) -> Result<Option<WallPage>> {
        let id: String = fleetdeck_core::ids::new_id(self.inner.ids.as_ref());
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let held: i64 = tx.query_row("SELECT count(*) FROM wall_pages", [], |r| r.get(0))?;
            if usize::try_from(held).unwrap_or(usize::MAX) >= MAX_WALL_PAGES {
                return Ok(None);
            }
            tx.execute(
                "INSERT INTO wall_pages (page_id, label, url, origin, fit, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, new.label, new.url, new.origin, fit_text(new.fit), now],
            )?;
            tx.commit()?;
            Ok(Some(WallPage {
                page_id: id,
                label: new.label,
                url: new.url,
                origin: new.origin,
                fit: new.fit,
                created_at: now,
            }))
        })
        .await
    }

    /// Removes the entry `page_id` and, in the same transaction, resets
    /// every display whose stored directive shows it (the display returns to
    /// `mission` and its generation moves up). Answers the number of
    /// displays that were reset; `None`: no such entry.
    ///
    /// # Errors
    /// A database error.
    pub async fn wall_page_delete(&self, page_id: String) -> Result<Option<usize>> {
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let n = tx.execute(
                "DELETE FROM wall_pages WHERE page_id = ?1",
                params![page_id],
            )?;
            if n == 0 {
                return Ok(None);
            }
            let shown: Vec<String> = {
                // `json_valid` guards `json_extract`: a row that is not JSON
                // never matches (and never fails the query).
                let mut stmt = tx.prepare(
                    "SELECT display_id FROM display_directives
                     WHERE mode = 'page'
                       AND CASE WHEN json_valid(body)
                                THEN json_extract(body, '$.directive.page.page_id') END = ?1",
                )?;
                let rows = stmt.query_map(params![page_id], |r| r.get(0))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            let mut reset = 0;
            for display_id in &shown {
                if invalidate_directive(&tx, display_id)? {
                    reset += 1;
                }
            }
            tx.commit()?;
            Ok(Some(reset))
        })
        .await
    }
}
