use axum::{
    body::Body,
    extract::Json,
    http::{Request, StatusCode},
    routing::post,
    Router,
};
use idempotency_keys::{idempotency_middleware, IdempotencyConfig, InMemoryStore};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

fn build_app(store: Arc<InMemoryStore>, cfg: IdempotencyConfig, counter: Arc<AtomicU32>) -> Router {
    let counter_for_handler = counter.clone();
    let handler = move |Json(body): Json<Value>| {
        let counter = counter_for_handler.clone();
        async move {
            let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
            Json(json!({
                "invocation": n,
                "amount": body.get("amount").cloned().unwrap_or(json!(0)),
            }))
        }
    };

    Router::new()
        .route("/charges", post(handler))
        .layer(axum::middleware::from_fn_with_state(
            (store, cfg),
            idempotency_middleware,
        ))
}

async fn body_to_value(body: Body) -> Value {
    let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn replays_response_for_same_key_and_body() {
    let store = Arc::new(InMemoryStore::new());
    let counter = Arc::new(AtomicU32::new(0));
    let app = build_app(store.clone(), IdempotencyConfig::default(), counter.clone());

    let req = || {
        Request::builder()
            .method("POST")
            .uri("/charges")
            .header("content-type", "application/json")
            .header("Idempotency-Key", "key-1")
            .body(Body::from(r#"{"amount":1000}"#))
            .unwrap()
    };

    let r1 = app.clone().oneshot(req()).await.unwrap();
    assert_eq!(r1.status(), StatusCode::OK);
    let v1 = body_to_value(r1.into_body()).await;

    let r2 = app.clone().oneshot(req()).await.unwrap();
    assert_eq!(r2.status(), StatusCode::OK);
    assert_eq!(
        r2.headers()
            .get("idempotent-replay")
            .map(|v| v.to_str().unwrap()),
        Some("true")
    );
    let v2 = body_to_value(r2.into_body()).await;

    assert_eq!(v1, v2, "second response should be byte-identical");
    assert_eq!(counter.load(Ordering::SeqCst), 1, "handler runs once");
}

#[tokio::test]
async fn rejects_same_key_with_different_body() {
    let store = Arc::new(InMemoryStore::new());
    let counter = Arc::new(AtomicU32::new(0));
    let app = build_app(store.clone(), IdempotencyConfig::default(), counter.clone());

    let r1 = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/charges")
                .header("content-type", "application/json")
                .header("Idempotency-Key", "k2")
                .body(Body::from(r#"{"amount":1000}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r1.status(), StatusCode::OK);

    let r2 = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/charges")
                .header("content-type", "application/json")
                .header("Idempotency-Key", "k2")
                .body(Body::from(r#"{"amount":9999}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r2.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(counter.load(Ordering::SeqCst), 1, "handler runs once");
}

#[tokio::test]
async fn missing_key_on_post_is_400() {
    let store = Arc::new(InMemoryStore::new());
    let counter = Arc::new(AtomicU32::new(0));
    let app = build_app(store, IdempotencyConfig::default(), counter);

    let r = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/charges")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"amount":10}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn get_requests_bypass_middleware() {
    // GET shouldn't need the header at all.
    let store = Arc::new(InMemoryStore::new());
    let cfg = IdempotencyConfig::default();
    let app: Router = Router::new()
        .route("/charges", axum::routing::get(|| async { "ok" }))
        .layer(axum::middleware::from_fn_with_state(
            (store, cfg),
            idempotency_middleware,
        ));

    let r = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/charges")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
}

#[tokio::test]
async fn ttl_expires_cached_response() {
    let store = Arc::new(InMemoryStore::new());
    let cfg = IdempotencyConfig::default().with_ttl(Duration::from_millis(80));
    let counter = Arc::new(AtomicU32::new(0));
    let app = build_app(store.clone(), cfg, counter.clone());

    let req = || {
        Request::builder()
            .method("POST")
            .uri("/charges")
            .header("content-type", "application/json")
            .header("Idempotency-Key", "ttl-key")
            .body(Body::from(r#"{"amount":1}"#))
            .unwrap()
    };

    let _ = app.clone().oneshot(req()).await.unwrap();
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    tokio::time::sleep(Duration::from_millis(150)).await;
    let _ = app.clone().oneshot(req()).await.unwrap();
    assert_eq!(counter.load(Ordering::SeqCst), 2, "should re-run after TTL");
}
