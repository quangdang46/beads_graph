//! Two-phase analyzer — port of Go `analysis.Analyzer` orchestration
//! (graph.go:65-200 status/config shapes, phase semantics).
//!
//! Phase 1 (sync): degree maps, topo order, density — always immediate.
//! Phase 2: per-metric work on std::threads with per-metric timeouts;
//! panic -> timeout status; MetricStatus per metric.

use crate::algorithms::{
    betweenness::{betweenness, betweenness_approx},
    critical_path::{critical_path_heights, critical_path_nodes},
    cycles::{enumerate_cycles, tarjan_scc},
    hits::hits_default,
    kcore::kcore,
    pagerank::pagerank_default,
    topo::topological_sort_gonum,
};
use bv_graph_core::DiGraph;
use serde::Serialize;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// Per-metric computation outcome (Go `statusEntry`).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct StatusEntry {
    /// computed|approx|timeout|skipped
    pub state: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub sample: usize,
    /// Milliseconds, omitted when zero (Go MarshalJSON omitempty).
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub ms: f64,
}

fn is_zero_usize(v: &usize) -> bool {
    *v == 0
}
fn is_zero_f64(v: &f64) -> bool {
    *v == 0.0
}

impl StatusEntry {
    pub fn computed(ms: f64) -> Self {
        StatusEntry {
            state: "computed".into(),
            ms,
            ..Default::default()
        }
    }
    pub fn skipped(reason: &str) -> Self {
        StatusEntry {
            state: "skipped".into(),
            reason: reason.into(),
            ..Default::default()
        }
    }
    pub fn timeout(ms: f64) -> Self {
        StatusEntry {
            state: "timeout".into(),
            ms,
            ..Default::default()
        }
    }
}

/// Per-metric status map keyed exactly as the Go JSON:
/// PageRank/Betweenness/Eigenvector/HITS/Critical/Cycles/KCore/Articulation/Slack.
/// BTreeMap keeps key order stable in serialized output.
#[derive(Debug, Clone, Default, Serialize)]
pub struct MetricStatus {
    pub page_rank: StatusEntry,
    pub betweenness: StatusEntry,
    pub eigenvector: StatusEntry,
    pub hits: StatusEntry,
    pub critical: StatusEntry,
    pub cycles: StatusEntry,
    pub kcore: StatusEntry,
    pub articulation: StatusEntry,
    pub slack: StatusEntry,
}

/// Go `sourceDateEpochActive` (cmd/bv/main.go:1174-1181): `SOURCE_DATE_EPOCH`
/// is set and, after trimming, parses as a base-10 64-bit integer.
///
/// Go's `strconv.ParseInt(value, 10, 64)` and Rust's `i64::from_str` accept
/// the same language — an optional sign then at least one digit, with no
/// underscores and no base prefix — so the two agree on every input.
pub fn source_date_epoch_active() -> bool {
    std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .map(|v| v.trim().parse::<i64>().is_ok())
        .unwrap_or(false)
}

/// Go's gate on the two order-dependent metrics (pkg/analysis/graph.go:2163 and
/// :2271): `len(stats.TopologicalOrder) == len(a.issueMap)`. Phase 1 only fills
/// `TopologicalOrder` when gonum's `topo.Sort` returns no error, so the gate is
/// "the whole graph is orderable".
///
/// gonum's `topo.Sort` is a DFS that errors on *any* back edge, and a self-edge
/// is a back edge. `topological_sort_gonum` here is Tarjan-based and only
/// rejects multi-node SCCs, so it would let a graph whose only cycle is a
/// self-loop through and report the metric as computed where Go skips it. The
/// scan below is what closes that gap; [`build_graph`] keeps self-loops, so
/// they really do reach here.
pub fn topological_order_available(g: &DiGraph) -> bool {
    let n = g.len();
    if topological_sort_gonum(g).map(|o| o.len() == n) != Some(true) {
        return false;
    }
    !(0..n).any(|v| g.successors_slice(v).contains(&v))
}

impl MetricStatus {
    /// Go `stabilizeRobotMetricStatusForPinnedClock` (cmd/bv/main.go:1190-1202):
    /// zero the elapsed duration of all nine entries when the clock is pinned.
    ///
    /// `statusEntry.MarshalJSON` (pkg/analysis/graph.go:130-147) drops `ms`
    /// whenever `Elapsed == 0`, so a pinned-clock document carries no `ms` key
    /// at all on any metric — that is what makes a golden reproducible at all.
    /// Rust stores a real measured duration and therefore emits nine
    /// wall-clock floats per document; the golden corpus has zero.
    ///
    /// Go applies this at each robot entrypoint, which covers every
    /// serialization of a `MetricStatus` (robot_registry.go:892, :1008, :1984,
    /// and the two triage sites via `stabilizeRobotTriageForPinnedClock`).
    /// Doing it where the status is built is equivalent for those and also
    /// covers the non-robot consumers, which serialize the same struct.
    pub fn stabilize_for_pinned_clock(&mut self) {
        if !source_date_epoch_active() {
            return;
        }
        self.page_rank.ms = 0.0;
        self.betweenness.ms = 0.0;
        self.eigenvector.ms = 0.0;
        self.hits.ms = 0.0;
        self.critical.ms = 0.0;
        self.cycles.ms = 0.0;
        self.kcore.ms = 0.0;
        self.articulation.ms = 0.0;
        self.slack.ms = 0.0;
    }

    /// Serialize with Go's exact JSON keys.
    pub fn to_json_map(&self) -> serde_json::Value {
        serde_json::json!({
            "PageRank": self.page_rank,
            "Betweenness": self.betweenness,
            "Eigenvector": self.eigenvector,
            "HITS": self.hits,
            "Critical": self.critical,
            "Cycles": self.cycles,
            "KCore": self.kcore,
            "Articulation": self.articulation,
            "Slack": self.slack,
        })
    }
}

/// Per-metric Phase 2 timeout budgets (Go `ConfigForSize`).
#[derive(Debug, Clone, Copy)]
pub struct AnalysisBudget {
    pub small_threshold: usize,
    pub medium_threshold: usize,
    pub xl_threshold: usize,
    /// Graph density = edges / (nodes * (nodes - 1)). Used for density-aware
    /// tier decisions matching Go `ConfigForSize` (pkg/analysis/config.go:86).
    pub density: f64,
    /// Per-metric override from BV_PHASE2_TIMEOUT_S (seconds).
    pub override_secs: Option<u64>,
    pub skip_phase2: bool,
    /// Go's `analyzer.SetConfig(analysis.FullAnalysisConfig())` — what
    /// `--force-full-analysis` installs. The tier thresholds above still
    /// describe `ConfigForSize`, so this flag is what makes every accessor
    /// below report the [`full_analysis_config`] answer instead of the
    /// size-derived one.
    pub force_full: bool,
}

impl Default for AnalysisBudget {
    fn default() -> Self {
        AnalysisBudget {
            small_threshold: 100,
            medium_threshold: 500,
            xl_threshold: 2000,
            density: 0.0,
            override_secs: None,
            skip_phase2: false,
            force_full: false,
        }
    }
}

impl AnalysisBudget {
    /// Go `ConfigForSize(nodeCount, edgeCount)` (pkg/analysis/config.go:86-88):
    /// the density that drives the sparse/dense branch is
    /// `edges / (nodes * (nodes - 1))`, the same expression Phase 1 uses.
    pub fn for_graph(g: &DiGraph) -> AnalysisBudget {
        let n = g.len() as f64;
        let density = if n <= 1.0 {
            0.0
        } else {
            g.edge_count() as f64 / (n * (n - 1.0))
        };
        AnalysisBudget {
            density,
            ..AnalysisBudget::default()
        }
    }

    /// Timeout for a metric at a given node count (Go ConfigForSize tiers).
    pub fn timeout_for(&self, nodes: usize) -> Duration {
        if let Some(s) = self.override_secs {
            return Duration::from_secs(s);
        }
        if self.force_full {
            // FullAnalysisConfig gives every timeout-raced metric the same
            // 30s budget (Go config.go:234, :237, :240, :243).
            return Duration::from_secs(30);
        }
        match nodes {
            n if n < self.small_threshold => Duration::from_secs(2),
            n if n < self.medium_threshold => Duration::from_millis(500),
            n if n < self.xl_threshold => Duration::from_millis(300),
            _ => Duration::from_millis(200),
        }
    }

    pub fn max_cycles(&self, nodes: usize) -> usize {
        if self.force_full {
            // Go config.go:244 — MaxCyclesToStore: 10000.
            return 10000;
        }
        match nodes {
            n if n < self.small_threshold => 1000,
            n if n < self.medium_threshold => 100,
            n if n < self.xl_threshold => 50,
            _ => 10,
        }
    }

    /// Whether cycles should be skipped entirely (Go: XL always skips).
    pub fn skip_cycles(&self, nodes: usize) -> bool {
        !self.force_full && nodes >= self.xl_threshold
    }

    /// Whether HITS should be skipped for this graph (Go: skip when XL + dense).
    pub fn skip_hits(&self, nodes: usize) -> bool {
        if self.force_full {
            return false;
        }
        // XL (>2000 nodes) AND density >= 0.001 -> skip HITS.
        nodes >= self.xl_threshold && self.density >= 0.001
    }

    /// Whether betweenness should use approximate mode (Go: approx when *sparse*).
    /// Returns `(use_approx, skip)` where skip means don't compute at all.
    pub fn betweenness_mode(&self, nodes: usize) -> (bool, bool) {
        if self.force_full {
            // BetweennessExact and ComputeBetweenness: true (Go config.go:232-233).
            return (false, false);
        }
        if nodes >= self.xl_threshold {
            // XL: always approximate (Go parity).
            (true, false)
        } else if nodes >= self.medium_threshold {
            // Large (500-2000). Go config.go:180-190 samples a *sparse* large
            // graph and refuses a dense one outright — density >= 0.01 is the
            // skip branch, not the sample branch.
            if self.density < 0.01 {
                (true, false)
            } else {
                (false, true)
            }
        } else {
            // Small/Medium: always exact.
            (false, false)
        }
    }

    /// Go `RecommendSampleSize` (pkg/analysis/betweenness_approx.go:489).
    /// `edge_count` is reserved in Go for future density-aware heuristics and
    /// is deliberately unused; the parameter is kept so call sites read the
    /// same as Go's.
    pub fn recommend_sample_size(&self, node_count: usize, _edge_count: usize) -> usize {
        if node_count == 0 {
            return 0;
        }
        if node_count < 100 {
            node_count
        } else if node_count < 500 {
            // 20% sample, floored at 50.
            std::cmp::max(node_count / 5, 50)
        } else if node_count < 2000 {
            100
        } else {
            200
        }
    }
}

/// Betweenness as Go's Phase 2 actually computes it — the value that lands in
/// `stats.betweenness` (graph.go:2300) and that every downstream consumer reads:
/// impact scoring (priority.go:231), label health, and the insights document.
///
/// `ConfigForSize` (config.go:86-190) picks the algorithm from the node count
/// and the graph's density rather than always running Brandes — exact below 500
/// nodes, sampled above it, and skipped outright when a large graph is dense
/// enough. Calling [`betweenness`] directly answers a different question, so a
/// caller that wants Go's number wants this instead. Skipping yields an empty
/// vector because Go's `localBetweenness` is left nil on that path, which every
/// reader treats as "no scores" rather than "a score of zero".
pub fn go_betweenness(g: &DiGraph) -> Vec<f64> {
    let n = g.len();
    let budget = AnalysisBudget::for_graph(g);
    let (use_approx, skip) = budget.betweenness_mode(n);
    if skip {
        return Vec::new();
    }
    if use_approx {
        betweenness_approx(g, budget.recommend_sample_size(n, g.edge_count()), Some(1))
    } else {
        betweenness(g)
    }
}

