//! The axum middleware function.

use crate::config::IdempotencyConfig;
use crate::store::{StoredResponse, Store};
use axum::{
    body::{Body, Bytes},
    extract::{Request, State},
    http::{HeaderName, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use http_body_util::BodyExt;
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// Axum middleware. Wire it up with `axum::middleware::from_fn_with_state`
/// and a `State<(Arc<dyn Store>, IdempotencyConfig)>`.
pub async fn idempotency_middleware<S>(
    State((store, config)): State<(Arc<S>, IdempotencyConfig)>,
    req: Request,
    next: Next,
) -> Response
where
    S: Store + ?Sized + 'static,
{
    // Skip non-mutating methods entirely.
    if !config.methods.contains(req.method()) {
        return next.run(req).await;
    }

    // Look up the key header. Missing key on a method we care about → 400.
    let key = match extract_key(&req, &config) {
        Ok(k) => k,
        Err(resp) => return resp,
    };

    // Buffer the body so we can (a) fingerprint it and (b) hand it back to
    // the handler unchanged.
    let (parts, body) = req.into_parts();
    let body_bytes = match read_limited(body, config.max_body_bytes).await {
        Ok(b) => b,
        Err(resp) => return resp,
    };

    let fingerprint = sha256_hex(&body_bytes);

    // Fast path: stored response for this key already.
    if let Some(stored) = store.get(&key).await {
        if config.fingerprint_body && stored.fingerprint != fingerprint {
            return error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "idempotency-key-reused",
                "key was used for a different request body",
            );
        }
        return rebuild_response(stored);
    }

    // Try to reserve the key. If someone else got there first, race: poll
    // briefly for them to populate, otherwise return a 409 telling the
    // client to retry.
    if !store.try_reserve(&key, config.ttl).await {
        // Tiny grace period for concurrent first-request.
        for _ in 0..10 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            if let Some(stored) = store.get(&key).await {
                if config.fingerprint_body && stored.fingerprint != fingerprint {
                    return error_response(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "idempotency-key-reused",
                        "key was used for a different request body",
                    );
                }
                return rebuild_response(stored);
            }
        }
        return error_response(
            StatusCode::CONFLICT,
            "idempotency-key-in-flight",
            "request with this key is still in progress",
        );
    }

    // Re-assemble the request with the buffered body.
    let req = Request::from_parts(parts, Body::from(body_bytes.clone()));

    // Run the inner handler.
    let response = next.run(req).await;

    // Capture status + headers + body, then store if it's a non-5xx.
    let (resp_parts, resp_body) = response.into_parts();
    let resp_bytes = match resp_body.collect().await {
        Ok(c) => c.to_bytes(),
        Err(e) => {
            store.forget(&key).await;
            tracing::warn!(error = %e, "failed to collect response body for caching");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "response-body-error",
                "could not collect response body",
            );
        }
    };

    let status = resp_parts.status;
    // 5xx responses are not cached — they're transient and you want the next
    // retry to actually re-attempt. Idempotency keys cache successful and
    // client-error responses (Stripe does the same).
    if status.is_server_error() {
        store.forget(&key).await;
    } else {
        let headers: Vec<(String, Vec<u8>)> = resp_parts
            .headers
            .iter()
            .map(|(n, v)| (n.as_str().to_string(), v.as_bytes().to_vec()))
            .collect();
        store
            .put(
                &key,
                StoredResponse {
                    status: status.as_u16(),
                    headers,
                    body: resp_bytes.to_vec(),
                    fingerprint,
                },
                config.ttl,
            )
            .await;
    }

    Response::from_parts(resp_parts, Body::from(resp_bytes))
}

// ---- helpers --------------------------------------------------------------

fn extract_key(req: &Request, cfg: &IdempotencyConfig) -> Result<String, Response> {
    let name = match HeaderName::try_from(cfg.header.as_str()) {
        Ok(n) => n,
        Err(_) => {
            return Err(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "config-invalid-header",
                "configured idempotency header name is invalid",
            ))
        }
    };
    let raw = req.headers().get(&name).ok_or_else(|| {
        error_response(
            StatusCode::BAD_REQUEST,
            "idempotency-key-missing",
            "missing Idempotency-Key header",
        )
    })?;
    let s = raw
        .to_str()
        .map_err(|_| {
            error_response(
                StatusCode::BAD_REQUEST,
                "idempotency-key-invalid",
                "Idempotency-Key must be ASCII",
            )
        })?
        .trim()
        .to_string();
    if s.is_empty() || s.len() > cfg.max_key_len {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "idempotency-key-invalid",
            "Idempotency-Key length out of range",
        ));
    }
    Ok(s)
}

async fn read_limited(body: Body, limit: usize) -> Result<Bytes, Response> {
    // Use http_body_util to read with a size limit.
    let collected = http_body_util::Limited::new(body, limit)
        .collect()
        .await
        .map_err(|_| {
            error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "body-too-large",
                "request body exceeded limit",
            )
        })?;
    Ok(collected.to_bytes())
}

fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex::encode(h.finalize())
}

fn rebuild_response(stored: StoredResponse) -> Response {
    let mut builder = Response::builder().status(stored.status);
    let headers = builder.headers_mut().expect("builder has headers");
    for (name, value) in &stored.headers {
        if let (Ok(n), Ok(v)) = (HeaderName::try_from(name.as_str()), HeaderValue::from_bytes(value))
        {
            headers.insert(n, v);
        }
    }
    headers.insert(
        HeaderName::from_static("idempotent-replay"),
        HeaderValue::from_static("true"),
    );
    builder
        .body(Body::from(stored.body))
        .expect("response builds")
}

fn error_response(status: StatusCode, code: &str, msg: &str) -> Response {
    let body = format!(r#"{{"error":"{code}","message":"{msg}"}}"#);
    (
        status,
        [("content-type", "application/json")],
        body,
    )
        .into_response()
}
