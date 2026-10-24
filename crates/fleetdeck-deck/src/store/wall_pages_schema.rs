//! The V48 migration of the wall pages (B17 §3.2.2): the table `wall_pages`
//! and the rebuild of `display_directives`, whose `mode` check gains
//! `'page'`. SQLite cannot change a `CHECK`, so the table is made again.
//!
//! `display_directives` has no index, trigger or foreign key (V25, V34), so
//! the rebuild loses nothing.
//!
//! To reverse the migration: delete the `page` rows of `display_directives`,
//! rebuild the table again without `'page'` in the check, and drop
//! `wall_pages`.

/// The migration (V48).
pub(super) const SQL: &str = "
CREATE TABLE wall_pages (
    page_id    TEXT    PRIMARY KEY,
    label      TEXT    NOT NULL,
    url        TEXT    NOT NULL,
    origin     TEXT    NOT NULL,
    fit        TEXT    NOT NULL CHECK (fit IN ('screen', 'long')),
    created_at INTEGER NOT NULL
) STRICT;

CREATE TABLE display_directives_v48 (
    display_id        TEXT    PRIMARY KEY,
    generation        INTEGER NOT NULL,
    mode              TEXT    NOT NULL
        CHECK (mode IN ('mission', 'focus', 'review', 'race', 'artifact', 'ambient', 'page')),
    sender_kind       TEXT    NOT NULL CHECK (sender_kind IN ('owner', 'station', 'coordinator')),
    sender_id         TEXT    NOT NULL DEFAULT '',
    body              TEXT    NOT NULL,
    created_at        INTEGER NOT NULL,
    sender_generation INTEGER,
    sender_station    TEXT,
    sender_session    TEXT
) STRICT;
INSERT INTO display_directives_v48
    (display_id, generation, mode, sender_kind, sender_id, body, created_at,
     sender_generation, sender_station, sender_session)
SELECT display_id, generation, mode, sender_kind, sender_id, body, created_at,
       sender_generation, sender_station, sender_session
FROM display_directives;
DROP TABLE display_directives;
ALTER TABLE display_directives_v48 RENAME TO display_directives;
";
