//! Label health / cross-label flow / attention scoring — port of Go
//! `pkg/analysis/label_health.go` (subset backing `robot-label-health`,
//! `robot-label-flow`, `robot-label-attention`).
//!
//! Deliberate scope cut vs Go: the deep blockage-cascade tree
//! (`ComputeBlockageCascade`) and per-label subgraph critical-path
//! (`ComputeLabelCriticalPath`) are not ported here — those back other,
//! still-undispatched commands. Multi-week historical velocity
//! (`ComputeHistoricalVelocity`/`ComputeAllHistoricalVelocity`) *is* ported; see
//! the "Historical velocity" section below.
//!
//! Per-label subgraph PageRank (`ComputeLabelSubgraph`/`ComputeLabelPageRank`)
//! *is* ported: `compute_label_attention` extracts the label's subgraph and
//! runs PageRank on it, so `pagerank_sum` is the sum of the core-issue scores
//! of a label-scoped graph — not a sum over the global graph's PageRank.

use bv_core::model::{Issue, Status};
use bv_graph_core::DiGraph;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

fn is_closed_like(s: Status) -> bool {
    matches!(s, Status::Closed | Status::Tombstone)
}

fn has_label(issue: &Issue, label: &str) -> bool {
    issue.labels.iter().any(|l| l == label)
}

fn parse_ts(raw: &Option<String>) -> Option<jiff::Timestamp> {
    raw.as_deref()
        .and_then(|s| s.parse::<jiff::Timestamp>().ok())
}

fn clamp_score(v: i64) -> i64 {
    v.clamp(0, 100)
}

// ---------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------

pub const DEFAULT_STALE_THRESHOLD_DAYS: i64 = 14;
pub const HEALTHY_THRESHOLD: i64 = 70;
pub const WARNING_THRESHOLD: i64 = 40;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct LabelHealthConfig {
    pub stale_threshold_days: i64,
    pub velocity_weight: f64,
    pub freshness_weight: f64,
    pub flow_weight: f64,
    pub criticality_weight: f64,
    pub min_issues_for_health: i64,
    pub include_closed_in_flow: bool,
}

impl Default for LabelHealthConfig {
    fn default() -> Self {
        LabelHealthConfig {
            stale_threshold_days: DEFAULT_STALE_THRESHOLD_DAYS,
            velocity_weight: 0.25,
            freshness_weight: 0.25,
            flow_weight: 0.25,
            criticality_weight: 0.25,
            min_issues_for_health: 1,
            include_closed_in_flow: false,
        }
    }
}

pub fn health_level_from_score(score: i64) -> &'static str {
    if score >= HEALTHY_THRESHOLD {
        "healthy"
    } else if score >= WARNING_THRESHOLD {
        "warning"
    } else {
        "critical"
    }
}

fn composite_health(
    velocity: i64,
    freshness: i64,
    flow: i64,
    criticality: i64,
    cfg: &LabelHealthConfig,
) -> i64 {
    let weighted = velocity as f64 * cfg.velocity_weight
        + freshness as f64 * cfg.freshness_weight
        + flow as f64 * cfg.flow_weight
        + criticality as f64 * cfg.criticality_weight;
    clamp_score((weighted + 0.5) as i64)
}

// ---------------------------------------------------------------------
// Label extraction
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize)]
pub struct LabelExtractionResult {
    pub labels: Vec<String>,
    pub label_count: usize,
    pub issue_count: usize,
    pub unlabeled_count: usize,
    pub top_labels: Vec<String>,
}

pub fn extract_labels(issues: &[Issue]) -> LabelExtractionResult {
    let mut result = LabelExtractionResult {
        issue_count: issues.len(),
        ..Default::default()
    };
    if issues.is_empty() {
        return result;
    }
    let mut set = std::collections::BTreeSet::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for issue in issues {
        if issue.labels.is_empty() {
            result.unlabeled_count += 1;
        }
        for label in &issue.labels {
            if label.is_empty() {
                continue;
            }
            set.insert(label.clone());
            *counts.entry(label.clone()).or_insert(0) += 1;
        }
    }
    result.labels = set.into_iter().collect();
    result.label_count = result.labels.len();
    let mut top: Vec<(String, usize)> = counts.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    result.top_labels = top.into_iter().map(|(l, _)| l).collect();
    result
}

// ---------------------------------------------------------------------
// Velocity / freshness
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct VelocityMetrics {
    pub closed_last_7_days: i64,
    pub closed_last_30_days: i64,
    pub avg_days_to_close: f64,
    pub trend_direction: String,
    pub trend_percent: f64,
    pub velocity_score: i64,
}

pub fn compute_velocity_metrics(issues: &[Issue], now: jiff::Timestamp) -> VelocityMetrics {
    let week_ago = now - jiff::SignedDuration::from_secs(7 * 86400);
    let month_ago = now - jiff::SignedDuration::from_secs(30 * 86400);
    let prev_week_start = now - jiff::SignedDuration::from_secs(14 * 86400);

    let (mut closed7, mut closed30, mut prev_week, mut current_week) = (0i64, 0i64, 0i64, 0i64);
    let mut total_close_ns = 0i128;
    let mut close_samples = 0i64;

    for iss in issues {
        if !is_closed_like(iss.status) {
            continue;
        }
        let Some(closed_at) = parse_ts(&iss.closed_at) else {
            continue;
        };
        if closed_at > week_ago {
            closed7 += 1;
        }
        if closed_at > month_ago {
            closed30 += 1;
        }
        if closed_at > prev_week_start && closed_at < week_ago {
            prev_week += 1;
        } else if closed_at > week_ago {
            current_week += 1;
        }
        if let Some(created_at) = parse_ts(&iss.created_at) {
            // Go accumulates a time.Duration — an exact nanosecond count — and
            // converts once at the end via `totalCloseDur.Hours() / 24.0`
            // (label_health.go:370-379). The port summed per-issue durations
            // as f64 HOURS, which rounds differently and left
            // avg_days_to_close off in the last digits.
            total_close_ns += (closed_at.as_second() - created_at.as_second()) as i128
                * 1_000_000_000
                + (closed_at.subsec_nanosecond() - created_at.subsec_nanosecond()) as i128;
            close_samples += 1;
        }
    }

    let avg_days = if close_samples > 0 {
        // Go's `Duration.Hours()` is the integer hours plus the fractional
        // remainder, not `ns / 3.6e12` (see the same shape in triage.rs).
        const NANOS_PER_HOUR: i128 = 3_600_000_000_000;
        let whole_hours = total_close_ns / NANOS_PER_HOUR;
        let remainder = total_close_ns % NANOS_PER_HOUR;
        let hours = whole_hours as f64 + (remainder as f64) / (NANOS_PER_HOUR as f64);
        hours / 24.0 / close_samples as f64
    } else {
        0.0
    };

    let (mut trend_dir, mut trend_percent) = ("stable".to_string(), 0.0);
    if prev_week > 0 {
        trend_percent = ((current_week - prev_week) as f64 / prev_week as f64) * 100.0;
        if trend_percent > 10.0 {
            trend_dir = "improving".into();
        } else if trend_percent < -10.0 {
            trend_dir = "declining".into();
        }
    } else if current_week > 0 {
        trend_dir = "improving".into();
        trend_percent = 100.0;
    }

    let mut velocity_score = if closed30 > 0 {
        (closed30 as f64 * 10.0).min(100.0) as i64
    } else {
        0
    };
    if trend_dir == "improving" && velocity_score < 100 {
        velocity_score = clamp_score(velocity_score + 10);
    }

    VelocityMetrics {
        closed_last_7_days: closed7,
        closed_last_30_days: closed30,
        avg_days_to_close: avg_days,
        trend_direction: trend_dir,
        trend_percent,
        velocity_score,
    }
}

/// Go `FreshnessMetrics` (label_health.go:73). Both timestamps are plain
/// `time.Time`, not pointers, so they are always present: an absent value
/// marshals as Go's zero time rather than as `null`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct FreshnessMetrics {
    pub most_recent_update: String,
    pub oldest_open_issue: String,
    pub avg_days_since_update: f64,
    pub stale_count: i64,
    pub stale_threshold_days: i64,
    pub freshness_score: i64,
}

/// Go's zero `time.Time` in RFC3339Nano form. Emitted when a label has no open
/// issues, so `oldest_open_issue` is not null.
const GO_ZERO_TIME: &str = "0001-01-01T00:00:00Z";

/// Render a timestamp the way Go marshals a `time.Time`: UTC, RFC3339Nano,
/// trailing zeros in the fractional part removed.
fn go_time_string(ts: Option<jiff::Timestamp>) -> String {
    match ts {
        None => GO_ZERO_TIME.to_string(),
        Some(t) => {
            // jiff already appends the "Z"; only the fractional part needs
            // Go's RFC3339Nano trailing-zero trim.
            let s = t.to_string();
            match s.find('.') {
                Some(dot) => {
                    let frac = s[dot + 1..].trim_end_matches(['0', 'Z']);
                    if frac.is_empty() {
                        format!("{}Z", &s[..dot])
                    } else {
                        format!("{}.{}Z", &s[..dot], frac)
                    }
                }
                None => s,
            }
        }
    }
}

