//! The store facts that the native-session list needs (B7): which sessions
//! of this station hold a native conversation, and what blocks an import of
//! it. One read transaction answers for many keys.

use std::collections::HashMap;

use fleetdeck_proto::SessionId;
use fleetdeck_proto::event::{ProviderKind, SessionState};
use fleetdeck_proto::native::KnownSession;
use rusqlite::OptionalExtension as _;

use super::resumes::{Purpose, exclusion};
use super::{Result, Store, call};

/// What blocks an import of a native conversation (the predicates of
/// `import_exclusion`, without the shim-host refusal).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportBlock {
    InUse(SessionId),
    Held(SessionId),
    ExecutionUnknown,
}

/// The facts about one (provider kind, native id).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowseFact {
    /// The newest session with this native id.
    pub known: Option<KnownSession>,
    pub block: Option<ImportBlock>,
}

fn kind_text(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Claude => "claude",
        ProviderKind::Codex => "codex",
        ProviderKind::Unknown => "unknown",
    }
}

impl Store {
    /// The facts of every key, in one read transaction.
    ///
    /// # Errors
    /// A database error.
    pub async fn native_browse_facts(
        &self,
        keys: Vec<(ProviderKind, String)>,
    ) -> Result<HashMap<(ProviderKind, String), BrowseFact>> {
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let mut out = HashMap::new();
            for (kind, native) in keys {
                let known = tx
                    .query_row(
                        "SELECT session_id, state FROM sessions
                         WHERE provider = ?1 AND native_session_id = ?2
                         ORDER BY created_at DESC, session_id DESC LIMIT 1",
                        [kind_text(kind), native.as_str()],
                        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                    )
                    .optional()?
                    .and_then(|(id, state)| {
                        let state: SessionState =
                            serde_json::from_value(serde_json::Value::String(state)).ok()?;
                        Some(KnownSession {
                            session_id: SessionId::from(id),
                            state,
                        })
                    });
                // The same two statements as `native_session_in_use` and
                // `native_session_held` (store/resumes.rs).
                let in_use = tx
                    .query_row(
                        "SELECT session_id FROM sessions WHERE native_session_id = ?1 \
                         AND state NOT IN ('exited', 'lost') LIMIT 1",
                        [native.as_str()],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()?;
                let held = tx
                    .query_row(
                        "SELECT session_id FROM sessions WHERE native_session_id = ?1 \
                         AND json_extract(summary, '$.terminal') IS NOT NULL LIMIT 1",
                        [native.as_str()],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()?;
                let block = if let Some(id) = in_use {
                    Some(ImportBlock::InUse(SessionId::from(id)))
                } else if let Some(id) = held {
                    Some(ImportBlock::Held(SessionId::from(id)))
                } else if exclusion(&tx, None, kind, Some(&native), None, Purpose::Import)?
                    .blocked
                    .is_some()
                {
                    Some(ImportBlock::ExecutionUnknown)
                } else {
                    None
                };
                out.insert((kind, native), BrowseFact { known, block });
            }
            Ok(out)
        })
        .await
    }
}
