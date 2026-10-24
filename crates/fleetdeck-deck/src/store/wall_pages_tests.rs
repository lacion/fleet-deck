//! Tests of the wall pages (B17 §3.2.2, §3.2.3): the V48 migration, the
//! validation of an entry, the limit of 50 and the delete transaction.

#![allow(clippy::unwrap_used)]

use fleetdeck_proto::wall_pages::{MAX_WALL_PAGES, NewWallPage, PageFit};
use fleetdeck_testkit::Det;
use rusqlite::Connection;

use super::{BOARD_TEXT, CheckedPage, HTTP_TEXT, LABEL_TEXT, URL_TEXT, check_page, origin_host};
use crate::store::displays::NewDisplay;
use crate::store::{DeckStore, call, schema};

const BOARD: &str = "https://board.example";

fn new(label: &str, url: &str) -> NewWallPage {
    NewWallPage {
        label: label.to_owned(),
        url: url.to_owned(),
        fit: PageFit::Screen,
    }
}

fn check(url: &str) -> Result<CheckedPage, &'static str> {
    check_page(new("Page", url), |origin| origin == BOARD)
}

async fn store() -> DeckStore {
    let det = Det::new();
    DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap()
}

/// Every row of `display_directives` as one JSON text, ordered by display.
fn directive_rows(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare(
            "SELECT json_array(display_id, generation, mode, sender_kind, sender_id, body,
                    created_at, sender_generation, sender_station, sender_session)
             FROM display_directives ORDER BY display_id",
        )
        .unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<String>>>()
        .unwrap()
}

fn insert_directive(conn: &Connection, row: [&str; 6]) {
    conn.execute(
        "INSERT INTO display_directives
             (display_id, generation, mode, sender_kind, sender_id, body, created_at,
              sender_generation, sender_station, sender_session)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        rusqlite::params![
            row[0],
            row[1].parse::<i64>().unwrap(),
            row[2],
            row[3],
            row[4],
            row[5],
            10,
            // The coordinator provenance only for a coordinator row.
            (row[3] == "coordinator").then_some(7_i64),
            (row[3] == "coordinator").then_some("st-2"),
            (row[3] == "coordinator").then_some("s-9"),
        ],
    )
    .unwrap();
}