pub fn compute_freshness_metrics(
    issues: &[Issue],
    now: jiff::Timestamp,
    stale_days: i64,
) -> FreshnessMetrics {
    let stale_days = if stale_days <= 0 {
        DEFAULT_STALE_THRESHOLD_DAYS
    } else {
        stale_days
    };
    let mut most_recent: Option<jiff::Timestamp> = None;
    let mut oldest_open: Option<jiff::Timestamp> = None;
    let mut total_staleness = 0.0;
    let mut count = 0i64;
    let mut stale_count = 0i64;
    let threshold = stale_days as f64;

    for iss in issues {
        if let Some(updated) = parse_ts(&iss.updated_at) {
            if most_recent.is_none_or(|m| updated > m) {
                most_recent = Some(updated);
            }
            // Go's `now.Sub(iss.UpdatedAt).Hours() / 24.0`
            // (label_health.go:440) — `Duration.Hours()` is the integer
            // hours plus the fractional remainder, not `seconds / 3600`, so
            // the port's per-issue value rounded differently and
            // avg_days_since_update came out a digit off.
            const NANOS_PER_HOUR: i128 = 3_600_000_000_000;
            let delta_ns = (now.as_second() - updated.as_second()) as i128 * 1_000_000_000
                + (now.subsec_nanosecond() - updated.subsec_nanosecond()) as i128;
            let whole_hours = delta_ns / NANOS_PER_HOUR;
            let remainder = delta_ns % NANOS_PER_HOUR;
            let hours = whole_hours as f64 + (remainder as f64) / (NANOS_PER_HOUR as f64);
            let days = hours / 24.0;
            total_staleness += days;
            count += 1;
            if days >= threshold {
                stale_count += 1;
            }
        }
        if !is_closed_like(iss.status) {
            if let Some(created) = parse_ts(&iss.created_at) {
                if oldest_open.is_none_or(|o| created < o) {
                    oldest_open = Some(created);
                }
            }
        }
    }

    let avg_staleness = if count > 0 {
        total_staleness / count as f64
    } else {
        0.0
    };
    let freshness_score = (100.0 - (avg_staleness / (threshold * 2.0)) * 100.0).max(0.0) as i64;

    FreshnessMetrics {
        // Go marshals these as time.Time, so they are re-rendered in UTC
        // RFC3339Nano rather than passed through verbatim from the source
        // record — the raw text can carry a different offset or precision.
        most_recent_update: go_time_string(most_recent),
        oldest_open_issue: go_time_string(oldest_open),
        avg_days_since_update: avg_staleness,
        stale_count,
        stale_threshold_days: stale_days,
        freshness_score: clamp_score(freshness_score),
    }
}

