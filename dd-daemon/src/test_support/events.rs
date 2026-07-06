//! `GET /events` tests: the `--until` bounded-stream termination + filter parsing.
use super::*;
use axum::extract::{Query, State};

/// Build an `EventsQ` from a JSON object (its fields are all `Option<String>`).
fn events_q(v: serde_json::Value) -> crate::events::EventsQ {
    serde_json::from_value(v).unwrap()
}

async fn body_bytes(resp: axum::response::Response) -> Vec<u8> {
    axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec()
}

// ---- docker events --until <past> terminates immediately (does NOT hang) --------------------
// Regression: `--until`/`--since` were deserialized but never applied, so `docker events --until
// <past-ts>` (a BOUNDED command) streamed forever. dd keeps no event history, so a past `--until`
// must close the stream at once rather than block the client.
#[tokio::test]
async fn events_until_in_the_past_closes_immediately() {
    let app = test_app();
    // A far-past bound: the stream must be empty and complete (not hang).
    let resp = crate::events::events(State(app.clone()), Query(events_q(serde_json::json!({"until":"1"})))).await;
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
    // to_bytes resolving at all proves the body ended; it must carry no events.
    let body = body_bytes(resp).await;
    assert!(body.is_empty(), "a past --until must yield an empty, closed stream, got {body:?}");
}