#[test]
fn migration_v48_keeps_every_directive_row() {
    let migrations = schema::migrations();
    let mut conn = Connection::open_in_memory().unwrap();
    migrations.to_version(&mut conn, 47).unwrap();
    insert_directive(
        &conn,
        [
            "d1",
            "3",
            "focus",
            "owner",
            "",
            r#"{"directive":{"mode":"mission"}}"#,
        ],
    );
    insert_directive(&conn, ["d2", "4", "artifact", "station", "st-1", "{}"]);
    insert_directive(&conn, ["d3", "5", "mission", "coordinator", "g1", "{}"]);
    let before = directive_rows(&conn);
    assert_eq!(before.len(), 3);

    migrations.to_latest(&mut conn).unwrap();
    assert_eq!(directive_rows(&conn), before);
    // The table `wall_pages` exists and is empty.
    let pages: i64 = conn
        .query_row("SELECT count(*) FROM wall_pages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(pages, 0);
}

#[test]
fn migration_v48_accepts_mode_page() {
    let migrations = schema::migrations();
    let mut conn = Connection::open_in_memory().unwrap();
    migrations.to_version(&mut conn, 47).unwrap();
    let page = |conn: &Connection| {
        conn.execute(
            "INSERT INTO display_directives (display_id, generation, mode, sender_kind, body, created_at)
             VALUES ('d9', 1, 'page', 'owner', '{}', 1)",
            [],
        )
    };
    // V47 refuses the mode.
    assert!(page(&conn).is_err());
    migrations.to_latest(&mut conn).unwrap();
    page(&conn).unwrap();
    // The other checks stay.
    assert!(
        conn.execute(
            "INSERT INTO display_directives (display_id, generation, mode, sender_kind, body, created_at)
             VALUES ('d8', 1, 'nope', 'owner', '{}', 1)",
            [],
        )
        .is_err()
    );
    assert!(
        conn.execute(
            "INSERT INTO display_directives (display_id, generation, mode, sender_kind, body, created_at)
             VALUES ('d7', 1, 'page', 'robot', '{}', 1)",
            [],
        )
        .is_err()
    );
    assert!(
        conn.execute(
            "INSERT INTO wall_pages (page_id, label, url, origin, fit, created_at)
             VALUES ('p', 'l', 'https://a.example', 'https://a.example', 'wide', 1)",
            [],
        )
        .is_err()
    );
}

#[test]
fn page_validation_table() {
    // The label: trimmed, 1 to 60 characters, no control character.
    let too_long = "x".repeat(61);
    for label in ["", "   ", "a\u{7}b", too_long.as_str()] {
        let r = check_page(new(label, "https://a.example"), |_| false);
        assert_eq!(r, Err(LABEL_TEXT), "{label:?}");
    }
    assert_eq!(LABEL_TEXT, "Give a name of 1 to 60 characters.");
    let ok = check_page(new("  Grafana  ", "https://a.example"), |_| false).unwrap();
    assert_eq!(ok.label, "Grafana");
    let long = check_page(new(&"x".repeat(60), "https://a.example"), |_| false);
    assert!(long.is_ok());
    // The address: http or https, the station grammar.
    for url in [
        "",
        "ftp://a.example",
        "https://",
        "https://a b",
        "https://user:pass@a.example",
        "https://a.example:0",
        "https://a.example:65536",
        "javascript:alert(1)",
        "https://a.example/\u{e9}",
    ] {
        assert_eq!(check(url), Err(URL_TEXT), "{url}");
    }
    assert_eq!(URL_TEXT, "Give an http or https address.");
    // A plain http address of a host that is not loopback.
    assert_eq!(check("http://example.com"), Err(HTTP_TEXT));
    assert_eq!(
        HTTP_TEXT,
        "Use https for this address. A wall blocks plain http pages, except pages of its own machine (localhost)."
    );
    // The board.
    assert_eq!(check("https://board.example/x?y=1"), Err(BOARD_TEXT));
    assert_eq!(
        BOARD_TEXT,
        "This is the address of the board. A wall cannot show the board in a frame."
    );
    // The first failure answers: a bad label wins over a bad address.
    assert_eq!(check_page(new("", "ftp://a"), |_| true), Err(LABEL_TEXT));
    assert_eq!(check_page(new("a", "ftp://a"), |_| true), Err(URL_TEXT));
    assert_eq!(
        check_page(new("a", "http://a.example"), |_| true),
        Err(HTTP_TEXT)
    );
}

#[test]
fn http_is_refused_for_every_host_but_loopback() {
    for url in [
        "http://example.com",
        "http://192.168.1.20:3000",
        "http://10.0.0.5",
        "http://172.16.0.1",
        "http://100.100.1.1",
        "http://nas.local",
        "http://box.tail1234.ts.net",
        "HTTP://Example.com/x",
        "http://localhost.example.com",
        "http://notlocalhost",
        "http://[::2]",
        "http://128.0.0.1",
    ] {
        assert_eq!(check(url), Err(HTTP_TEXT), "{url}");
    }
}

#[test]
fn http_is_allowed_for_loopback() {
    for (url, origin) in [
        ("http://localhost", "http://localhost"),
        ("http://grafana.localhost", "http://grafana.localhost"),
        ("http://127.0.0.1:3000", "http://127.0.0.1:3000"),
        ("http://127.8.8.8", "http://127.8.8.8"),
        ("http://[::1]:8080", "http://[::1]:8080"),
        ("HTTP://LocalHost/a", "http://localhost"),
    ] {
        let page = check(url).unwrap();
        assert_eq!(page.origin, origin, "{url}");
        assert_eq!(page.url, url);
    }
}

#[test]
fn https_is_allowed_for_private_and_tailnet_hosts() {
    for url in [
        "https://example.com",
        "https://192.168.1.20:3000/d",
        "https://10.0.0.5",
        "https://172.16.0.1",
        "https://100.100.1.1",
        "https://nas.local",
        "https://box.tail1234.ts.net/path?q=1#h",
        "https://[fd7a:115c:a1e0::1]:8443",
    ] {
        assert!(check(url).is_ok(), "{url}");
    }
}

#[test]
fn a_board_origin_is_refused() {
    let boards = [
        "https://board.example",
        "https://127.0.0.1:8443",
        "https://[::1]:8443",
    ];
    for url in [
        "https://board.example",
        "https://BOARD.example:443/anything",
        "https://127.0.0.1:8443/x",
        "https://[::1]:8443",
    ] {
        let r = check_page(new("Board", url), |o| boards.contains(&o));
        assert_eq!(r, Err(BOARD_TEXT), "{url}");
    }
    // Another port or scheme is another origin.
    for url in ["https://board.example:8443", "https://127.0.0.1:8444"] {
        assert!(
            check_page(new("Other", url), |o| boards.contains(&o)).is_ok(),
            "{url}"
        );
    }
}

#[test]
fn origin_drops_the_default_port() {
    for (url, origin) in [
        ("https://Example.COM:443/a", "https://example.com"),
        ("https://example.com:0443", "https://example.com"),
        ("https://example.com:8443", "https://example.com:8443"),
        ("http://localhost:80", "http://localhost"),
        ("http://localhost:443", "http://localhost:443"),
        ("https://example.com:80", "https://example.com:80"),
        ("HTTPS://A.Example/x?Y=1#Z", "https://a.example"),
        ("https://[FD7A::1]:443", "https://[fd7a::1]"),
    ] {
        assert_eq!(check(url).unwrap().origin, origin, "{url}");
    }
    assert_eq!(origin_host("https://example.com:8443"), "example.com");
    assert_eq!(origin_host("http://[::1]:8080"), "[::1]");
    assert_eq!(origin_host("http://localhost"), "localhost");
}

#[tokio::test]
async fn the_fifty_first_page_is_refused() {
    let s = store().await;
    for n in 0..MAX_WALL_PAGES {
        let page = check(&format!("https://site{n}.example")).unwrap();
        let added = s.wall_page_add(page).await.unwrap().unwrap();
        assert_eq!(added.page_id.len(), 26);
    }
    let page = check("https://one-more.example").unwrap();
    assert_eq!(s.wall_page_add(page).await.unwrap(), None);
    let listed = s.wall_pages().await.unwrap();
    assert_eq!(listed.len(), MAX_WALL_PAGES);
    // Oldest first.
    assert_eq!(listed[0].origin, "https://site0.example");
    assert_eq!(listed[49].origin, "https://site49.example");
    // A delete makes room again.
    let first = listed[0].page_id.clone();
    assert_eq!(s.wall_page_delete(first).await.unwrap(), Some(0));
    let page = check("https://one-more.example").unwrap();
    assert!(s.wall_page_add(page).await.unwrap().is_some());
}

#[tokio::test]
async fn page_delete_resets_only_the_displays_that_show_it() {
    let s = store().await;
    let a = s
        .wall_page_add(check("https://a.example").unwrap())
        .await
        .unwrap()
        .unwrap();
    let b = s
        .wall_page_add(check("https://b.example").unwrap())
        .await
        .unwrap()
        .unwrap();
    let mut displays = Vec::new();
    for name in ["One", "Two", "Three"] {
        let d = s
            .create_display(NewDisplay {
                name: name.to_owned(),
                ceiling: Vec::new(),
            })
            .await
            .unwrap();
        displays.push(d.display_id);
    }
    let body = |page: &str| {
        format!(
            r#"{{"directive":{{"mode":"page","page":{{"page_id":"{page}"}},"pin":false,"filter":null}},"refs":[]}}"#
        )
    };
    let (one, two, three) = (
        displays[0].clone(),
        displays[1].clone(),
        displays[2].clone(),
    );
    let (body_a, body_b) = (body(&a.page_id), body(&b.page_id));
    let (one2, two2) = (one.clone(), two.clone());
    call(&s.inner.db, move |conn| {
        insert_directive(
            conn,
            [one2.as_str(), "1", "page", "owner", "", body_a.as_str()],
        );
        insert_directive(
            conn,
            [two2.as_str(), "1", "page", "owner", "", body_b.as_str()],
        );
        Ok(())
    })
    .await
    .unwrap();

    assert_eq!(
        s.wall_page_delete(a.page_id.clone()).await.unwrap(),
        Some(1)
    );
    // The deleted entry is gone; the other stays.
    let left = s.wall_pages().await.unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].page_id, b.page_id);
    // Display one returned to mission with a new generation.
    let view = s.display_directive(one).await.unwrap().unwrap();
    assert_eq!(view.generation, 1);
    assert_eq!(
        view.directive,
        fleetdeck_proto::display_directive::DirectiveBody::mission()
    );
    // Display two keeps its row and generation; display three never had one.
    let rows = call(&s.inner.db, |conn| Ok(directive_rows(conn)))
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].contains(&two), "{rows:?}");
    let view = s.display_directive(three).await.unwrap().unwrap();
    assert_eq!(view.generation, 0);
    // An unknown id answers `None` and changes nothing.
    assert_eq!(s.wall_page_delete("NOPE".to_owned()).await.unwrap(), None);
    assert_eq!(s.wall_pages().await.unwrap().len(), 1);
}
