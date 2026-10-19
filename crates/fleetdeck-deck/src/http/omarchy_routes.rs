//! The Omarchy routes of a station (design `omarchy-admin.md` revision 5
//! section 4): `GET` reads the status and `POST` runs one action, through
//! the link methods `omarchy.status` and `omarchy.act`.
//!
//! Both routes are owner only. The deck checks the owner's elevation for
//! every action (header `x-fleetdeck-elevation`) and tells the station with
//! `elevated: true`. The deck answers with its own fixed text for each
//! station error, never with the text of the station. Each `POST` writes one
//! audit row (`station.omarchy`) that holds the action, the outcome and the
//! code, never a message.
//!
//! The installer repair (`omarchy_repair.v1`, `board-gaps.md` §3) adds three
//! owner-only reads (`install-plan`, `repairs`, `repairs/{id}/restore-plan`)
//! and the actions `install_replace`, `install_resume` and `restore`. The
//! deck binds each plan to the station and to the owner login that read it
//! (`PLANS`); an action of another login, or after two minutes, is refused
//! with 409 `plan_changed` before the station call. The three repair actions
//! write the row `station.omarchy_repair`: counts and IDs, never a path.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::Duration;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, StatusCode};
use fleetdeck_proto::link::{Outcome, WireError};
use fleetdeck_proto::omarchy_admin::{
    ActOutcome, OmarchyActParams, OmarchyActResult, OmarchyAction, OmarchyStatus, code,
};
use fleetdeck_proto::omarchy_repair::{
    InstallPlanView, OmarchyAssetId, RepairFileState, RepairStatusView, RestorePlanView,
    code as repair_code,
};
use fleetdeck_proto::version::feature;
use serde::Deserialize;
use serde_json::{Value, json};

use super::Board;
use super::artifact_delete::ELEVATION_HEADER;
use super::guard::ApiError;
use super::settings_routes::{SETTINGS_WAIT, is_elevated, owner, station_call_wait};
use crate::store::{AuditResult, LinkFeature, LoginSession, NewAudit, Principal};

/// The longest wait for an action that runs a child on the station.
const ACT_WAIT: Duration = Duration::from_secs(75);

const UNSUPPORTED_TEXT: &str = "This station's build cannot manage Omarchy from the board.";
const ELEVATION_TEXT: &str = "This action needs the owner's elevation.";

/// How long the deck remembers a plan: the life of a plan on the station.
const PLAN_MEMORY_MS: i64 = 120_000;
const PLAN_CHANGED_TEXT: &str =
    "The files changed since this review. Review the replacement plan again.";
const INVALID_TEXT: &str = "The request is not valid.";

/// The `POST` body. The five optional fields belong to the repair actions;
/// the deck passes them on and the station checks the rules of each action.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActBody {
    action: OmarchyAction,
    plan_id: Option<String>,
    replace_recorded_edits: Option<Vec<OmarchyAssetId>>,
    repair_id: Option<String>,
    asset_ids: Option<Vec<OmarchyAssetId>>,
    request_id: Option<String>,
}

/// Who may use a plan: the station and the owner login that read it.
struct PlanBinding {
    station: String,
    session_hash: String,
    expires_at: i64,
}

/// The plans that passed through the deck, by `plan_id`.
static PLANS: LazyLock<Mutex<HashMap<String, PlanBinding>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Remembers that `session_hash` read the plan `plan_id` of `station`. The
/// expired entries go at each insert.
fn remember_plan(plan_id: &str, station: &str, session_hash: &str, now: i64) {
    let mut plans = PLANS.lock().unwrap_or_else(PoisonError::into_inner);
    plans.retain(|_, b| b.expires_at > now);
    plans.insert(
        plan_id.to_owned(),
        PlanBinding {
            station: station.to_owned(),
            session_hash: session_hash.to_owned(),
            expires_at: now.saturating_add(PLAN_MEMORY_MS),
        },
    );
}