/// Go `AnalysisConfig` as it appears in robot output
/// (pkg/analysis/config.go:12). Field order is part of the compatibility
/// contract: Go serializes the struct positionally, and the differential
/// harness compares canonicalized JSON, so a reordered field is a drift.
///
/// The two execution-state fields Go marks `json:"-"` (`DisableCache`,
/// `RunToCompletion`) are not emitted and so are absent here too.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct AnalysisConfigReport {
    #[serde(rename = "ComputeBetweenness")]
    pub compute_betweenness: bool,
    /// Nanoseconds — Go marshals `time.Duration` as an int64 count of ns.
    #[serde(rename = "BetweennessTimeout")]
    pub betweenness_timeout_ns: i64,
    #[serde(rename = "BetweennessSkipReason")]
    pub betweenness_skip_reason: &'static str,
    /// "exact" | "approximate" | "skip" (Go `BetweennessMode`).
    #[serde(rename = "BetweennessMode")]
    pub betweenness_mode: &'static str,
    #[serde(rename = "BetweennessSampleSize")]
    pub betweenness_sample_size: usize,
    #[serde(rename = "BetweennessIsApproximate")]
    pub betweenness_is_approximate: bool,

    #[serde(rename = "ComputePageRank")]
    pub compute_page_rank: bool,
    #[serde(rename = "PageRankTimeout")]
    pub page_rank_timeout_ns: i64,
    #[serde(rename = "PageRankSkipReason")]
    pub page_rank_skip_reason: &'static str,

    #[serde(rename = "ComputeHITS")]
    pub compute_hits: bool,
    #[serde(rename = "HITSTimeout")]
    pub hits_timeout_ns: i64,
    #[serde(rename = "HITSSkipReason")]
    pub hits_skip_reason: &'static str,

    #[serde(rename = "ComputeCycles")]
    pub compute_cycles: bool,
    #[serde(rename = "CyclesTimeout")]
    pub cycles_timeout_ns: i64,
    #[serde(rename = "MaxCyclesToStore")]
    pub max_cycles_to_store: usize,
    #[serde(rename = "CyclesSkipReason")]
    pub cycles_skip_reason: &'static str,

    #[serde(rename = "ComputeEigenvector")]
    pub compute_eigenvector: bool,
    #[serde(rename = "ComputeCriticalPath")]
    pub compute_critical_path: bool,
    #[serde(rename = "ComputeKCore")]
    pub compute_kcore: bool,
    #[serde(rename = "ComputeArticulation")]
    pub compute_articulation: bool,
    #[serde(rename = "ComputeSlack")]
    pub compute_slack: bool,
}

/// A metric the configuration turns off, and why — Go `SkippedMetric`
/// (pkg/analysis/config.go:345-349).
#[derive(Debug, Clone, PartialEq)]
pub struct SkippedMetric {
    pub name: &'static str,
    pub reason: &'static str,
}

impl Default for AnalysisConfigReport {
    /// Go's zero `AnalysisConfig`: every metric off, every timeout zero, every
    /// betweenness mode empty. `config_for_size` and `full_analysis_config` are
    /// the constructors for a usable configuration.
    fn default() -> Self {
        Self {
            compute_betweenness: false,
            betweenness_timeout_ns: 0,
            betweenness_skip_reason: "",
            betweenness_mode: "",
            betweenness_sample_size: 0,
            betweenness_is_approximate: false,
            compute_page_rank: false,
            page_rank_timeout_ns: 0,
            page_rank_skip_reason: "",
            compute_hits: false,
            hits_timeout_ns: 0,
            hits_skip_reason: "",
            compute_cycles: false,
            cycles_timeout_ns: 0,
            max_cycles_to_store: 0,
            cycles_skip_reason: "",
            compute_eigenvector: false,
            compute_critical_path: false,
            compute_kcore: false,
            compute_articulation: false,
            compute_slack: false,
        }
    }
}

impl AnalysisConfigReport {
    /// The metrics this configuration skips, in Go's order (Betweenness,
    /// PageRank, HITS, Cycles) — Go `AnalysisConfig.SkippedMetrics`
    /// (pkg/analysis/config.go:314-343). The startup profile report prints the
    /// names; the recommendation engine tests the list's emptiness.
    pub fn skipped_metrics(&self) -> Vec<SkippedMetric> {
        let mut skipped = Vec::new();
        if !self.compute_betweenness {
            skipped.push(SkippedMetric {
                name: "Betweenness",
                reason: self.betweenness_skip_reason,
            });
        }
        if !self.compute_page_rank {
            skipped.push(SkippedMetric {
                name: "PageRank",
                reason: self.page_rank_skip_reason,
            });
        }
        if !self.compute_hits {
            skipped.push(SkippedMetric {
                name: "HITS",
                reason: self.hits_skip_reason,
            });
        }
        if !self.compute_cycles {
            skipped.push(SkippedMetric {
                name: "Cycles",
                reason: self.cycles_skip_reason,
            });
        }
        skipped
    }
}

const NS_PER_SEC: i64 = 1_000_000_000;
const NS_PER_MS: i64 = 1_000_000;

/// Go `ConfigForSize` (pkg/analysis/config.go:98) — byte port.
///
/// Tiers, all keyed on node count:
/// - `< 100`   small: exact betweenness, 2s budgets, 1000 cycles stored
/// - `< 500`   medium: exact betweenness, 500ms budgets, 100 cycles
/// - `< 2000`  large: approximate betweenness when density < 0.01 (500ms
///   budget, sampled), otherwise betweenness is skipped entirely
/// - `>= 2000` XL: approximate betweenness always; cycles are skipped; HITS
///   only survives on very sparse graphs (density < 0.001)
///
/// `node_count` is the issue count, not the number of nodes carrying edges —
/// the tier boundary is drawn against the full set.
pub fn config_for_size(node_count: usize, edge_count: usize, density: f64) -> AnalysisConfigReport {
    let sample = |n: usize| {
        if n == 0 {
            0
        } else if n < 100 {
            n
        } else if n < 500 {
            std::cmp::max(n / 5, 50)
        } else if n < 2000 {
            100
        } else {
            200
        }
    };
    let _ = edge_count; // reserved in Go too

    match node_count {
        n if n < 100 => AnalysisConfigReport {
            compute_betweenness: true,
            betweenness_timeout_ns: 2 * NS_PER_SEC,
            betweenness_skip_reason: "",
            betweenness_mode: "exact",
            betweenness_sample_size: 0,
            betweenness_is_approximate: false,
            compute_page_rank: true,
            page_rank_timeout_ns: 2 * NS_PER_SEC,
            page_rank_skip_reason: "",
            compute_hits: true,
            hits_timeout_ns: 2 * NS_PER_SEC,
            hits_skip_reason: "",
            compute_cycles: true,
            cycles_timeout_ns: 2 * NS_PER_SEC,
            max_cycles_to_store: 1000,
            cycles_skip_reason: "",
            compute_eigenvector: true,
            compute_critical_path: true,
            compute_kcore: true,
            compute_articulation: true,
            compute_slack: true,
        },
        n if n < 500 => AnalysisConfigReport {
            compute_betweenness: true,
            betweenness_timeout_ns: 500 * NS_PER_MS,
            betweenness_skip_reason: "",
            betweenness_mode: "exact",
            betweenness_sample_size: 0,
            betweenness_is_approximate: false,
            compute_page_rank: true,
            page_rank_timeout_ns: 500 * NS_PER_MS,
            page_rank_skip_reason: "",
            compute_hits: true,
            hits_timeout_ns: 500 * NS_PER_MS,
            hits_skip_reason: "",
            compute_cycles: true,
            cycles_timeout_ns: 500 * NS_PER_MS,
            max_cycles_to_store: 100,
            cycles_skip_reason: "",
            compute_eigenvector: true,
            compute_critical_path: true,
            compute_kcore: true,
            compute_articulation: true,
            compute_slack: true,
        },
        n if n < 2000 => {
            // Large: approximate betweenness for sparse graphs, skip for dense.
            let (bw, bw_timeout, bw_reason, bw_mode, bw_sample) = if density < 0.01 {
                (true, 500 * NS_PER_MS, "", "approximate", sample(node_count))
            } else {
                (false, 0, "graph too dense (density > 0.01)", "skip", 0)
            };
            AnalysisConfigReport {
                compute_betweenness: bw,
                betweenness_timeout_ns: bw_timeout,
                betweenness_skip_reason: bw_reason,
                betweenness_mode: bw_mode,
                betweenness_sample_size: bw_sample,
                betweenness_is_approximate: false,
                compute_page_rank: true,
                page_rank_timeout_ns: 300 * NS_PER_MS,
                page_rank_skip_reason: "",
                compute_hits: true,
                hits_timeout_ns: 300 * NS_PER_MS,
                hits_skip_reason: "",
                compute_cycles: true,
                cycles_timeout_ns: 300 * NS_PER_MS,
                max_cycles_to_store: 50,
                cycles_skip_reason: "",
                compute_eigenvector: true,
                compute_critical_path: true,
                compute_kcore: true,
                compute_articulation: true,
                compute_slack: true,
            }
        }
        _ => {
            // XL: HITS only on very sparse graphs; cycles never run.
            let (hits, hits_timeout, hits_reason) = if density < 0.001 {
                (true, 200 * NS_PER_MS, "")
            } else {
                (false, 0, "graph too large and dense")
            };
            AnalysisConfigReport {
                compute_betweenness: true,
                betweenness_timeout_ns: 500 * NS_PER_MS,
                betweenness_skip_reason: "",
                betweenness_mode: "approximate",
                betweenness_sample_size: sample(node_count),
                betweenness_is_approximate: false,
                compute_page_rank: true,
                page_rank_timeout_ns: 200 * NS_PER_MS,
                page_rank_skip_reason: "",
                compute_hits: hits,
                hits_timeout_ns: hits_timeout,
                hits_skip_reason: hits_reason,
                compute_cycles: false,
                cycles_timeout_ns: 0,
                max_cycles_to_store: 10,
                cycles_skip_reason: "graph too large (>2000 nodes)",
                compute_eigenvector: true,
                compute_critical_path: true,
                compute_kcore: true,
                compute_articulation: true,
                compute_slack: true,
            }
        }
    }
}

/// Go `FullAnalysisConfig` (pkg/analysis/config.go:230) — byte port.
///
/// Every metric enabled regardless of graph size, exact betweenness forced,
/// and a 30s budget on each of the four timeout-raced metrics instead of the
/// sub-second tier budgets `config_for_size` hands out. This is the config
/// `--force-full-analysis` swaps in at each of its six call sites (Go
/// cmd/bv/robot_registry.go:860, :927, :1861; cmd/bv/main.go:3575, :3654,
/// :4984) — it is a config override, not a cache switch.
///
/// `BetweennessSampleSize` and `BetweennessIsApproximate` are left unset in
/// Go, so they land on Go's zero values here too. `ApplyEnvOverrides`
/// (config.go:369) is not applied by this function for the same reason
/// `config_for_size` does not apply it: the `BV_SKIP_PHASE2` and
/// `BV_PHASE2_TIMEOUT_S` knobs live on [`AnalysisBudget`], which is what
/// drives the actual computation.
pub fn full_analysis_config() -> AnalysisConfigReport {
    AnalysisConfigReport {
        compute_betweenness: true,
        betweenness_timeout_ns: 30 * NS_PER_SEC,
        betweenness_skip_reason: "",
        betweenness_mode: "exact",
        betweenness_sample_size: 0,
        betweenness_is_approximate: false,
        compute_page_rank: true,
        page_rank_timeout_ns: 30 * NS_PER_SEC,
        page_rank_skip_reason: "",
        compute_hits: true,
        hits_timeout_ns: 30 * NS_PER_SEC,
        hits_skip_reason: "",
        compute_cycles: true,
        cycles_timeout_ns: 30 * NS_PER_SEC,
        max_cycles_to_store: 10000,
        cycles_skip_reason: "",
        compute_eigenvector: true,
        compute_critical_path: true,
        compute_kcore: true,
        compute_articulation: true,
        compute_slack: true,
    }
}

/// Detailed timing profile of a synchronous analysis pass — port of Go
/// `StartupProfile` (pkg/analysis/graph.go:27-61), populated by
/// [`analyze_with_profile`].
///
/// Every timing serializes as an integer count of nanoseconds because that is
/// how Go's `time.Duration` marshals, and the `--profile-startup` JSON compares
/// the emitted document shape. Field order is Go's struct order.
#[derive(Debug, Clone, Default, Serialize)]
pub struct StartupProfile {
    // Data characteristics
    pub node_count: usize,
    pub edge_count: usize,
    pub density: f64,

    // Phase 1 timings
    /// Analyzer construction. Go's `runProfileStartup` overwrites this after
    /// the fact with the time `NewAnalyzer` spent (cmd/bv/main.go:5000); the
    /// analysis pass itself never sets it.
    #[serde(serialize_with = "ser_duration_ns")]
    pub build_graph: Duration,
    #[serde(serialize_with = "ser_duration_ns")]
    pub degree: Duration,
    #[serde(serialize_with = "ser_duration_ns")]
    pub topo_sort: Duration,
    #[serde(rename = "phase1_total", serialize_with = "ser_duration_ns")]
    pub phase1: Duration,

