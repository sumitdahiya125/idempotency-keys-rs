# idempotency-keys

HTTP **idempotency-key** middleware for [Axum](https://github.com/tokio-rs/axum) — the Stripe-style mechanism that makes safe retries on mutating endpoints possible.

## What it does

1. Client sends a mutating request (`POST`, `PUT`, `PATCH`, `DELETE`) with an `Idempotency-Key` header — typically a UUID/ULID it picked.
2. The middleware looks up the key in storage.
   - **Not found** → runs the handler, captures the response, stores `key → response` with a TTL, and returns it.
   - **Found with matching body** → returns the cached response verbatim. Handler never runs again. The replay carries an `Idempotent-Replay: true` response header.
   - **Found with different body** → returns `422 Unprocessable Entity` with code `idempotency-key-reused`. Almost always a client bug.
3. After the TTL, the entry expires and the key is reusable.

## Why this exists

In payments — Stripe, Hyperswitch, Razorpay, Adyen — every serious retry strategy needs idempotency keys. If `POST /charges` times out, the client doesn't know whether the server already charged the card. Without idempotency, retrying could double-charge. With it, retry is safe: same key → same response → no second charge.

This crate is a small, focused implementation of that pattern, with a pluggable storage trait so you can back it with Redis/Postgres/Dynamo in production while running tests against the in-memory store.

## Quick start

```rust
use std::sync::Arc;
use std::time::Duration;
use axum::{extract::Json, routing::post, Router};
use idempotency_keys::{InMemoryStore, IdempotencyConfig, idempotency_middleware};
use serde_json::{json, Value};

async fn create_charge(Json(body): Json<Value>) -> Json<Value> {
    Json(json!({ "id": "ch_xyz", "amount": body["amount"] }))
}

#[tokio::main]
async fn main() {
    let store = Arc::new(InMemoryStore::new());
    let config = IdempotencyConfig::default()
        .with_ttl(Duration::from_secs(24 * 3600));

    let app = Router::new()
        .route("/charges", post(create_charge))
        .layer(axum::middleware::from_fn_with_state(
            (store, config),
            idempotency_middleware,
        ));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
```

See [`examples/server.rs`](./examples/server.rs) for the runnable version.

```bash
$ cargo run --example server
listening on http://localhost:3000

$ curl -i -XPOST localhost:3000/charges \
    -H 'Idempotency-Key: ord_001' \
    -H 'content-type: application/json' \
    -d '{"amount": 1000}'
HTTP/1.1 201 Created
content-type: application/json
{"amount":1000,"id":"ch_a91b2c34","status":"succeeded"}

$ curl -i -XPOST localhost:3000/charges \
    -H 'Idempotency-Key: ord_001' \
    -H 'content-type: application/json' \
    -d '{"amount": 1000}'
HTTP/1.1 201 Created
content-type: application/json
idempotent-replay: true
{"amount":1000,"id":"ch_a91b2c34","status":"succeeded"}
```

Notice the second response has the same `id` — it's a byte-identical replay.

## Behaviour, with the messy details

### Methods covered
By default: `POST`, `PUT`, `PATCH`, `DELETE`. `GET` and `HEAD` are idempotent by spec, so they bypass the middleware entirely.

### Body fingerprinting (on by default)
The middleware SHA-256s the request body and stores the fingerprint alongside the response. On a replay attempt, if the new body's fingerprint doesn't match the stored one, we return:

```
HTTP/1.1 422 Unprocessable Entity
{"error":"idempotency-key-reused","message":"key was used for a different request body"}
```

This catches client bugs where the same key is reused for a different request (e.g. a bad `useEffect` retry that mutates the body between attempts). You can disable it with `.without_body_fingerprint()` if you want pure "first response wins" semantics.

### 5xx responses are NOT cached
The middleware caches `2xx`, `3xx`, and `4xx` responses — but not `5xx`. The rationale matches Stripe's: 5xx represents a transient server failure and the next retry should actually re-attempt the work, not replay the error.

### Concurrent requests with the same key
The store has a `try_reserve` operation: the first concurrent request for a given key wins the slot; the others see "in-flight" and poll briefly for the first to finish, then replay its response. If polling exhausts, they get `409 Conflict` with code `idempotency-key-in-flight` — the client should retry.

### Body size limit
1 MiB by default. Larger requests get `413 Payload Too Large`. Configure with `.with_max_body_bytes(n)`.

### TTL semantics
TTL is measured from the time the response is stored. Default is 24 hours, configurable. Stripe uses 24 hours; some teams use shorter windows for high-traffic endpoints. After expiry, the same key can be safely reused for a fresh request.

## Pluggable storage

The crate ships with [`InMemoryStore`] for tests and single-process services. For production you'd implement the [`Store`] trait against your backing store:

```rust
#[async_trait::async_trait]
pub trait Store: Send + Sync + 'static {
    async fn get(&self, key: &str) -> Option<StoredResponse>;
    async fn try_reserve(&self, key: &str, ttl: Duration) -> bool;
    async fn put(&self, key: &str, value: StoredResponse, ttl: Duration);
    async fn forget(&self, key: &str);
}
```

Implementing this against Redis is ~50 lines using `redis::aio::MultiplexedConnection` and `SET NX EX`. A reference impl is on the roadmap.

## Roadmap

- **Redis-backed `Store`** — feature-flagged, behind `redis` feature.
- **Postgres-backed `Store`** — same.
- **Telemetry** — emit `tracing` spans + counters for hits/misses/conflicts.
- **Per-route TTL** — different endpoints often want different windows.
- **Tower layer** — for non-axum users (actix, warp).

## License

MIT.