/// True when the plan `plan_id` is live and belongs to this station and this
/// login; the entry is removed then (a plan is used once). An expired entry
/// is removed too; any other mismatch keeps it.
fn take_plan(plan_id: &str, station: &str, session_hash: &str, now: i64) -> bool {
    let mut plans = PLANS.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(binding) = plans.get(plan_id) else {
        return false;
    };
    if binding.expires_at <= now {
        plans.remove(plan_id);
        return false;
    }
    if binding.station != station || binding.session_hash != session_hash {
        return false;
    }
    plans.remove(plan_id);
    true
}

/// True for the three actions of `omarchy_repair.v1`.
const fn is_repair(action: OmarchyAction) -> bool {
    matches!(
        action,
        OmarchyAction::InstallReplace | OmarchyAction::InstallResume | OmarchyAction::Restore
    )
}

/// The fixed text of a repair code (§3.7).
fn repair_text(name: &str) -> Option<&'static str> {
    Some(match name {
        repair_code::PLAN_CHANGED => PLAN_CHANGED_TEXT,
        repair_code::UNSAFE_PATH => "Fleet Deck cannot safely replace one of these paths.",
        repair_code::BACKUP_FAILED => {
            "Fleet Deck could not keep a copy of a file. The repair stopped."
        }
        repair_code::REPAIR_INCOMPLETE => {
            "The repair stopped after changing some files. Review the kept copies and repair status."
        }
        repair_code::RESTORE_CONFLICT => {
            "A current file changed after the repair. Fleet Deck kept both versions and did not overwrite it."
        }
        repair_code::BUILD_MISMATCH => {
            "This repair was started by another build. Restore the kept copies instead."
        }
        _ => return None,
    })
}

/// 32 lowercase hex characters.
fn is_hex_id(text: &str) -> bool {
    text.len() == 32 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn plan_changed() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        repair_code::PLAN_CHANGED,
        PLAN_CHANGED_TEXT,
    )
}

/// The longest wait for the answer of `action`: a provisioning retry
/// answers at once.
pub const fn wait_for(action: OmarchyAction) -> Duration {
    match action {
        OmarchyAction::ProvisionRetry => SETTINGS_WAIT,
        _ => ACT_WAIT,
    }
}

fn unsupported() -> ApiError {
    ApiError::new(StatusCode::NOT_IMPLEMENTED, "unsupported", UNSUPPORTED_TEXT)
}

fn bad_answer() -> ApiError {
    ApiError::new(
        StatusCode::BAD_GATEWAY,
        "station_error",
        "the station answered with data that the deck does not know",
    )
}

/// The error for a station answer: a fixed status, code and text for each
/// wire code; never the message of the station.
fn wire_failure(e: &WireError) -> ApiError {
    let (status, name, text) = match e.code.as_str() {
        code::ELEVATION_REQUIRED => (
            StatusCode::BAD_REQUEST,
            "elevation_required",
            ELEVATION_TEXT,
        ),
        code::BUSY => (
            StatusCode::CONFLICT,
            "busy",
            "Another Omarchy action is running on this station.",
        ),
        code::OMARCHY_MISSING => (
            StatusCode::CONFLICT,
            "omarchy_missing",
            "Omarchy was not found on this station.",
        ),
        code::NOT_PROVISIONED => (
            StatusCode::CONFLICT,
            "not_provisioned",
            "Provisioning cannot be retried from the board in this state.",
        ),
        code::INVALID_REQUEST => (
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "The request is not valid.",
        ),
        name @ (repair_code::PLAN_CHANGED
        | repair_code::UNSAFE_PATH
        | repair_code::BACKUP_FAILED
        | repair_code::REPAIR_INCOMPLETE
        | repair_code::RESTORE_CONFLICT
        | repair_code::BUILD_MISMATCH) => {
            return ApiError::new(
                StatusCode::CONFLICT,
                name_of(name),
                repair_text(name).unwrap_or(INVALID_TEXT),
            );
        }
        "unavailable" | "unsupported" | "feature_not_negotiated" | "unknown_method" => {
            return unsupported();
        }
        _ => {
            return ApiError::new(
                StatusCode::BAD_GATEWAY,
                "station_error",
                "the station answered with an error",
            );
        }
    };
    ApiError::new(status, name, text)
}

