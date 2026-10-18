//! The local socket op `station.fingerprint` (R13 P8 T2, design
//! `omarchy-p8.md` 2.1): the running identity, the paired deck, the
//! connection-local link readiness, the build identity, the binary hash and
//! an optional fresh provider probe.
//!
//! The op reads the identity of the running node. It never generates or
//! loads a key. Without a link node it answers `provision_error` with the
//! code `unavailable`; when the binary hash could not be read it answers
//! `binary_unreadable` (never an empty hash).

use fleetdeck_proto::link::StationProvider;
use serde::{Deserialize, Serialize};

use crate::link::StationNode;
use crate::local::LocalResponse;
use crate::manager::SessionManager;

/// The feature identifier of the provisioning commands of the station.
pub const FEATURE: &str = "station_provision.v1";

/// The build of the running binary (`fleetdeck_core::build`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildIdentity {
    pub version: String,
    pub commit: String,
    pub release: bool,
}

impl BuildIdentity {
    /// The identity of this build.
    #[must_use]
    pub fn current() -> Self {
        Self {
            version: fleetdeck_core::build::version().to_owned(),
            commit: fleetdeck_core::build::COMMIT.to_owned(),
            release: fleetdeck_core::build::RELEASE,
        }
    }
}

fn provision_error(code: &str) -> LocalResponse {
    LocalResponse::ProvisionError {
        code: code.to_owned(),
    }
}

/// The op as `local::handle` calls it.
pub(crate) async fn fingerprint(
    manager: &SessionManager,
    node: Option<&StationNode>,
    probe_providers: bool,
) -> LocalResponse {
    let Some(node) = node else {
        return provision_error("unavailable");
    };
    let own = node.link().node.identity.fingerprint();
    let paired = match node.paired_deck().await {
        Ok(paired) => paired,
        Err(e) => {
            tracing::warn!(error = %e, "station.fingerprint: the paired deck could not be read");
            return provision_error("unavailable");
        }
    };
    let link_up = paired.is_some_and(|deck| node.link().ready.is_up(&deck));
    let Some(binary_sha256) = crate::binary_hash::get().await else {
        return provision_error("binary_unreadable");
    };
    let providers: Option<Vec<StationProvider>> = if probe_providers {
        Some(manager.providers_fresh().await)
    } else {
        None
    };
    LocalResponse::Fingerprint {
        features: vec![FEATURE.to_owned()],
        fingerprint: own.to_string(),
        paired_deck: paired.map(|deck| deck.to_string()),
        link_up,
        build: BuildIdentity::current(),
        binary_sha256,
        providers,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use fleetdeck_testkit::Det;

    use super::*;
    use crate::local::{LocalClient, LocalRequest, bind, serve, socket_path};
    use crate::manager::{Adapters, Policy};
    use crate::store::{OfflineApprovalPolicy, Store, StoreConfig};

    const FP: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn the_request_wire_is_station_dot_fingerprint() {
        let request = serde_json::to_string(&LocalRequest::StationFingerprint {
            probe_providers: false,
        })
        .unwrap();
        assert_eq!(
            request,
            r#"{"op":"station.fingerprint","probe_providers":false}"#
        );
        let back: LocalRequest =
            serde_json::from_str(r#"{"op":"station.fingerprint","probe_providers":true}"#).unwrap();
        assert_eq!(
            back,
            LocalRequest::StationFingerprint {
                probe_providers: true
            }
        );
        // `probe_providers` has a default.
        let bare: LocalRequest = serde_json::from_str(r#"{"op":"station.fingerprint"}"#).unwrap();
        assert_eq!(
            bare,
            LocalRequest::StationFingerprint {
                probe_providers: false
            }
        );
    }

    #[test]
    fn the_fingerprint_response_wire_has_the_fields_of_the_design() {
        let response = LocalResponse::Fingerprint {
            features: vec![FEATURE.to_owned()],
            fingerprint: FP.to_owned(),
            paired_deck: None,
            link_up: false,
            build: BuildIdentity {
                version: "2.0.0".to_owned(),
                commit: "abc1234".to_owned(),
                release: true,
            },
            binary_sha256: FP.to_owned(),
            providers: None,
        };
        let value = serde_json::to_value(&response).unwrap();
        let object = value.as_object().unwrap();
        let mut keys: Vec<_> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "binary_sha256",
                "build",
                "features",
                "fingerprint",
                "link_up",
                "paired_deck",
                "providers",
                "status"
            ]
        );
        assert_eq!(value["status"], "fingerprint");
        assert_eq!(
            value["features"],
            serde_json::json!(["station_provision.v1"])
        );
        assert!(value["paired_deck"].is_null());
        assert!(value["providers"].is_null());
        assert_eq!(
            value["build"],
            serde_json::json!({"version":"2.0.0","commit":"abc1234","release":true})
        );
        let back: LocalResponse = serde_json::from_value(value).unwrap();
        assert_eq!(back, response);
    }

    #[test]
    fn the_provision_error_wire_is_status_and_code() {
        let text = serde_json::to_string(&provision_error("unavailable")).unwrap();
        assert_eq!(text, r#"{"status":"provision_error","code":"unavailable"}"#);
    }

    #[tokio::test]
    async fn without_a_node_the_op_answers_unavailable() {
        let det = Det::new();
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
            .await
            .unwrap();
        let policy = Policy::new(
            vec![dir.path().to_owned()],
            2,
            OfflineApprovalPolicy::Wait,
            BTreeMap::new(),
        );
        let manager = Arc::new(
            SessionManager::start(
                store,
                policy,
                Adapters::new(),
                det.ids(),
                Arc::new(crate::workspace::PrivatePaths::default()),
            )
            .await
            .unwrap(),
        );
        let path = socket_path(dir.path());
        let listener = bind(&path).await.unwrap();
        let (shutdown, rx) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(serve(listener, Arc::clone(&manager), None, rx));
        let mut client = LocalClient::connect(&path).await.unwrap();
        for probe_providers in [false, true] {
            let got = client
                .call(&LocalRequest::StationFingerprint { probe_providers })
                .await
                .unwrap();
            assert_eq!(got, provision_error("unavailable"));
        }
        shutdown.send(true).unwrap();
        task.await.unwrap();
    }
}
