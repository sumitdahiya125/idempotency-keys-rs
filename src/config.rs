use http::Method;
use std::collections::HashSet;
use std::time::Duration;

/// Behaviour tunables for the idempotency middleware.
#[derive(Debug, Clone)]
pub struct IdempotencyConfig {
    /// Header that carries the idempotency key. Defaults to `Idempotency-Key`.
    pub header: String,
    /// Maximum length of an accepted key (after which the request is rejected).
    pub max_key_len: usize,
    /// How long stored responses live. After this, the same key can be reused.
    pub ttl: Duration,
    /// HTTP methods that are subject to idempotency. Defaults to `POST`, `PUT`,
    /// `PATCH`, `DELETE`. `GET` and `HEAD` are excluded since they're
    /// idempotent by spec.
    pub methods: HashSet<Method>,
    /// If true, the middleware fingerprints the request body and rejects
    /// reused keys with a mismatched body (`422 Unprocessable Entity`).
    /// If false, the first stored response is replayed regardless of body.
    pub fingerprint_body: bool,
    /// Maximum body size buffered for fingerprinting + caching, in bytes.
    /// Defaults to 1 MiB. Anything larger is rejected with 413.
    pub max_body_bytes: usize,
}

impl Default for IdempotencyConfig {
    fn default() -> Self {
        let mut methods = HashSet::new();
        methods.insert(Method::POST);
        methods.insert(Method::PUT);
        methods.insert(Method::PATCH);
        methods.insert(Method::DELETE);
        Self {
            header: "Idempotency-Key".into(),
            max_key_len: 255,
            ttl: Duration::from_secs(24 * 3600),
            methods,
            fingerprint_body: true,
            max_body_bytes: 1024 * 1024,
        }
    }
}

impl IdempotencyConfig {
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }
    pub fn with_max_body_bytes(mut self, n: usize) -> Self {
        self.max_body_bytes = n;
        self
    }
    pub fn with_header(mut self, h: impl Into<String>) -> Self {
        self.header = h.into();
        self
    }
    pub fn without_body_fingerprint(mut self) -> Self {
        self.fingerprint_body = false;
        self
    }
}