/// The `&'static str` of a repair code.
fn name_of(name: &str) -> &'static str {
    [
        repair_code::PLAN_CHANGED,
        repair_code::UNSAFE_PATH,
        repair_code::BACKUP_FAILED,
        repair_code::REPAIR_INCOMPLETE,
        repair_code::RESTORE_CONFLICT,
        repair_code::BUILD_MISMATCH,
    ]
    .into_iter()
    .find(|known| *known == name)
    .unwrap_or(code::FAILED)
}

/// Refuses a station whose live links did not negotiate `feature`
/// (`omarchy_admin.v1`, or `omarchy_repair.v1` for the repair routes). An
/// unknown station is a 404 here; a station without a live link goes on to
/// the call, which answers 409 `station_unreachable`.
async fn require_feature(board: &Board, name: &str, feature: &str) -> Result<(), ApiError> {
    let rec = board
        .store
        .find_station(name.to_owned())
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_station",
                format!("no station is named `{name}`"),
            )
        })?;
    if board.store.link_feature(&rec.station_id, feature) == LinkFeature::Without {
        return Err(unsupported());
    }
    Ok(())
}

/// One call of an Omarchy method and its parsed answer.
async fn call<T: serde::de::DeserializeOwned>(
    board: &Board,
    name: &str,
    method: &str,
    params: Value,
    wait: Duration,
) -> Result<T, ApiError> {
    let (_, outcome) = station_call_wait(board, name, method, params, wait).await?;
    match outcome {
        Outcome::Ok(v) => serde_json::from_value(v).map_err(|_| bad_answer()),
        Outcome::Err(e) => Err(wire_failure(&e)),
    }
}

/// `GET /api/stations/{station}/omarchy`: link method `omarchy.status`.
///
/// # Errors
/// 403 `owner_only`; 404 `unknown_station`; 409 `station_unreachable`,
/// `omarchy_missing`; 501 `unsupported`; 502 `station_error`; 504 `timeout`.
pub async fn status(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(name): Path<String>,
) -> Result<Json<OmarchyStatus>, ApiError> {
    owner(
        &board,
        &principal,
        "station.omarchy_status",
        &name,
        "reads the Omarchy status of a station",
    )
    .await?;
    require_feature(&board, &name, feature::OMARCHY_ADMIN).await?;
    call(&board, &name, "omarchy.status", json!({}), SETTINGS_WAIT)
        .await
        .map(Json)
}

/// `GET /api/stations/{station}/omarchy/install-plan`: link method
/// `omarchy.install_plan`. The plan is bound to the owner login.
///
/// # Errors
/// 403 `owner_only`; 404 `unknown_station`; 409 `station_unreachable`,
/// `busy`; 501 `unsupported`; 502 `station_error`; 504 `timeout`.
pub async fn install_plan(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(name): Path<String>,
) -> Result<Json<InstallPlanView>, ApiError> {
    let session = owner(
        &board,
        &principal,
        "station.omarchy_install_plan",
        &name,
        "reads the Omarchy install plan of a station",
    )
    .await?;
    require_feature(&board, &name, feature::OMARCHY_REPAIR).await?;
    let view: InstallPlanView = call(
        &board,
        &name,
        "omarchy.install_plan",
        json!({}),
        SETTINGS_WAIT,
    )
    .await?;
    remember_plan(
        &view.plan_id,
        &name,
        &session.session_hash,
        board.store.now_ms(),
    );
    Ok(Json(view))
}

/// `GET /api/stations/{station}/omarchy/repairs`: link method
/// `omarchy.repair_status`.
///
/// # Errors
/// As [`install_plan`].
pub async fn repairs(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(name): Path<String>,
) -> Result<Json<RepairStatusView>, ApiError> {
    owner(
        &board,
        &principal,
        "station.omarchy_repairs",
        &name,
        "reads the Omarchy repair status of a station",
    )
    .await?;
    require_feature(&board, &name, feature::OMARCHY_REPAIR).await?;
    call(
        &board,
        &name,
        "omarchy.repair_status",
        json!({}),
        SETTINGS_WAIT,
    )
    .await
    .map(Json)
}

