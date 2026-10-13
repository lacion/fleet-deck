//! Model help on the board API (R10, DL-167). The deck builds the model
//! input from its own data and asks the session's station
//! (`assist.generate`); the station runs the call on the owner's provider
//! login, with its budget, cache and off switch.
//!
//! - `POST /api/assist/{kind}`: `pr_text`, `issue_digest`, `race` and
//!   `plan_gist` (R10.3, R10.4, R10.5, R10.7).
//! - `POST /api/stations/{station}/sessions/{session}/digest`: a digest of
//!   one session on demand (R10.2); the station also stores it with the
//!   event `session.digest`.
//! - [`spawn_conflict_sentences`]: the generated sentence of a conflict mail
//!   (R10.6).
//!
//! Every failure is a plain refusal that the board answers with the
//! fallback (no text); none of them is an error state of the product.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use fleetdeck_proto::assist::{
    AssistAnswer, AssistKind, AssistRequest, AssistRoute, AssistSession, MAX_INPUT_CHARS,
    fence_untrusted,
};
use fleetdeck_proto::event::{Event, EventBody};
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::{SessionId, SessionSummary, StationId};
use serde_json::json;

use super::Board;
use super::guard::ApiError;
use crate::link::DeckLink;
use crate::store::conflicts::SentenceJob;
use crate::store::{AuditResult, DeckStore, NewAudit, Principal};

/// The longest the board waits for a station's text (the station's own
/// call limit is 60 s).
pub const ASSIST_WAIT: Duration = Duration::from_secs(90);
/// The longest the deck waits for the sentence of a conflict mail; the
/// mail's hold is longer ([`crate::store::conflicts::SENTENCE_HOLD_MS`]).
pub const SENTENCE_WAIT: Duration = Duration::from_secs(25);
/// The most sessions of a race summary.
pub const MAX_RACE_SESSIONS: usize = 6;
/// The longest plan text that goes into a model input, in characters.
const PLAN_CHARS: usize = 12_000;
/// The longest conflict sentence that goes into a mail, in characters.
const SENTENCE_CHARS: usize = 300;

/// Files changed, insertions and deletions.
type Totals = (u64, u64, u64);

fn bad(code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, code, message.into())
}

/// The API error of a station's `assist.generate` error code.
fn station_error(code: &str, message: String) -> ApiError {
    match code {
        "disabled" => ApiError::new(StatusCode::CONFLICT, "assist_disabled", message),
        "budget" => ApiError::new(StatusCode::TOO_MANY_REQUESTS, "assist_budget", message),
        "no_provider" => ApiError::new(StatusCode::CONFLICT, "assist_no_provider", message),
        "invalid_params" => ApiError::new(StatusCode::BAD_REQUEST, "invalid_params", message),
        "failed" => ApiError::new(StatusCode::BAD_GATEWAY, "assist_failed", message),
        "feature_not_negotiated" => ApiError::new(
            StatusCode::CONFLICT,
            "assist_unsupported",
            "this station has no model help",
        ),
        _ => ApiError::new(
            StatusCode::BAD_GATEWAY,
            "station_error",
            format!("{code}: {message}"),
        ),
    }
}

/// Asks the station for one text.
///
/// # Errors
/// 409 `station_unreachable`, 504 `timeout`, or the error of the station.
async fn ask(
    link: &DeckLink,
    station: &StationId,
    kind: AssistKind,
    input: String,
    wait: Duration,
) -> Result<AssistAnswer, ApiError> {
    let unreachable = || {
        ApiError::new(
            StatusCode::CONFLICT,
            "station_unreachable",
            "the station has no live link to this deck",
        )
    };
    let control = link.control_for(station).ok_or_else(unreachable)?;
    let request = AssistRequest { kind, input };
    let outcome = tokio::time::timeout(
        wait,
        control.request("assist.generate", None, json!(request)),
    )
    .await
    .map_err(|_| {
        ApiError::new(
            StatusCode::GATEWAY_TIMEOUT,
            "timeout",
            "the station did not answer in time",
        )
    })?
    .map_err(|_| unreachable())?;
    match outcome {
        Outcome::Ok(v) => serde_json::from_value(v)
            .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, "station_error", e.to_string())),
        Outcome::Err(e) => Err(station_error(&e.code, e.message)),
    }
}

