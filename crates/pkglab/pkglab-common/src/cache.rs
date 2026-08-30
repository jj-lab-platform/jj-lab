//! Process-wide TTL cache for upstream index/metadata documents.
//!
//! Caching the index layer (not just artifacts) is what makes a pull-through
//! mirror fast for repeated builds: every client resolution otherwise
//! re-fetches the same small index documents through the proxy.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long upstream index responses are served from cache before re-fetch.
pub const DEFAULT_INDEX_TTL: Duration = Duration::from_secs(60 * 60);

struct Entry {
    body: String,
    expires: Instant,
}

/// A TTL cache mapping string keys to string bodies, shared across adapter
/// index/metadata fetches. Cheap to clone (arc-backed).
#[derive(Clone)]
pub struct MemCache {
    inner: Arc<Mutex<HashMap<String, Entry>>>,
    ttl: Duration,
    cap: usize,
}

impl Default for MemCache {
    fn default() -> Self {
        Self::new(DEFAULT_INDEX_TTL)
    }
}

impl MemCache {
    pub fn new(ttl: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            ttl,
            // Bound the cache so a long-running process cannot grow without
            // limit; 4096 small index documents is a generous ceiling.
            cap: 4096,
        }
    }

    /// Cached body for `key`, or `None` when absent/expired.
    pub fn get(&self, key: &str) -> Option<String> {
        let mut m = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match m.get(key) {
            Some(e) if Instant::now() < e.expires => Some(e.body.clone()),
            Some(_) => {
                m.remove(key);
                None
            }
            None => None,
        }
    }

    /// Store `body` under `key` with a fresh expiry.
    pub fn set(&self, key: &str, body: String) {
        let mut m = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if m.len() >= self.cap && !m.contains_key(key) {
            // Evict the first expired (or arbitrary) entry.
            let now = Instant::now();
            if let Some(k) = m
                .iter()
                .find(|(_, e)| now >= e.expires)
                .map(|(k, _)| k.clone())
                .or_else(|| m.keys().next().cloned())
            {
                m.remove(&k);
            }
        }
        m.insert(key.to_string(), Entry { body, expires: Instant::now() + self.ttl });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn get_set_expire() {
        let c = MemCache::new(Duration::from_millis(20));
        assert_eq!(c.get("k"), None);
        c.set("k", "v".into());
        assert_eq!(c.get("k"), Some("v".into()));
        sleep(Duration::from_millis(40));
        assert_eq!(c.get("k"), None);
    }
}
