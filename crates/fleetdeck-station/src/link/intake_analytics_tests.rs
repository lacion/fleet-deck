//! Tests of `station.analytics` on a store (B29). The method row is tested
//! in the link crate.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use fleetdeck_proto::link::Outcome;
use fleetdeck_testkit::Det;
use serde_json::{Value, json};

use super::serve;
use crate::analytics::ScanStatus;
use crate::store::{Store, StoreConfig};

async fn call(params: Value) -> Outcome {
    let det = Det::new();
    let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    store.recover(Vec::new()).await.unwrap();
    serve(&store, &ScanStatus::new(), params).await
}

fn code(outcome: &Outcome) -> &str {
    match outcome {
        Outcome::Err(e) => &e.code,
        Outcome::Ok(_) => "ok",
    }
}

#[tokio::test]
async fn station_analytics_refuses_bad_params() {
    // A window that is not an hour start.
    let bad = json!({"since": 1, "until": 7_200_000, "audience": "owner"});
    assert_eq!(code(&call(bad).await), "invalid_params");
    // A missing audience, an unknown field, a wrong type.
    let no_audience = json!({"since": 0, "until": 7_200_000});
    assert_eq!(code(&call(no_audience).await), "invalid_params");
    let unknown = json!({"since": 0, "until": 7_200_000, "audience": "owner", "x": 1});
    assert_eq!(code(&call(unknown).await), "invalid_params");
    assert_eq!(code(&call(json!("text")).await), "invalid_params");
    // A good query is answered.
    let good = json!({"since": 0, "until": 7_200_000, "audience": "member"});
    assert_eq!(code(&call(good).await), "ok");
}
