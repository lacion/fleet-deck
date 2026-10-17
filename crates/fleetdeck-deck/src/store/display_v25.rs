//! The V25 migration (R13 §5D item 4, §5E item 2, §5F item 4, §5G item 2).
//!
//! - `displays`: `auth_generation` (fences link redemption, pairing approval,
//!   pairing claim and session issue), `binding_revision` (the checked
//!   operation revision of a `wall.open` launch and of the binding) and
//!   `directive_generation` (a monotonic counter that continues across the
//!   deletion of a directive).
//! - `display_links`, `display_sessions`, `display_pairings`: the
//!   `auth_generation` at creation. A pairing also stores the display that a
//!   managed launch expects and the operation revision of that launch.
//! - `display_directives`: one validated directive for each display (the
//!   directive store of the P2b directives task).
//! - `wake_seen` and `wake_state`: restart-safe wake deduplication and the
//!   persisted suppression state (`store::wake_seen`).
//!
//! To reverse the migration: drop the three new tables and the added
//! columns. Existing rows read generation 0, which equals the generation 0
//! of every existing display, so existing logins, links and pairings stay
//! valid.

/// The V25 migration.
pub const V25_SQL: &str = "
ALTER TABLE displays ADD COLUMN auth_generation INTEGER NOT NULL DEFAULT 0;
ALTER TABLE displays ADD COLUMN binding_revision INTEGER NOT NULL DEFAULT 0;
ALTER TABLE displays ADD COLUMN directive_generation INTEGER NOT NULL DEFAULT 0;
ALTER TABLE display_links ADD COLUMN auth_generation INTEGER NOT NULL DEFAULT 0;
ALTER TABLE display_sessions ADD COLUMN auth_generation INTEGER NOT NULL DEFAULT 0;
ALTER TABLE display_pairings ADD COLUMN auth_generation INTEGER NOT NULL DEFAULT 0;
ALTER TABLE display_pairings ADD COLUMN expected_display_id TEXT;
ALTER TABLE display_pairings ADD COLUMN operation_revision INTEGER;

CREATE TABLE display_directives (
    display_id   TEXT    PRIMARY KEY,
    generation   INTEGER NOT NULL,
    mode         TEXT    NOT NULL
        CHECK (mode IN ('mission', 'focus', 'review', 'race', 'artifact', 'ambient')),
    sender_kind  TEXT    NOT NULL CHECK (sender_kind IN ('owner', 'station', 'coordinator')),
    sender_id    TEXT    NOT NULL DEFAULT '',
    body         TEXT    NOT NULL,
    created_at   INTEGER NOT NULL
) STRICT;

CREATE TABLE wake_seen (
    station_id     TEXT    NOT NULL,
    request_id     TEXT    NOT NULL,
    first_seen_ms  INTEGER NOT NULL,
    PRIMARY KEY (station_id, request_id)
) STRICT;
CREATE INDEX wake_seen_first_seen ON wake_seen(first_seen_ms);

CREATE TABLE wake_state (
    id                  INTEGER PRIMARY KEY CHECK (id = 1),
    suspended_until_ms  INTEGER NOT NULL DEFAULT 0
) STRICT;
INSERT INTO wake_state (id, suspended_until_ms) VALUES (1, 0);
";
