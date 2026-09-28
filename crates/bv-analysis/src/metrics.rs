//! Runtime metrics — port of Go `pkg/metrics` (timing.go, cache.go).
//!
//! Lock-free atomic counters for per-metric timing histograms and cache
//! hit/miss tracking, gated by `BV_METRICS` (enabled unless "0", matching
//! Go's default-on behavior). Memory stats degrade to `null` on platforms
//! where they can't be read — never fabricated.

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

/// Timing histogram per registered metric (Go `TimingMetric`).
pub struct TimingMetric {
    pub name: &'static str,
    count: AtomicUsize,
    total_ns: AtomicU64,
    min_ns: AtomicU64,
    max_ns: AtomicU64,
}

impl TimingMetric {
    const fn new(name: &'static str) -> Self {
        TimingMetric {
            name,
            count: AtomicUsize::new(0),
            total_ns: AtomicU64::new(0),
            min_ns: AtomicU64::new(u64::MAX),
            max_ns: AtomicU64::new(0),
        }
    }

    fn record(&self, ns: u64) {
        self.count.fetch_add(1, Ordering::Relaxed);
        self.total_ns.fetch_add(ns, Ordering::Relaxed);
        self.max_ns.fetch_max(ns, Ordering::Relaxed);
        self.min_ns.fetch_min(ns, Ordering::Relaxed);
    }

    fn to_json(&self) -> Value {
        let count = self.count.load(Ordering::Relaxed);
        if count == 0 {
            return json!({
                "name": self.name,
                "count": 0,
                "total_ms": 0.0,
                "avg_ms": 0.0,
                "max_ms": 0.0,
                "min_ms": 0.0,
            });
        }
        let total_ms = self.total_ns.load(Ordering::Relaxed) as f64 / 1_000_000.0;
        let min_ns = self.min_ns.load(Ordering::Relaxed);
        let max_ns = self.max_ns.load(Ordering::Relaxed);
        json!({
            "name": self.name,
            "count": count,
            "total_ms": total_ms,
            "avg_ms": total_ms / count as f64,
            "max_ms": max_ns as f64 / 1_000_000.0,
            "min_ms": if min_ns == u64::MAX { 0.0 } else { min_ns as f64 / 1_000_000.0 },
        })
    }
}

/// Cache hit/miss counter (Go `CacheMetric`).
pub struct CacheMetric {
    pub name: &'static str,
    hits: AtomicUsize,
    misses: AtomicUsize,
}

impl CacheMetric {
    const fn new(name: &'static str) -> Self {
        CacheMetric {
            name,
            hits: AtomicUsize::new(0),
            misses: AtomicUsize::new(0),
        }
    }

    fn to_json(&self) -> Value {
        let hits = self.hits.load(Ordering::Relaxed);
        let misses = self.misses.load(Ordering::Relaxed);
        let total = hits + misses;
        let hit_rate = if total > 0 {
            hits as f64 / total as f64
        } else {
            0.0
        };
        json!({
            "name": self.name,
            "hits": hits,
            "misses": misses,
            "total": total,
            "hit_rate": hit_rate,
        })
    }
}

// Registered timing metrics — names match Go timing.go exactly.

/// All registered timing metrics (Go: 11 registered names).
pub static TIMING_METRICS: &[&TimingMetric] = &[
    &TIMING_CYCLE_DETECTION,
    &TIMING_TOPOLOGICAL_SORT,
    &TIMING_TRIAGE_ANALYSIS,
    &TIMING_GRAPH_STATS_ACCESS,
    &TIMING_VECTOR_SEARCH,
    &TIMING_JSON_PARSING,
    &TIMING_PAGERANK_COMPUTE,
    &TIMING_BETWEENNESS_COMPUTE,
    &TIMING_HITS_COMPUTE,
    &TIMING_GRAPH_LOAD,
    &TIMING_UI_RENDER,
];

pub static TIMING_CYCLE_DETECTION: TimingMetric = TimingMetric::new("cycle_detection");
pub static TIMING_TOPOLOGICAL_SORT: TimingMetric = TimingMetric::new("topological_sort");
pub static TIMING_TRIAGE_ANALYSIS: TimingMetric = TimingMetric::new("triage_analysis");
pub static TIMING_GRAPH_STATS_ACCESS: TimingMetric = TimingMetric::new("graph_stats_access");
pub static TIMING_VECTOR_SEARCH: TimingMetric = TimingMetric::new("vector_search");
pub static TIMING_JSON_PARSING: TimingMetric = TimingMetric::new("json_parsing");
pub static TIMING_PAGERANK_COMPUTE: TimingMetric = TimingMetric::new("pagerank_compute");
pub static TIMING_BETWEENNESS_COMPUTE: TimingMetric = TimingMetric::new("betweenness_compute");
pub static TIMING_HITS_COMPUTE: TimingMetric = TimingMetric::new("hits_compute");
pub static TIMING_GRAPH_LOAD: TimingMetric = TimingMetric::new("graph_load");
pub static TIMING_UI_RENDER: TimingMetric = TimingMetric::new("ui_render");