// ---------------------------------------------------------------------
// Cross-label flow
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct BlockingPair {
    pub blocker_id: String,
    pub blocked_id: String,
    pub blocker_label: String,
    pub blocked_label: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct LabelDependency {
    pub from_label: String,
    pub to_label: String,
    pub issue_count: i64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub issue_ids: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub blocking_pairs: Vec<BlockingPair>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct CrossLabelFlow {
    pub labels: Vec<String>,
    pub flow_matrix: Vec<Vec<i64>>,
    pub dependencies: Vec<LabelDependency>,
    pub critical_paths: Vec<serde_json::Value>,
    pub bottleneck_labels: Vec<String>,
    pub total_cross_label_deps: i64,
}

pub fn compute_cross_label_flow(issues: &[Issue], cfg: &LabelHealthConfig) -> CrossLabelFlow {
    let extraction = extract_labels(issues);
    let label_list = extraction.labels;
    let n = label_list.len();
    let index: BTreeMap<&str, usize> = label_list
        .iter()
        .enumerate()
        .map(|(i, l)| (l.as_str(), i))
        .collect();
    let mut matrix = vec![vec![0i64; n]; n];

    let issue_map: std::collections::HashMap<&str, &Issue> =
        issues.iter().map(|i| (i.id.as_str(), i)).collect();

    let mut dep_map: BTreeMap<(String, String), LabelDependency> = BTreeMap::new();
    let mut total_deps = 0i64;

    for blocked in issues {
        if !cfg.include_closed_in_flow && is_closed_like(blocked.status) {
            continue;
        }
        for dep in &blocked.dependencies {
            if !dep.r#type.is_blocking() {
                continue;
            }
            let Some(blocker) = issue_map.get(dep.depends_on_id.as_str()) else {
                continue;
            };
            if !cfg.include_closed_in_flow && is_closed_like(blocker.status) {
                continue;
            }
            for from in &blocker.labels {
                for to in &blocked.labels {
                    if from.is_empty() || to.is_empty() || from == to {
                        continue;
                    }
                    let (Some(&i_from), Some(&i_to)) =
                        (index.get(from.as_str()), index.get(to.as_str()))
                    else {
                        continue;
                    };
                    matrix[i_from][i_to] += 1;
                    total_deps += 1;
                    let key = (from.clone(), to.clone());
                    let entry = dep_map.entry(key).or_insert_with(|| LabelDependency {
                        from_label: from.clone(),
                        to_label: to.clone(),
                        issue_count: 0,
                        issue_ids: Vec::new(),
                        blocking_pairs: Vec::new(),
                    });
                    entry.issue_count += 1;
                    entry.issue_ids.push(blocked.id.clone());
                    entry.blocking_pairs.push(BlockingPair {
                        blocker_id: blocker.id.clone(),
                        blocked_id: blocked.id.clone(),
                        blocker_label: from.clone(),
                        blocked_label: to.clone(),
                    });
                }
            }
        }
    }

    let mut deps: Vec<LabelDependency> = dep_map.into_values().collect();
    deps.sort_by(|a, b| {
        a.from_label
            .cmp(&b.from_label)
            .then_with(|| a.to_label.cmp(&b.to_label))
            .then_with(|| b.issue_count.cmp(&a.issue_count))
    });

    let mut out_counts: BTreeMap<&str, i64> = BTreeMap::new();
    let mut max_out = 0i64;
    for (i, row) in matrix.iter().enumerate() {
        let sum: i64 = row.iter().sum();
        out_counts.insert(label_list[i].as_str(), sum);
        if sum > max_out {
            max_out = sum;
        }
    }
    let mut bottlenecks: Vec<String> = out_counts
        .iter()
        .filter(|(_, &c)| c == max_out && c > 0)
        .map(|(l, _)| l.to_string())
        .collect();
    bottlenecks.sort();

    CrossLabelFlow {
        labels: label_list,
        flow_matrix: matrix,
        dependencies: deps,
        critical_paths: Vec::new(),
        bottleneck_labels: bottlenecks,
        total_cross_label_deps: total_deps,
    }
}

// ---------------------------------------------------------------------
// Per-label health
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct FlowMetrics {
    pub incoming_deps: i64,
    pub outgoing_deps: i64,
    pub incoming_labels: Option<Vec<String>>,
    pub outgoing_labels: Option<Vec<String>>,
    pub blocked_by_external: i64,
    pub blocking_external: i64,
    pub flow_score: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct CriticalityMetrics {
    pub avg_pagerank: f64,
    pub avg_betweenness: f64,
    pub max_betweenness: f64,
    pub critical_path_count: i64,
    pub bottleneck_count: i64,
    pub criticality_score: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct LabelHealth {
    pub label: String,
    pub issue_count: i64,
    pub open_count: i64,
    pub closed_count: i64,
    #[serde(rename = "blocked_count")]
    pub blocked: i64,
    pub health: i64,
    pub health_level: &'static str,
    pub velocity: VelocityMetrics,
    pub freshness: FreshnessMetrics,
    pub flow: FlowMetrics,
    pub criticality: CriticalityMetrics,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<String>,
}

/// Precomputed graph centrality, shared across all labels for efficiency
/// (Go: `*GraphStats` passed into `ComputeAllLabelHealth`/`ComputeLabelHealthForLabel`).
pub struct GraphStats {
    pub pagerank: BTreeMap<String, f64>,
    pub betweenness: BTreeMap<String, f64>,
    pub critical_path: BTreeMap<String, f64>,
}

pub fn compute_graph_stats(issues: &[Issue]) -> GraphStats {
    let g = crate::build_graph(issues);
    let pr = bv_graph_core::algorithms::pagerank::pagerank_default(&g);
    // Go does not call Brandes directly here. `ComputeAllLabelHealth` builds
    // its own `Analyzer` and calls `Analyze()` (label_health.go:570-571), which
    // runs `ConfigForSize` — and that selects *approximate* betweenness for a
    // large or sparse graph (config.go:180-184), sampling
    // `RecommendSampleSize(nodeCount, edgeCount)` pivots with seed 1.
    //
    // Calling the exact algorithm instead changes the answer, not just its
    // cost. Measured on large_cyclic_600: exact gives 337 nodes above zero
    // with max 184 and mean 17.48, where Go reports 176 / 210 / 13.72 — the
    // approximate path reproduces Go exactly. Since `BottleneckCount` counts
    // nodes with betweenness > 0 (label_health.go:592-594), using the wrong
    // one inflated it from 176 to 337.
    let bw = crate::analyzer::go_betweenness(&g);
    let cp = bv_graph_core::algorithms::critical_path::critical_path_heights(&g);
    let mut pagerank = BTreeMap::new();
    let mut betweenness = BTreeMap::new();
    let mut critical_path = BTreeMap::new();
    for i in 0..g.len() {
        let id = g.node_id(i).unwrap_or_default().to_string();
        pagerank.insert(id.clone(), pr.get(i).copied().unwrap_or(0.0));
        betweenness.insert(id.clone(), bw.get(i).copied().unwrap_or(0.0));
        critical_path.insert(id, cp.get(i).copied().unwrap_or(0.0));
    }
    GraphStats {
        pagerank,
        betweenness,
        critical_path,
    }
}

fn labels_for_issue<'a>(issues: &'a [Issue], id: &str) -> Vec<&'a str> {
    issues
        .iter()
        .find(|i| i.id == id)
        .map(|i| i.labels.iter().map(|s| s.as_str()).collect())
        .unwrap_or_default()
}

pub fn compute_label_health_for_label(
    label: &str,
    issues: &[Issue],
    cfg: &LabelHealthConfig,
    now: jiff::Timestamp,
    stats: &GraphStats,
) -> LabelHealth {
    let labeled: Vec<&Issue> = issues.iter().filter(|i| has_label(i, label)).collect();
    let issue_ids: Vec<String> = labeled.iter().map(|i| i.id.clone()).collect();
    let issue_count = labeled.len() as i64;

    if issue_count == 0 {
        return LabelHealth {
            label: label.to_string(),
            issue_count: 0,
            open_count: 0,
            closed_count: 0,
            blocked: 0,
            health: 0,
            health_level: "critical",
            velocity: compute_velocity_metrics(&[], now),
            freshness: compute_freshness_metrics(&[], now, cfg.stale_threshold_days),
            flow: FlowMetrics {
                incoming_deps: 0,
                outgoing_deps: 0,
                incoming_labels: None,
                outgoing_labels: None,
                blocked_by_external: 0,
                blocking_external: 0,
                flow_score: 100,
            },
            criticality: CriticalityMetrics {
                avg_pagerank: 0.0,
                avg_betweenness: 0.0,
                max_betweenness: 0.0,
                critical_path_count: 0,
                bottleneck_count: 0,
                criticality_score: 50,
            },
            issues: vec![],
        };
    }

    let labeled_owned: Vec<Issue> = labeled.iter().map(|i| (*i).clone()).collect();
    let (mut open_count, mut closed_count, mut blocked) = (0i64, 0i64, 0i64);
    for iss in &labeled {
        match iss.status {
            Status::Closed | Status::Tombstone => closed_count += 1,
            Status::Blocked => blocked += 1,
            _ => open_count += 1,
        }
    }

    let velocity = compute_velocity_metrics(&labeled_owned, now);
    let freshness = compute_freshness_metrics(&labeled_owned, now, cfg.stale_threshold_days);

    let labeled_set: std::collections::HashSet<&str> =
        labeled.iter().map(|i| i.id.as_str()).collect();
    let mut seen_in = std::collections::BTreeSet::new();
    let mut seen_out = std::collections::BTreeSet::new();
    let (mut incoming_deps, mut outgoing_deps) = (0i64, 0i64);
    let (mut blocked_by_external, mut blocking_external) = (0i64, 0i64);

    for iss in &labeled {
        let mut has_external_blocker = false;
        for dep in &iss.dependencies {
            if !dep.r#type.is_blocking() {
                continue;
            }
            for bl in labels_for_issue(issues, &dep.depends_on_id) {
                if bl != label {
                    incoming_deps += 1;
                    seen_in.insert(bl.to_string());
                    has_external_blocker = true;
                }
            }
        }
        if has_external_blocker {
            blocked_by_external += 1;
        }
    }
    for other in issues {
        if labeled_set.contains(other.id.as_str()) {
            continue;
        }
        let mut counted = false;
        for dep in &other.dependencies {
            if !dep.r#type.is_blocking() || !labeled_set.contains(dep.depends_on_id.as_str()) {
                continue;
            }
            for ol in &other.labels {
                if ol != label {
                    outgoing_deps += 1;
                    seen_out.insert(ol.clone());
                }
            }
            if !counted {
                blocking_external += 1;
                counted = true;
            }
        }
    }
    let flow = FlowMetrics {
        incoming_deps,
        outgoing_deps,
        incoming_labels: {
            let v: Vec<String> = seen_in.into_iter().collect();
            if v.is_empty() {
                None
            } else {
                Some(v)
            }
        },
        outgoing_labels: {
            let v: Vec<String> = seen_out.into_iter().collect();
            if v.is_empty() {
                None
            } else {
                Some(v)
            }
        },
        blocked_by_external,
        blocking_external,
        flow_score: clamp_score(100 - incoming_deps * 5),
    };

    let max_pr = stats.pagerank.values().copied().fold(0.0, f64::max);
    let max_bw = stats.betweenness.values().copied().fold(0.0, f64::max);
    let (mut pr_sum, mut bw_sum, mut max_bw_label) = (0.0, 0.0, 0.0);
    let (mut crit_count, mut bottleneck_count) = (0i64, 0i64);
    for iss in &labeled {
        let pr = stats.pagerank.get(&iss.id).copied().unwrap_or(0.0);
        let bw = stats.betweenness.get(&iss.id).copied().unwrap_or(0.0);
        pr_sum += pr;
        bw_sum += bw;
        if bw > max_bw_label {
            max_bw_label = bw;
        }
        if stats.critical_path.get(&iss.id).copied().unwrap_or(0.0) > 0.0 {
            crit_count += 1;
        }
        if bw > 0.0 {
            bottleneck_count += 1;
        }
    }
    let avg_pr = pr_sum / issue_count as f64;
    let avg_bw = bw_sum / issue_count as f64;
    let mut crit_score = 0i64;
    if max_pr > 0.0 {
        crit_score += ((avg_pr / max_pr) * 50.0) as i64;
    }
    if max_bw > 0.0 {
        crit_score += ((max_bw_label / max_bw) * 50.0) as i64;
    }
    let criticality = CriticalityMetrics {
        avg_pagerank: avg_pr,
        avg_betweenness: avg_bw,
        max_betweenness: max_bw_label,
        critical_path_count: crit_count,
        bottleneck_count,
        criticality_score: clamp_score(crit_score),
    };

    let health = composite_health(
        velocity.velocity_score,
        freshness.freshness_score,
        flow.flow_score,
        criticality.criticality_score,
        cfg,
    );

    LabelHealth {
        label: label.to_string(),
        issue_count,
        open_count,
        closed_count,
        blocked,
        health,
        health_level: health_level_from_score(health),
        velocity,
        freshness,
        flow,
        criticality,
        issues: issue_ids,
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct LabelSummary {
    pub label: String,
    pub issue_count: i64,
    pub open_count: i64,
    pub health: i64,
    pub health_level: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_issue: Option<String>,
    pub needs_attention: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct LabelAnalysisResult {
    pub generated_at: String,
    pub total_labels: i64,
    pub healthy_count: i64,
    pub warning_count: i64,
    pub critical_count: i64,
    pub labels: Vec<LabelHealth>,
    pub summaries: Vec<LabelSummary>,
    pub attention_needed: Vec<String>,
}

pub fn compute_all_label_health(
    issues: &[Issue],
    cfg: &LabelHealthConfig,
    now: jiff::Timestamp,
) -> LabelAnalysisResult {
    let extraction = extract_labels(issues);
    let stats = compute_graph_stats(issues);
    let mut result = LabelAnalysisResult {
        generated_at: now.to_string(),
        total_labels: extraction.label_count as i64,
        healthy_count: 0,
        warning_count: 0,
        critical_count: 0,
        labels: Vec::new(),
        summaries: Vec::new(),
        attention_needed: Vec::new(),
    };

    for label in &extraction.labels {
        let health = compute_label_health_for_label(label, issues, cfg, now, &stats);
        let summary = LabelSummary {
            label: label.clone(),
            issue_count: health.issue_count,
            open_count: health.open_count,
            health: health.health,
            health_level: health.health_level,
            top_issue: health.issues.first().cloned(),
            needs_attention: health.health < HEALTHY_THRESHOLD,
        };
        match health.health_level {
            "healthy" => result.healthy_count += 1,
            "warning" => {
                result.warning_count += 1;
                result.attention_needed.push(label.clone());
            }
            _ => {
                result.critical_count += 1;
                result.attention_needed.push(label.clone());
            }
        }
        result.labels.push(health);
        result.summaries.push(summary);
    }

    result
        .summaries
        .sort_by(|a, b| b.health.cmp(&a.health).then_with(|| a.label.cmp(&b.label)));
    result
}

// ---------------------------------------------------------------------
// Label-scoped PageRank
// ---------------------------------------------------------------------

/// One label's extracted subgraph: the issues carrying `label` (core) plus
/// their direct neighbours (blockers they depend on, and issues that depend on
/// them), with the blocking edges among that set.
///
/// Port of Go `LabelSubgraph` (`pkg/analysis/label_health.go:1343`).
struct LabelSubgraph {
    /// Issues carrying `label`, sorted.
    core_issues: Vec<String>,
    /// `core_issues` plus the direct neighbours outside `label` (Go's
    /// `CoreIssues` + `DependencyIssues`), sorted. Go keeps the dependency
    /// list only for reporting; PageRank needs the union.
    all_issues: Vec<String>,
    /// blocker id -> sorted blocked ids; only blocking edges, both ends in the
    /// subgraph. Empty-list entries are dropped, as in Go.
    adjacency: BTreeMap<String, Vec<String>>,
}

/// Reverse dependency lookup over the whole issue set: blocker id -> the ids
/// of the issues that declare a dependency on it (any dependency type, matching
/// Go's untyped inner scan). Hoisted out of the per-label extraction so that
/// building it costs one pass over the corpus instead of one pass per label.
struct ReverseDeps {
    /// Every known issue id — Go's `fullIssueMap` membership test.
    known: BTreeSet<String>,
    /// blocker id -> dependent issue ids, deduplicated.
    dependents: BTreeMap<String, Vec<String>>,
}

impl ReverseDeps {
    fn build(issues: &[Issue]) -> Self {
        let mut known = BTreeSet::new();
        let mut seen: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for iss in issues {
            known.insert(iss.id.clone());
            for dep in &iss.dependencies {
                let blocker = dep.effective_depends_on();
                if blocker.is_empty() {
                    continue;
                }
                seen.entry(blocker.to_string())
                    .or_default()
                    .insert(iss.id.clone());
            }
        }
        let dependents = seen
            .into_iter()
            .map(|(blocker, ids)| (blocker, ids.into_iter().collect()))
            .collect();
        ReverseDeps { known, dependents }
    }
}

/// Extract the subgraph for `label`.
///
/// Port of Go `ComputeLabelSubgraph` (`pkg/analysis/label_health.go:1351`).
/// Membership expansion deliberately ignores dependency *type* (as Go does) —
/// only the adjacency edges filter on `is_blocking`.
/// The ids a `--label` scope selects, mirroring Go `scopeLoadedIssues`
/// (cmd/bv/main.go:4870-4900): the label's own issues become the candidate
/// set, and their direct neighbours stay in the analysis as context.
///
/// Returns `(candidate_ids, analysis_ids)`; the caller feeds the first to the
/// scope hash and loads the second into the analyzer.
pub fn label_scope_ids(label: &str, issues: &[Issue]) -> (Vec<String>, Vec<String>) {
    if label.is_empty() {
        let all: Vec<String> = issues.iter().map(|i| i.id.clone()).collect();
        return (all.clone(), all);
    }
    let rev = ReverseDeps::build(issues);
    let sg = compute_label_subgraph(label, issues, &rev);
    (sg.core_issues, sg.all_issues)
}

fn compute_label_subgraph(label: &str, issues: &[Issue], rev: &ReverseDeps) -> LabelSubgraph {
    if label.is_empty() || issues.is_empty() {
        return LabelSubgraph {
            core_issues: Vec::new(),
            all_issues: Vec::new(),
            adjacency: BTreeMap::new(),
        };
    }

    // Core issues: those carrying the label. BTreeSet iteration is already the
    // sorted order Go reaches via `sort.Strings`.
    let mut core: BTreeSet<&str> = BTreeSet::new();
    for iss in issues {
        if has_label(iss, label) {
            core.insert(iss.id.as_str());
        }
    }

    // Dependency issues: blockers a core issue depends on, plus issues that
    // depend on a core issue — either side, excluding the core itself.
    let mut deps: BTreeSet<&str> = BTreeSet::new();
    for iss in issues {
        if !core.contains(iss.id.as_str()) {
            continue;
        }
        for dep in &iss.dependencies {
            let blocker = dep.effective_depends_on();
            if !core.contains(blocker) && rev.known.contains(blocker) {
                deps.insert(blocker);
            }
        }
        if let Some(dependents) = rev.dependents.get(iss.id.as_str()) {
            for id in dependents {
                if !core.contains(id.as_str()) {
                    deps.insert(id.as_str());
                }
            }
        }
    }

    let core_issues: Vec<String> = core.into_iter().map(str::to_string).collect();
    let mut all_issues = core_issues.clone();
    all_issues.extend(deps.into_iter().map(str::to_string));
    all_issues.sort_unstable();

    let in_subgraph: BTreeSet<&str> = all_issues.iter().map(String::as_str).collect();
    let by_id: BTreeMap<&str, &Issue> = issues.iter().map(|i| (i.id.as_str(), i)).collect();

    // Edges: blocker -> blocked, blocking types only, both ends in the subgraph.
    let mut adjacency: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for id in &all_issues {
        let Some(iss) = by_id.get(id.as_str()) else {
            continue;
        };
        for dep in &iss.dependencies {
            if !dep.r#type.is_blocking() {
                continue;
            }
            let blocker = dep.effective_depends_on();
            if !in_subgraph.contains(blocker) {
                continue;
            }
            adjacency
                .entry(blocker.to_string())
                .or_default()
                .push(id.clone());
        }
    }
    for targets in adjacency.values_mut() {
        targets.sort_unstable();
    }

    LabelSubgraph {
        core_issues,
        all_issues,
        adjacency,
    }
}

/// Sum of the PageRank scores restricted to the subgraph's core issues.
///
/// Port of Go `ComputeLabelPageRank` + the `CoreOnly` accumulation in
/// `computeLabelAttention` (`pkg/analysis/label_health.go:1552`, `:2038`).
/// Nodes are added in sorted id order so the power iteration visits them in
/// the same order as Go's gonum ids, keeping the accumulation byte-identical.
fn compute_label_pagerank_core_sum(sg: &LabelSubgraph) -> f64 {
    if sg.all_issues.is_empty() {
        return 0.0;
    }
    let mut g = DiGraph::with_capacity(sg.all_issues.len(), sg.all_issues.len() * 2);
    for id in &sg.all_issues {
        g.add_node(id);
    }
    for (blocker, targets) in &sg.adjacency {
        let Some(from) = g.node_idx(blocker) else {
            continue;
        };
        for target in targets {
            let Some(to) = g.node_idx(target) else {
                continue;
            };
            // Go's gonum `simple.DirectedGraph.SetEdge` panics on a self edge;
            // dropping it keeps a malformed corpus from aborting the run.
            if from == to {
                continue;
            }
            g.add_edge(from, to);
        }
    }

    let scores = bv_graph_core::algorithms::pagerank::pagerank_default(&g);
    // Go accumulates over a Go map (random order); sorted ids give us the
    // deterministic order, which is identical whenever Go's own result is.
    let mut sum = 0.0;
    for id in &sg.core_issues {
        if let Some(idx) = g.node_idx(id) {
            sum += scores.get(idx).copied().unwrap_or(0.0);
        }
    }
    sum
}

// ---------------------------------------------------------------------
// Attention scoring
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct LabelAttentionScore {
    pub label: String,
    pub attention_score: f64,
    pub normalized_score: f64,
    pub rank: i64,
    pub pagerank_sum: f64,
    pub staleness_factor: f64,
    pub block_impact: f64,
    pub velocity_factor: f64,
    pub open_count: i64,
    pub blocked_count: i64,
    pub stale_count: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct LabelAttentionResult {
    pub generated_at: String,
    pub labels: Vec<LabelAttentionScore>,
    pub top_attention: Vec<String>,
    pub low_attention: Vec<String>,
    pub max_score: f64,
    pub min_score: f64,
    pub total_labels: i64,
}

fn compute_label_attention(
    label: &str,
    issues: &[Issue],
    rev: &ReverseDeps,
    cfg: &LabelHealthConfig,
    now: jiff::Timestamp,
) -> LabelAttentionScore {
    let labeled: Vec<&Issue> = issues.iter().filter(|i| has_label(i, label)).collect();
    let mut score = LabelAttentionScore {
        label: label.to_string(),
        attention_score: 0.0,
        normalized_score: 0.0,
        rank: 0,
        pagerank_sum: 0.0,
        staleness_factor: 0.0,
        block_impact: 0.0,
        velocity_factor: 0.0,
        open_count: 0,
        blocked_count: 0,
        stale_count: 0,
    };
    if labeled.is_empty() {
        return score;
    }
    for iss in &labeled {
        if !is_closed_like(iss.status) {
            score.open_count += 1;
        }
    }

    // PageRank over this label's own subgraph, summed over the core issues
    // (Go: `ComputeLabelSubgraph` -> `ComputeLabelPageRank` -> `CoreOnly`).
    let sg = compute_label_subgraph(label, issues, rev);
    score.pagerank_sum = compute_label_pagerank_core_sum(&sg);

    let labeled_owned: Vec<Issue> = labeled.iter().map(|i| (*i).clone()).collect();
    let freshness = compute_freshness_metrics(&labeled_owned, now, cfg.stale_threshold_days);
    score.stale_count = freshness.stale_count;
    score.staleness_factor = if score.open_count > 0 {
        1.0 + score.stale_count as f64 / score.open_count as f64
    } else {
        1.0
    };

    let mut block_impact = 0i64;
    for iss in &labeled {
        for other in issues {
            if other.id == iss.id {
                continue;
            }
            for dep in &other.dependencies {
                if dep.depends_on_id == iss.id && dep.r#type.is_blocking() {
                    block_impact += 1;
                }
            }
        }
    }
    score.block_impact = block_impact as f64;
    score.blocked_count = block_impact;

    let velocity = compute_velocity_metrics(&labeled_owned, now);
    score.velocity_factor = velocity.closed_last_30_days as f64 + 1.0;

    let numerator = score.pagerank_sum * score.staleness_factor * (1.0 + score.block_impact);
    score.attention_score = numerator / score.velocity_factor;
    score
}

pub fn compute_label_attention_scores(
    issues: &[Issue],
    cfg: &LabelHealthConfig,
    now: jiff::Timestamp,
) -> LabelAttentionResult {
    let mut result = LabelAttentionResult {
        generated_at: now.to_string(),
        labels: Vec::new(),
        top_attention: Vec::new(),
        low_attention: Vec::new(),
        max_score: 0.0,
        min_score: 0.0,
        total_labels: 0,
    };
    let extraction = extract_labels(issues);
    if extraction.label_count == 0 {
        return result;
    }
    let rev = ReverseDeps::build(issues);

    let mut scores: Vec<LabelAttentionScore> = extraction
        .labels
        .iter()
        .map(|label| compute_label_attention(label, issues, &rev, cfg, now))
        .collect();

    let (mut max_score, mut min_score) = (0.0, 0.0);
    for (i, s) in scores.iter().enumerate() {
        if i == 0 {
            max_score = s.attention_score;
            min_score = s.attention_score;
        } else {
            if s.attention_score > max_score {
                max_score = s.attention_score;
            }
            if s.attention_score < min_score {
                min_score = s.attention_score;
            }
        }
    }
    result.max_score = max_score;
    result.min_score = min_score;
    let range = max_score - min_score;
    for s in &mut scores {
        s.normalized_score = if range > 0.0 {
            (s.attention_score - min_score) / range
        } else {
            0.5
        };
    }

    scores.sort_by(|a, b| {
        const EPS: f64 = 1e-6;
        let diff = a.attention_score - b.attention_score;
        if diff.abs() > EPS {
            b.attention_score
                .partial_cmp(&a.attention_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        } else {
            a.label.cmp(&b.label)
        }
    });
    for (i, s) in scores.iter_mut().enumerate() {
        s.rank = i as i64 + 1;
    }

    let top_n = scores.len().min(3);
    result.top_attention = scores[..top_n].iter().map(|s| s.label.clone()).collect();
    let low_start = (scores.len().saturating_sub(top_n)).max(top_n);
    result.low_attention = scores[low_start..]
        .iter()
        .map(|s| s.label.clone())
        .collect();

    result.total_labels = scores.len() as i64;
    result.labels = scores;
    result
}

// ---------------------------------------------------------------------
// Historical velocity
// ---------------------------------------------------------------------

/// Go `WeeklySnapshot` (label_health.go:59-66). `WeekStart`/`WeekEnd` are
/// plain `time.Time` there, not pointers, so they are always serialized; the
/// rendered form is Go's UTC RFC3339Nano.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct WeeklySnapshot {
    pub week_start: String,
    pub week_end: String,
    pub closed: i64,
    pub weeks_ago: i64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub issue_ids: Vec<String>,
    pub cumulative: i64,
}

/// Go's `omitempty` on a float field: the zero value is dropped from the JSON.
fn is_zero_f64(v: &f64) -> bool {
    *v == 0.0
}

/// Go `HistoricalVelocity` (label_health.go:48-60).
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct HistoricalVelocity {
    pub label: String,
    pub weekly_velocity: Vec<WeeklySnapshot>,
    pub weeks_analyzed: i64,
    pub moving_avg_4_week: f64,
    #[serde(skip_serializing_if = "is_zero_f64")]
    pub moving_avg_8_week: f64,
    pub peak_week: i64,
    pub peak_velocity: i64,
    pub trough_week: i64,
    pub trough_velocity: i64,
    pub variance: f64,
    pub consistency_score: i64,
}

impl HistoricalVelocity {
    /// Go `(*HistoricalVelocity).GetVelocityTrend` (label_health.go:2268):
    /// splits the window in half — index 0 is the *most recent* week, so the
    /// first half is the recent half — and compares the two sums.
    pub fn get_velocity_trend(&self) -> &'static str {
        let n = self.weekly_velocity.len() as i64;
        if n < 4 {
            return "insufficient_data";
        }

        let half_point = n / 2;
        let mut recent_sum = 0i64;
        let mut older_sum = 0i64;
        for i in 0..half_point {
            recent_sum += self.weekly_velocity[i as usize].closed;
        }
        for i in half_point..n {
            older_sum += self.weekly_velocity[i as usize].closed;
        }

        if older_sum == 0 && recent_sum > 0 {
            return "accelerating";
        }
        if older_sum == 0 && recent_sum == 0 {
            return "stable";
        }

        let ratio = recent_sum as f64 / older_sum as f64;
        if ratio > 1.3 {
            "accelerating"
        } else if ratio < 0.7 {
            "decelerating"
        } else if self.variance > self.peak_velocity as f64 * 0.5 {
            "erratic"
        } else {
            "stable"
        }
    }

    /// Go `(*HistoricalVelocity).GetWeeklyAverage` (label_health.go:2307).
    pub fn get_weekly_average(&self) -> f64 {
        if self.weeks_analyzed == 0 {
            return 0.0;
        }
        let total: i64 = self.weekly_velocity.iter().map(|s| s.closed).sum();
        total as f64 / self.weeks_analyzed as f64
    }
}

