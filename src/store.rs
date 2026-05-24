//! Storage abstraction for cached responses.
//!
//! Implement [`Store`] for Redis, Postgres, etc. The built-in
//! [`InMemoryStore`] is fine for tests and single-process services.

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// What we cache per key.
#[derive(Debug, Clone)]
pub struct StoredResponse {
    pub status: u16,
    /// Headers as (name, value) pairs. Values are kept as raw bytes so we
    /// preserve binary header values verbatim.
    pub headers: Vec<(String, Vec<u8>)>,
    pub body: Vec<u8>,
    /// SHA-256 of the request body that produced this response — used to
    /// detect "same key, different body" conflicts.
    pub fingerprint: String,
}

#[async_trait]
pub trait Store: Send + Sync + 'static {
    /// Returns the stored response for `key` if present and not yet expired.
    async fn get(&self, key: &str) -> Option<StoredResponse>;
    /// Atomically reserves a key with empty body. Returns `Ok(true)` if we
    /// got the slot (we must populate it via `put`), `Ok(false)` if it's
    /// already taken — caller should `get` and replay.
    async fn try_reserve(&self, key: &str, ttl: Duration) -> bool;
    /// Stores (or replaces) the full response. Caller already won
    /// `try_reserve` for this key.
    async fn put(&self, key: &str, value: StoredResponse, ttl: Duration);
    /// Removes a key. Used to release a reservation if the handler panicked
    /// or returned 5xx and we don't want to cache it.
    async fn forget(&self, key: &str);
}

/// In-memory implementation. Thread-safe via [`Mutex`], TTL-aware.
pub struct InMemoryStore {
    inner: Mutex<HashMap<String, Slot>>,
}

#[derive(Debug, Clone)]
enum Slot {
    Reserved { expires_at: Instant },
    Stored { value: StoredResponse, expires_at: Instant },
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    fn purge_expired(map: &mut HashMap<String, Slot>) {
        let now = Instant::now();
        map.retain(|_, slot| match slot {
            Slot::Reserved { expires_at } | Slot::Stored { expires_at, .. } => *expires_at > now,
        });
    }
}

impl Default for InMemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Store for InMemoryStore {
    async fn get(&self, key: &str) -> Option<StoredResponse> {
        let mut map = self.inner.lock().unwrap();
        Self::purge_expired(&mut map);
        match map.get(key) {
            Some(Slot::Stored { value, .. }) => Some(value.clone()),
            _ => None,
        }
    }

    async fn try_reserve(&self, key: &str, ttl: Duration) -> bool {
        let mut map = self.inner.lock().unwrap();
        Self::purge_expired(&mut map);
        if map.contains_key(key) {
            return false;
        }
        map.insert(
            key.to_string(),
            Slot::Reserved {
                expires_at: Instant::now() + ttl,
            },
        );
        true
    }

    async fn put(&self, key: &str, value: StoredResponse, ttl: Duration) {
        let mut map = self.inner.lock().unwrap();
        map.insert(
            key.to_string(),
            Slot::Stored {
                value,
                expires_at: Instant::now() + ttl,
            },
        );
    }

    async fn forget(&self, key: &str) {
        let mut map = self.inner.lock().unwrap();
        map.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resp() -> StoredResponse {
        StoredResponse {
            status: 200,
            headers: vec![("content-type".into(), b"application/json".to_vec())],
            body: b"{}".to_vec(),
            fingerprint: "abcd".into(),
        }
    }

    #[tokio::test]
    async fn reserve_then_put() {
        let s = InMemoryStore::new();
        assert!(s.try_reserve("k", Duration::from_secs(60)).await);
        assert!(!s.try_reserve("k", Duration::from_secs(60)).await);
        assert!(s.get("k").await.is_none(), "reserved is not gettable");
        s.put("k", resp(), Duration::from_secs(60)).await;
        let v = s.get("k").await.expect("should be present");
        assert_eq!(v.status, 200);
    }

    #[tokio::test]
    async fn ttl_expires_keys() {
        let s = InMemoryStore::new();
        s.put("k", resp(), Duration::from_millis(50)).await;
        assert!(s.get("k").await.is_some());
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(s.get("k").await.is_none(), "should expire");
    }

    #[tokio::test]
    async fn forget_releases_reservation() {
        let s = InMemoryStore::new();
        assert!(s.try_reserve("k", Duration::from_secs(60)).await);
        s.forget("k").await;
        assert!(s.try_reserve("k", Duration::from_secs(60)).await);
    }
}