    // Phase 2 timings (zero if skipped)
    #[serde(serialize_with = "ser_duration_ns")]
    pub pagerank: Duration,
    #[serde(rename = "pagerank_timeout")]
    pub pagerank_timeout: bool,
    #[serde(serialize_with = "ser_duration_ns")]
    pub betweenness: Duration,
    #[serde(rename = "betweenness_timeout")]
    pub betweenness_timeout: bool,
    #[serde(serialize_with = "ser_duration_ns")]
    pub eigenvector: Duration,
    #[serde(serialize_with = "ser_duration_ns")]
    pub hits: Duration,
    #[serde(rename = "hits_timeout")]
    pub hits_timeout: bool,
    #[serde(rename = "critical_path", serialize_with = "ser_duration_ns")]
    pub critical_path: Duration,
    #[serde(serialize_with = "ser_duration_ns")]
    pub cycles: Duration,
    #[serde(rename = "cycles_timeout")]
    pub cycles_timeout: bool,
    /// One stored representative per cyclic component.
    pub cycle_count: usize,
    #[serde(serialize_with = "ser_duration_ns")]
    pub kcore: Duration,
    #[serde(serialize_with = "ser_duration_ns")]
    pub articulation: Duration,
    #[serde(serialize_with = "ser_duration_ns")]
    pub slack: Duration,
    #[serde(rename = "phase2_total", serialize_with = "ser_duration_ns")]
    pub phase2: Duration,

    // Configuration used
    pub config: AnalysisConfigReport,

    // Totals
    #[serde(serialize_with = "ser_duration_ns")]
    pub total: Duration,
}

/// Go marshals `time.Duration` as an int64 count of nanoseconds.
fn ser_duration_ns<S: serde::Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_i64(i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
}

/// Phase 1 results — always available immediately.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Phase1Stats {
    /// out-degree per node (dependencies this issue has).
    pub out_degree: BTreeMap<String, usize>,
    /// in-degree per node (issues depending on this one).
    pub in_degree: BTreeMap<String, usize>,
    /// Topological order, gonum `topo.Sort` walked backwards like Go
    /// (dependencies first). `None` when gonum reports a cyclic graph, which
    /// is how Go leaves the field nil (serialized as JSON `null`).
    pub topological_order: Option<Vec<String>>,
    pub density: f64,
    pub node_count: usize,
    pub edge_count: usize,
}

/// Full analysis result. Phase 2 metrics are Option until computed.
#[derive(Debug, Clone, Default)]
pub struct GraphAnalysis {
    pub phase1: Phase1Stats,
    pub status: MetricStatus,

    pub page_rank: Option<BTreeMap<String, f64>>,
    pub betweenness: Option<BTreeMap<String, f64>>,
    pub eigenvector: Option<BTreeMap<String, f64>>,
    pub hubs: Option<BTreeMap<String, f64>>,
    pub authorities: Option<BTreeMap<String, f64>>,
    pub critical_path_score: Option<BTreeMap<String, f64>>,
    pub core_number: Option<BTreeMap<String, u32>>,
    pub articulation: Option<Vec<String>>,
    pub slack: Option<BTreeMap<String, f64>>,
    pub cycles: Option<Vec<Vec<String>>>,
}

/// Build the blocking-only DiGraph from loaded issues.
/// Edge direction matches wasm crate convention: issue -> its dependency
/// (u depends on v => edge u -> v). Empty type counts as blocks (legacy).
pub fn build_graph(issues: &[bv_core::model::Issue]) -> DiGraph {
    // Go sorts the issue ids before assigning dense node indices
    // (graph.go:1570-1585), and HITS sums over those indices, so the node
    // order changes the floating-point result. Build in sorted-id order to
    // match.
    let mut sorted: Vec<&bv_core::model::Issue> = issues.iter().collect();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));

    let mut g = DiGraph::with_capacity(sorted.len(), sorted.len() * 2);
    for i in &sorted {
        g.add_node(&i.id);
    }
    let mut edges: Vec<(usize, usize)> = Vec::with_capacity(sorted.len() * 2);
    for i in &sorted {
        let from = match g.node_idx(&i.id) {
            Some(x) => x,
            None => continue,
        };
        for dep in &i.dependencies {
            // Only blocking types gate the graph (related/parent-child don't).
            if !dep.r#type.is_blocking() {
                continue;
            }
            let target = dep.effective_depends_on().to_string();
            if let Some(to) = g.node_idx(&target) {
                // Go keeps self-loops (gonum SetEdge accepts them); they
                // participate in out-degree and cycle detection there.
                edges.push((from, to));
            }
        }
    }
    // Go's `buildCachedAdjacency` sorts every node's neighbour list before the
    // Brandes walk (betweenness_approx.go:59, :70), and `network.Betweenness`
    // consumes the same gonum graph. Successor order is not cosmetic: the BFS
    // stack order follows it, and the delta accumulation is floating point, so
    // an unsorted list changes the per-node scores. Measured on
    // large_cyclic_600, 473 of 600 nodes differ between insertion order and
    // target-index order.
    //
    // Node indices are already id-sorted (see above), so sorting by target
    // index is exactly Go's `sort.Ints(neighbors)`.
    edges.sort_by_key(|&(from, to)| (from, to));
    for (from, to) in edges {
        g.add_edge(from, to);
    }
    g
}

/// Phase 1: degrees + topo + density (sync, cheap).
pub fn analyze_phase1(g: &DiGraph) -> Phase1Stats {
    phase1_timed(g).0
}

/// [`analyze_phase1`] plus the two sub-phase timings Go's
/// `computePhase1WithProfile` records (graph.go:1934-1964).
fn phase1_timed(g: &DiGraph) -> (Phase1Stats, Duration, Duration) {
    let n = g.len();
    let degree_start = Instant::now();
    let mut out_degree = BTreeMap::new();
    let mut in_degree = BTreeMap::new();
    for idx in 0..n {
        let id = g.node_id(idx).unwrap_or_default().to_string();
        out_degree.insert(id.clone(), g.out_degree(idx));
        in_degree.insert(id, g.in_degree(idx));
    }
    let degree = degree_start.elapsed();

    let topo_start = Instant::now();
    // Go: `sorted, err := topo.Sort(a.g)` then walks the slice backwards
    // (graph.go:1950-1954). gonum's topo.Sort is Tarjan-SCC based, so a Kahn
    // order here is valid but ordered differently; the insights field has to
    // reproduce gonum exactly. On a cyclic graph gonum returns an Unorderable
    // error and Go leaves the field nil, so `None` stays `None` here.
    let topological_order = topological_sort_gonum(g).map(|order| {
        order
            .iter()
            .rev()
            .map(|&idx| g.node_id(idx).unwrap_or_default().to_string())
            .collect()
    });
    let topo_sort = topo_start.elapsed();

    (
        Phase1Stats {
            out_degree,
            in_degree,
            topological_order,
            density: g.density(),
            node_count: n,
            edge_count: g.edge_count(),
        },
        degree,
        topo_sort,
    )
}

fn idx_to_score_map(g: &DiGraph, scores: Vec<f64>) -> BTreeMap<String, f64> {
    scores
        .into_iter()
        .enumerate()
        .map(|(i, s)| (g.node_id(i).unwrap_or_default().to_string(), s))
        .collect()
}

/// Run one Phase-2 metric on a thread with timeout; panic or overrun -> Err.
fn run_with_timeout<T: Send + 'static>(
    budget: Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, ()> {
    // A zero budget is an already-fired timer, not a race. Go arms
    // `time.NewTimer(config.PageRankTimeout)` and selects against it
    // (graph.go:2020-2022); a zero duration arms a timer that is firing now,
    // so the timeout branch is taken without the worker having been given a
    // chance to run.
    //
    // `recv_timeout(Duration::ZERO)` does not model that — it degrades to a
    // non-blocking `try_recv`, which *loses* whenever the worker happens to
    // win the scheduler. On an 8-node graph that is a genuine coin-flip, and
    // the metric it flips is the parity-visible `pagerank_timeout` flag. It
    // failed intermittently here for exactly that reason.
    //
    // `AnalysisBudget::timeout_for` never returns zero, so this is reached only
    // by an explicit zero in `AnalysisConfigReport`, which is the case Go's
    // timer also resolves as a timeout.
    if budget.is_zero() {
        return Err(());
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // Panic inside f is caught by converting the join result.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        let _ = tx.send(result);
    });
    match rx.recv_timeout(budget) {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(_)) => Err(()), // panic -> timeout status (Go parity)
        Err(_) => Err(()),     // recv_timeout elapsed
    }
}

