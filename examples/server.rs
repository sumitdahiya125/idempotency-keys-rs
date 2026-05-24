//! Minimal axum server demonstrating the middleware.
//!
//! Run with `cargo run --example server`, then:
//!
//! ```bash
//! curl -i -XPOST http://localhost:3000/charges \
//!   -H 'content-type: application/json' \
//!   -H 'Idempotency-Key: ord_001' \
//!   -d '{"amount": 1000}'
//!
//! # The same request again — note the `idempotent-replay: true` header:
//! curl -i -XPOST http://localhost:3000/charges \
//!   -H 'content-type: application/json' \
//!   -H 'Idempotency-Key: ord_001' \
//!   -d '{"amount": 1000}'
//! ```

use axum::{extract::Json, http::StatusCode, response::IntoResponse, routing::post, Router};
use idempotency_keys::{idempotency_middleware, IdempotencyConfig, InMemoryStore};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

#[tokio::main]
async fn main() {
    let store = Arc::new(InMemoryStore::new());
    let config = IdempotencyConfig::default().with_ttl(Duration::from_secs(30 * 60));

    let app = Router::new().route("/charges", post(create_charge)).layer(
        axum::middleware::from_fn_with_state((store, config), idempotency_middleware),
    );

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!("listening on http://localhost:3000");
    axum::serve(listener, app).await.unwrap();
}

async fn create_charge(Json(body): Json<Value>) -> impl IntoResponse {
    // Pretend we did something expensive (DB write, talk to a processor, etc).
    let id = format!("ch_{:08x}", rand_id());
    (
        StatusCode::CREATED,
        Json(json!({
            "id": id,
            "amount": body.get("amount").cloned().unwrap_or(json!(0)),
            "status": "succeeded",
        })),
    )
}

fn rand_id() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos()
}
