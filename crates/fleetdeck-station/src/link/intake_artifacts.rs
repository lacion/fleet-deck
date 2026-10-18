//! The `artifacts.*` link methods (R12, DL-165; R13 P5a `artifacts.delete`).

use fleetdeck_proto::link::{Outcome, WireError};
use serde::Serialize;

use super::{NOT_IMPLEMENTED, bad_params, ok};
use crate::link::StationLink;

/// The `artifacts.*` methods (R12, DL-165). A station without an artifact
/// directory answers `unsupported`; a service error keeps its code.
pub(super) async fn artifact_method(
    ctx: &StationLink,
    method: &str,
    params: serde_json::Value,
) -> Outcome {
    use crate::artifacts::ArtifactError;
    use fleetdeck_proto::artifacts::code;
    fn done<T: Serialize>(r: Result<T, ArtifactError>) -> Outcome {
        match r {
            Ok(v) => ok(v),
            Err(e) => Outcome::Err(WireError::new(e.code, e.message)),
        }
    }
    let Some(service) = ctx.manager.artifacts() else {
        return Outcome::Err(WireError::new(
            code::UNSUPPORTED,
            "this station has no artifact directory",
        ));
    };
    match method {
        "artifacts.publish" => match serde_json::from_value(params) {
            Ok(q) => done(service.publish(q).await),
            Err(e) => bad_params(e),
        },
        "artifacts.list" => match serde_json::from_value(params) {
            Ok(q) => done(service.list(q).await),
            Err(e) => bad_params(e),
        },
        "artifacts.get" => match serde_json::from_value(params) {
            Ok(q) => done(service.get(q).await),
            Err(e) => bad_params(e),
        },
        "artifacts.file" => match serde_json::from_value(params) {
            Ok(q) => done(service.file(q).await),
            Err(e) => bad_params(e),
        },
        "artifacts.delete" => match serde_json::from_value(params) {
            Ok(q) => done(service.delete(q).await),
            Err(e) => bad_params(e),
        },
        other => Outcome::Err(WireError::new(
            NOT_IMPLEMENTED,
            format!("`{other}` is not served by this station build"),
        )),
    }
}