/// Execute Phase 2 synchronously-with-timeouts (call from a worker thread of
/// your own; this function blocks until all metrics settle or time out).
/// Mirrors Go's goroutine-per-metric fan-out but joins before returning.
/// Takes `Arc<DiGraph>` so metric closures own a handle ('static bound).
pub fn analyze_phase2_blocking(
    g: std::sync::Arc<DiGraph>,
    budget: &AnalysisBudget,
) -> (MetricStatus, GraphAnalysisPhase2) {
    let mut status = MetricStatus::default();
    let mut out = GraphAnalysisPhase2::default();
    let n = g.len();
    // Go runs the two order-dependent metrics only when Phase 1's topological
    // sort covered every issue (graph.go:2163, :2271); on a cyclic graph it
    // skips both, reports the cycle as the reason, and leaves the map nil.
    // Both height DPs need a valid order, so running them anyway would report
    // a metric Go never computed.
    let order_available = topological_order_available(&g);

    if budget.skip_phase2 {
        let reason = "BV_SKIP_PHASE2 set";
        status.page_rank = StatusEntry::skipped(reason);
        status.betweenness = StatusEntry::skipped(reason);
        status.eigenvector = StatusEntry::skipped(reason);
        status.hits = StatusEntry::skipped(reason);
        status.critical = StatusEntry::skipped(reason);
        status.cycles = StatusEntry::skipped(reason);
        // k-core/articulation/slack REMAIN enabled (Go ApplyEnvOverrides).
    } else {
        // PageRank
        let t0 = Instant::now();
        let gc = std::sync::Arc::clone(&g);
        match run_with_timeout(budget.timeout_for(n), move || {
            let _t = crate::metrics::time(&crate::metrics::TIMING_PAGERANK_COMPUTE);
            pagerank_default(&gc)
        }) {
            Ok(pr) => {
                status.page_rank = StatusEntry::computed(t0.elapsed().as_secs_f64() * 1000.0);
                out.page_rank = Some(idx_to_score_map(&g, pr));
            }
            Err(()) => status.page_rank = StatusEntry::timeout(0.0),
        }

        // Betweenness: density-aware mode selection (Go ConfigForSize parity).
        let t0 = Instant::now();
        let sample = recommend_sample_size(n);
        let (bw_approx, bw_skip) = budget.betweenness_mode(n);
        if bw_skip {
            status.betweenness = StatusEntry::skipped("graph too dense (density > 0.01)");
        } else if !bw_approx {
            let gc = std::sync::Arc::clone(&g);
            match run_with_timeout(budget.timeout_for(n), move || {
                let _t = crate::metrics::time(&crate::metrics::TIMING_BETWEENNESS_COMPUTE);
                betweenness(&gc)
            }) {
                Ok(bw) => {
                    status.betweenness = StatusEntry::computed(t0.elapsed().as_secs_f64() * 1000.0);
                    out.betweenness = Some(idx_to_score_map(&g, bw));
                }
                Err(()) => status.betweenness = StatusEntry::timeout(0.0),
            }
        } else {
            let gc = std::sync::Arc::clone(&g);
            match run_with_timeout(budget.timeout_for(n), move || {
                let _t = crate::metrics::time(&crate::metrics::TIMING_BETWEENNESS_COMPUTE);
                betweenness_approx(&gc, sample, Some(1))
            }) {
                Ok(bw) => {
                    // Go: state stays "computed" with reason "approximate"
                    // (betweennessReason) plus the sample size.
                    let mut e = StatusEntry::computed(t0.elapsed().as_secs_f64() * 1000.0);
                    e.reason = "approximate".into();
                    e.sample = sample;
                    status.betweenness = e;
                    out.betweenness = Some(idx_to_score_map(&g, bw));
                }
                Err(()) => status.betweenness = StatusEntry::timeout(0.0),
            }
        }

        // Eigenvector (fixed 50 iterations — Go parity).
        let t0 = Instant::now();
        let gc = std::sync::Arc::clone(&g);
        match run_with_timeout(budget.timeout_for(n), move || {
            crate::algorithms::eigenvector::eigenvector_default(&gc)
        }) {
            Ok(ev) => {
                status.eigenvector = StatusEntry::computed(t0.elapsed().as_secs_f64() * 1000.0);
                out.eigenvector = Some(idx_to_score_map(&g, ev));
            }
            Err(()) => status.eigenvector = StatusEntry::timeout(0.0),
        }

        // HITS (tol 1e-3 — Go network.HITS(g, 1e-3)).
        // Skip for XL dense graphs (Go ConfigForSize: density >= 0.001).
        if budget.skip_hits(n) {
            status.hits = StatusEntry::skipped("graph too large and dense");
        } else {
            let t0 = Instant::now();
            let gc = std::sync::Arc::clone(&g);
            match run_with_timeout(budget.timeout_for(n), move || {
                let _t = crate::metrics::time(&crate::metrics::TIMING_HITS_COMPUTE);
                hits_default(&gc)
            }) {
                Ok(h) => {
                    status.hits = StatusEntry::computed(t0.elapsed().as_secs_f64() * 1000.0);
                    out.hubs = Some(idx_to_score_map(&g, h.hubs));
                    out.authorities = Some(idx_to_score_map(&g, h.authorities));
                }
                Err(()) => status.hits = StatusEntry::timeout(0.0),
            }
        }

        // Critical path heights DP. Go's unavailable branch still charges the
        // elapsed time to the profile (graph.go:2179), but a measured duration
        // is not reproducible and the pinned-clock pass zeroes it, so the
        // skipped entry carries ms 0 — byte-identical to Go under
        // SOURCE_DATE_EPOCH, and the only deterministic choice without one.
        if order_available {
            let t0 = Instant::now();
            let gc = std::sync::Arc::clone(&g);
            match run_with_timeout(budget.timeout_for(n), move || critical_path_heights(&gc)) {
                Ok(heights) => {
                    status.critical = StatusEntry::computed(t0.elapsed().as_secs_f64() * 1000.0);
                    let mut m = BTreeMap::new();
                    for (i, v) in heights.into_iter().enumerate() {
                        m.insert(g.node_id(i).unwrap_or_default().to_string(), v);
                    }
                    out.critical_path_score = Some(m);
                }
                Err(()) => status.critical = StatusEntry::timeout(0.0),
            }
        } else {
            status.critical = StatusEntry::skipped(CYCLE_UNAVAILABLE_REASON);
        }

        // Cycles: skip entirely for XL graphs (Go ConfigForSize: >2000 nodes).
        // Tarjan pre-check then enumerate capped by tier.
        if budget.skip_cycles(n) {
            status.cycles = StatusEntry::skipped("graph too large (>2000 nodes)");
        } else {
            let t0 = Instant::now();
            let cap = budget.max_cycles(n);
            let scc = tarjan_scc(&g);
            if !scc.has_cycles {
                status.cycles = StatusEntry::computed(t0.elapsed().as_secs_f64() * 1000.0);
                out.cycles = Some(Vec::new());
            } else {
                let gc = std::sync::Arc::clone(&g);
                match run_with_timeout(budget.timeout_for(n), move || enumerate_cycles(&gc, cap)) {
                    Ok(cycles) => {
                        status.cycles = StatusEntry::computed(t0.elapsed().as_secs_f64() * 1000.0);
                        let named: Vec<Vec<String>> = cycles
                            .into_iter()
                            .map(|c| {
                                c.into_iter()
                                    .map(|i| g.node_id(i).unwrap_or_default().to_string())
                                    .collect()
                            })
                            .collect();
                        out.cycles = Some(named);
                    }
                    Err(()) => status.cycles = StatusEntry::timeout(0.0),
                }
            }
        } // end else !skip_cycles
    }

    // k-core / articulation / slack: always-on even under BV_SKIP_PHASE2.
    let t0 = Instant::now();
    let gc = std::sync::Arc::clone(&g);
    match run_with_timeout(budget.timeout_for(n), move || kcore(&gc)) {
        Ok(cores) => {
            status.kcore = StatusEntry::computed(t0.elapsed().as_secs_f64() * 1000.0);
            let mut m = BTreeMap::new();
            for (i, v) in cores.into_iter().enumerate() {
                m.insert(g.node_id(i).unwrap_or_default().to_string(), v);
            }
            out.core_number = Some(m);
        }
        Err(()) => status.kcore = StatusEntry::timeout(0.0),
    }

    let gc = std::sync::Arc::clone(&g);
    match run_with_timeout(budget.timeout_for(n), move || {
        crate::algorithms::articulation::articulation_points(&gc)
    }) {
        Ok(points) => {
            // Go records `profile.Articulation = 0` with the comment "Computed
            // together with k-core" (pkg/analysis/graph.go:2266) — the cut
            // vertices fall out of the k-core pass, so there is no separate
            // duration to report. Timing this independently made the status
            // entry carry an `ms` the oracle never emits, and since
            // `statusEntry` drops the key only at exactly zero, the extra key
            // showed up in every `--robot-insights` payload.
            status.articulation = StatusEntry::computed(0.0);
            out.articulation = Some(
                points
                    .into_iter()
                    .map(|i| g.node_id(i).unwrap_or_default().to_string())
                    .collect(),
            );
        }
        Err(()) => status.articulation = StatusEntry::timeout(0.0),
    }

    let t0 = Instant::now();
    // Go gate at pkg/analysis/graph.go:2271, with the same unavailable branch
    // as critical path above (graph.go:2163).
    if order_available {
        let gc = std::sync::Arc::clone(&g);
        match run_with_timeout(budget.timeout_for(n), move || {
            crate::algorithms::slack::slack(&gc)
        }) {
            Ok(slacks) => {
                status.slack = StatusEntry::computed(t0.elapsed().as_secs_f64() * 1000.0);
                out.slack = Some(idx_to_score_map(&g, slacks));
            }
            Err(()) => status.slack = StatusEntry::timeout(0.0),
        }
    } else {
        status.slack = StatusEntry::skipped(CYCLE_UNAVAILABLE_REASON);
    }

    status.stabilize_for_pinned_clock();
    (status, out)
}

/// Phase 2 metric payloads.
#[derive(Debug, Clone, Default)]
pub struct GraphAnalysisPhase2 {
    pub page_rank: Option<BTreeMap<String, f64>>,
    pub betweenness: Option<BTreeMap<String, f64>>,
    pub eigenvector: Option<BTreeMap<String, f64>>,
    pub hubs: Option<BTreeMap<String, f64>>,
    pub authorities: Option<BTreeMap<String, f64>>,
    pub critical_path_score: Option<BTreeMap<String, f64>>,
    pub core_number: Option<BTreeMap<String, u32>>,
    pub articulation: Option<Vec<String>>,
    pub slack: Option<BTreeMap<String, f64>>,
    pub cycles: Option<Vec<Vec<String>>>,
}

/// Go: `RecommendSampleSize` tiers.
pub fn recommend_sample_size(nodes: usize) -> usize {
    match nodes {
        0..=99 => nodes,
        100..=499 => (nodes / 5).max(50),
        500..=1999 => 100,
        _ => 200,
    }
}

/// Critical path node list convenience (max height nodes).
pub fn critical_path(g: &DiGraph) -> Vec<usize> {
    critical_path_nodes(g)
}

/// Go's reason for an order-dependent metric it could not run
/// (pkg/analysis/graph.go:2177 and :2274).
pub const CYCLE_UNAVAILABLE_REASON: &str =
    "dependency graph contains a cycle; topological order unavailable";

/// Go `stateFromTiming` (pkg/analysis/graph.go:229-238).
fn state_from_timing(enabled: bool, timed_out: bool) -> &'static str {
    if !enabled {
        "skipped"
    } else if timed_out {
        "timeout"
    } else {
        "computed"
    }
}

/// Go `emptyGraphMetricStatus` (pkg/analysis/graph.go:240-254). k-core and
/// articulation share a computation, so either switch marks both computed.
fn empty_graph_metric_status(config: &AnalysisConfigReport) -> MetricStatus {
    let kcore = config.compute_kcore || config.compute_articulation;
    let articulation = config.compute_articulation || config.compute_kcore;
    let entry = |on: bool| StatusEntry {
        state: state_from_timing(on, false).to_string(),
        ..Default::default()
    };
    MetricStatus {
        page_rank: entry(config.compute_page_rank),
        betweenness: entry(config.compute_betweenness),
        eigenvector: entry(config.compute_eigenvector),
        hits: entry(config.compute_hits),
        critical: entry(config.compute_critical_path),
        cycles: entry(config.compute_cycles),
        kcore: entry(kcore),
        articulation: entry(articulation),
        slack: entry(config.compute_slack),
    }
}

/// Go `betweennessReason` (pkg/analysis/graph.go:256-264).
fn betweenness_reason(config: &AnalysisConfigReport) -> &'static str {
    if !config.betweenness_skip_reason.is_empty() {
        return config.betweenness_skip_reason;
    }
    if config.betweenness_mode == "approximate" {
        return "approximate";
    }
    ""
}

/// Convert a Go nanosecond timeout to a `Duration`, clamping the pathological
/// negative values a hand-edited config could carry the way Go's negative
/// `time.NewTimer` duration would fire immediately.
fn timeout_from_ns(ns: i64) -> Duration {
    Duration::from_nanos(ns.max(0) as u64)
}

/// Synchronous analysis that records per-phase timing — port of Go
/// `Analyzer.AnalyzeWithProfile` (pkg/analysis/graph.go:1877-1932).
///
/// Unlike [`analyze_phase2_blocking`], which derives every decision from the
/// node-count tier through an [`AnalysisBudget`], this entry point is driven by
/// an explicit [`AnalysisConfigReport`], exactly as Go's profiled pass is driven
/// by the `AnalysisConfig` the caller handed it. That is what lets
/// `--force-full-analysis` reach the profile (Go `FullAnalysisConfig`), whose
/// 30s budgets and 10000-cycle cap no size tier ever produces.
///
/// `build_graph` is left at zero: Go's `runProfileStartup` overwrites it with
/// the time `NewAnalyzer` spent before it calls this (cmd/bv/main.go:4978-5000),
/// so the caller owns that one field.
pub fn analyze_with_profile(
    g: std::sync::Arc<DiGraph>,
    config: &AnalysisConfigReport,
) -> (GraphAnalysis, StartupProfile) {
    let total_start = Instant::now();
    let node_count = g.len();
    let edge_count = g.edge_count();
    let mut profile = StartupProfile {
        node_count,
        edge_count,
        config: *config,
        ..Default::default()
    };
    let mut out = GraphAnalysis::default();

    // Go graph.go:1906-1912 — an empty graph still returns a ready stats value,
    // with every metric reported from the configuration alone.
    if node_count == 0 {
        out.status = empty_graph_metric_status(config);
        profile.total = total_start.elapsed();
        return (out, profile);
    }

    let phase1_start = Instant::now();
    let (phase1, degree, topo_sort) = phase1_timed(&g);
    profile.degree = degree;
    profile.topo_sort = topo_sort;
    profile.phase1 = phase1_start.elapsed();
    profile.density = phase1.density;
    out.phase1 = phase1.clone();

    let phase2_start = Instant::now();
    analyze_phase2_with_profile(&g, config, &phase1, &mut out, &mut profile);
    profile.phase2 = phase2_start.elapsed();
    profile.total = total_start.elapsed();

    (out, profile)
}