/// `GET /api/stations/{station}/omarchy/repairs/{repair_id}/restore-plan`:
/// link method `omarchy.restore_plan`. The plan is bound to the owner login.
///
/// # Errors
/// As [`install_plan`], and 400 `invalid_request` for a `repair_id` that is
/// not 32 lowercase hex characters (before the feature check).
pub async fn restore_plan(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((name, repair_id)): Path<(String, String)>,
) -> Result<Json<RestorePlanView>, ApiError> {
    let session = owner(
        &board,
        &principal,
        "station.omarchy_restore_plan",
        &name,
        "reads the Omarchy restore plan of a station",
    )
    .await?;
    if !is_hex_id(&repair_id) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            code::INVALID_REQUEST,
            INVALID_TEXT,
        ));
    }
    require_feature(&board, &name, feature::OMARCHY_REPAIR).await?;
    let params = json!({ "repair_id": repair_id });
    let view: RestorePlanView =
        call(&board, &name, "omarchy.restore_plan", params, SETTINGS_WAIT).await?;
    remember_plan(
        &view.plan_id,
        &name,
        &session.session_hash,
        board.store.now_ms(),
    );
    Ok(Json(view))
}

/// `POST /api/stations/{station}/omarchy { action }` with the header
/// `x-fleetdeck-elevation`: link method `omarchy.act`.
///
/// # Errors
/// 400 `invalid_body`, `elevation_required`, `invalid_request`; 403
/// `owner_only`; 404 `unknown_station`; 409 `station_unreachable`, `busy`,
/// `omarchy_missing`, `not_provisioned`; 501 `unsupported`; 502
/// `station_error`; 504 `timeout`.
pub async fn act(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: Result<Json<ActBody>, JsonRejection>,
) -> Result<Json<OmarchyActResult>, ApiError> {
    const ACTION: &str = "station.omarchy_act";
    let session = owner(
        &board,
        &principal,
        ACTION,
        &name,
        "manages Omarchy on a station",
    )
    .await?;
    let actor = principal.actor();
    let body = match body {
        Ok(Json(b)) => b,
        Err(_) => {
            let result = Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_body",
                "the request body is not valid",
            ));
            audit(&board, &actor, &name, None, &result).await;
            return result.map(Json);
        }
    };
    let token = headers.get(ELEVATION_HEADER).and_then(|v| v.to_str().ok());
    let result = if is_elevated(&board, &session, token) {
        run(&board, &name, &session, &body).await
    } else {
        Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "elevation_required",
            ELEVATION_TEXT,
        ))
    };
    audit(&board, &actor, &name, Some(&body), &result).await;
    result.map(Json)
}

async fn run(
    board: &Board,
    name: &str,
    session: &LoginSession,
    body: &ActBody,
) -> Result<OmarchyActResult, ApiError> {
    let action = body.action;
    let needed = if is_repair(action) {
        feature::OMARCHY_REPAIR
    } else {
        feature::OMARCHY_ADMIN
    };
    require_feature(board, name, needed).await?;
    // The plan of a replacement or a restore belongs to the login that read
    // it. Without a `plan_id` the station answers `invalid_request`.
    if matches!(
        action,
        OmarchyAction::InstallReplace | OmarchyAction::Restore
    ) && let Some(plan_id) = &body.plan_id
        && !take_plan(plan_id, name, &session.session_hash, board.store.now_ms())
    {
        return Err(plan_changed());
    }
    let params = json!(OmarchyActParams {
        action,
        elevated: true,
        plan_id: body.plan_id.clone(),
        replace_recorded_edits: body.replace_recorded_edits.clone(),
        repair_id: body.repair_id.clone(),
        asset_ids: body.asset_ids.clone(),
        request_id: body.request_id.clone(),
    });
    call(board, name, "omarchy.act", params, wait_for(action)).await
}