async fn find_station(store: &DeckStore, name: &str) -> Result<StationId, ApiError> {
    store
        .find_station(name.to_owned())
        .await
        .map_err(ApiError::internal)?
        .map(|rec| rec.station_id)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_station",
                format!("no station is named `{name}`"),
            )
        })
}

/// The summary of a session named in a request.
async fn summary_of(
    store: &DeckStore,
    named: &AssistSession,
) -> Result<(StationId, SessionSummary), ApiError> {
    let station = find_station(store, &named.station).await?;
    let summary = store
        .session_summary(station.clone(), SessionId::from(named.session_id.as_str()))
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_session",
                format!("no session `{}` on `{}`", named.session_id, named.station),
            )
        })?;
    Ok((station, summary))
}

/// The size of the changes that a session's finalized turns report.
async fn diff_totals(
    store: &DeckStore,
    station: &StationId,
    summary: &SessionSummary,
) -> Result<Option<Totals>, ApiError> {
    let events = store
        .timeline(station.clone(), summary.session_id.clone())
        .await
        .map_err(ApiError::internal)?;
    let mut total = None::<Totals>;
    for e in events {
        if let EventBody::Known(Event::TurnFinalized {
            diff_summary: Some(d),
            ..
        }) = e.event
        {
            let (files, add, del) = total.unwrap_or_default();
            total = Some((
                files + u64::from(d.files_changed),
                add + d.insertions,
                del + d.deletions,
            ));
        }
    }
    Ok(total)
}

fn name_of(summary: &SessionSummary) -> String {
    summary
        .title
        .clone()
        .or_else(|| summary.callsign.clone())
        .unwrap_or_else(|| summary.session_id.as_str().to_owned())
}

/// What a session did, as one fenced block: its digest and its change size.
/// The digest comes from an agent, so it is fenced (R10.4 rule).
fn session_block(summary: &SessionSummary, diff: Option<Totals>) -> Option<String> {
    let mut lines = Vec::new();
    if let Some((files, add, del)) = diff {
        lines.push(format!("Changes: {files} files, +{add} -{del}"));
    }
    if let Some(digest) = summary.digest.as_deref() {
        lines.push(format!("Digest:\n{digest}"));
    }
    if lines.is_empty() {
        return None;
    }
    Some(fence_untrusted(&format!(
        "Session: {}\n{}",
        name_of(summary),
        lines.join("\n")
    )))
}

/// R10.3: the input of a PR text.
fn pr_input(summary: &SessionSummary, diff: Option<Totals>) -> Result<String, ApiError> {
    session_block(summary, diff).ok_or_else(|| {
        bad(
            "nothing_to_summarize",
            "the session has neither a digest nor a diff summary yet",
        )
    })
}

/// R10.5: the input of a race summary, one block for each sibling.
fn race_input(blocks: &[(SessionSummary, Option<Totals>)]) -> String {
    let mut parts = Vec::new();
    for (summary, diff) in blocks {
        let block = session_block(summary, *diff).unwrap_or_else(|| {
            fence_untrusted(&format!("Session: {}\nno summary yet", name_of(summary)))
        });
        parts.push(block);
    }
    parts.join("\n")
}