/// Go `computePhase2WithProfile` (pkg/analysis/graph.go:1967-2361).
fn analyze_phase2_with_profile(
    g: &std::sync::Arc<DiGraph>,
    config: &AnalysisConfigReport,
    phase1: &Phase1Stats,
    out: &mut GraphAnalysis,
    profile: &mut StartupProfile,
) {
    let n = g.len();
    // Go accumulates every metric into locals and publishes `stats.status` only
    // at the end (graph.go:2320-2360), so the intermediate writes are not made
    // here either.
    // Go requires a complete topological order before the two order-dependent
    // metrics run (graph.go:2013, :2110): a cyclic graph leaves the order nil.
    let topo_order = phase1
        .topological_order
        .as_ref()
        .filter(|order| order.len() == n);

    // --- PageRank (graph.go:1993-2044) ---
    if config.compute_page_rank {
        let t0 = Instant::now();
        let gc = std::sync::Arc::clone(g);
        match run_with_timeout(timeout_from_ns(config.page_rank_timeout_ns), move || {
            pagerank_default(&gc)
        }) {
            Ok(pr) => {
                out.page_rank = Some(idx_to_score_map(g, pr));
            }
            Err(()) => {
                profile.pagerank_timeout = true;
                // Go's uniform fallback keeps downstream normalization fed with
                // a full map instead of an empty one (graph.go:2005-2009).
                let uniform = 1.0 / n as f64;
                let fallback = (0..n)
                    .map(|i| (g.node_id(i).unwrap_or_default().to_string(), uniform))
                    .collect();
                out.page_rank = Some(fallback);
            }
        }
        profile.pagerank = t0.elapsed();
    }

    // --- Betweenness (graph.go:2046-2106) ---
    let mut betweenness_sample_used = 0usize;
    if config.compute_betweenness {
        let t0 = Instant::now();
        let sample = config.betweenness_sample_size;
        if config.betweenness_mode == "approximate" && sample > 0 {
            betweenness_sample_used = sample;
            let gc = std::sync::Arc::clone(g);
            match run_with_timeout(timeout_from_ns(config.betweenness_timeout_ns), move || {
                betweenness_approx(&gc, sample, Some(1))
            }) {
                Ok(bw) => {
                    out.betweenness = Some(idx_to_score_map(g, bw));
                }
                Err(()) => {
                    profile.betweenness_timeout = true;
                }
            }
        } else {
            let gc = std::sync::Arc::clone(g);
            match run_with_timeout(timeout_from_ns(config.betweenness_timeout_ns), move || {
                betweenness(&gc)
            }) {
                Ok(bw) => {
                    out.betweenness = Some(idx_to_score_map(g, bw));
                }
                Err(()) => {
                    profile.betweenness_timeout = true;
                }
            }
        }
        profile.betweenness = t0.elapsed();
    }

    // --- Eigenvector (graph.go:2108-2116) ---
    // Go calls this one inline with no timeout race, and `StartupProfile` has no
    // eigenvector-timeout field to report into, so the direct call is what
    // reproduces the document.
    if config.compute_eigenvector {
        let t0 = Instant::now();
        let ev = crate::algorithms::eigenvector::eigenvector_default(g);
        out.eigenvector = Some(idx_to_score_map(g, ev));
        profile.eigenvector = t0.elapsed();
    }

    // --- HITS (graph.go:2118-2158) ---
    if config.compute_hits && g.edge_count() > 0 {
        let t0 = Instant::now();
        let gc = std::sync::Arc::clone(g);
        match run_with_timeout(timeout_from_ns(config.hits_timeout_ns), move || {
            hits_default(&gc)
        }) {
            Ok(h) => {
                out.hubs = Some(idx_to_score_map(g, h.hubs));
                out.authorities = Some(idx_to_score_map(g, h.authorities));
            }
            Err(()) => {
                profile.hits_timeout = true;
            }
        }
        profile.hits = t0.elapsed();
    }

    // --- Critical path (graph.go:2160-2180) ---
    let mut critical_unavailable = "";
    if config.compute_critical_path {
        let t0 = Instant::now();
        if topo_order.is_some() {
            let heights = critical_path_heights(g);
            out.critical_path_score = Some(idx_to_score_map(g, heights));
        } else {
            critical_unavailable =
                "dependency graph contains a cycle; topological order unavailable";
        }
        profile.critical_path = t0.elapsed();
    }

    // --- Cycles (graph.go:2182-2253) ---
    // `truncation_note` carries Go's "truncated to N of M cycle
    // representatives" suffix, which is only built when the cap actually bit.
    let mut truncation_note = String::new();
    if config.compute_cycles {
        let t0 = Instant::now();
        let scc = tarjan_scc(g);
        if scc.has_cycles {
            let cap = if config.max_cycles_to_store == 0 {
                100
            } else {
                config.max_cycles_to_store
            };
            // Go stores one representative per cyclic component (graph_cycles.go:27-41)
            // and keeps the pre-limit count so it can report truncation without
            // implying every simple cycle in an SCC was enumerated.
            let found = crate::algorithms::cycles::enumerate_cycles_with_info(g, cap);
            profile.cycle_count = scc.cycle_count;
            if found.truncated {
                truncation_note = format!(
                    "truncated to {} of {} cycle representatives",
                    found.cycles.len(),
                    profile.cycle_count
                );
            }
            out.cycles = Some(
                found
                    .cycles
                    .into_iter()
                    .map(|c| {
                        c.into_iter()
                            .map(|i| g.node_id(i).unwrap_or_default().to_string())
                            .collect()
                    })
                    .collect(),
            );
        } else {
            // An acyclic graph computed its cycles metric successfully and
            // found none — the same `Some(empty)` the unprofiled pass reports.
            out.cycles = Some(Vec::new());
        }
        profile.cycles = t0.elapsed();
    }

    // --- k-core + articulation (graph.go:2262-2268) ---
    // Go computes the pair inside one timed window and reports the whole
    // duration on KCore, leaving Articulation at zero. The Rust port keeps the
    // two algorithms separate but times them as one window so the emitted
    // numbers mean the same thing.
    let mut kcore_ran = false;
    if config.compute_kcore || config.compute_articulation {
        let t0 = Instant::now();
        if config.compute_kcore {
            let gc = std::sync::Arc::clone(g);
            let cores = kcore(&gc);
            out.core_number = Some(
                cores
                    .into_iter()
                    .enumerate()
                    .map(|(i, v)| (g.node_id(i).unwrap_or_default().to_string(), v))
                    .collect(),
            );
        }
        if config.compute_articulation {
            let gc = std::sync::Arc::clone(g);
            out.articulation = Some(
                crate::algorithms::articulation::articulation_points(&gc)
                    .into_iter()
                    .map(|i| g.node_id(i).unwrap_or_default().to_string())
                    .collect(),
            );
        }
        profile.kcore = t0.elapsed();
        kcore_ran = true;
    }

    // --- Slack (graph.go:2270-2278) ---
    let mut slack_unavailable = "";
    if config.compute_slack {
        let t0 = Instant::now();
        if topo_order.is_some() {
            out.slack = Some(idx_to_score_map(g, crate::algorithms::slack::slack(g)));
        } else {
            slack_unavailable = "dependency graph contains a cycle; topological order unavailable";
        }
        profile.slack = t0.elapsed();
    }

    // Go graph.go:2320-2360 — the status snapshot is built from the config
    // switches and the recorded timeouts, not from what each branch happened to
    // store. A few entries carry a reason the branch above discovered.
    let articulation_ran = config.compute_articulation || config.compute_kcore;
    let mut cycles_reason = config.cycles_skip_reason.to_string();
    if !truncation_note.is_empty() {
        if !cycles_reason.is_empty() {
            cycles_reason.push_str("; ");
        }
        cycles_reason.push_str(&truncation_note);
    }
    let entry = |on: bool, timed_out: bool, elapsed: Duration| StatusEntry {
        state: state_from_timing(on, timed_out).to_string(),
        ms: ms_total(elapsed),
        ..Default::default()
    };
    let mut critical = entry(config.compute_critical_path, false, profile.critical_path);
    if config.compute_critical_path && !critical_unavailable.is_empty() {
        critical.state = "skipped".into();
        critical.reason = critical_unavailable.to_string();
    }
    let mut slack = entry(config.compute_slack, false, profile.slack);
    if config.compute_slack && !slack_unavailable.is_empty() {
        slack.state = "skipped".into();
        slack.reason = slack_unavailable.to_string();
    }
    out.status = MetricStatus {
        page_rank: entry(
            config.compute_page_rank,
            profile.pagerank_timeout,
            profile.pagerank,
        ),
        betweenness: StatusEntry {
            state: state_from_timing(config.compute_betweenness, profile.betweenness_timeout)
                .to_string(),
            reason: betweenness_reason(config).to_string(),
            // Go reports `actualBetweennessSample`, which only becomes non-zero
            // when the approximate branch actually ran.
            sample: betweenness_sample_used,
            ms: ms_total(profile.betweenness),
        },
        eigenvector: entry(config.compute_eigenvector, false, profile.eigenvector),
        hits: StatusEntry {
            state: state_from_timing(config.compute_hits, profile.hits_timeout).to_string(),
            reason: config.hits_skip_reason.to_string(),
            sample: 0,
            ms: ms_total(profile.hits),
        },
        critical,
        cycles: StatusEntry {
            state: state_from_timing(config.compute_cycles, profile.cycles_timeout).to_string(),
            reason: cycles_reason,
            sample: 0,
            ms: ms_total(profile.cycles),
        },
        kcore: entry(kcore_ran, false, profile.kcore),
        articulation: entry(articulation_ran, false, profile.articulation),
        slack,
    };
    out.status.stabilize_for_pinned_clock();
}

