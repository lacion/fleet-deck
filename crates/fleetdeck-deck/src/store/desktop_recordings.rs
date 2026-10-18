//! The V35 migration (R13 P5b, task F): the recording columns of
//! `desktop_captures`. Forward only: no existing row is rewritten, and every
//! old row reads `operation_kind = 'screenshot'`. To reverse it, drop the
//! index and the added columns.

/// The migration (V35).
pub(crate) const SQL: &str = "
ALTER TABLE desktop_captures ADD COLUMN operation_kind TEXT NOT NULL DEFAULT 'screenshot'
  CHECK (operation_kind IN ('screenshot','recording'));
ALTER TABLE desktop_captures ADD COLUMN requested_seconds INTEGER
  CHECK (requested_seconds IS NULL OR requested_seconds BETWEEN 1 AND 120);
ALTER TABLE desktop_captures ADD COLUMN owner_login TEXT;      -- login_sessions.session_hash of the initiator
ALTER TABLE desktop_captures ADD COLUMN link_epoch TEXT;       -- Route.epoch at dispatch
ALTER TABLE desktop_captures ADD COLUMN stop_intent TEXT NOT NULL DEFAULT 'none'
  CHECK (stop_intent IN ('none','save','discard'));
ALTER TABLE desktop_captures ADD COLUMN stop_reason TEXT;
ALTER TABLE desktop_captures ADD COLUMN started_at INTEGER;    -- first record-phase lease (start-audit dedupe)
ALTER TABLE desktop_captures ADD COLUMN audited_terminal INTEGER NOT NULL DEFAULT 0
  CHECK (audited_terminal IN (0,1));
CREATE INDEX desktop_captures_recording ON desktop_captures (station_id, state)
  WHERE operation_kind = 'recording';
";
