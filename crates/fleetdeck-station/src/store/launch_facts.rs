//! The launch facts of a provider process (B12 §4.4, DL-191): where the
//! process writes its files. The row commits in the same transaction as the
//! `processes` row, so a restart at any later point finds it.

use std::path::PathBuf;

use fleetdeck_proto::SessionId;
use rusqlite::{OptionalExtension, params};

use super::effects::ProcessIncarnation;
use super::{Result, Store, call, recovery};
use crate::history::subagent::LaunchFacts;

impl Store {
    /// Records a new provider process of a session ([`Self::process_started`])
    /// and, with `facts` whose home is absolute and UTF-8, its launch facts,
    /// in **one** transaction. A failed facts insert rolls the process row
    /// back too.
    ///
    /// # Errors
    /// [`super::StoreError::UnknownSession`] or a database error.
    pub async fn process_launched(
        &self,
        session_id: SessionId,
        process: ProcessIncarnation,
        pid: Option<u32>,
        facts: Option<LaunchFacts>,
    ) -> Result<()> {
        self.write(move |conn, now, _| {
            recovery::process_started(conn, &session_id, &process, pid, now)?;
            let Some(facts) = facts else {
                return Ok(());
            };
            let Some(home) = facts.provider_home.to_str().filter(|_| {
                facts.provider_home.is_absolute()
            }) else {
                tracing::debug!(session = %session_id, "launch facts: the home is not absolute UTF-8");
                return Ok(());
            };
            conn.execute(
                "INSERT INTO process_launch_facts
                   (process_incarnation, session_id, provider_home, cwd, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![process.as_str(), session_id.as_str(), home, facts.cwd, now],
            )?;
            Ok(())
        })
        .await
        .map(|_| ())
    }

    /// The launch facts of `process`, if the row names `session_id` too.
    ///
    /// # Errors
    /// A database error.
    pub async fn launch_facts_of(
        &self,
        session_id: SessionId,
        process: ProcessIncarnation,
    ) -> Result<Option<LaunchFacts>> {
        call(&self.inner.db, move |conn| {
            let row = conn
                .query_row(
                    "SELECT provider_home, cwd FROM process_launch_facts
                     WHERE process_incarnation = ?1 AND session_id = ?2",
                    params![process.as_str(), session_id.as_str()],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                )
                .optional()?;
            Ok(row.map(|(home, cwd)| LaunchFacts {
                provider_home: PathBuf::from(home),
                cwd,
            }))
        })
        .await
    }
}