fn ms_total(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(n: usize) -> DiGraph {
        let mut g = DiGraph::with_capacity(n, n.saturating_sub(1));
        for i in 1..=n {
            g.add_node(&format!("FIX-{i}"));
        }
        // Node indices are 0-based; FIX-k has index k-1.
        // Edge index j -> j+1 = dependency chain FIX-(j+1) depends on FIX-(j+...
        // matching wasm convention: u depends on v => edge u -> v.
        for j in 0..n - 1 {
            g.add_edge(j, j + 1);
        }
        g
    }

    fn issue_with_blocking_deps(id: &str, depends_on: &[&str]) -> bv_core::model::Issue {
        use bv_core::model::{Dependency, DependencyType, Issue, Status};
        Issue {
            id: id.to_string(),
            content_hash: String::new(),
            title: id.to_string(),
            description: String::new(),
            design: String::new(),
            acceptance_criteria: String::new(),
            notes: String::new(),
            status: Status::Open,
            priority: 2,
            issue_type: "task".into(),
            assignee: String::new(),
            estimated_minutes: None,
            created_at: None,
            updated_at: None,
            due_date: None,
            defer_until: None,
            closed_at: None,
            external_ref: None,
            compaction_level: 0,
            compacted_at: None,
            compacted_at_commit: None,
            original_size: 0,
            labels: vec![],
            dependencies: depends_on
                .iter()
                .map(|target| Dependency {
                    issue_id: id.to_string(),
                    depends_on_id: (*target).to_string(),
                    depends_on_legacy: String::new(),
                    target_id_legacy: String::new(),
                    r#type: DependencyType::Blocks,
                    created_at: None,
                    created_by: String::new(),
                })
                .collect(),
            comments: vec![],
            source_repo: String::new(),
        }
    }

    #[test]
    fn phase1_degrees_and_density() {
        let g = chain(12);
        let p1 = analyze_phase1(&g);
        assert_eq!(p1.node_count, 12);
        assert_eq!(p1.edge_count, 11);
        assert_eq!(p1.out_degree["FIX-1"], 1); // FIX-1 -> FIX-2
        assert_eq!(p1.in_degree["FIX-1"], 0); // first node unblocked
        assert_eq!(p1.in_degree["FIX-12"], 1); // edge FIX-11 -> FIX-12
        let order = p1.topological_order.as_deref().unwrap();
        assert_eq!(order.len(), 12);
        assert_eq!(order[0], "FIX-12");
    }

    #[test]
    fn phase1_topological_order_matches_gonum() {
        // Diamond over sorted ids: a->b, a->c, b->d, c->d. gonum's topo.Sort
        // returns [a, c, b, d] and Go walks that slice backwards
        // (graph.go:1952-1954), so the reported order is d, b, c, a. Feeding
        // the Kahn order [a, b, c, d] through the same backwards walk would
        // report d, c, b, a instead — the tie is what differs.
        let issues = vec![
            issue_with_blocking_deps("a", &["b", "c"]),
            issue_with_blocking_deps("b", &["d"]),
            issue_with_blocking_deps("c", &["d"]),
            issue_with_blocking_deps("d", &[]),
        ];
        let g = build_graph(&issues);
        let p1 = analyze_phase1(&g);
        assert_eq!(
            p1.topological_order,
            Some(["d", "b", "c", "a"].map(String::from).to_vec())
        );
    }

    #[test]
    fn phase1_topological_order_empty_when_cyclic() {
        // gonum returns Unorderable for a cyclic graph and Go leaves
        // TopologicalOrder nil, i.e. JSON null (graph.go:1951-1954).
        let issues = vec![
            issue_with_blocking_deps("a", &["b"]),
            issue_with_blocking_deps("b", &["a"]),
        ];
        let g = build_graph(&issues);
        let p1 = analyze_phase1(&g);
        assert_eq!(p1.topological_order, None);
    }

    #[test]
    fn phase2_computes_all_metrics_small_graph() {
        let g = chain(12);
        let (status, out) =
            analyze_phase2_blocking(std::sync::Arc::new(g), &AnalysisBudget::default());
        assert_eq!(status.page_rank.state, "computed", "{:?}", status.page_rank);
        assert_eq!(status.betweenness.state, "computed");
        assert_eq!(status.kcore.state, "computed");
        let pr = out.page_rank.expect("pagerank present");
        assert_eq!(pr.len(), 12);
        let sum: f64 = pr.values().sum();
        assert!((sum - 1.0).abs() < 1e-6);
        // acyclic graph -> zero cycles but computed
        assert_eq!(out.cycles, Some(Vec::new()));
    }

    #[test]
    fn skip_phase2_keeps_kcore_family_enabled() {
        let g = chain(12);
        let budget = AnalysisBudget {
            skip_phase2: true,
            ..Default::default()
        };
        let (status, out) = analyze_phase2_blocking(std::sync::Arc::new(g), &budget);
        assert_eq!(status.page_rank.state, "skipped");
        assert_eq!(status.hits.state, "skipped");
        assert_eq!(status.kcore.state, "computed", "k-core stays enabled");
        assert_eq!(status.slack.state, "computed");
        assert!(out.core_number.is_some());
        assert!(out.page_rank.is_none());
    }

    /// Go gates both order-dependent metrics on a complete topological order
    /// (pkg/analysis/graph.go:2163 and :2271) and reports Go's exact reason
    /// string when it is missing.
    #[test]
    fn cyclic_graph_skips_critical_path_and_slack_with_go_reason() {
        let issues = vec![
            issue_with_blocking_deps("a", &["b"]),
            issue_with_blocking_deps("b", &["c"]),
            issue_with_blocking_deps("c", &["a"]),
        ];
        let g = build_graph(&issues);
        let (status, out) =
            analyze_phase2_blocking(std::sync::Arc::new(g), &AnalysisBudget::default());
        assert_eq!(status.critical.state, "skipped");
        assert_eq!(status.critical.reason, CYCLE_UNAVAILABLE_REASON);
        assert_eq!(status.slack.state, "skipped");
        assert_eq!(status.slack.reason, CYCLE_UNAVAILABLE_REASON);
        // Go leaves both maps nil when the metric did not run.
        assert_eq!(out.critical_path_score, None);
        assert_eq!(out.slack, None);
    }

    /// A self-edge is a back edge for gonum's `topo.Sort`, so Go reports the
    /// graph as unorderable even though its only SCC is a singleton — which is
    /// the case Tarjan-based `topological_sort_gonum` cannot see.
    #[test]
    fn self_loop_is_not_an_available_order() {
        let mut g = DiGraph::new();
        g.add_node("a");
        g.add_node("b");
        let a = g.node_idx("a").unwrap();
        let b = g.node_idx("b").unwrap();
        g.add_edge(a, b);
        g.add_edge(a, a);
        assert!(
            topological_sort_gonum(&g).is_some(),
            "Tarjan sees no multi-node SCC; only the self-edge scan catches it"
        );
        assert!(!topological_order_available(&g));

        let (status, out) =
            analyze_phase2_blocking(std::sync::Arc::new(g), &AnalysisBudget::default());
        assert_eq!(status.slack.state, "skipped");
        assert_eq!(status.slack.reason, CYCLE_UNAVAILABLE_REASON);
        assert_eq!(out.slack, None);
    }

    #[test]
    fn acyclic_graph_still_computes_critical_path_and_slack() {
        let g = chain(12);
        assert!(topological_order_available(&g));
        let (status, out) =
            analyze_phase2_blocking(std::sync::Arc::new(g), &AnalysisBudget::default());
        assert_eq!(status.critical.state, "computed");
        assert_eq!(status.critical.reason, "");
        assert_eq!(status.slack.state, "computed");
        assert_eq!(status.slack.reason, "");
        assert_eq!(out.critical_path_score.expect("heights present").len(), 12);
        assert_eq!(out.slack.expect("slack present").len(), 12);
    }

    /// Go `stabilizeRobotMetricStatusForPinnedClock` zeroes all nine elapsed
    /// values when SOURCE_DATE_EPOCH parses, and `statusEntry.MarshalJSON`
    /// drops `ms` for a zero elapsed, so the pinned document carries no `ms`
    /// key on any metric (pkg/analysis/graph.go:130-147).
    ///
    /// The env var is process-global, so these run in one test to keep the
    /// mutation from racing another test in the same binary.
    #[test]
    fn pinned_clock_drops_ms_and_unpinned_keeps_it() {
        fn entry(ms: f64) -> StatusEntry {
            StatusEntry {
                state: "computed".into(),
                ms,
                ..Default::default()
            }
        }
        fn status_with_ms() -> MetricStatus {
            MetricStatus {
                page_rank: entry(1.5),
                betweenness: entry(1.5),
                eigenvector: entry(1.5),
                hits: entry(1.5),
                critical: entry(1.5),
                cycles: entry(1.5),
                kcore: entry(1.5),
                articulation: entry(1.5),
                slack: entry(1.5),
            }
        }
        fn all_ms_are_zero(s: &MetricStatus) -> bool {
            [
                &s.page_rank,
                &s.betweenness,
                &s.eigenvector,
                &s.hits,
                &s.critical,
                &s.cycles,
                &s.kcore,
                &s.articulation,
                &s.slack,
            ]
            .iter()
            .all(|e| e.ms == 0.0)
        }

        // Unset: Go's ParseInt never runs on an empty value, so the status
        // keeps its measured durations.
        std::env::remove_var("SOURCE_DATE_EPOCH");
        let mut unpinned = status_with_ms();
        unpinned.stabilize_for_pinned_clock();
        assert!(!all_ms_are_zero(&unpinned), "ms must survive unpinned");

        // A value that is not a base-10 integer is not a pinned clock.
        std::env::set_var("SOURCE_DATE_EPOCH", "not-a-number");
        let mut garbage = status_with_ms();
        garbage.stabilize_for_pinned_clock();
        assert!(
            !all_ms_are_zero(&garbage),
            "ms must survive an unparsable epoch"
        );

        // Go trims before parsing, so surrounding whitespace still pins.
        std::env::set_var("SOURCE_DATE_EPOCH", "  1787407612  ");
        assert!(source_date_epoch_active(), "TrimSpace then ParseInt");
        let mut padded = status_with_ms();
        padded.stabilize_for_pinned_clock();
        assert!(all_ms_are_zero(&padded));

        std::env::set_var("SOURCE_DATE_EPOCH", "1787407612");
        let (status, _) =
            analyze_phase2_blocking(std::sync::Arc::new(chain(8)), &AnalysisBudget::default());
        let json = status.to_json_map();
        for key in [
            "PageRank",
            "Betweenness",
            "Eigenvector",
            "HITS",
            "Critical",
            "Cycles",
            "KCore",
            "Articulation",
            "Slack",
        ] {
            assert!(
                json[key].get("ms").is_none(),
                "{key} still carries ms under a pinned clock: {json}"
            );
        }
        std::env::remove_var("SOURCE_DATE_EPOCH");
    }

    #[test]
    fn timeout_budget_tiers_match_go_configforsize() {
        let b = AnalysisBudget::default();
        assert_eq!(b.timeout_for(50), Duration::from_secs(2));
        assert_eq!(b.timeout_for(300), Duration::from_millis(500));
        assert_eq!(b.timeout_for(900), Duration::from_millis(300));
        assert_eq!(b.timeout_for(5000), Duration::from_millis(200));
        assert_eq!(b.max_cycles(50), 1000);
        assert_eq!(b.max_cycles(300), 100);
        assert_eq!(b.max_cycles(900), 50);
        assert_eq!(b.max_cycles(5000), 10);
    }

    #[test]
    fn recommend_sample_size_tiers() {
        assert_eq!(recommend_sample_size(50), 50);
        assert_eq!(recommend_sample_size(300), 60);
        assert_eq!(recommend_sample_size(150), 50); // max(50, 30)
        assert_eq!(recommend_sample_size(1000), 100);
        assert_eq!(recommend_sample_size(5000), 200);
    }

    #[test]
    fn status_entry_json_matches_go_shape() {
        let e = StatusEntry {
            state: "computed".into(),
            ms: 5.5,
            ..Default::default()
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["state"], "computed");
        assert_eq!(v["ms"], 5.5);
        assert!(v.get("reason").is_none()); // omitempty
        let skipped = StatusEntry::skipped("too dense");
        let v = serde_json::to_value(&skipped).unwrap();
        assert_eq!(v["reason"], "too dense");
        assert!(v.get("ms").is_none());
    }

    #[test]
    fn metric_status_json_uses_go_keys_in_order() {
        let ms = MetricStatus::default();
        let v = ms.to_json_map();
        let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            vec![
                "PageRank",
                "Betweenness",
                "Eigenvector",
                "HITS",
                "Critical",
                "Cycles",
                "KCore",
                "Articulation",
                "Slack"
            ]
        );
    }
}

#[cfg(test)]
mod config_for_size_tests {
    use super::*;
    use serde_json::Value;

    /// The three tiers that appear in the frozen corpus, pinned to the exact
    /// `analysis_config` objects Go emitted for them. Read from
    /// `golden/{medium_tree,large_cyclic_600,xl_2500}____robot_insights.json`.
    fn golden(name: &str) -> Value {
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .expect("repo root");
        let txt = std::fs::read_to_string(
            repo.join("golden")
                .join(format!("{name}____robot_insights.json")),
        )
        .expect("golden readable");
        serde_json::from_str::<Value>(&txt).expect("golden parses")["analysis_config"].clone()
    }

    fn assert_matches_golden(node_count: usize, edge_count: usize, density: f64, name: &str) {
        let got = serde_json::to_value(config_for_size(node_count, edge_count, density))
            .expect("serializes");
        let want = golden(name);
        for (k, wv) in want.as_object().expect("golden object") {
            let gv = got
                .get(k)
                .unwrap_or_else(|| panic!("{name}: missing field {k}"));
            assert_eq!(gv, wv, "{name}: field {k}");
        }
        // No field beyond Go's set may appear — extra fields are a parity break.
        for k in got.as_object().expect("serialized object").keys() {
            assert!(
                want.get(k).is_some(),
                "{name}: Rust emitted extra field {k} that Go does not"
            );
        }
    }

    #[test]
    fn medium_tier_matches_go() {
        // medium_tree: 121 issues -> 100..499 tier.
        assert_matches_golden(121, 120, 0.0, "medium_tree");
    }

    #[test]
    fn large_tier_matches_go() {
        // large_cyclic_600: 600 issues, sparse -> approximate, sample 100.
        assert_matches_golden(600, 900, 0.0025, "large_cyclic_600");
    }

    #[test]
    fn xl_tier_matches_go() {
        // xl_2500: 2500 issues -> approximate sample 200, cycles skipped.
        assert_matches_golden(2500, 5000, 0.0008, "xl_2500");
    }

    #[test]
    fn small_tier_is_exact_with_two_second_budgets() {
        let c = config_for_size(12, 11, 0.0);
        assert_eq!(c.betweenness_mode, "exact");
        assert_eq!(c.betweenness_timeout_ns, 2_000_000_000);
        assert_eq!(c.max_cycles_to_store, 1000);
        assert!(c.compute_cycles && c.compute_hits);
    }

    #[test]
    fn large_dense_graph_skips_betweenness() {
        let c = config_for_size(800, 6000, 0.02);
        assert!(!c.compute_betweenness);
        assert_eq!(c.betweenness_mode, "skip");
        assert_eq!(c.betweenness_timeout_ns, 0);
        assert_eq!(
            c.betweenness_skip_reason,
            "graph too dense (density > 0.01)"
        );
    }