/// The snake case name of an action, for the audit row.
fn action_name(action: OmarchyAction) -> &'static str {
    match action {
        OmarchyAction::Install => "install",
        OmarchyAction::Uninstall => "uninstall",
        OmarchyAction::ScreensaverOn => "screensaver_on",
        OmarchyAction::ScreensaverOff => "screensaver_off",
        OmarchyAction::ScreensaverRebaseline => "screensaver_rebaseline",
        OmarchyAction::ProvisionRetry => "provision_retry",
        OmarchyAction::InstallReplace => "install_replace",
        OmarchyAction::InstallResume => "install_resume",
        OmarchyAction::Restore => "restore",
    }
}

fn outcome_name(outcome: ActOutcome) -> &'static str {
    match outcome {
        ActOutcome::Done => "done",
        ActOutcome::Failed => "failed",
        ActOutcome::Started => "started",
    }
}

/// The act code of a `failed` result for the audit row. A code that the
/// contract does not name becomes `failed`, so that the row never holds text
/// that the station chose.
fn audit_code(result: &OmarchyActResult) -> &'static str {
    let Some(text) = result.code.as_deref() else {
        return "-";
    };
    [
        code::INTEGRATION_OUTDATED,
        code::INSTALL_REFUSED,
        code::UNINSTALL_INCOMPLETE,
        code::SCREENSAVER_REFUSED,
        code::TIMEOUT,
        code::OUTPUT_TOO_LARGE,
        code::FAILED,
    ]
    .into_iter()
    .find(|known| *known == text)
    .unwrap_or(code::FAILED)
}

/// The act code of a `failed` repair result for the audit row. A code that
/// the contract does not name becomes `failed`.
fn repair_audit_code(result: &OmarchyActResult) -> &'static str {
    let Some(text) = result.code.as_deref() else {
        return "-";
    };
    [
        repair_code::PLAN_CHANGED,
        repair_code::UNSAFE_PATH,
        repair_code::BACKUP_FAILED,
        repair_code::REPAIR_INCOMPLETE,
        repair_code::RESTORE_CONFLICT,
        repair_code::BUILD_MISMATCH,
        code::BUSY,
        code::TIMEOUT,
        code::OUTPUT_TOO_LARGE,
        code::FAILED,
    ]
    .into_iter()
    .find(|known| *known == text)
    .unwrap_or(code::FAILED)
}

/// The audit row of a `POST`: `action=<action> outcome=<outcome or error
/// code> code=<act code or ->`. Never a message. A repair action writes the
/// row `station.omarchy_repair` instead (see [`audit_repair`]).
async fn audit(
    board: &Board,
    actor: &str,
    target: &str,
    body: Option<&ActBody>,
    result: &Result<OmarchyActResult, ApiError>,
) {
    let action = body.map(|b| b.action);
    if let Some(body) = body.filter(|b| is_repair(b.action)) {
        return audit_repair(board, actor, target, body, result).await;
    }
    let (audit_result, outcome, act_code) = match result {
        Ok(r) if r.outcome == ActOutcome::Failed => {
            (AuditResult::Failed, outcome_name(r.outcome), audit_code(r))
        }
        Ok(r) => (AuditResult::Ok, outcome_name(r.outcome), audit_code(r)),
        Err(e) => (AuditResult::Refused, e.code, "-"),
    };
    let detail = format!(
        "action={} outcome={outcome} code={act_code}",
        action.map_or("-", action_name)
    );
    board
        .store
        .audit_or_log(
            NewAudit::new(actor, "station.omarchy", audit_result)
                .target(target)
                .detail(detail),
        )
        .await;
}

/// The repair ID for the audit row: the one of the result, else the one of
/// the request, else `-`. Only 32 lowercase hex characters are written.
fn audit_repair_id(body: &ActBody, result: Option<&OmarchyActResult>) -> String {
    let from_result = result
        .and_then(|r| r.repair.as_ref())
        .and_then(|r| r.repair_id.as_deref());
    from_result
        .into_iter()
        .chain(body.repair_id.as_deref())
        .find(|id| is_hex_id(id))
        .map_or_else(|| "-".to_owned(), str::to_owned)
}

