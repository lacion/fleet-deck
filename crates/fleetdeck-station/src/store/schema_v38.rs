//! The V38 migration (R13 P7b, task F): the kind check of `desktop_ops`
//! admits the five P7b coordinator input claims `input_pointer_click`,
//! `input_any_key_press`, `input_any_type_text`, `input_any_pointer_move` and
//! `input_any_pointer_click`. SQLite cannot change a CHECK, so the table is
//! rebuilt as in V37: every column, every row and the primary key stay as they
//! are (the table has no other index). Forward only. To reverse it, rebuild
//! the table with the V37 kind check after deleting the rows of the new kinds.

pub(super) const V38: &str = "
CREATE TABLE desktop_ops_v38 (           -- R13 P6/P7/P7b: one row per coordinator desktop mutation, kept forever
    deck_fp        TEXT    NOT NULL,     -- the authenticated deck fingerprint (64 hex)
    operation_id   TEXT    NOT NULL,     -- the deck's operation ULID, equal to the authority action_id
    action_hash    TEXT    NOT NULL,
    kind           TEXT    NOT NULL CHECK (kind IN ('notify','preview_open','preview_close',
                                                    'input_key_press','input_type_text','input_pointer_move',
                                                    'input_pointer_click','input_any_key_press',
                                                    'input_any_type_text','input_any_pointer_move',
                                                    'input_any_pointer_click')),
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
INSERT INTO desktop_ops_v38
    (deck_fp, operation_id, action_hash, kind, grant_id, caller_station, caller_session,
     state, result, code, claimed_at, updated_at)
SELECT deck_fp, operation_id, action_hash, kind, grant_id, caller_station, caller_session,
       state, result, code, claimed_at, updated_at
FROM desktop_ops;
DROP TABLE desktop_ops;
ALTER TABLE desktop_ops_v38 RENAME TO desktop_ops;
";
