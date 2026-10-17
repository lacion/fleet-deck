//! The V26 migration (DL-194): the digest rows of the shim-hosted session
//! bindings. Forward only; no row is rewritten.

pub(super) const V26: &str = "
-- DL-194: one row for each binding of a shim-hosted session. The digest is the
-- SHA-256 of the credential; the credential itself is never stored.
--   shim_id        the shim execution that the binding belongs to
--   grant_id       empty for a fleet-only binding
--   native_id      the provider's native session id, once the binding is bound
--   classes        a JSON array of the tool class names
--   scope          the JSON of the grant scope
--   state          pending, active, ended, or suspended (refused until a resume)
--   control_skill  the JSON of the control skill
--   expires_at     Unix ms; NULL for a fleet-only binding
CREATE TABLE shim_bindings (
    digest            TEXT PRIMARY KEY,
    shim_id           TEXT NOT NULL,
    session_id        TEXT NOT NULL,
    binding_id        TEXT NOT NULL,
    grant_id          TEXT NOT NULL,
    provider_instance TEXT NOT NULL,
    native_id         TEXT,
    generation        INTEGER NOT NULL,
    classes           TEXT NOT NULL,
    scope             TEXT NOT NULL,
    state             TEXT NOT NULL CHECK (state IN ('pending', 'active', 'ended', 'suspended')),
    control_skill     TEXT NOT NULL,
    expires_at        INTEGER,
    created_at        INTEGER NOT NULL
) STRICT;
CREATE INDEX shim_bindings_shim ON shim_bindings(shim_id);
";
