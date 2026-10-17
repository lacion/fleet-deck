//! The request digest of `project.ensure_home` (design §8.4, FG-5). The deck
//! computes it at the home add; the station recomputes it from the params and
//! refuses another value (`attempt_reused`). It covers every param except
//! `attempt_id` and `request_digest`: the link binds the station, and the
//! station derives `dest_path` from `folder` and its `clone_root`.

use fleetdeck_proto::fleet_git::{EnsureHomeParams, RemoteIdentity};
use serde::Serialize;

/// The canonical JSON: this field order, no other field (FG-5).
#[derive(Serialize)]
struct DigestInput<'a> {
    project_id: &'a str,
    project_name: &'a str,
    folder: &'a str,
    url: &'a str,
    remote: &'a RemoteIdentity,
    remote_name: &'a str,
    generation: u64,
    retry_of: Option<&'a str>,
}

/// The lower-case hex SHA-256 of the canonical JSON of `params` (FG-5).
#[must_use]
pub fn ensure_home_digest(params: &EnsureHomeParams) -> String {
    let input = DigestInput {
        project_id: &params.project_id,
        project_name: &params.project_name,
        folder: &params.folder,
        url: &params.url,
        remote: &params.remote,
        remote_name: &params.remote_name,
        generation: params.generation,
        retry_of: params.retry_of.as_deref(),
    };
    let bytes = serde_json::to_vec(&input).unwrap_or_default();
    crate::settings::revision_of(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> EnsureHomeParams {
        EnsureHomeParams {
            project_id: "p1".into(),
            project_name: "App".into(),
            folder: "App".into(),
            url: "https://example.com/acme/app.git".into(),
            remote: RemoteIdentity {
                repo: "example.com/acme/app".into(),
                dest: "https://example.com:443/acme/app.git".into(),
            },
            remote_name: "origin".into(),
            generation: 1,
            attempt_id: "ha-1".into(),
            request_digest: String::new(),
            retry_of: None,
        }
    }

    #[test]
    fn the_digest_is_fixed_and_ignores_the_attempt_id() {
        let a = ensure_home_digest(&params());
        assert_eq!(a.len(), 64);
        let mut other = params();
        other.attempt_id = "ha-2".into();
        other.request_digest = "x".into();
        assert_eq!(ensure_home_digest(&other), a);
    }

    #[test]
    fn each_covered_param_changes_the_digest() {
        let base = ensure_home_digest(&params());
        let changes: [fn(&mut EnsureHomeParams); 8] = [
            |p| p.project_id = "p2".into(),
            |p| p.project_name = "App2".into(),
            |p| p.folder = "App2".into(),
            |p| p.url = "https://example.com/acme/other.git".into(),
            |p| p.remote.dest = "https://example.com:443/acme/other.git".into(),
            |p| p.remote_name = "upstream".into(),
            |p| p.generation = 2,
            |p| p.retry_of = Some("ha-0".into()),
        ];
        for change in changes {
            let mut p = params();
            change(&mut p);
            assert_ne!(ensure_home_digest(&p), base);
        }
    }
}