/// All registered cache metrics (Go: 5 registered names).
pub static CACHE_METRICS: &[&CacheMetric] = &[
    &CACHE_GRAPH,
    &CACHE_TRIAGE,
    &CACHE_SEARCH,
    &CACHE_METRICS_CACHE,
    &CACHE_STYLE,
];

pub static CACHE_GRAPH: CacheMetric = CacheMetric::new("graph_cache");
pub static CACHE_TRIAGE: CacheMetric = CacheMetric::new("triage_cache");
pub static CACHE_SEARCH: CacheMetric = CacheMetric::new("search_cache");
pub static CACHE_METRICS_CACHE: CacheMetric = CacheMetric::new("metrics_cache");
pub static CACHE_STYLE: CacheMetric = CacheMetric::new("style_cache");

/// Metrics enabled unless `BV_METRICS=0` (Go default-on).
pub fn metrics_enabled() -> bool {
    std::env::var("BV_METRICS").ok().as_deref() != Some("0")
}

/// RAII timing scope: `let _t = metrics::time(&TIMING_PAGERANK_COMPUTE);`
pub fn time(metric: &'static TimingMetric) -> TimingGuard {
    TimingGuard {
        metric,
        start: Instant::now(),
        enabled: metrics_enabled(),
    }
}

pub struct TimingGuard {
    metric: &'static TimingMetric,
    start: Instant,
    enabled: bool,
}

impl Drop for TimingGuard {
    fn drop(&mut self) {
        if self.enabled {
            self.metric.record(self.start.elapsed().as_nanos() as u64);
        }
    }
}