/// Go's `int(now.Weekday())` is Sunday=0..Saturday=6. jiff's `Weekday` numbers
/// Monday=1..Sunday=7, so the two only differ on Sunday.
fn go_weekday(now: jiff::Timestamp) -> i64 {
    use jiff::civil::Weekday;
    match now.to_zoned(jiff::tz::TimeZone::UTC).date().weekday() {
        Weekday::Monday => 1,
        Weekday::Tuesday => 2,
        Weekday::Wednesday => 3,
        Weekday::Thursday => 4,
        Weekday::Friday => 5,
        Weekday::Saturday => 6,
        Weekday::Sunday => 0,
    }
}

/// Go `currentWeekStart := now.AddDate(0, 0, -(weekday - 1)).Truncate(24 * time.Hour)`
/// (label_health.go:2133-2136) — the Monday of `now`'s week, at UTC midnight.
/// `Truncate` floors the absolute instant to a multiple of 24h, and a UTC day is
/// exactly 24h, so that is midnight UTC.
fn current_week_start(now: jiff::Timestamp) -> jiff::Timestamp {
    let mut weekday = go_weekday(now);
    if weekday == 0 {
        weekday = 7; // Sunday = 7
    }
    let monday = now
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date()
        .checked_sub(jiff::Span::new().days(weekday - 1))
        .expect("subtracting at most 6 days from a civil date cannot overflow");
    monday
        .to_datetime(jiff::civil::Time::midnight())
        .to_zoned(jiff::tz::TimeZone::UTC)
        .expect("UTC has no DST, so midnight is never skipped or ambiguous")
        .timestamp()
}

