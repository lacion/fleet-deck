//! The audit import of provider updates (B28, design `cli-update.md` §3.11).
//!
//! After a `station.info` answer with `cli_update`, the deck asks the
//! station for the attempts that it has not copied yet
//! (`cli_update.attempts`) and writes one audit row for each, once. The
//! incarnation is compared before the cursor is used: a new incarnation (a
//! snapshot restore) restarts at `seq` 0.

use std::sync::Arc;
use std::time::Duration;

use fleetdeck_link::control::Control;
use fleetdeck_proto::cli_update::{CliUpdateAttemptsPage, CliUpdateAttemptsQuery};
use fleetdeck_proto::link::{Outcome, StationInfo};
use fleetdeck_proto::version::feature;
use fleetdeck_proto::{IncarnationId, StationId};
use serde_json::json;

use super::DeckLink;
use crate::store::{DeckStore, LinkFeature};

/// The rows of one `cli_update.attempts` page.
pub const PAGE_LIMIT: u32 = 100;
/// The longest the deck waits for one page.
const PAGE_WAIT: Duration = Duration::from_secs(10);

/// Starts the import for the `station.info` answer `info`, when the answer
/// has a `cli_update` report and the link has `cli_update.v1`.
pub(super) fn after_info(
    ctx: &Arc<DeckLink>,
    control: &Control,
    id: &StationId,
    info: &StationInfo,
) {
    let Some(report) = &info.cli_update else {
        return;
    };
    if !matches!(
        ctx.store.link_feature(id, feature::CLI_UPDATE),
        LinkFeature::Negotiated
    ) {
        return;
    }
    let (ctx, control, id) = (Arc::clone(ctx), control.clone(), id.clone());
    let (incarnation, last_seq) = (info.incarnation.clone(), report.last_seq);
    tokio::spawn(async move {
        import(&ctx.store, &control, &id, incarnation, last_seq).await;
    });
}

/// Copies the new attempts of a station into the audit log (§3.11 steps 1 to
/// 3). It ends when a page is short, when a request fails, or after the one
/// restart that a changed incarnation allows.
pub async fn import(
    store: &DeckStore,
    control: &Control,
    id: &StationId,
    incarnation: IncarnationId,
    last_seq: u64,
) {
    let ask = |q: CliUpdateAttemptsQuery| {
        let control = control.clone();
        async move { ask_page(&control, q).await }
    };
    import_with(store, id, incarnation, last_seq, ask).await;
}

/// The loop of [`import`], with the request as a parameter.
async fn import_with<F, Fut>(
    store: &DeckStore,
    id: &StationId,
    mut incarnation: IncarnationId,
    last_seq: u64,
    mut ask: F,
) where
    F: FnMut(CliUpdateAttemptsQuery) -> Fut,
    Fut: Future<Output = Option<CliUpdateAttemptsPage>>,
{
    let mut restarted = false;
    'start: loop {
        // Step 1: the incarnation first.
        let mut after = if restarted {
            0
        } else {
            match store.cli_update_cursor(id).await {
                Ok(Some((inc, seq))) if inc == incarnation => seq,
                Ok(_) => 0,
                Err(e) => {
                    tracing::warn!(error = %e, station = %id, "cli_update cursor");
                    return;
                }
            }
        };
        if !restarted && last_seq <= after {
            return;
        }
        // Step 2: the pages.
        loop {
            let query = CliUpdateAttemptsQuery {
                after_seq: after,
                limit: PAGE_LIMIT,
            };
            let Some(page) = ask(query).await else {
                return;
            };
            if page.incarnation != incarnation {
                if restarted {
                    return;
                }
                restarted = true;
                incarnation = page.incarnation;
                continue 'start;
            }
            let count = page.rows.len();
            let top = page.rows.iter().map(|r| r.seq).max().unwrap_or(after);
            // Step 3.
            let name = match store.station(id.clone()).await {
                Ok(Some(rec)) => rec.name,
                _ => return,
            };
            if let Err(e) = store
                .import_cli_update_attempts(id.clone(), name, incarnation.clone(), page.rows)
                .await
            {
                tracing::warn!(error = %e, station = %id, "cli_update audit import");
                return;
            }
            if count < PAGE_LIMIT as usize || top <= after {
                return;
            }
            after = top;
        }
    }
}

/// One `cli_update.attempts` request. `None` when the link is gone, the
/// station refuses, or the answer does not parse.
async fn ask_page(
    control: &Control,
    query: CliUpdateAttemptsQuery,
) -> Option<CliUpdateAttemptsPage> {
    let params = json!(query);
    let outcome = tokio::time::timeout(
        PAGE_WAIT,
        control.request("cli_update.attempts", None, params),
    )
    .await
    .ok()?
    .ok()?;
    match outcome {
        Outcome::Ok(v) => serde_json::from_value(v).ok(),
        Outcome::Err(e) => {
            tracing::debug!(code = %e.code, "cli_update.attempts refused");
            None
        }
    }
}
