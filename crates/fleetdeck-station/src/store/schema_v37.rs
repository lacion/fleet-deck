//! The V37 migration (R13 P7, task S1): the kind check of `desktop_ops`
//! admits the three coordinator input claims `input_key_press`,
//! `input_type_text` and `input_pointer_move`. SQLite cannot change a CHECK,
//! so the table is rebuilt: every column, every row and the primary key stay
//! as they are (the table has no other index). Forward only. To reverse it,
//! rebuild the table with the V35 kind check after deleting the input rows.

pub(super) const V37: &str = "
CREATE TABLE desktop_ops_v37 (           -- R13 P6/P7: one row per coordinator desktop mutation, kept forever
    deck_fp        TEXT    NOT NULL,     -- the authenticated deck fingerprint (64 hex)
    operation_id   TEXT    NOT NULL,     -- the deck's operation ULID, equal to the authority action_id
    action_hash    TEXT    NOT NULL,
    kind           TEXT    NOT NULL CHECK (kind IN ('notify','preview_open','preview_close',
                                                    'input_key_press','input_type_text','input_pointer_move')),
    grant_id       TEXT    NOT NULL,
    caller_station TEXT    NOT NULL,
    caller_session TEXT    NOT NULL,
    state          TEXT    NOT NULL CHECK (state IN ('claimed','done','failed')),
    result         TEXT,
    code           TEXT,
    claimed_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL,
    PRIMARY KEY (deck_fp, operation_id)
) STRICT;
INSERT INTO desktop_ops_v37
    (deck_fp, operation_id, action_hash, kind, grant_id, caller_station, caller_session,
     state, result, code, claimed_at, updated_at)
SELECT deck_fp, operation_id, action_hash, kind, grant_id, caller_station, caller_session,
       state, result, code, claimed_at, updated_at
FROM desktop_ops;
DROP TABLE desktop_ops;
ALTER TABLE desktop_ops_v37 RENAME TO desktop_ops;
";