/// Go's `AddDate(0, 0, n)` on a `time.Time` — calendar days, not a fixed number
/// of seconds. At UTC midnight the two coincide.
fn add_days(ts: jiff::Timestamp, days: i64) -> jiff::Timestamp {
    ts.to_zoned(jiff::tz::TimeZone::UTC)
        .checked_add(jiff::Span::new().days(days))
        .expect("shifting a UTC instant by a whole number of days cannot overflow")
        .timestamp()
}

/// Go `ComputeHistoricalVelocity` (label_health.go:2109): closure counts bucketed
/// into whole weeks, index 0 being the current week.
pub fn compute_historical_velocity(
    issues: &[Issue],
    label: &str,
    num_weeks: i64,
    now: jiff::Timestamp,
) -> HistoricalVelocity {
    let num_weeks = if num_weeks <= 0 { 8 } else { num_weeks };

    // Week buckets, newest first.
    let this_week_start = current_week_start(now);
    let mut week_bounds: Vec<(jiff::Timestamp, jiff::Timestamp)> = Vec::new();
    let mut weekly: Vec<WeeklySnapshot> = Vec::new();
    for i in 0..num_weeks {
        let start = add_days(this_week_start, -7 * i);
        let end = add_days(start, 7);
        week_bounds.push((start, end));
        weekly.push(WeeklySnapshot {
            week_start: go_time_string(Some(start)),
            week_end: go_time_string(Some(end)),
            weeks_ago: i,
            ..Default::default()
        });
    }

    // Bucket closures. Only closed-like issues carrying the label and a
    // ClosedAt count, and a closure outside the window is dropped entirely.
    for iss in issues {
        if !is_closed_like(iss.status) || !has_label(iss, label) {
            continue;
        }
        let Some(closed_at) = parse_ts(&iss.closed_at) else {
            continue;
        };
        for (i, (start, end)) in week_bounds.iter().enumerate() {
            if closed_at >= *start && closed_at < *end {
                weekly[i].closed += 1;
                weekly[i].issue_ids.push(iss.id.clone());
                break;
            }
        }
    }

    // Running total, oldest (highest index) to newest.
    let mut cumulative = 0i64;
    for i in (0..num_weeks).rev() {
        cumulative += weekly[i as usize].closed;
        weekly[i as usize].cumulative = cumulative;
    }

    // Peak is the highest week; trough is the lowest week *that has closures*
    // (a zero week is skipped, not treated as the trough).
    let mut peak_velocity = 0i64;
    let mut trough_velocity = i64::MAX;
    let mut peak_week = 0i64;
    let mut trough_week = 0i64;
    let mut has_non_zero = false;
    for (i, snap) in weekly.iter().enumerate() {
        if snap.closed > peak_velocity {
            peak_velocity = snap.closed;
            peak_week = i as i64;
        }
        if snap.closed > 0 && snap.closed < trough_velocity {
            trough_velocity = snap.closed;
            trough_week = i as i64;
            has_non_zero = true;
        }
    }
    if !has_non_zero {
        // Go leaves both trough fields at their zero value when every week is 0.
        trough_week = 0;
        trough_velocity = 0;
    }

    let mut moving_avg_4_week = 0.0;
    if num_weeks >= 4 {
        let sum: i64 = weekly[..4].iter().map(|s| s.closed).sum();
        moving_avg_4_week = sum as f64 / 4.0;
    }
    let mut moving_avg_8_week = 0.0;
    if num_weeks >= 8 {
        let sum: i64 = weekly[..8].iter().map(|s| s.closed).sum();
        moving_avg_8_week = sum as f64 / 8.0;
    }

    // Population variance over the window, then a coefficient-of-variation
    // consistency score: no closures at all scores 0, not 100.
    let mut variance = 0.0f64;
    let mut consistency_score = 0i64;
    if num_weeks > 0 {
        let mut sum = 0.0f64;
        for snap in &weekly {
            sum += snap.closed as f64;
        }
        let mean = sum / num_weeks as f64;

        let mut accum = 0.0f64;
        for snap in &weekly {
            let diff = snap.closed as f64 - mean;
            // Go writes `variance += diff * diff` (label_health.go:2228-2233) and
            // its compiler contracts the mul+add into a single FMA, exactly as
            // it does for the risk composite (see impact.rs). The unfused form
            // lands one ULP low — a 3-week window of 4/2/1 closures gives
            // 1.5555555555555554 where the oracle gives 1.5555555555555556 —
            // so this has to be `mul_add` to match byte for byte.
            accum = diff.mul_add(diff, accum);
        }
        variance = accum / num_weeks as f64;

        consistency_score = if mean > 0.0 {
            let cv = variance.sqrt() / mean;
            clamp_score((100.0 * (1.0 - cv)) as i64)
        } else {
            0
        };
    }

    HistoricalVelocity {
        label: label.to_string(),
        weekly_velocity: weekly,
        weeks_analyzed: num_weeks,
        moving_avg_4_week,
        moving_avg_8_week,
        peak_week,
        peak_velocity,
        trough_week,
        trough_velocity,
        variance,
        consistency_score,
    }
}

