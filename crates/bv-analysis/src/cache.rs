//! Robot analysis disk cache v3 — design parity with Go
//! `pkg/analysis/cache.go` (keying, bounds, eviction, XFetch), new binary
//! format + renamed file so the Go binary never misreads our cache.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub const CACHE_FILE_NAME: &str = "bv_analysis_cache_v3.bin";
const MAX_ENTRIES: usize = 10;
const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_ENTRY_BYTES: usize = 10 * 1024 * 1024;
/// XFetch beta=1.0 probabilistic early refresh window fraction.
const XFETCH_BETA: f64 = 1.0;

#[derive(Debug, Clone)]
struct CacheRecord {
    payload: Vec<u8>,
    created_at: Instant,
    last_access: Instant,
}

fn cache_key(data_hash16: &str, config_hash16: &str) -> String {
    format!("{data_hash16}|{config_hash16}")
}

/// In-memory robot analysis cache with LRU eviction + age bound.
/// Disk persistence lands with the datasource layer; the in-proc half is
/// what robot-command paths hit per-invocation.
pub struct AnalysisCache {
    entries: HashMap<String, CacheRecord>,
}

impl AnalysisCache {
    pub fn new() -> Self {
        AnalysisCache {
            entries: HashMap::new(),
        }
    }

    /// Resolve cache dir: $BV_CACHE_DIR else platform cache dir /bv
    /// (Go convention: BV_CACHE_DIR override, else UserCacheDir).
    pub fn cache_dir(&self) -> Option<PathBuf> {
        if let Ok(custom) = std::env::var("BV_CACHE_DIR") {
            if !custom.is_empty() {
                return Some(PathBuf::from(custom).join("bv"));
            }
        }
        let base = std::env::var("XDG_CACHE_HOME")
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                if cfg!(target_os = "macos") {
                    std::env::var("HOME")
                        .ok()
                        .map(|h| PathBuf::from(h).join("Library/Caches"))
                } else {
                    std::env::var("HOME")
                        .ok()
                        .map(|h| PathBuf::from(h).join(".cache"))
                }
            })?;
        Some(base.join("bv"))
    }

    /// Robot disk-cache gate: BV_ROBOT=1 && BV_NO_CACHE!=1 (Go parity).
    pub fn disk_cache_enabled() -> bool {
        std::env::var("BV_ROBOT").as_deref() == Ok("1")
            && std::env::var("BV_NO_CACHE").as_deref() != Ok("1")
    }

    pub fn get(&mut self, data_hash16: &str, config_hash16: &str) -> Option<&[u8]> {
        let key = cache_key(data_hash16, config_hash16);
        // Age check + touch must not hold a mutable borrow across remove().
        if self
            .entries
            .get(&key)
            .map(|e| e.created_at.elapsed() > MAX_AGE)
            .unwrap_or(false)
        {
            self.entries.remove(&key);
            return None;
        }
        match self.entries.get_mut(&key) {
            Some(entry) => {
                entry.last_access = Instant::now();
                crate::metrics::record_cache_hit(&crate::metrics::CACHE_GRAPH);
                Some(entry.payload.as_slice())
            }
            None => {
                crate::metrics::record_cache_miss(&crate::metrics::CACHE_GRAPH);
                None
            }
        }
    }

    pub fn put(&mut self, data_hash16: &str, config_hash16: &str, payload: Vec<u8>) {
        if payload.len() > MAX_ENTRY_BYTES {
            return;
        }
        let key = cache_key(data_hash16, config_hash16);
        // LRU bound: evict least-recently-accessed when over capacity.
        // `min_by_key` over a HashMap resolves `last_access` ties by
        // iteration order, which is randomized per process — two entries
        // touched in the same `Instant` tick (or a test that never touches)
        // evicted a different key on every run. Tie-break on the key so the
        // victim is deterministic.
        if self.entries.len() >= MAX_ENTRIES && !self.entries.contains_key(&key) {
            let oldest = self
                .entries
                .iter()
                .min_by(|x, y| {
                    x.1.last_access
                        .cmp(&y.1.last_access)
                        .then_with(|| x.0.cmp(y.0))
                })
                .map(|(k, _)| k.clone());
            if let Some(k) = oldest {
                self.entries.remove(&k);
            }
        }
        self.entries.insert(
            key,
            CacheRecord {
                payload,
                created_at: Instant::now(),
                last_access: Instant::now(),
            },
        );
    }

    /// Go: XFetch beta=1.0 — probabilistically refresh an entry whose age
    /// exceeds typical recompute time; simplified deterministic variant here.
    pub fn xfetch_should_refresh(created_at: Instant, typical_recompute: Duration) -> bool {
        let age = created_at.elapsed();
        if age <= typical_recompute {
            return false;
        }
        let delta = age.as_secs_f64() / typical_recompute.as_secs_f64().max(0.001);
        delta > XFETCH_BETA * 2.0
    }
}

impl Default for AnalysisCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key16(seed: u8) -> String {
        format!("{:032x}", seed)
    }

    #[test]
    fn put_and_get_roundtrip() {
        let mut c = AnalysisCache::new();
        c.put("aaaa", "bbbb", vec![1, 2, 3]);
        assert_eq!(c.get("aaaa", "bbbb"), Some(&[1u8, 2, 3][..]));
    }

    #[test]
    fn key_mismatch_misses() {
        let mut c = AnalysisCache::new();
        c.put("aaaa", "bbbb", vec![1]);
        assert!(c.get("aaaa", "cccc").is_none());
    }

    #[test]
    fn lru_eviction_at_max_entries() {
        let mut c = AnalysisCache::new();
        for i in 0..12u8 {
            c.put(&key16(i), "cfg", vec![i]);
        }
        assert_eq!(c.entries.len(), MAX_ENTRIES);
        assert!(c.get(&key16(0), "cfg").is_none());
        assert!(c.get(&key16(11), "cfg").is_some());
    }

    /// `min_by_key` over a HashMap resolves `last_access` ties by iteration
    /// order, which Rust randomizes per process — the same put sequence could
    /// evict a different key on every run. The tie-break on the key makes the
    /// victim deterministic: the lexicographically smallest key goes first.
    #[test]
    fn lru_eviction_tie_breaks_on_key_deterministically() {
        let mut c = AnalysisCache::new();
        // Fill to capacity with IDENTICAL last_access instants.
        let t = Instant::now();
        for i in 0..MAX_ENTRIES {
            let k = key16(i as u8);
            c.entries.insert(
                cache_key(&k, "cfg"),
                CacheRecord {
                    payload: vec![i as u8],
                    created_at: t,
                    last_access: t,
                },
            );
        }
        c.put("ffff", "cfg", vec![9]);
        assert_eq!(c.entries.len(), MAX_ENTRIES);
        // key16(0) is the lexicographically smallest data-hash -> evicted.
        assert!(!c.entries.contains_key(&cache_key(&key16(0), "cfg")));
        assert!(c.entries.contains_key(&cache_key(&key16(1), "cfg")));
        assert!(c.entries.contains_key(&cache_key("ffff", "cfg")));
    }

    #[test]
    fn oversized_payload_rejected() {
        let mut c = AnalysisCache::new();
        c.put("big", "cfg", vec![0u8; MAX_ENTRY_BYTES + 1]);
        assert!(c.get("big", "cfg").is_none());
    }

    #[test]
    fn disk_gate_predicate_exists() {
        // env matrix covered by integration tests in Phase 3
        let _ = AnalysisCache::disk_cache_enabled();
    }
}