    #[test]
    fn xl_dense_graph_skips_hits_but_keeps_betweenness() {
        let c = config_for_size(3000, 9000, 0.01);
        assert!(c.compute_betweenness);
        assert_eq!(c.betweenness_mode, "approximate");
        assert!(!c.compute_hits);
        assert_eq!(c.hits_skip_reason, "graph too large and dense");
        assert_eq!(c.hits_timeout_ns, 0);
    }

    #[test]
    fn recommend_sample_size_matches_go_tiers() {
        let b = AnalysisBudget::default();
        assert_eq!(b.recommend_sample_size(0, 0), 0);
        assert_eq!(b.recommend_sample_size(12, 11), 12);
        assert_eq!(b.recommend_sample_size(121, 120), 50);
        assert_eq!(b.recommend_sample_size(600, 900), 100);
        assert_eq!(b.recommend_sample_size(2500, 5000), 200);
        // 1000/5 = 200 beats the floor of 50.
        assert_eq!(b.recommend_sample_size(400, 300), 80);
    }

    #[test]
    fn tier_boundaries_are_exact() {
        assert_eq!(config_for_size(99, 0, 0.0).max_cycles_to_store, 1000);
        assert_eq!(config_for_size(100, 0, 0.0).max_cycles_to_store, 100);
        assert_eq!(config_for_size(499, 0, 0.0).max_cycles_to_store, 100);
        assert_eq!(config_for_size(500, 0, 0.0).max_cycles_to_store, 50);
        assert_eq!(config_for_size(1999, 0, 0.0).max_cycles_to_store, 50);
        assert_eq!(config_for_size(2000, 0, 0.0).max_cycles_to_store, 10);
        // Cycles flip off exactly at the XL boundary.
        assert!(config_for_size(1999, 0, 0.0).compute_cycles);
        assert!(!config_for_size(2000, 0, 0.0).compute_cycles);
    }
}

#[cfg(test)]
mod full_analysis_config_tests {
    use super::*;
    use serde_json::Value;
    use std::time::Duration;

    const THIRTY_SEC_NS: i64 = 30_000_000_000;

    /// Go config.go:232-252, field by field. Every value below is transcribed
    /// from that literal; the serialized key set is checked separately so a
    /// field Go does not emit can never sneak in.
    fn expected_go_json() -> Value {
        serde_json::json!({
            "ComputeBetweenness": true,
            "BetweennessTimeout": THIRTY_SEC_NS,
            "BetweennessSkipReason": "",
            "BetweennessMode": "exact",
            "BetweennessSampleSize": 0,
            "BetweennessIsApproximate": false,
            "ComputePageRank": true,
            "PageRankTimeout": THIRTY_SEC_NS,
            "PageRankSkipReason": "",
            "ComputeHITS": true,
            "HITSTimeout": THIRTY_SEC_NS,
            "HITSSkipReason": "",
            "ComputeCycles": true,
            "CyclesTimeout": THIRTY_SEC_NS,
            "MaxCyclesToStore": 10000,
            "CyclesSkipReason": "",
            "ComputeEigenvector": true,
            "ComputeCriticalPath": true,
            "ComputeKCore": true,
            "ComputeArticulation": true,
            "ComputeSlack": true,
        })
    }

    #[test]
    fn serialized_config_equals_go_literal() {
        let got = serde_json::to_value(full_analysis_config()).expect("serializes");
        let want = expected_go_json();
        let got_obj = got.as_object().expect("object");
        let want_obj = want.as_object().expect("object");
        assert_eq!(got_obj.len(), want_obj.len(), "field count");
        for (k, wv) in want_obj {
            assert_eq!(got_obj.get(k), Some(wv), "field {k}");
        }
        for k in got_obj.keys() {
            assert!(want_obj.contains_key(k), "extra field {k} Go does not emit");
        }
    }

    #[test]
    fn field_order_matches_go_struct_order() {
        // Go serializes AnalysisConfig positionally (pkg/analysis/config.go:12).
        let got: Vec<String> = serde_json::to_value(full_analysis_config())
            .expect("serializes")
            .as_object()
            .expect("object")
            .keys()
            .cloned()
            .collect();
        assert_eq!(
            got,
            expected_go_json()
                .as_object()
                .expect("object")
                .keys()
                .cloned()
                .collect::<Vec<String>>()
        );
    }

    #[test]
    fn every_metric_is_enabled_and_no_skip_reason_is_set() {
        let c = full_analysis_config();
        assert!(c.compute_betweenness);
        assert!(c.compute_page_rank);
        assert!(c.compute_hits);
        assert!(c.compute_cycles);
        assert!(c.compute_eigenvector);
        assert!(c.compute_critical_path);
        assert!(c.compute_kcore);
        assert!(c.compute_articulation);
        assert!(c.compute_slack);
        assert_eq!(c.betweenness_skip_reason, "");
        assert_eq!(c.page_rank_skip_reason, "");
        assert_eq!(c.hits_skip_reason, "");
        assert_eq!(c.cycles_skip_reason, "");
    }

    #[test]
    fn betweenness_is_forced_exact_with_zero_sample() {
        let c = full_analysis_config();
        assert_eq!(c.betweenness_mode, "exact");
        // Go leaves BetweennessSampleSize / BetweennessIsApproximate unset.
        assert_eq!(c.betweenness_sample_size, 0);
        assert!(!c.betweenness_is_approximate);
    }

    #[test]
    fn all_four_timeouts_are_thirty_seconds_and_cycles_cap_is_10000() {
        let c = full_analysis_config();
        assert_eq!(c.betweenness_timeout_ns, THIRTY_SEC_NS);
        assert_eq!(c.page_rank_timeout_ns, THIRTY_SEC_NS);
        assert_eq!(c.hits_timeout_ns, THIRTY_SEC_NS);
        assert_eq!(c.cycles_timeout_ns, THIRTY_SEC_NS);
        assert_eq!(c.max_cycles_to_store, 10000);
    }

    /// The whole point of the flag: a graph that `ConfigForSize` would throttle
    /// or disable gets the identical full config at every size. Sizes and
    /// densities below straddle every tier and both density cutoffs
    /// (config.go:181, :217) that the tiered config reacts to.
    #[test]
    fn force_full_config_ignores_size_and_density() {
        for nodes in [0usize, 1, 12, 99, 121, 499, 600, 1999, 2500, 100_000] {
            for density in [0.0f64, 0.0008, 0.0025, 0.0099, 0.01, 0.02, 0.9] {
                let b = full_budget(density);
                assert!(
                    b.timeout_for(nodes) == Duration::from_secs(30)
                        && !b.skip_cycles(nodes)
                        && !b.skip_hits(nodes)
                        && b.betweenness_mode(nodes) == (false, false)
                        && b.max_cycles(nodes) == 10000,
                    "tier {nodes}/{density} escaped the forced full config"
                );
            }
        }
    }

    // --- AnalysisBudget::force_full: the computation-side half of
    // `analyzer.SetConfig(FullAnalysisConfig())`. ---

    fn full_budget(density: f64) -> AnalysisBudget {
        AnalysisBudget {
            density,
            force_full: true,
            ..AnalysisBudget::default()
        }
    }

    #[test]
    fn force_full_budget_reports_thirty_second_timeouts() {
        for nodes in [0usize, 12, 121, 600, 2500, 100_000] {
            assert_eq!(
                full_budget(0.0).timeout_for(nodes),
                Duration::from_secs(30),
                "{nodes} nodes"
            );
        }
    }

    #[test]
    fn force_full_budget_matches_the_config_it_advertises() {
        // The ns the config advertises and the Duration the analyzer enforces
        // are the same budget expressed twice; they must not drift. One
        // `timeout_for` feeds all four timeout-raced metrics, so checking it
        // against the shared 30_000_000_000 covers Betweenness/PageRank/HITS/
        // Cycles together.
        let c = full_analysis_config();
        let b = full_budget(0.5);
        let thirty = Duration::from_secs(30);
        assert_eq!(b.betweenness_mode(2500), (false, false));
        assert_eq!(b.timeout_for(2500), thirty);
        assert_eq!(c.betweenness_timeout_ns, 30_000_000_000);
        assert_eq!(
            b.timeout_for(2500).as_nanos() as i64,
            c.betweenness_timeout_ns
        );
        assert_eq!(b.max_cycles(2500), c.max_cycles_to_store);
    }

    #[test]
    fn force_full_budget_never_skips_cycles_or_hits() {
        for nodes in [2500usize, 2501, 100_000] {
            for density in [0.0f64, 0.0009, 0.001, 0.01, 0.5] {
                let b = full_budget(density);
                assert!(!b.skip_cycles(nodes), "cycles skipped at {nodes}/{density}");
                assert!(!b.skip_hits(nodes), "HITS skipped at {nodes}/{density}");
            }
        }
    }

    #[test]
    fn force_full_budget_uses_exact_betweenness_everywhere() {
        for nodes in [0usize, 12, 600, 2500, 100_000] {
            for density in [0.0f64, 0.005, 0.02, 0.5] {
                assert_eq!(
                    full_budget(density).betweenness_mode(nodes),
                    (false, false),
                    "betweenness not exact at {nodes}/{density}"
                );
            }
        }
    }

    #[test]
    fn force_full_budget_cycles_cap_is_10000_at_every_size() {
        for nodes in [0usize, 12, 121, 600, 2500, 100_000] {
            assert_eq!(full_budget(0.0).max_cycles(nodes), 10000);
        }
    }

    /// Default (flag absent) must be byte-identical to before this change —
    /// every golden was captured without `--force-full-analysis`.
    #[test]
    fn default_budget_is_unaffected() {
        let b = AnalysisBudget::default();
        assert!(!b.force_full);
        assert_eq!(b.timeout_for(12), Duration::from_secs(2));
        assert_eq!(b.timeout_for(121), Duration::from_millis(500));
        assert_eq!(b.timeout_for(600), Duration::from_millis(300));
        assert_eq!(b.timeout_for(2500), Duration::from_millis(200));
        assert_eq!(b.max_cycles(12), 1000);
        assert_eq!(b.max_cycles(121), 100);
        assert_eq!(b.max_cycles(600), 50);
        assert_eq!(b.max_cycles(2500), 10);
        assert!(!b.skip_cycles(1999));
        assert!(b.skip_cycles(2000));
        // Default density 0.0 — a sparse XL graph still runs HITS.
        assert!(!b.skip_hits(2500));
        let dense_xl = AnalysisBudget {
            density: 0.01,
            ..AnalysisBudget::default()
        };
        assert!(dense_xl.skip_hits(2500));
        assert_eq!(b.betweenness_mode(2500), (true, false));
        assert_eq!(b.betweenness_mode(600), (true, false));
        assert_eq!(b.betweenness_mode(12), (false, false));
        let dense_large = AnalysisBudget {
            density: 0.02,
            ..AnalysisBudget::default()
        };
        assert_eq!(dense_large.betweenness_mode(600), (false, true));
    }

    /// Go ApplyEnvOverrides (config.go:373-401) runs *after* the
    /// FullAnalysisConfig literal, so BV_PHASE2_TIMEOUT_S still wins over the
    /// 30s budgets, and BV_SKIP_PHASE2 still wins over everything.
    #[test]
    fn env_overrides_still_win_over_force_full() {
        let b = AnalysisBudget {
            override_secs: Some(7),
            force_full: true,
            ..AnalysisBudget::default()
        };
        assert_eq!(b.timeout_for(2500), Duration::from_secs(7));
    }
}

#[cfg(test)]
mod startup_profile_tests {
    use super::*;
    use serde_json::Value;

    /// A dependency chain FIX-1 -> FIX-2 -> ... -> FIX-n, i.e. FIX-k depends on
    /// FIX-(k+1) so the graph is acyclic and has a topological order.
    fn chain(n: usize) -> DiGraph {
        let mut g = DiGraph::with_capacity(n, n.saturating_sub(1));
        for i in 1..=n {
            g.add_node(&format!("FIX-{i}"));
        }
        for j in 0..n.saturating_sub(1) {
            g.add_edge(j, j + 1);
        }
        g
    }

    /// Two issues that block each other: no topological order exists, so Go
    /// leaves TopologicalOrder nil and the two order-dependent metrics report a
    /// skip reason.
    fn cycle() -> DiGraph {
        let mut g = DiGraph::with_capacity(2, 2);
        g.add_node("a");
        g.add_node("b");
        g.add_edge(0, 1);
        g.add_edge(1, 0);
        g
    }

    /// Every Phase-2 metric on with 30s budgets — the shape
    /// --force-full-analysis --profile-startup produces.
    fn all_on() -> AnalysisConfigReport {
        full_analysis_config()
    }