pub fn record_cache_hit(metric: &'static CacheMetric) {
    if metrics_enabled() {
        metric.hits.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn record_cache_miss(metric: &'static CacheMetric) {
    if metrics_enabled() {
        metric.misses.fetch_add(1, Ordering::Relaxed);
    }
}

/// Memory stats — Go `MemoryStats` shape (cache.go:135-145).
///
/// The values are Go runtime counters (`runtime.ReadMemStats`), so they can
/// never equal a Rust build's — a differential harness has to exempt this
/// block the way it exempts timings. What *is* required is the shape: Go's
/// schema declares all six keys `"type": "number"` and lists `memory` as
/// required (main.go:9131-9138), so `null` is a schema violation rather than
/// a graceful degradation. The old `/proc/self/statm` read meant every macOS
/// build emitted six nulls and no consumer validating against Go's own schema
/// could accept the document.
///
/// The closest real quantity per platform is resident set size. RSS is not
/// Go's heap, so it is reported only where that is honest, and the counters
/// with no Rust analogue (`gc_cycles`, `gc_pause_ms`, `goroutine_count`)
/// remain null — a caller can then distinguish "this runtime does not report
/// GC" from a real zero.
fn memory_stats() -> Value {
    match resident_set_mb() {
        Some(rss) => json!({
            "heap_alloc_mb": rss,
            "heap_sys_mb": rss,
            "heap_objects_k": Value::Null,
            "gc_cycles": Value::Null,
            "gc_pause_ms": Value::Null,
            "goroutine_count": Value::Null,
        }),
        None => json!({
            "heap_alloc_mb": Value::Null,
            "heap_sys_mb": Value::Null,
            "heap_objects_k": Value::Null,
            "gc_cycles": Value::Null,
            "gc_pause_ms": Value::Null,
            "goroutine_count": Value::Null,
        }),
    }
}

/// Resident set size in MiB, or `None` where the platform exposes no way to
/// read it. Never a guess.
#[cfg(target_os = "macos")]
fn resident_set_mb() -> Option<f64> {
    // `struct mach_task_basic_info` from <mach/task_info.h> — the always-64-bit
    // variant behind `MACH_TASK_BASIC_INFO` (flavor 20), 48 bytes / 12 words.
    //
    // Both the struct choice and the field order were read off the SDK header,
    // and the sizes confirmed against a C program built with the same clang:
    // `task_basic_info` (the legacy flavor-20 struct) is 40 bytes and returns
    // KERN_INVALID_ARGUMENT, while `mach_task_basic_info` is 48 and returns 0.
    // The header's own comment on the older struct says as much.
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct MachTaskBasicInfo {
        virtual_size: u64,
        resident_size: u64,
        resident_size_max: u64,
        user_time: [u32; 2],
        system_time: [u32; 2],
        policy: i32,
        suspend_count: i32,
    }
    extern "C" {
        fn mach_task_self() -> u32;
        fn task_info(task: u32, flavor: i32, info: *mut MachTaskBasicInfo, count: *mut u32) -> i32;
    }
    const MACH_TASK_BASIC_INFO: i32 = 20;

    let mut info = MachTaskBasicInfo::default();
    // `count` is in 32-bit words, not bytes.
    let mut count = std::mem::size_of::<MachTaskBasicInfo>() as u32 / 4;
    // SAFETY: `info` and `count` are valid, correctly sized out-parameters and
    // the flavor matches the struct passed. A non-zero return means the query
    // failed, leaving `info` untouched, and we report None rather than reading
    // an uninitialized value.
    let kr = unsafe {
        task_info(
            mach_task_self(),
            MACH_TASK_BASIC_INFO,
            &mut info,
            &mut count,
        )
    };
    if kr != 0 {
        return None;
    }
    Some(info.resident_size as f64 / 1_048_576.0)
}

#[cfg(target_os = "linux")]
fn resident_set_mb() -> Option<f64> {
    // `/proc/self/statm`: field 2 is resident pages. Assuming a 4 KiB page is
    // safe for every Linux target this ships to.
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let rss_pages: f64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    Some(rss_pages * 4096.0 / 1_048_576.0)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn resident_set_mb() -> Option<f64> {
    None
}

/// Go `GetAllMetrics` — the full `--robot-metrics` payload body.
pub fn get_all_metrics() -> Value {
    let timing: Vec<Value> = TIMING_METRICS.iter().map(|m| m.to_json()).collect();
    let cache: Vec<Value> = CACHE_METRICS.iter().map(|m| m.to_json()).collect();
    json!({
        "timing": timing,
        "cache": cache,
        "memory": memory_stats(),
    })
}

/// In-memory snapshot helper used by tests.
pub fn timing_snapshot() -> BTreeMap<&'static str, usize> {
    TIMING_METRICS
        .iter()
        .map(|m| (m.name, m.count.load(Ordering::Relaxed)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timing_records_counts_and_extrema() {
        let m = TimingMetric::new("test_metric");
        m.record(1_000_000); // 1ms
        m.record(3_000_000); // 3ms
        let j = m.to_json();
        assert_eq!(j["count"], 2);
        assert_eq!(j["total_ms"], 4.0);
        assert_eq!(j["avg_ms"], 2.0);
        assert_eq!(j["min_ms"], 1.0);
        assert_eq!(j["max_ms"], 3.0);
    }

    #[test]
    fn zero_count_reports_zeroes_not_nulls() {
        let m = TimingMetric::new("unused_metric");
        let j = m.to_json();
        assert_eq!(j["count"], 0);
        assert_eq!(j["total_ms"], 0.0);
    }

    #[test]
    fn cache_hit_rate() {
        let c = CacheMetric::new("test_cache");
        c.hits.fetch_add(3, Ordering::Relaxed);
        c.misses.fetch_add(1, Ordering::Relaxed);
        let j = c.to_json();
        assert_eq!(j["total"], 4);
        assert_eq!(j["hit_rate"], 0.75);
    }

    #[test]
    fn registered_metric_names_match_go() {
        let names: Vec<&str> = TIMING_METRICS.iter().map(|m| m.name).collect();
        assert_eq!(
            names,
            vec![
                "cycle_detection",
                "topological_sort",
                "triage_analysis",
                "graph_stats_access",
                "vector_search",
                "json_parsing",
                "pagerank_compute",
                "betweenness_compute",
                "hits_compute",
                "graph_load",
                "ui_render",
            ]
        );
        let cache_names: Vec<&str> = CACHE_METRICS.iter().map(|m| m.name).collect();
        assert_eq!(
            cache_names,
            vec![
                "graph_cache",
                "triage_cache",
                "search_cache",
                "metrics_cache",
                "style_cache"
            ]
        );
    }

    #[test]
    fn guard_records_on_drop() {
        // Can't gate on env in test (BV_METRICS may be unset = enabled).
        let before = TIMING_JSON_PARSING.count.load(Ordering::Relaxed);
        {
            let _g = time(&TIMING_JSON_PARSING);
        }
        let after = TIMING_JSON_PARSING.count.load(Ordering::Relaxed);
        assert_eq!(after, before + 1);
    }

    #[test]
    fn memory_stats_shape() {
        let m = memory_stats();
        assert!(m.is_object());
        // Either real values or nulls — never missing keys.
        for key in [
            "heap_alloc_mb",
            "heap_sys_mb",
            "heap_objects_k",
            "gc_cycles",
            "gc_pause_ms",
            "goroutine_count",
        ] {
            assert!(m.get(key).is_some(), "missing {key}");
        }
    }
}