/// The audit row `station.omarchy_repair`: `action=<action> outcome=<outcome
/// or error code> code=<code or -> repair=<repair ID or -> assets=<files>
/// replaced=<files replaced or restored>`. No path, hash, file content or
/// child text.
async fn audit_repair(
    board: &Board,
    actor: &str,
    target: &str,
    body: &ActBody,
    result: &Result<OmarchyActResult, ApiError>,
) {
    let (audit_result, outcome, act_code, repair, assets, replaced) = match result {
        Ok(r) => {
            let done = match body.action {
                OmarchyAction::Restore => RepairFileState::Restored,
                _ => RepairFileState::Replaced,
            };
            let files = r.repair.as_ref().map_or(&[][..], |x| x.files.as_slice());
            let audit_result = if r.outcome == ActOutcome::Failed {
                AuditResult::Failed
            } else {
                AuditResult::Ok
            };
            (
                audit_result,
                outcome_name(r.outcome),
                repair_audit_code(r),
                audit_repair_id(body, Some(r)),
                files.len(),
                files.iter().filter(|f| f.state == done).count(),
            )
        }
        Err(e) => (
            AuditResult::Refused,
            e.code,
            "-",
            audit_repair_id(body, None),
            0,
            0,
        ),
    };
    let detail = format!(
        "action={} outcome={outcome} code={act_code} repair={repair} assets={assets} replaced={replaced}",
        action_name(body.action)
    );
    board
        .store
        .audit_or_log(
            NewAudit::new(actor, "station.omarchy_repair", audit_result)
                .target(target)
                .detail(detail),
        )
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plan_binds_the_station_and_the_login_and_is_used_once() {
        let id = "t1".to_owned() + &"0".repeat(30);
        remember_plan(&id, "alpha", "login-1", 1_000);
        // Another station, another login and an unknown ID do not take it,
        // and do not remove it.
        assert!(!take_plan(&id, "beta", "login-1", 1_001));
        assert!(!take_plan(&id, "alpha", "login-2", 1_001));
        assert!(!take_plan("unknown", "alpha", "login-1", 1_001));
        assert!(take_plan(&id, "alpha", "login-1", 1_001));
        assert!(!take_plan(&id, "alpha", "login-1", 1_002));
    }

    #[test]
    fn a_plan_is_forgotten_after_two_minutes() {
        let id = "t2".to_owned() + &"0".repeat(30);
        remember_plan(&id, "alpha", "login-1", 5_000);
        assert!(!take_plan(&id, "alpha", "login-1", 5_000 + PLAN_MEMORY_MS));
        // The expired entry is gone.
        remember_plan(&id, "alpha", "login-1", 6_000);
        assert!(take_plan(
            &id,
            "alpha",
            "login-1",
            6_000 + PLAN_MEMORY_MS - 1
        ));
    }

    #[test]
    fn an_insert_drops_the_expired_entries() {
        let old = "t3".to_owned() + &"0".repeat(30);
        let new = "t4".to_owned() + &"0".repeat(30);
        remember_plan(&old, "alpha", "login-1", 10_000);
        remember_plan(&new, "alpha", "login-1", 10_000 + PLAN_MEMORY_MS);
        let plans = PLANS.lock().unwrap_or_else(PoisonError::into_inner);
        assert!(!plans.contains_key(&old));
        assert!(plans.contains_key(&new));
    }

    #[test]
    fn the_repair_id_is_32_lowercase_hex_characters() {
        assert!(is_hex_id("0123456789abcdef0123456789abcdef"));
        for bad in [
            "",
            "0123456789ABCDEF0123456789abcdef",
            "0123456789abcdef0123456789abcde",
            "0123456789abcdef0123456789abcdef0",
            "0123456789abcdef0123456789abcdeg",
        ] {
            assert!(!is_hex_id(bad), "{bad}");
        }
    }
}
