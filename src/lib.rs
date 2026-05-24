//! # idempotency-keys
//!
//! Axum middleware that implements **HTTP idempotency keys** — the
//! standard "Stripe-style" mechanism for safe retries on mutating
//! endpoints.
//!
//! ## What it does
//!
//! 1. Client sends a mutating request (e.g. `POST /payments`) with an
//!    `Idempotency-Key` header containing a UUID/ULID it picked.
//! 2. Middleware checks storage for that key.
//!    - **Not found:** runs the handler, captures the response, stores
//!      `(key → response)` with a TTL, returns it to the client.
//!    - **Found with same body fingerprint:** returns the stored response
//!      verbatim. The handler does not run twice.
//!    - **Found with different body fingerprint:** returns
//!      `422 Unprocessable Entity` with an `idempotency-key-reused`
//!      error code — the same key was used for a different request,
//!      which is almost always a client bug.
//! 3. After the TTL, the entry expires and the key can be reused.
//!
//! ## Why it matters
//!
//! In payments (Stripe, Hyperswitch, Razorpay, Adyen) any non-trivial
//! retry strategy needs this. If a `POST /charges` times out, the
//! client doesn't know whether the server processed it. Without
//! idempotency keys, retrying could double-charge. With them, the
//! retry is safe: same key → same response.
//!
//! ## Quick example
//!
//! See [`examples/server.rs`](https://github.com/sumitdahiya125/idempotency-keys-rs/blob/main/examples/server.rs)
//! for a runnable axum server. The minimal wiring is:
//!
//! ```ignore
//! let store = Arc::new(InMemoryStore::new());
//! let config = IdempotencyConfig::default()
//!     .with_ttl(Duration::from_secs(24 * 3600));
//!
//! let app = Router::new()
//!     .route("/charges", post(create_charge))
//!     .layer(axum::middleware::from_fn_with_state(
//!         (store, config),
//!         idempotency_middleware,
//!     ));
//! ```
//!
//! ## Storage backends
//!
//! - [`InMemoryStore`]: backed by a `HashMap` behind a `Mutex`, with
//!   TTL-based eviction. Suitable for single-process services, tests, and demos.
//! - Custom: implement [`Store`] for Redis, Postgres, DynamoDB, etc.
//!   See `src/store.rs` for the trait.

pub mod config;
pub mod error;
pub mod middleware;
pub mod store;

pub use config::IdempotencyConfig;
pub use error::IdempotencyError;
pub use middleware::idempotency_middleware;
pub use store::{InMemoryStore, Store, StoredResponse};