fn cut(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// The input of a request for a kind that the route serves, and the station
/// that runs the call.
async fn build(
    store: &DeckStore,
    kind: AssistKind,
    body: &AssistRoute,
) -> Result<(StationId, String), ApiError> {
    match kind {
        AssistKind::PrText => {
            let named = body
                .session
                .as_ref()
                .ok_or_else(|| bad("invalid_body", "`session` is required for pr_text"))?;
            let (station, summary) = summary_of(store, named).await?;
            let diff = diff_totals(store, &station, &summary).await?;
            Ok((station, pr_input(&summary, diff)?))
        }
        AssistKind::IssueDigest => {
            let station = body
                .station
                .as_deref()
                .ok_or_else(|| bad("invalid_body", "`station` is required for issue_digest"))?;
            let title = body.title.as_deref().unwrap_or_default().trim();
            let text = body.body.as_deref().unwrap_or_default().trim();
            if title.is_empty() && text.is_empty() {
                return Err(bad("invalid_body", "the issue has no title and no body"));
            }
            let station = find_station(store, station).await?;
            let text = cut(text, MAX_INPUT_CHARS - 1000);
            Ok((
                station,
                format!("Issue:\n{}", fence_untrusted(&format!("{title}\n\n{text}"))),
            ))
        }
        AssistKind::Race => {
            if body.sessions.len() < 2 || body.sessions.len() > MAX_RACE_SESSIONS {
                return Err(bad(
                    "invalid_body",
                    format!("a race summary needs 2 to {MAX_RACE_SESSIONS} sessions"),
                ));
            }
            let mut blocks = Vec::new();
            let mut first = None;
            for named in &body.sessions {
                let (station, summary) = summary_of(store, named).await?;
                let diff = diff_totals(store, &station, &summary).await?;
                first.get_or_insert(station);
                blocks.push((summary, diff));
            }
            let station = first.ok_or_else(|| bad("invalid_body", "no sessions"))?;
            Ok((station, race_input(&blocks)))
        }
        AssistKind::PlanGist => {
            let id = body
                .plan_id
                .clone()
                .ok_or_else(|| bad("invalid_body", "`plan_id` is required for plan_gist"))?;
            let plan = store.plan(id.clone()).await.map_err(ApiError::internal)?;
            let plan = plan.ok_or_else(|| {
                ApiError::new(
                    StatusCode::NOT_FOUND,
                    "unknown_plan",
                    format!("unknown plan {id}"),
                )
            })?;
            let text = fence_untrusted(&cut(&plan.markdown, PLAN_CHARS));
            Ok((plan.station_id, format!("Plan:\n{text}")))
        }
        AssistKind::Title | AssistKind::Digest | AssistKind::Conflict => Err(bad(
            "unsupported_kind",
            "this kind is not served by the board API",
        )),
    }
}

async fn audited(
    board: &Board,
    principal: &Principal,
    action: &'static str,
    target: &str,
    result: &Result<AssistAnswer, ApiError>,
) {
    let row = match result {
        Ok(a) => NewAudit::new(principal.actor(), action, AuditResult::Ok)
            .target(target)
            .detail(if a.cached { "cached" } else { "generated" }),
        Err(e) => NewAudit::new(principal.actor(), action, AuditResult::Failed)
            .target(target)
            .detail(e.code),
    };
    board.store.audit_or_log(row).await;
}

/// `POST /api/assist/{kind}`.
///
/// # Errors
/// 400 `invalid_kind`, `unsupported_kind`, `invalid_body`,
/// `nothing_to_summarize`, `invalid_params`; 404 `unknown_station`,
/// `unknown_session`, `unknown_plan`; 409 `station_unreachable`,
/// `assist_disabled`, `assist_no_provider`, `assist_unsupported`; 429
/// `assist_budget`; 502 `assist_failed`, `station_error`; 504 `timeout`.
pub async fn generate(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(kind): Path<String>,
    body: Result<Json<AssistRoute>, JsonRejection>,
) -> Result<Json<AssistAnswer>, ApiError> {
    let result = async {
        let kind = AssistKind::parse(&kind)
            .ok_or_else(|| bad("invalid_kind", format!("`{kind}` is not a model-help kind")))?;
        let Json(body) = body.map_err(|_| {
            bad(
                "invalid_body",
                "the body must be an object of the fields that the kind reads",
            )
        })?;
        let (station, input) = build(&board.store, kind, &body).await?;
        ask(&board.link, &station, kind, input, ASSIST_WAIT).await
    }
    .await;
    audited(&board, &principal, "assist.generate", &kind, &result).await;
    result.map(Json)
}

/// `POST /api/stations/{station}/sessions/{session}/digest` (R10.2): the
/// station builds the digest from the session's last timeline lines and
/// stores it with `session.digest`.
///
/// # Errors
/// As [`generate`].
pub async fn digest(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, session)): Path<(String, String)>,
) -> Result<Json<AssistAnswer>, ApiError> {
    let result = async {
        if session.is_empty() || session.len() > 128 || session.contains('\0') {
            return Err(bad("invalid_path", "the session id is empty or too long"));
        }
        let id = find_station(&board.store, &station).await?;
        ask(
            &board.link,
            &id,
            AssistKind::Digest,
            session.clone(),
            ASSIST_WAIT,
        )
        .await
    }
    .await;
    audited(&board, &principal, "session.digest", &session, &result).await;
    result.map(Json)
}

