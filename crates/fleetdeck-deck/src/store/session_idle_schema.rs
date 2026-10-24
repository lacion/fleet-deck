//! The V42 migration (knobs pre-PR round 1, `docs/v2/design/knobs.md` rule
//! 11): an idle deadline on the owner login and display sessions.
//!
//! `idle_until` is the time after which the session is idle-expired, set to
//! `now + <the idle limit then in force>` when the session is issued and at
//! every successful use. A check requires `idle_until > now`, so a later
//! raise of the idle limit cannot revive a session whose deadline passed.
//! Existing rows get `last_seen` plus the default idle limit that was in
//! force when they were written (7 days for a login, 30 days for a display
//! session).
//!
//! Forward only. To reverse it, drop the two columns.

pub(crate) const SQL: &str = "
ALTER TABLE login_sessions ADD COLUMN idle_until INTEGER NOT NULL DEFAULT 0;
ALTER TABLE display_sessions ADD COLUMN idle_until INTEGER NOT NULL DEFAULT 0;
UPDATE login_sessions SET idle_until = last_seen + 604800000;
UPDATE display_sessions SET idle_until = last_seen + 2592000000;
";