/// Go `ComputeAllHistoricalVelocity` (label_health.go:2255). A `BTreeMap`
/// because Go's `encoding/json` sorts map keys, so this keeps the serialized
/// key order identical to the oracle's.
pub fn compute_all_historical_velocity(
    issues: &[Issue],
    num_weeks: i64,
    now: jiff::Timestamp,
) -> BTreeMap<String, HistoricalVelocity> {
    let labels = extract_labels(issues);
    let mut result = BTreeMap::new();
    for label in &labels.labels {
        result.insert(
            label.clone(),
            compute_historical_velocity(issues, label, num_weeks, now),
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use bv_core::model::{Dependency, DependencyType, Issue};

    fn issue(id: &str, status: Status, labels: &[&str]) -> Issue {
        Issue {
            id: id.to_string(),
            content_hash: String::new(),
            title: id.to_string(),
            description: String::new(),
            design: String::new(),
            acceptance_criteria: String::new(),
            notes: String::new(),
            status,
            priority: 2,
            issue_type: "task".into(),
            assignee: String::new(),
            estimated_minutes: None,
            created_at: Some("2026-01-01T00:00:00Z".into()),
            updated_at: Some("2026-01-01T00:00:00Z".into()),
            due_date: None,
            defer_until: None,
            closed_at: None,
            external_ref: None,
            compaction_level: 0,
            compacted_at: None,
            compacted_at_commit: None,
            original_size: 0,
            labels: labels.iter().map(|s| s.to_string()).collect(),
            dependencies: vec![],
            comments: vec![],
            source_repo: String::new(),
        }
    }

    fn blocks(issue_id: &str, depends_on_id: &str) -> Dependency {
        Dependency {
            issue_id: issue_id.to_string(),
            depends_on_id: depends_on_id.to_string(),
            depends_on_legacy: String::new(),
            target_id_legacy: String::new(),
            r#type: DependencyType::Blocks,
            created_at: None,
            created_by: String::new(),
        }
    }

    #[test]
    fn extract_labels_counts_and_sorts() {
        let issues = vec![
            issue("A-1", Status::Open, &["backend", "urgent"]),
            issue("A-2", Status::Open, &["backend"]),
            issue("A-3", Status::Open, &[]),
        ];
        let r = extract_labels(&issues);
        assert_eq!(r.labels, vec!["backend".to_string(), "urgent".to_string()]);
        assert_eq!(r.unlabeled_count, 1);
        assert_eq!(r.top_labels[0], "backend"); // 2 issues > urgent's 1
    }

    #[test]
    fn cross_label_flow_counts_blocking_pairs_across_labels() {
        let mut blocker = issue("A-1", Status::Open, &["backend"]);
        let mut blocked = issue("A-2", Status::Open, &["frontend"]);
        blocked.dependencies.push(blocks("A-2", "A-1"));
        blocker.dependencies.clear();
        let issues = vec![blocker, blocked];
        let cfg = LabelHealthConfig::default();
        let flow = compute_cross_label_flow(&issues, &cfg);
        assert_eq!(flow.total_cross_label_deps, 1);
        assert_eq!(flow.dependencies.len(), 1);
        assert_eq!(flow.dependencies[0].from_label, "backend");
        assert_eq!(flow.dependencies[0].to_label, "frontend");
        assert_eq!(flow.bottleneck_labels, vec!["backend".to_string()]);
    }

    #[test]
    fn cross_label_flow_ignores_same_label_and_non_blocking() {
        let mut blocker = issue("A-1", Status::Open, &["backend"]);
        let mut blocked = issue("A-2", Status::Open, &["backend"]); // same label
        blocked.dependencies.push(blocks("A-2", "A-1"));
        blocker.dependencies.clear();
        let issues = vec![blocker, blocked];
        let flow = compute_cross_label_flow(&issues, &LabelHealthConfig::default());
        assert_eq!(
            flow.total_cross_label_deps, 0,
            "same-label deps must not count as cross-label"
        );
    }

    #[test]
    fn label_health_empty_label_is_critical_zero() {
        let issues = vec![issue("A-1", Status::Open, &["backend"])];
        let stats = compute_graph_stats(&issues);
        let h = compute_label_health_for_label(
            "nonexistent",
            &issues,
            &LabelHealthConfig::default(),
            jiff::Timestamp::now(),
            &stats,
        );
        assert_eq!(h.issue_count, 0);
        assert_eq!(h.health, 0);
        assert_eq!(h.health_level, "critical");
    }

    #[test]
    fn all_label_health_covers_every_extracted_label() {
        let issues = vec![
            issue("A-1", Status::Open, &["backend"]),
            issue("A-2", Status::Blocked, &["frontend"]),
        ];
        let result = compute_all_label_health(
            &issues,
            &LabelHealthConfig::default(),
            jiff::Timestamp::now(),
        );
        assert_eq!(result.total_labels, 2);
        assert_eq!(result.labels.len(), 2);
        assert_eq!(result.summaries.len(), 2);
        // healthy + warning + critical must partition all labels
        assert_eq!(
            result.healthy_count + result.warning_count + result.critical_count,
            2
        );
    }

    #[test]
    fn attention_scores_rank_by_score_desc_with_label_tiebreak() {
        let issues = vec![
            issue("A-1", Status::Open, &["quiet"]),
            issue("A-2", Status::Open, &["busy"]),
        ];
        let result = compute_label_attention_scores(
            &issues,
            &LabelHealthConfig::default(),
            jiff::Timestamp::now(),
        );
        assert_eq!(result.total_labels, 2);
        assert_eq!(result.labels.len(), 2);
        // ranks are contiguous starting at 1
        let mut ranks: Vec<i64> = result.labels.iter().map(|l| l.rank).collect();
        ranks.sort();
        assert_eq!(ranks, vec![1, 2]);
    }

    #[test]
    fn empty_issue_set_yields_empty_attention_result() {
        let result = compute_label_attention_scores(
            &[],
            &LabelHealthConfig::default(),
            jiff::Timestamp::now(),
        );
        assert_eq!(result.total_labels, 0);
        assert!(result.labels.is_empty());
    }

    /// A 3-node chain `A-3 -> A-2 -> A-1` (blocker -> blocked) where `A-1` and
    /// `A-2` carry `core` and `A-3` is pulled in only as a dependency, so the
    /// label's subgraph splits 2 core / 1 dependency.
    ///
    /// PageRank over that 3-node subgraph (d=0.85, uniform start, dangling
    /// mass of the sink `A-1` spread over all 3 nodes) has the closed form
    ///     A-1 = 1029/2169, A-2 = 740/2169, A-3 = 400/2169  (sums to 1)
    /// so the core-only sum the attention score uses is `1769/2169`.
    const CHAIN_CORE_PAGERANK_SUM: f64 = 1769.0 / 2169.0;

    fn core_chain_fixture() -> Vec<Issue> {
        let mut a1 = issue("A-1", Status::Open, &["core"]);
        a1.dependencies.push(blocks("A-1", "A-2"));
        let mut a2 = issue("A-2", Status::Open, &["core"]);
        a2.dependencies.push(blocks("A-2", "A-3"));
        let a3 = issue("A-3", Status::Open, &[]);
        vec![a1, a2, a3]
    }

    #[test]
    fn label_subgraph_splits_core_from_dependencies() {
        let issues = core_chain_fixture();
        let rev = ReverseDeps::build(&issues);
        let sg = compute_label_subgraph("core", &issues, &rev);

        // `A-3` has no label but is a blocker of core `A-2`, so it joins the
        // subgraph as a dependency without becoming a core issue.
        assert_eq!(sg.core_issues, vec!["A-1".to_string(), "A-2".to_string()]);
        let deps: Vec<&String> = sg
            .all_issues
            .iter()
            .filter(|id| !sg.core_issues.contains(id))
            .collect();
        assert_eq!(deps, vec![&"A-3".to_string()]);
        assert_eq!(
            sg.all_issues,
            vec!["A-1".to_string(), "A-2".to_string(), "A-3".to_string()]
        );
        // Blocking edges, blocker -> blocked.
        let mut edges: Vec<(&str, Vec<&str>)> = sg
            .adjacency
            .iter()
            .map(|(k, v)| (k.as_str(), v.iter().map(String::as_str).collect()))
            .collect();
        edges.sort();
        assert_eq!(
            edges,
            vec![("A-2", vec!["A-1"]), ("A-3", vec!["A-2"])],
            "adjacency must be the two chain edges only"
        );
    }

    #[test]
    fn label_pagerank_sum_is_subgraph_scoped_not_global() {
        let issues = core_chain_fixture();
        let rev = ReverseDeps::build(&issues);
        let sg = compute_label_subgraph("core", &issues, &rev);

        let sum = compute_label_pagerank_core_sum(&sg);
        assert!(
            (sum - CHAIN_CORE_PAGERANK_SUM).abs() < 1e-5,
            "subgraph-scoped PageRankSum was {sum}, want {CHAIN_CORE_PAGERANK_SUM}"
        );
        // This expectation is discriminating: dropping the edges entirely
        // would give 2/3, and inverting them (blocked -> blocker) would give
        // 1140/2169, so both a missing and a reversed adjacency fail here.
        assert!(
            (2.0 / 3.0 - CHAIN_CORE_PAGERANK_SUM).abs() > 1e-3
                && (1140.0 / 2169.0 - CHAIN_CORE_PAGERANK_SUM).abs() > 1e-3,
            "the expected value must not be reachable by a wrong edge set"
        );

        // The old approximation summed the *global* PageRank of the two core
        // issues, which is a different (much smaller) number — assert we no
        // longer take that path.
        let stats = compute_graph_stats(&issues);
        let global: f64 = ["A-1", "A-2"]
            .iter()
            .map(|id| stats.pagerank.get(*id).copied().unwrap_or(0.0))
            .sum();
        assert!(
            (global - CHAIN_CORE_PAGERANK_SUM).abs() > 1e-3,
            "fixture is degenerate: global PageRank sum {global} collides with the subgraph sum"
        );

        // End to end: the attention score carries the subgraph sum through.
        let result = compute_label_attention_scores(
            &issues,
            &LabelHealthConfig::default(),
            jiff::Timestamp::now(),
        );
        let core = result
            .labels
            .iter()
            .find(|l| l.label == "core")
            .expect("label `core` must be scored");
        assert!(
            (core.pagerank_sum - CHAIN_CORE_PAGERANK_SUM).abs() < 1e-5,
            "attention pagerank_sum was {}, want {CHAIN_CORE_PAGERANK_SUM}",
            core.pagerank_sum
        );
        // `open_count` counts label-carrying issues only (A-1, A-2) — the
        // pulled-in dependency A-3 is in the graph, not in the label's count.
        assert_eq!(core.open_count, 2);
    }

    #[test]
    fn non_blocking_dependency_still_expands_subgraph_but_adds_no_edge() {
        // Go expands the dependency set for every dependency type but only
        // builds adjacency from blocking ones; this is the shape the real
        // corpus has (a parent-child parent contributing no graph edge).
        let mut a1 = issue("A-1", Status::Open, &["core"]);
        a1.dependencies.push(Dependency {
            issue_id: "A-1".to_string(),
            depends_on_id: "A-2".to_string(),
            depends_on_legacy: String::new(),
            target_id_legacy: String::new(),
            r#type: DependencyType::ParentChild,
            created_at: None,
            created_by: String::new(),
        });
        let issues = vec![a1, issue("A-2", Status::Open, &[])];

        let rev = ReverseDeps::build(&issues);
        let sg = compute_label_subgraph("core", &issues, &rev);
        assert_eq!(sg.all_issues, vec!["A-1".to_string(), "A-2".to_string()]);
        assert!(sg.adjacency.is_empty(), "parent-child is not a graph edge");

        // Two edgeless nodes split their mass evenly, so the single core issue
        // scores 0.5 — the value the Go oracle reports for the real corpus.
        let sum = compute_label_pagerank_core_sum(&sg);
        assert!((sum - 0.5).abs() < 1e-5, "got {sum}, want 0.5");
    }

    // -----------------------------------------------------------------
    // Historical velocity
    // -----------------------------------------------------------------

    /// Monday 2025-12-15 12:00 UTC — the anchor Go's own velocity tests use, so
    /// week 0 is Dec 15-21, week 1 is Dec 8-14, week 2 is Dec 1-7 and week 3
    /// is Nov 24-30.
    fn now_monday() -> jiff::Timestamp {
        "2025-12-15T12:00:00Z".parse().unwrap()
    }

    fn ts(s: &str) -> jiff::Timestamp {
        s.parse().unwrap()
    }

    fn closed_on(id: &str, status: Status, labels: &[&str], closed_at: &str) -> Issue {
        let mut i = issue(id, status, labels);
        i.closed_at = Some(closed_at.to_string());
        i
    }

    #[test]
    fn historical_velocity_buckets_closures_into_weeks() {
        // Go: TestComputeHistoricalVelocity_BasicCounting.
        let issues = vec![
            closed_on("bv-1", Status::Closed, &["api"], "2025-12-16T10:00:00Z"),
            closed_on("bv-2", Status::Closed, &["api"], "2025-12-16T10:00:00Z"),
            closed_on("bv-3", Status::Closed, &["api"], "2025-12-10T10:00:00Z"),
            closed_on("bv-4", Status::Closed, &["api"], "2025-12-03T10:00:00Z"),
            closed_on("bv-5", Status::Closed, &["api"], "2025-12-03T10:00:00Z"),
            closed_on("bv-6", Status::Closed, &["api"], "2025-12-03T10:00:00Z"),
            closed_on("bv-7", Status::Closed, &["api"], "2025-11-26T10:00:00Z"),
            closed_on("bv-8", Status::Closed, &["ui"], "2025-12-16T10:00:00Z"),
        ];

        let r = compute_historical_velocity(&issues, "api", 4, now_monday());
        assert_eq!(r.label, "api");
        assert_eq!(r.weeks_analyzed, 4);
        assert_eq!(r.weekly_velocity[0].closed, 2); // Dec 15-21
        assert_eq!(r.weekly_velocity[1].closed, 1); // Dec 8-14
        assert_eq!(r.weekly_velocity[2].closed, 3); // Dec 1-7
        assert_eq!(r.weekly_velocity[3].closed, 1); // Nov 24-30

        // Week boundaries are Monday midnight to the following Monday, and
        // weeks_ago counts back from the current week.
        assert_eq!(r.weekly_velocity[0].week_start, "2025-12-15T00:00:00Z");
        assert_eq!(r.weekly_velocity[0].week_end, "2025-12-22T00:00:00Z");
        assert_eq!(r.weekly_velocity[3].weeks_ago, 3);
        assert_eq!(r.weekly_velocity[3].week_start, "2025-11-24T00:00:00Z");

        // Cumulative accumulates oldest to newest, so it is smallest on the
        // oldest bucket and largest on the newest.
        assert_eq!(r.weekly_velocity[3].cumulative, 1); // Nov 24-30
        assert_eq!(r.weekly_velocity[2].cumulative, 4); // + Dec 1-7
        assert_eq!(r.weekly_velocity[1].cumulative, 5); // + Dec 8-14
        assert_eq!(r.weekly_velocity[0].cumulative, 7); // + Dec 15-21
    }

    #[test]
    fn historical_velocity_ignores_open_issues_that_carry_a_closed_at() {
        // Go: TestComputeHistoricalVelocity_IgnoresNonClosedWithClosedAt.
        let issues = vec![
            closed_on(
                "open-closedat",
                Status::Open,
                &["api"],
                "2025-12-15T14:00:00Z",
            ),
            closed_on("closed", Status::Closed, &["api"], "2025-12-15T14:00:00Z"),
        ];
        let h = compute_historical_velocity(&issues, "api", 1, now_monday());
        assert_eq!(h.weekly_velocity.len(), 1);
        assert_eq!(h.weekly_velocity[0].closed, 1);
        assert_eq!(h.weekly_velocity[0].issue_ids, vec!["closed".to_string()]);
    }

    #[test]
    fn historical_velocity_peak_and_trough_skip_empty_weeks() {
        // Go: TestComputeHistoricalVelocity_PeakAndTrough. Weeks 1 and 3 are the
        // peak/trough, and the zeroed-out weeks never win the trough.
        let mk = |id: &str, at: &str| closed_on(id, Status::Closed, &["test"], at);
        let mut issues = vec![
            mk("w0-1", "2025-12-16T10:00:00Z"),
            mk("w0-2", "2025-12-16T10:00:00Z"),
            mk("w1-1", "2025-12-10T10:00:00Z"),
            mk("w1-2", "2025-12-10T10:00:00Z"),
            mk("w1-3", "2025-12-10T10:00:00Z"),
            mk("w1-4", "2025-12-10T10:00:00Z"),
            mk("w1-5", "2025-12-10T10:00:00Z"),
            mk("w2-1", "2025-12-03T10:00:00Z"),
            mk("w3-1", "2025-11-26T10:00:00Z"),
            mk("w3-2", "2025-11-26T10:00:00Z"),
            mk("w3-3", "2025-11-26T10:00:00Z"),
        ];
        issues.shrink_to_fit();

        let r = compute_historical_velocity(&issues, "test", 4, now_monday());
        assert_eq!((r.peak_week, r.peak_velocity), (1, 5));
        assert_eq!((r.trough_week, r.trough_velocity), (2, 1));
    }

    #[test]
    fn historical_velocity_trough_stays_zero_when_nothing_closed() {
        // Go's `hasNonZero` guard: a window with no closures leaves both trough
        // fields at zero rather than reporting MaxInt or a spurious 0-week.
        let r = compute_historical_velocity(
            &[issue("bv-1", Status::Open, &["other"])],
            "nonexistent",
            4,
            now_monday(),
        );
        assert_eq!(r.label, "nonexistent");
        assert_eq!(r.peak_velocity, 0);
        assert_eq!((r.trough_week, r.trough_velocity), (0, 0));
        assert!(r.weekly_velocity.iter().all(|w| w.closed == 0));
        assert_eq!(r.consistency_score, 0, "no closures means no score");
    }

    #[test]
    fn historical_velocity_moving_averages_need_enough_weeks() {
        // Go: TestComputeHistoricalVelocity_MovingAverages. Week w closes w+1
        // issues, so weeks 0..=3 are 1,2,3,4 and weeks 0..=7 are 1..8.
        let mut issues = Vec::new();
        for w in 0..8i64 {
            let at = add_days(current_week_start(now_monday()), -7 * w + 2);
            for i in 0..=w {
                issues.push(closed_on(
                    &format!("w{w}-{i}"),
                    Status::Closed,
                    &["avg"],
                    &go_time_string(Some(at)),
                ));
            }
        }

        let r = compute_historical_velocity(&issues, "avg", 8, now_monday());
        assert_eq!(r.moving_avg_4_week, 2.5); // (1+2+3+4)/4
        assert_eq!(r.moving_avg_8_week, 4.5); // (1+..+8)/8

        // Three weeks is below both thresholds, so both averages stay 0 (and
        // the omitempty on moving_avg_8_week drops it from the JSON).
        let short = compute_historical_velocity(&issues, "avg", 3, now_monday());
        assert_eq!(short.moving_avg_4_week, 0.0);
        assert_eq!(short.moving_avg_8_week, 0.0);
        let json = serde_json::to_string(&short).unwrap();
        assert!(!json.contains("moving_avg_8_week"), "got {json}");
        assert!(json.contains("\"moving_avg_4_week\":0"), "got {json}");

        // Four weeks is exactly the 4-week threshold.
        let four = compute_historical_velocity(&issues, "avg", 4, now_monday());
        assert_eq!(four.moving_avg_4_week, 2.5);
    }

    #[test]
    fn historical_velocity_defaults_to_eight_weeks() {
        let r = compute_historical_velocity(&[], "api", 0, now_monday());
        assert_eq!(r.weeks_analyzed, 8);
        assert_eq!(r.weekly_velocity.len(), 8);
        let neg = compute_historical_velocity(&[], "api", -3, now_monday());
        assert_eq!(neg.weeks_analyzed, 8);
    }

    #[test]
    fn historical_velocity_trend_needs_four_weeks() {
        // Under the 4-week minimum there is no ratio to compare, so every
        // window short of 4 is "insufficient_data" — including exactly 3.
        for n in [0i64, 1, 2, 3] {
            let hv = HistoricalVelocity {
                weekly_velocity: (0..n)
                    .map(|i| WeeklySnapshot {
                        closed: 3,
                        weeks_ago: i,
                        ..Default::default()
                    })
                    .collect(),
                weeks_analyzed: n,
                ..Default::default()
            };
            assert_eq!(hv.get_velocity_trend(), "insufficient_data", "n={n}");
        }
    }

    #[test]
    fn historical_velocity_trend_boundary_is_four_weeks() {
        // Exactly 4 is the first length that classifies: 2 recent vs 2 older
        // weeks, so 4/1 is accelerating and 1/4 is decelerating. A perfectly
        // even 2/2 split is stable (ratio 1.0 sits inside the 0.7..1.3 band).
        let build = |recent: [i64; 2], older: [i64; 2]| {
            let mut weekly_velocity: Vec<WeeklySnapshot> = recent
                .iter()
                .chain(older.iter())
                .enumerate()
                .map(|(i, c)| WeeklySnapshot {
                    closed: *c,
                    weeks_ago: i as i64,
                    ..Default::default()
                })
                .collect();
            weekly_velocity.shrink_to_fit();
            HistoricalVelocity {
                weekly_velocity,
                weeks_analyzed: 4,
                ..Default::default()
            }
        };
        assert_eq!(build([4, 0], [1, 0]).get_velocity_trend(), "accelerating");
        assert_eq!(build([1, 0], [4, 0]).get_velocity_trend(), "decelerating");
        assert_eq!(build([2, 0], [2, 0]).get_velocity_trend(), "stable");
        // 1.3 / 0.7 are strict bounds, so an exact 1.3 stays stable.
        assert_eq!(build([13, 0], [10, 0]).get_velocity_trend(), "stable");
        assert_eq!(build([10, 0], [13, 0]).get_velocity_trend(), "stable");
        // Zero older with some recent is accelerating; zero on both is stable.
        assert_eq!(build([1, 1], [0, 0]).get_velocity_trend(), "accelerating");
        assert_eq!(build([0, 0], [0, 0]).get_velocity_trend(), "stable");
    }

    #[test]
    fn historical_velocity_trend_rising_falling_and_flat() {
        // Go: TestHistoricalVelocity_GetVelocityTrend. weekly[0] is the most
        // recent week, so these are week-over-week sequences read backwards.
        let build = |weekly: &[i64]| {
            let hv = HistoricalVelocity {
                weekly_velocity: weekly
                    .iter()
                    .enumerate()
                    .map(|(i, c)| WeeklySnapshot {
                        closed: *c,
                        weeks_ago: i as i64,
                        ..Default::default()
                    })
                    .collect(),
                weeks_analyzed: weekly.len() as i64,
                ..Default::default()
            };
            let mut sum = 0.0f64;
            for snap in &hv.weekly_velocity {
                sum += snap.closed as f64;
            }
            let mean = sum / weekly.len() as f64;
            let mut variance = 0.0f64;
            for snap in &hv.weekly_velocity {
                let d = snap.closed as f64 - mean;
                variance += d * d;
            }
            let hv = HistoricalVelocity {
                variance: variance / weekly.len() as f64,
                ..hv
            };
            hv.get_velocity_trend()
        };

        // Rising: 1,1,2,2,4,4,5,5 newest-first.
        assert_eq!(build(&[5, 4, 4, 2, 2, 1, 1, 0]), "accelerating");
        assert_eq!(build(&[5, 4, 4, 3, 2, 2, 1, 1]), "accelerating");
        // Falling: the mirror image.
        assert_eq!(build(&[1, 1, 2, 2, 4, 4, 5, 5]), "decelerating");
        // Flat.
        assert_eq!(build(&[3, 3, 3, 3, 3, 3, 3, 3]), "stable");
    }

    #[test]
    fn historical_velocity_trend_erratic_when_variance_dominates() {
        // A ratio inside the stable band but a variance over half the peak
        // week is "erratic" rather than "stable".
        let hv = HistoricalVelocity {
            weekly_velocity: vec![
                WeeklySnapshot {
                    closed: 5,
                    weeks_ago: 0,
                    ..Default::default()
                },
                WeeklySnapshot {
                    closed: 3,
                    weeks_ago: 1,
                    ..Default::default()
                },
                WeeklySnapshot {
                    closed: 5,
                    weeks_ago: 2,
                    ..Default::default()
                },
                WeeklySnapshot {
                    closed: 3,
                    weeks_ago: 3,
                    ..Default::default()
                },
            ],
            weeks_analyzed: 4,
            // peak 5 -> threshold 2.5; a variance above it is erratic.
            variance: 1.0,
            peak_velocity: 5,
            ..Default::default()
        };
        assert_eq!(hv.get_velocity_trend(), "stable");
        let spiky = HistoricalVelocity {
            variance: 2.6,
            ..hv
        };
        assert_eq!(spiky.get_velocity_trend(), "erratic");
    }

    #[test]
    fn historical_weekly_average_matches_go() {
        // Go: TestHistoricalVelocity_GetWeeklyAverage.
        let hv = HistoricalVelocity {
            weeks_analyzed: 4,
            weekly_velocity: [2, 4, 6, 8]
                .iter()
                .map(|c| WeeklySnapshot {
                    closed: *c,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        assert_eq!(hv.get_weekly_average(), 5.0);
        let empty = HistoricalVelocity::default();
        assert_eq!(empty.get_weekly_average(), 0.0);
    }

    #[test]
    fn historical_velocity_consistency_scores_pure_and_lumpy() {
        // Zero variance with a positive mean is a perfect 100; a lumpy window
        // (one week carrying everything) drives the CV up and the score down.
        let mk = |weekly: &[i64]| {
            let issues: Vec<Issue> = weekly
                .iter()
                .enumerate()
                .flat_map(|(w, count)| {
                    let at = add_days(current_week_start(now_monday()), -7 * w as i64 + 2);
                    (0..*count).map(move |i| {
                        closed_on(
                            &format!("w{w}-{i}"),
                            Status::Closed,
                            &["api"],
                            &go_time_string(Some(at)),
                        )
                    })
                })
                .collect();
            compute_historical_velocity(&issues, "api", weekly.len() as i64, now_monday())
        };

        let even = mk(&[2, 2, 2, 2]);
        assert_eq!(even.variance, 0.0);
        assert_eq!(even.consistency_score, 100);

        let lumpy = mk(&[8, 0, 0, 0]);
        assert_eq!(lumpy.variance, 12.0); // population variance of 8,0,0,0
        assert_eq!(lumpy.consistency_score, 0); // CV = 1.0 -> 100*(1-1)
    }

    #[test]
    fn historical_velocity_variance_matches_the_go_oracle_bit_for_bit() {
        // Recorded from the Go v0.25.0 oracle (beads_viewer @ 18afafa) over a
        // fixed issue set: 16 closures spread across the labels api/ui/backend
        // and weeks Nov 24 - Dec 21 2025, anchored at 2025-12-15T12:00:00Z.
        //
        // These four windows are the ones where the FMA in `variance += diff *
        // diff` changes the answer. A plain `accum + diff * diff` gives
        // 1.5555555555555554 / 0.22222222222222224 / 0.24000000000000005 /
        // 1.8400000000000003, so a reversion to the unfused form fails here
        // rather than silently drifting a ULP.
        let mk = |weekly: &[i64]| {
            let issues: Vec<Issue> = weekly
                .iter()
                .enumerate()
                .flat_map(|(w, count)| {
                    let at = add_days(current_week_start(now_monday()), -7 * w as i64 + 2);
                    (0..*count).map(move |i| {
                        closed_on(
                            &format!("w{w}-{i}"),
                            Status::Closed,
                            &["api"],
                            &go_time_string(Some(at)),
                        )
                    })
                })
                .collect();
            compute_historical_velocity(&issues, "api", weekly.len() as i64, now_monday())
        };

        for (weekly, variance, consistency) in [
            (&[4i64, 2, 1][..], 1.5555555555555556f64, 46i64),
            (&[1, 1, 0][..], 0.2222222222222222, 29),
            (&[1, 1, 0, 0, 0][..], 0.24, 0),
            (&[4, 2, 1, 1, 0][..], 1.8400000000000003, 15),
        ] {
            let r = mk(weekly);
            assert_eq!(
                r.variance.to_bits(),
                variance.to_bits(),
                "variance for {weekly:?}: got {variance:e} want oracle"
            );
            assert_eq!(r.consistency_score, consistency, "score for {weekly:?}");
        }
    }

    #[test]
    fn historical_velocity_ignores_closures_outside_the_window() {
        // A closure older than the oldest bucket is dropped, not clamped into it.
        let issues = vec![
            closed_on("in", Status::Closed, &["api"], "2025-12-16T10:00:00Z"),
            closed_on("out", Status::Closed, &["api"], "2025-11-01T10:00:00Z"),
        ];
        let r = compute_historical_velocity(&issues, "api", 4, now_monday());
        assert_eq!(r.weekly_velocity[0].closed, 1);
        assert_eq!(r.weekly_velocity.iter().map(|w| w.closed).sum::<i64>(), 1);
    }

    #[test]
    fn historical_velocity_week_alignment_from_sunday() {
        // `now` on a Sunday still belongs to the week that started the previous
        // Monday — Go maps Sunday to 7 and steps back six days, not seven.
        let sunday = ts("2025-12-21T09:00:00Z"); // Sunday
        assert_eq!(go_weekday(sunday), 0);
        let r = compute_historical_velocity(&[], "api", 2, sunday);
        assert_eq!(r.weekly_velocity[0].week_start, "2025-12-15T00:00:00Z");
        assert_eq!(r.weekly_velocity[0].week_end, "2025-12-22T00:00:00Z");

        // A Wednesday steps back two days.
        let wednesday = ts("2025-12-17T09:00:00Z");
        assert_eq!(go_weekday(wednesday), 3);
        let r = compute_historical_velocity(&[], "api", 1, wednesday);
        assert_eq!(r.weekly_velocity[0].week_start, "2025-12-15T00:00:00Z");
    }

    #[test]
    fn all_historical_velocity_covers_every_label() {
        // Go: TestComputeAllHistoricalVelocity.
        let issues = vec![
            closed_on("bv-1", Status::Closed, &["api"], "2025-12-16T10:00:00Z"),
            closed_on("bv-2", Status::Closed, &["ui"], "2025-12-16T10:00:00Z"),
            closed_on(
                "bv-3",
                Status::Closed,
                &["api", "ui"],
                "2025-12-16T10:00:00Z",
            ),
            // An empty label is skipped by extract_labels and so never appears.
            closed_on("bv-4", Status::Closed, &[""], "2025-12-16T10:00:00Z"),
        ];
        let all = compute_all_historical_velocity(&issues, 4, now_monday());
        assert_eq!(all.len(), 2);
        assert_eq!(all["api"].weekly_velocity[0].closed, 2);
        assert_eq!(all["ui"].weekly_velocity[0].closed, 2);
        assert_eq!(all["api"].weeks_analyzed, 4);
    }
}