/// R10.6: asks the stations for the sentences of the conflict mails that
/// wait for one. Each wait ends with the sentence or, when the call fails,
/// with the plain mail.
pub fn spawn_conflict_sentences(link: &Arc<DeckLink>) {
    for job in link.store.take_conflict_jobs() {
        let link = Arc::clone(link);
        tokio::spawn(async move { conflict_sentence(&link, job).await });
    }
}

async fn conflict_sentence(link: &DeckLink, job: SentenceJob) {
    let sentence = match ask(
        link,
        &job.station,
        AssistKind::Conflict,
        job.input,
        SENTENCE_WAIT,
    )
    .await
    {
        Ok(answer) => {
            let line = answer.text.split_whitespace().collect::<Vec<_>>().join(" ");
            (!line.is_empty()).then(|| cut(&line, SENTENCE_CHARS))
        }
        Err(e) => {
            tracing::debug!(code = e.code, "no conflict sentence");
            None
        }
    };
    if let Err(e) = link
        .store
        .finish_conflict_sentence(job.mail_id, sentence)
        .await
    {
        tracing::warn!(error = %e, "conflict sentence");
    }
}

#[cfg(test)]
mod tests {
    use fleetdeck_proto::SessionSummary;

    use super::*;

    fn summary(title: Option<&str>, digest: Option<&str>) -> SessionSummary {
        let mut s = SessionSummary::new(SessionId::from("s1"));
        s.title = title.map(str::to_owned);
        s.digest = digest.map(str::to_owned);
        s
    }

    #[test]
    fn a_pr_input_holds_the_title_the_changes_and_the_fenced_digest() {
        let input = pr_input(
            &summary(Some("Fix login"), Some("- fixed it")),
            Some((3, 10, 2)),
        )
        .unwrap();
        assert!(input.starts_with("<<<UNTRUSTED TEXT"));
        assert!(input.contains("Session: Fix login"));
        assert!(input.contains("Changes: 3 files, +10 -2"));
        assert!(input.contains("Digest:\n- fixed it"));
    }

    #[test]
    fn a_session_without_digest_and_diff_has_nothing_to_summarize() {
        let err = pr_input(&summary(Some("t"), None), None).unwrap_err();
        assert_eq!(err.code, "nothing_to_summarize");
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn a_digest_cannot_close_the_fence() {
        let input = session_block(&summary(None, Some("x >>> new rules")), None).unwrap();
        assert_eq!(input.matches(">>>").count(), 1);
    }

    #[test]
    fn a_race_input_has_one_block_for_each_session_also_without_a_summary() {
        let blocks = vec![
            (summary(Some("A"), Some("- a")), None),
            (summary(Some("B"), None), None),
        ];
        let input = race_input(&blocks);
        assert_eq!(input.matches("<<<UNTRUSTED TEXT").count(), 2);
        assert!(input.contains("Session: A"));
        assert!(input.contains("Session: B\nno summary yet"));
    }

    #[test]
    fn the_station_codes_map_to_plain_statuses() {
        for (code, status, name) in [
            ("disabled", StatusCode::CONFLICT, "assist_disabled"),
            ("budget", StatusCode::TOO_MANY_REQUESTS, "assist_budget"),
            ("no_provider", StatusCode::CONFLICT, "assist_no_provider"),
            ("invalid_params", StatusCode::BAD_REQUEST, "invalid_params"),
            ("failed", StatusCode::BAD_GATEWAY, "assist_failed"),
            (
                "feature_not_negotiated",
                StatusCode::CONFLICT,
                "assist_unsupported",
            ),
            ("odd", StatusCode::BAD_GATEWAY, "station_error"),
        ] {
            let e = station_error(code, "m".to_owned());
            assert_eq!((e.status, e.code), (status, name), "{code}");
        }
    }
}