    #[test]
    fn startup_profile_json_key_order_matches_go_struct_order() {
        // Go graph.go:27-61 declares the fields in this order and
        // encoding/json preserves struct order.
        let raw = serde_json::to_string(&StartupProfile {
            config: all_on(),
            ..Default::default()
        })
        .expect("serializes");
        let expected = [
            "\"node_count\"",
            "\"edge_count\"",
            "\"density\"",
            "\"build_graph\"",
            "\"degree\"",
            "\"topo_sort\"",
            "\"phase1_total\"",
            "\"pagerank\"",
            "\"pagerank_timeout\"",
            "\"betweenness\"",
            "\"betweenness_timeout\"",
            "\"eigenvector\"",
            "\"hits\"",
            "\"hits_timeout\"",
            "\"critical_path\"",
            "\"cycles\"",
            "\"cycles_timeout\"",
            "\"cycle_count\"",
            "\"kcore\"",
            "\"articulation\"",
            "\"slack\"",
            "\"phase2_total\"",
            "\"config\"",
            "\"total\"",
        ];
        let mut at = 0usize;
        for key in expected {
            let found = raw[at..]
                .find(key)
                .unwrap_or_else(|| panic!("{key} missing or out of order in {raw}"));
            at += found + key.len();
        }
        // And nothing extra.
        let v: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v.as_object().unwrap().len(), expected.len());
    }

    #[test]
    fn startup_profile_durations_serialize_as_nanosecond_integers() {
        // Go marshals time.Duration as an int64 nanosecond count, so
        // --profile-startup --profile-json emits integers, never floats.
        let profile = StartupProfile {
            build_graph: Duration::from_millis(2),
            pagerank: Duration::from_nanos(1234),
            total: Duration::from_micros(1500),
            config: all_on(),
            ..Default::default()
        };
        let v = serde_json::to_value(&profile).expect("serializes");
        assert_eq!(v["build_graph"], Value::from(2_000_000i64));
        assert_eq!(v["pagerank"], Value::from(1234i64));
        assert_eq!(v["total"], Value::from(1_500_000i64));
        assert_eq!(v["pagerank_timeout"], Value::from(false));
        assert_eq!(v["cycle_count"], Value::from(0));
    }

    #[test]
    fn analyze_with_profile_populates_every_phase_timing() {
        let g = chain(12);
        let (out, profile) = analyze_with_profile(std::sync::Arc::new(g), &all_on());

        assert_eq!(profile.node_count, 12);
        assert_eq!(profile.edge_count, 11);
        assert!((profile.density - 11.0 / (12.0 * 11.0)).abs() < 1e-12);
        assert_eq!(profile.config, all_on());

        // Go's runProfileStartup owns BuildGraph (it times NewAnalyzer itself
        // and overwrites the field), so the analysis pass leaves it zero.
        assert_eq!(profile.build_graph, Duration::ZERO);

        // The phase aggregates and the two centralities do enough work on a
        // 12-node chain to clear any plausible clock granularity. The cheaper
        // per-metric timings are only checked for being recorded, because
        // `Instant` can report a zero delta for a sub-tick operation on
        // Windows and a test that demands `> 0` there is flaky, not strict.
        for (name, d) in [
            ("phase1", profile.phase1),
            ("phase2", profile.phase2),
            ("total", profile.total),
            ("pagerank", profile.pagerank),
            ("betweenness", profile.betweenness),
        ] {
            assert!(d > Duration::ZERO, "{name} was never timed");
        }
        // Total brackets both phases.
        assert!(profile.total >= profile.phase1);
        assert!(profile.total >= profile.phase2);

        // No metric timed out or was skipped on a 12-node chain.
        assert!(!profile.pagerank_timeout);
        assert!(!profile.betweenness_timeout);
        assert!(!profile.hits_timeout);
        assert!(!profile.cycles_timeout);
        assert_eq!(profile.cycle_count, 0);
        assert_eq!(out.status.page_rank.state, "computed");
        assert_eq!(out.status.critical.state, "computed");
        assert_eq!(out.status.slack.state, "computed");
        assert!(out.page_rank.is_some());
        assert!(out.betweenness.is_some());
        assert_eq!(out.cycles, Some(Vec::new()));
    }

    #[test]
    fn analyze_with_profile_results_match_the_unprofiled_pass() {
        // The profiled pass is a different code path from
        // analyze_phase1 + analyze_phase2_blocking; it exists only to add
        // timings, so it must not change a single score.
        let g = chain(20);
        let arc = std::sync::Arc::new(g);
        let (profiled, _) = analyze_with_profile(std::sync::Arc::clone(&arc), &all_on());
        let (status, phase2) =
            analyze_phase2_blocking(std::sync::Arc::clone(&arc), &AnalysisBudget::default());
        assert_eq!(profiled.page_rank, phase2.page_rank);
        assert_eq!(profiled.betweenness, phase2.betweenness);
        assert_eq!(profiled.eigenvector, phase2.eigenvector);
        assert_eq!(profiled.hubs, phase2.hubs);
        assert_eq!(profiled.authorities, phase2.authorities);
        assert_eq!(profiled.critical_path_score, phase2.critical_path_score);
        assert_eq!(profiled.core_number, phase2.core_number);
        assert_eq!(profiled.articulation, phase2.articulation);
        assert_eq!(profiled.slack, phase2.slack);
        assert_eq!(profiled.cycles, phase2.cycles);
        for (a, b) in [
            (&profiled.status.page_rank, &status.page_rank),
            (&profiled.status.betweenness, &status.betweenness),
            (&profiled.status.eigenvector, &status.eigenvector),
            (&profiled.status.hits, &status.hits),
            (&profiled.status.critical, &status.critical),
            (&profiled.status.cycles, &status.cycles),
            (&profiled.status.kcore, &status.kcore),
            (&profiled.status.articulation, &status.articulation),
            (&profiled.status.slack, &status.slack),
        ] {
            assert_eq!(a.state, b.state);
        }
    }

    #[test]
    fn analyze_with_profile_empty_graph_reports_status_from_config_alone() {
        // Go graph.go:1906-1912 short-circuits before Phase 1.
        let config = all_on();
        let (out, profile) = analyze_with_profile(std::sync::Arc::new(DiGraph::default()), &config);
        assert_eq!(profile.node_count, 0);
        assert_eq!(profile.edge_count, 0);
        // Go still stamps `profile.Total = time.Since(totalStart)` on the
        // short-circuit path (graph.go:1911), so it is small but not zero.
        assert!(profile.total > Duration::ZERO);
        assert_eq!(profile.phase1, Duration::ZERO);
        assert_eq!(profile.phase2, Duration::ZERO);
        assert_eq!(out.status.page_rank.state, "computed");
        assert_eq!(out.status.betweenness.state, "computed");
        assert_eq!(out.status.critical.state, "computed");
    }

    #[test]
    fn analyze_with_profile_empty_graph_with_nothing_enabled_is_all_skipped() {
        let (out, _) = analyze_with_profile(
            std::sync::Arc::new(DiGraph::default()),
            &AnalysisConfigReport::default(),
        );
        for state in [
            &out.status.page_rank.state,
            &out.status.betweenness.state,
            &out.status.eigenvector.state,
            &out.status.hits.state,
            &out.status.critical.state,
            &out.status.cycles.state,
            &out.status.kcore.state,
            &out.status.articulation.state,
            &out.status.slack.state,
        ] {
            assert_eq!(state, "skipped");
        }
    }

    #[test]
    fn analyze_with_profile_honours_disabled_metrics() {
        // Go gates every metric on its config switch (graph.go:1993, :2046,
        // :2118, :2160, :2182, :2262, :2270): a disabled metric leaves its
        // timing at zero and reports "skipped".
        let config = AnalysisConfigReport {
            compute_cycles: false,
            cycles_skip_reason: "graph too large (>2000 nodes)",
            ..all_on()
        };
        let (out, profile) = analyze_with_profile(std::sync::Arc::new(chain(8)), &config);
        assert_eq!(profile.cycles, Duration::ZERO);
        assert_eq!(profile.cycle_count, 0);
        assert_eq!(out.cycles, None);
        assert_eq!(out.status.cycles.state, "skipped");
        assert_eq!(out.status.cycles.reason, "graph too large (>2000 nodes)");
        // The other metrics still ran.
        assert!(profile.pagerank > Duration::ZERO);
        assert_eq!(out.status.page_rank.state, "computed");
    }

    #[test]
    fn analyze_with_profile_cyclic_graph_skips_order_dependent_metrics() {
        // Go graph.go:2160-2180 and :2270-2278 both bail when the topological
        // order is unavailable, and record why. The timing fields stay
        // unasserted here: on this path the work is a two-node check, which can
        // legitimately measure as a zero delta.
        let (out, profile) = analyze_with_profile(std::sync::Arc::new(cycle()), &all_on());
        assert_eq!(out.critical_path_score, None);
        assert_eq!(out.slack, None);
        assert_eq!(out.status.critical.state, "skipped");
        assert_eq!(
            out.status.critical.reason,
            "dependency graph contains a cycle; topological order unavailable"
        );
        assert_eq!(out.status.slack.state, "skipped");
        assert_eq!(
            out.status.slack.reason,
            "dependency graph contains a cycle; topological order unavailable"
        );
        assert!(profile.cycle_count > 0, "a 2-cycle has a cyclic component");
    }

    #[test]
    fn analyze_with_profile_uses_the_configured_betweenness_mode() {
        let approx = AnalysisConfigReport {
            betweenness_mode: "approximate",
            betweenness_sample_size: 5,
            ..all_on()
        };
        let (out, profile) = analyze_with_profile(std::sync::Arc::new(chain(20)), &approx);
        assert!(!profile.betweenness_timeout);
        assert_eq!(out.status.betweenness.state, "computed");
        assert_eq!(out.status.betweenness.reason, "approximate");
        assert_eq!(out.status.betweenness.sample, 5);
        assert!(out.betweenness.is_some());

        // Exact mode reports no approximation reason and no sample.
        let (out, _) = analyze_with_profile(std::sync::Arc::new(chain(20)), &all_on());
        assert_eq!(out.status.betweenness.reason, "");
        assert_eq!(out.status.betweenness.sample, 0);
    }

    #[test]
    fn analyze_with_profile_reports_a_zero_timeout_as_immediate_failure() {
        // A zero ns budget cannot elapse, so the metric reports the timeout
        // state Go would reach, and PageRank still gets its uniform fallback.
        let starved = AnalysisConfigReport {
            page_rank_timeout_ns: 0,
            ..all_on()
        };
        let (out, profile) = analyze_with_profile(std::sync::Arc::new(chain(8)), &starved);
        assert!(profile.pagerank_timeout);
        assert_eq!(out.status.page_rank.state, "timeout");
        let pr = out
            .page_rank
            .expect("Go's uniform fallback keeps the map full");
        assert_eq!(pr.len(), 8);
        for v in pr.values() {
            assert!((v - 0.125).abs() < 1e-12, "{v}");
        }
    }

    #[test]
    fn skipped_metrics_matches_go_order_and_reasons() {
        // Go config.go:314-343 checks the four switches in this order.
        let xl = config_for_size(2500, 5000, 0.01);
        let skipped: Vec<(&str, &str)> = xl
            .skipped_metrics()
            .iter()
            .map(|s| (s.name, s.reason))
            .collect();
        assert_eq!(
            skipped,
            vec![
                ("HITS", "graph too large and dense"),
                ("Cycles", "graph too large (>2000 nodes)"),
            ]
        );
        assert!(full_analysis_config().skipped_metrics().is_empty());
        assert!(config_for_size(12, 11, 0.0).skipped_metrics().is_empty());
    }

    #[test]
    fn analysis_config_default_is_go_zero_value() {
        let c = AnalysisConfigReport::default();
        assert!(!c.compute_betweenness);
        assert!(!c.compute_page_rank);
        assert!(!c.compute_hits);
        assert!(!c.compute_cycles);
        assert!(!c.compute_eigenvector);
        assert!(!c.compute_critical_path);
        assert!(!c.compute_kcore);
        assert!(!c.compute_articulation);
        assert!(!c.compute_slack);
        assert_eq!(c.betweenness_timeout_ns, 0);
        assert_eq!(c.max_cycles_to_store, 0);
        assert_eq!(c.skipped_metrics().len(), 4);
    }
}
