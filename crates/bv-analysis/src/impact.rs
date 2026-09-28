//! Composite impact scoring — port of Go `pkg/analysis/priority.go`
//! ComputeImpactScoresFromStats + component functions.

use bv_core::model::{Issue, Status};
use bv_graph_core::DiGraph;
use serde::Serialize;
use std::collections::BTreeMap;

pub const URGENCY_LABELS: [&str; 5] = ["urgent", "critical", "blocker", "hotfix", "asap"];
/// Cap for critical-path-depth normalization.
pub const MAX_CRITICAL_PATH_DEPTH: f64 = 10.0;
/// Half-life for urgency decay.
pub const URGENCY_DECAY_DAYS: f64 = 7.0;
/// Default estimated minutes when no issues have estimates (Go parity).
pub const DEFAULT_ESTIMATED_MINUTES: i64 = 60;

fn normalize(v: f64, max: f64) -> f64 {
    if max == 0.0 {
        0.0
    } else {
        v / max
    }
}

/// Go: `computeStaleness` — 30-day cap, unknown = 0.5.
pub fn compute_staleness(updated_at: Option<&str>, now: &jiff::Timestamp) -> f64 {
    let Some(raw) = updated_at else {
        return 0.5;
    };
    match raw.parse::<jiff::Timestamp>() {
        Ok(t) => {
            let secs = (*now - t).total(jiff::Unit::Second).unwrap_or(0.0);
            let days = secs / 86400.0;
            (days / 30.0).clamp(0.0, 1.0)
        }
        Err(_) => 0.5,
    }
}

/// Go: `computePriorityBoost` — P0=1.0 .. P4+=0.
pub fn compute_priority_boost(priority: i32) -> f64 {
    match priority {
        0 => 1.0,
        1 => 0.75,
        2 => 0.5,
        3 => 0.25,
        _ => 0.0,
    }
}

/// Go: `computeTimeToImpact` — depth .7 + time-efficiency .3.
pub fn compute_time_to_impact(
    critical_path_depth: f64,
    estimated_minutes: Option<i64>,
    median_minutes: i64,
) -> f64 {
    let effective = match estimated_minutes {
        Some(m) if m > 0 => m as f64,
        _ => median_minutes as f64,
    };
    let depth_norm = (critical_path_depth / MAX_CRITICAL_PATH_DEPTH).min(1.0);
    const MAX_MINUTES: f64 = 480.0;
    let time_factor = (1.0 - (effective / MAX_MINUTES)).clamp(0.0, 1.0);
    depth_norm * 0.7 + time_factor * 0.3
}

/// Go: `computeUrgency` — label weights + exponential decay (half-life 7d).
pub fn compute_urgency(labels: &[String], created_at: Option<&str>, now: &jiff::Timestamp) -> f64 {
    let mut score = 0.0f64;
    'outer: for label in labels {
        let lower = label.to_lowercase();
        for urgent in URGENCY_LABELS {
            if lower.contains(urgent) {
                score += match urgent {
                    "critical" | "blocker" => 1.0,
                    "urgent" | "hotfix" => 0.8,
                    "asap" => 0.6,
                    _ => 0.0,
                };
                break 'outer;
            }
        }
    }
    if let Some(raw) = created_at {
        if let Ok(created) = raw.parse::<jiff::Timestamp>() {
            let secs = (*now - created).total(jiff::Unit::Second).unwrap_or(0.0);
            let days = secs / 86400.0;
            if days > 0.0 {
                // 0.5 * (1 - e^(-days/halfLife))
                score += 0.5 * (1.0 - (-(days / URGENCY_DECAY_DAYS)).exp());
            }
        }
    }
    score.min(1.0)
}

/// Go `computeTimeToImpact` explanation (pkg/analysis/priority.go:512).
pub fn time_to_impact_explanation(
    critical_path_depth: f64,
    estimated_minutes: Option<i64>,
    median_minutes: i64,
) -> String {
    let (effective, source) = match estimated_minutes {
        Some(m) if m > 0 => (m, "explicit"),
        _ => (median_minutes, "median"),
    };
    if critical_path_depth >= 3.0 {
        format!(
            "Deep in critical path (depth {:.0}), {} estimate {}m",
            critical_path_depth, source, effective
        )
    } else if critical_path_depth >= 1.0 {
        format!(
            "On dependency chain (depth {:.0}), {} estimate {}m",
            critical_path_depth, source, effective
        )
    } else {
        format!("Leaf node, {} estimate {}m", source, effective)
    }
}

/// Go `computeUrgency` explanation (pkg/analysis/priority.go:590).
///
/// `score` is the same capped urgency score `compute_urgency` returned — Go
/// builds the explanation from it in the same function, and its
/// `else if score > 0.1` arm is what emits "moderate time pressure" for an
/// aging issue that has no urgent label and so accumulated no reasons.
pub fn urgency_explanation(
    labels: &[String],
    created_at: Option<&str>,
    now: &jiff::Timestamp,
    score: f64,
) -> String {
    let mut reasons: Vec<String> = Vec::new();
    let mut urgent_label = String::new();
    'outer: for label in labels {
        let lower = label.to_lowercase();
        for urgent in URGENCY_LABELS {
            if lower.contains(urgent) {
                urgent_label = label.clone();
                break 'outer;
            }
        }
    }
    if !urgent_label.is_empty() {
        reasons.push(format!("has '{urgent_label}' label"));
    }
    let Some(raw) = created_at else {
        return reasons.join(", ");
    };
    let Ok(created) = raw.parse::<jiff::Timestamp>() else {
        return reasons.join(", ");
    };
    let days = (*now - created).total(jiff::Unit::Second).unwrap_or(0.0) / 86400.0;
    if days >= 14.0 {
        reasons.push(format!("aging ({days:.0} days)"));
    }
    if reasons.is_empty() {
        // Go priority.go:593-595 — an issue past the decay threshold with no
        // urgent label has score but no reasons, and Go still explains it.
        return if score > 0.1 {
            "moderate time pressure".to_string()
        } else {
            String::new()
        };
    }
    reasons.join(", ")
}

/// Go `generateRiskExplanation` (pkg/analysis/risk.go:117).
pub fn risk_explanation(r: &RiskSignals) -> String {
    if r.composite_risk < 0.2 {
        return "Low risk - stable dependency structure".to_string();
    }
    let mut factors: Vec<&str> = Vec::new();
    if r.fan_variance > 0.5 {
        factors.push("high dependency variance");
    }
    if r.activity_churn > 0.6 {
        factors.push("high activity churn");
    }
    if r.cross_repo_risk > 0.3 {
        factors.push("cross-repo dependencies");
    }
    if r.status_risk > 0.5 {
        factors.push("status indicates potential blockers");
    }
    if factors.is_empty() {
        return "Moderate risk".to_string();
    }
    // Go `joinRiskFactors`: "a", "a and b", "a, b, and c".
    let joined = match factors.len() {
        1 => factors[0].to_string(),
        2 => format!("{} and {}", factors[0], factors[1]),
        n => format!("{}, and {}", factors[..n - 1].join(", "), factors[n - 1]),
    };
    format!("Risk factors: {joined}")
}

/// Risk signals composite — port of Go `pkg/analysis/risk.go` weights.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct RiskSignals {
    pub fan_variance: f64,
    pub activity_churn: f64,
    pub cross_repo_risk: f64,
    pub status_risk: f64,
    pub composite_risk: f64,
}

/// Compute fan variance: coefficient of variation of in-degrees of blocking
/// dependencies. Requires >= 2 blocking deps for non-zero result (Go parity).
fn compute_fan_variance(issue: &Issue, g: &DiGraph) -> f64 {
    let mut degrees: Vec<f64> = Vec::new();
    for dep in &issue.dependencies {
        if !dep.r#type.is_blocking() {
            continue;
        }
        let neighbor_id = dep.effective_depends_on();
        if neighbor_id.is_empty() {
            continue;
        }
        if let Some(nid) = g.node_idx(neighbor_id) {
            degrees.push(g.in_degree(nid) as f64);
        }
    }
    if degrees.len() < 2 {
        return 0.0;
    }
    let mean = degrees.iter().sum::<f64>() / degrees.len() as f64;
    if mean == 0.0 {
        return 0.0;
    }
    let variance = degrees.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / degrees.len() as f64;
    let std_dev = variance.sqrt();
    let cv = std_dev / mean;
    // Normalize: CV > 2 is considered high variance
    (cv / 2.0).min(1.0)
}

/// Compute activity churn: comment frequency + update recency (Go parity:
/// `computeActivityChurn` in risk.go:141).
fn compute_activity_churn(issue: &Issue, now: &jiff::Timestamp) -> f64 {
    let created = match issue.created_at.as_deref() {
        Some(raw) => match raw.parse::<jiff::Timestamp>() {
            Ok(t) => t,
            Err(_) => return 0.0,
        },
        None => return 0.0,
    };

    let age_secs = (*now - created).total(jiff::Unit::Second).unwrap_or(0.0);
    let age_days = (age_secs / 86400.0).max(1.0); // minimum 1 day

    // Comment frequency: comments per day (normalized around 1)
    let comment_count = issue.comments.len() as f64;
    let comments_per_day = comment_count / age_days;

    // Update recency: how much of the issue's lifetime has seen updates
    let update_recency = if let Some(updated_raw) = issue.updated_at.as_deref() {
        if let Ok(updated) = updated_raw.parse::<jiff::Timestamp>() {
            let update_span =
                (updated - created).total(jiff::Unit::Second).unwrap_or(0.0) / 86400.0;
            if update_span > 0.0 && age_days > 1.0 {
                update_span / age_days
            } else {
                0.0
            }
        } else {
            0.0
        }
    } else {
        0.0
    };

    let churn = comments_per_day * 0.6 + update_recency * 0.4;
    churn.min(1.0)
}

/// Compute status risk: blocked/in_progress/open risk signals (Go parity:
/// `computeStatusRisk` in risk.go:216).
fn compute_status_risk(issue: &Issue, now: &jiff::Timestamp) -> f64 {
    match issue.status {
        Status::Closed | Status::Tombstone => 0.0,
        Status::Blocked => {
            if let Some(raw) = issue.updated_at.as_deref() {
                if let Ok(t) = raw.parse::<jiff::Timestamp>() {
                    let days = (*now - t).total(jiff::Unit::Second).unwrap_or(0.0) / 86400.0;
                    if days > 7.0 {
                        return 0.9;
                    }
                }
            }
            0.7
        }
        Status::InProgress => {
            if let Some(raw) = issue.updated_at.as_deref() {
                if let Ok(t) = raw.parse::<jiff::Timestamp>() {
                    let days = (*now - t).total(jiff::Unit::Second).unwrap_or(0.0) / 86400.0;
                    if days > 14.0 {
                        return 0.8;
                    } else if days > 7.0 {
                        return 0.4;
                    } else {
                        return 0.1;
                    }
                }
            }
            0.3
        }
        Status::Open => {
            if let Some(raw) = issue.created_at.as_deref() {
                if let Ok(t) = raw.parse::<jiff::Timestamp>() {
                    let days = (*now - t).total(jiff::Unit::Second).unwrap_or(0.0) / 86400.0;
                    if days > 30.0 {
                        return 0.3;
                    }
                }
            }
            0.1
        }
        _ => 0.0,
    }
}

/// Go `computeCrossRepoRisk` (pkg/analysis/risk.go) — the share of an issue's
/// *blocking* dependencies that point at a bead in a different repository.
///
/// Returns 0 when the issue has no `source_repo` or no dependencies at all, and
/// 0 when none of its blocking deps resolve to a different repo. A previous
/// stub returned a flat 0.2 for any non-empty `source_repo`, which inflated
/// `composite_risk` (and therefore every score) by 0.04 on dependency-free
/// issues — the single cause of a uniform +0.0034 score delta against Go.
fn compute_cross_repo_risk(issue: &Issue, issues: &[Issue]) -> f64 {
    if issue.source_repo.is_empty() || issue.dependencies.is_empty() {
        return 0.0;
    }
    let this_repo = &issue.source_repo;
    let mut cross_repo_count = 0usize;
    let mut total_blocking = 0usize;
    for dep in &issue.dependencies {
        if !dep.r#type.is_blocking() {
            continue;
        }
        total_blocking += 1;
        let target = dep.effective_depends_on();
        // Go looks the dependency up in the full issue map and only counts it
        // when the target exists, has a repo, and differs from this one.
        if let Some(dep_issue) = issues.iter().find(|i| i.id == target) {
            if !dep_issue.source_repo.is_empty() && &dep_issue.source_repo != this_repo {
                cross_repo_count += 1;
            }
        }
    }
    if total_blocking == 0 {
        return 0.0;
    }
    cross_repo_count as f64 / total_blocking as f64
}

/// Simplified risk computation using graph-local signals.
/// Full churn/cross-repo ports land with correlation integration; the
/// weights and composition match Go DefaultRiskWeights exactly.
pub fn compute_risk_signals(
    issue: &Issue,
    g: &DiGraph,
    issues: &[Issue],
    now: &jiff::Timestamp,
) -> RiskSignals {
    let fan_variance = compute_fan_variance(issue, g);
    let churn = compute_activity_churn(issue, now);
    let cross_repo = compute_cross_repo_risk(issue, issues);
    let status_risk = compute_status_risk(issue, now);
    // Go writes this as one expression (risk.go:85-88):
    //   FanVariance*w.FanVariance + ActivityChurn*w.ActivityChurn
    //     + CrossRepoRisk*w.CrossRepoRisk + StatusRisk*w.StatusRisk
    // and its compiler contracts each `mul + add` into a single FMA. Rust does
    // not contract, so the unrounded product sum lands on 0.2 exactly where Go
    // lands on 0.19999999999999998 — and `generateRiskExplanation` tests
    // `CompositeRisk < 0.2` (risk.go:269), so one ULP flips the verdict from
    // "Low risk - stable dependency structure" to "Moderate risk" and changes
    // every triage recommendation's breakdown.
    //
    // `mul_add` is the single-rounding form on every platform, matching the
    // FMADDD the Go binary actually emits. Same principle as the HITS
    // normalisation in bv-graph-core, applied in the other direction: there Go
    // fused and Rust did not, and here Go fuses and Rust did not.
    // Go writes this as `FanVariance*w + ActivityChurn*w + CrossRepoRisk*w +
    // StatusRisk*w`, and the Go compiler FUSES each product into the
    // following add, so the expression rounds once per fused step rather than
    // once per product and once per sum. `mul_add` reproduces that.
    //
    // I briefly replaced this with a plain left-to-right sum, reasoning from the
    // source text that every operation rounded separately. Measurement
    // contradicts that, decisively: with the four components identical on both
    // sides, the plain sum yields 0.06002140519983835 and the oracle publishes
    // 0.060021405199838354. Both fused association orders -- left and right --
    // reproduce the oracle's value exactly, and no plain ordering does. The
    // fused form is correct; the source text is not the whole story.
    let composite = status_risk.mul_add(
        0.20,
        cross_repo.mul_add(0.20, churn.mul_add(0.30, fan_variance.mul_add(0.30, 0.0))),
    );
    RiskSignals {
        fan_variance,
        activity_churn: churn,
        cross_repo_risk: cross_repo,
        status_risk,
        composite_risk: composite.min(1.0),
    }
}

/// Go `reasons.ActionHint` (triage.go:1538-1546). Suggested next action, keyed
/// off status; the deferred branch is handled by the CLI, which owns
/// `defer_until` parsing.
fn action_hint(issue: &Issue) -> String {
    match issue.status {
        Status::InProgress => "Continue work on this issue".to_string(),
        Status::Blocked => "Resolve blocked status before claiming this issue".to_string(),
        Status::Open => "Start work on this issue".to_string(),
        other => format!(
            "Wait for status {} to become open before claiming",
            other.as_str()
        ),
    }
}

/// Analysis-local half of Go `isClaimableRecommendation` (triage.go:1191).
///
/// Go also requires an empty `BlockedBy` and an explicit not-ready-label /
/// parent-with-open-children gate, both of which need graph context this
/// function does not receive; the CLI completes the gate on top of this value.
fn is_claimable(issue: &Issue) -> bool {
    issue.status == Status::Open
        && !issue.issue_type.eq_ignore_ascii_case("epic")
        && issue.assignee.trim().is_empty()
}

/// Serialize a label list the way Go marshals a nil slice for a field declared
/// without `omitempty`: `null` when empty, a JSON array otherwise.
pub fn serialize_labels<S: serde::Serializer>(labels: &[String], s: S) -> Result<S::Ok, S::Error> {
    if labels.is_empty() {
        s.serialize_none()
    } else {
        s.collect_seq(labels)
    }
}

/// Per-issue impact result matching golden `recommendations[]` breakdown.
///
/// Field order mirrors Go `analysis.Recommendation` (triage.go:120): the
/// weighted score block, then the human-readable `action` hint, then the
/// machine-readable `reasons`, `actions` and `claimable` gate.
#[derive(Debug, Clone, Serialize)]
pub struct IssueImpact {
    pub id: String,
    pub title: String,
    #[serde(rename = "type")]
    pub issue_type: String,
    pub status: String,
    pub priority: i32,
    /// Go `Recommendation.Labels` is a plain `[]string` with no omitempty, so
    /// an issue with no labels serializes as `null` rather than `[]`.
    #[serde(serialize_with = "crate::impact::serialize_labels")]
    pub labels: Vec<String>,
    pub score: f64,
    pub breakdown: Breakdown,
    /// Go `reasons.ActionHint` (triage.go:1538-1546) — the suggested next
    /// action, derived from status (and defer_until when in the future).
    pub action: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
    /// Go `Recommendation.UnblocksIDs` (triage.go:947) — issues this one is
    /// the sole open blocker of, from the triage unblocks map.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unblocks_ids: Vec<String>,
    /// Go `Recommendation.BlockedBy` (triage.go:949) — this issue's open
    /// blockers; omitted when there are none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<String>,
    /// Go `Recommendation.Claimable` (triage.go:658) — true iff this item
    /// passes `isClaimableRecommendation`. Declared before `actions` because
    /// that is Go's order (triage.go:133-140); the two are adjacent in the
    /// wire form, so the swap is a byte diff whenever both are present.
    pub claimable: bool,
    /// Go `model.IssueActions` — the live tracker route for this issue.
    /// `None` when no tracker origin could be resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actions: Option<serde_json::Value>,
}

/// Golden field names + order. Mirrors Go `ScoreBreakdown`
/// (pkg/analysis/priority.go:24): weighted values first, then the raw
/// normalized values, then the explanation text, then the risk detail.
#[derive(Debug, Clone, Serialize)]
pub struct Breakdown {
    pub pagerank: f64,
    pub betweenness: f64,
    pub blocker_ratio: f64,
    pub staleness: f64,
    pub priority_boost: f64,
    pub time_to_impact: f64,
    pub urgency: f64,
    pub risk: f64,
    pub pagerank_norm: f64,
    pub betweenness_norm: f64,
    pub blocker_ratio_norm: f64,
    pub staleness_norm: f64,
    pub priority_boost_norm: f64,
    pub time_to_impact_norm: f64,
    pub urgency_norm: f64,
    pub risk_norm: f64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub time_to_impact_explanation: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub urgency_explanation: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub risk_explanation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub risk_signals: Option<RiskSignalsDetail>,
}

/// Go `RiskSignals` JSON shape (pkg/analysis/risk.go:20) including `explanation`.
#[derive(Debug, Clone, Serialize)]
pub struct RiskSignalsDetail {
    pub fan_variance: f64,
    pub activity_churn: f64,
    pub cross_repo_risk: f64,
    pub status_risk: f64,
    pub composite_risk: f64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub explanation: String,
}

/// Compute the median estimated_minutes across all issues that have estimates.
/// Returns `DEFAULT_ESTIMATED_MINUTES` if no issues have estimates (Go parity:
/// `computeMedianEstimatedMinutes` in pkg/analysis/priority.go:304).
fn compute_median_estimated_minutes(issues: &[bv_core::model::Issue]) -> i64 {
    let mut estimates: Vec<i64> = issues
        .iter()
        .filter_map(|i| i.estimated_minutes)
        .filter(|&m| m > 0)
        .collect();

    if estimates.is_empty() {
        return DEFAULT_ESTIMATED_MINUTES;
    }

    estimates.sort_unstable();
    let mid = estimates.len() / 2;
    if estimates.len().is_multiple_of(2) {
        (estimates[mid - 1] + estimates[mid]) / 2
    } else {
        estimates[mid]
    }
}

/// Inputs gathered from Phase-1/2 stats for one scoring pass.
pub struct ImpactInputs<'a> {
    pub issues: &'a [Issue],
    pub pagerank: &'a BTreeMap<String, f64>,
    pub betweenness: &'a BTreeMap<String, f64>,
    /// critical path heights keyed by id (None = not computed, skip time-to-impact).
    pub critical_path: Option<&'a BTreeMap<String, f64>>,
    pub g: &'a DiGraph,
    pub now: jiff::Timestamp,
}

/// Score all open issues, ranked by score desc then ID asc (Go tie-break) with
/// the documented default factor weights.
pub fn compute_impact_scores(inputs: &ImpactInputs) -> Vec<IssueImpact> {
    compute_impact_scores_with_weights(inputs, crate::scoring::default_weights())
}

/// Score all open issues with caller-supplied factor weights — the Rust
/// counterpart of Go's `w := a.Weights()` lookup at priority.go:252, which
/// reads the defaults unless feedback (or another caller) installed adjusted
/// weights via `Analyzer.SetWeights`.
pub fn compute_impact_scores_with_weights(
    inputs: &ImpactInputs,
    w: crate::scoring::Weights,
) -> Vec<IssueImpact> {
    // Compute median estimated_minutes once (Go parity: computeMedianEstimatedMinutes).
    let median_minutes = compute_median_estimated_minutes(inputs.issues);

    // Max values for normalization
    let max_pr = inputs.pagerank.values().copied().fold(0.0, f64::max);
    let max_bw = inputs.betweenness.values().copied().fold(0.0, f64::max);
    // Blocker counts = in-degree over blocking edges; max across nodes.
    let max_blockers = (0..inputs.g.len())
        .map(|i| inputs.g.in_degree(i))
        .max()
        .unwrap_or(0);

    let mut results: Vec<IssueImpact> = Vec::new();
    for issue in inputs.issues {
        // Skip closed/tombstone (Go parity).
        if matches!(issue.status, Status::Closed | Status::Tombstone) {
            continue;
        }
        let pr_norm = normalize(
            inputs.pagerank.get(&issue.id).copied().unwrap_or(0.0),
            max_pr,
        );
        let bw_norm = normalize(
            inputs.betweenness.get(&issue.id).copied().unwrap_or(0.0),
            max_bw,
        );
        let idx = inputs.g.node_idx(&issue.id).unwrap_or(usize::MAX);
        let blockers = if idx == usize::MAX {
            0
        } else {
            inputs.g.in_degree(idx)
        };
        let blocker_norm = normalize(blockers as f64, max_blockers as f64);
        let staleness_norm = compute_staleness(issue.updated_at.as_deref(), &inputs.now);
        let prio_norm = compute_priority_boost(issue.priority);
        let depth = inputs
            .critical_path
            .and_then(|cp| cp.get(&issue.id))
            .copied()
            .unwrap_or(0.0);
        let tti_norm = compute_time_to_impact(depth, issue.estimated_minutes, median_minutes);
        let urgency_norm = compute_urgency(&issue.labels, issue.created_at.as_deref(), &inputs.now);
        let risk = compute_risk_signals(issue, inputs.g, inputs.issues, &inputs.now);

        let risk_expl = risk_explanation(&risk);
        let b = Breakdown {
            pagerank: pr_norm * w.pagerank,
            betweenness: bw_norm * w.betweenness,
            blocker_ratio: blocker_norm * w.blocker_ratio,
            staleness: staleness_norm * w.staleness,
            priority_boost: prio_norm * w.priority_boost,
            time_to_impact: tti_norm * w.time_to_impact,
            urgency: urgency_norm * w.urgency,
            risk: risk.composite_risk * w.risk,
            pagerank_norm: pr_norm,
            betweenness_norm: bw_norm,
            blocker_ratio_norm: blocker_norm,
            staleness_norm,
            priority_boost_norm: prio_norm,
            time_to_impact_norm: tti_norm,
            urgency_norm,
            risk_norm: risk.composite_risk,
            time_to_impact_explanation: time_to_impact_explanation(
                depth,
                issue.estimated_minutes,
                median_minutes,
            ),
            urgency_explanation: urgency_explanation(
                &issue.labels,
                issue.created_at.as_deref(),
                &inputs.now,
                urgency_norm,
            ),
            risk_explanation: risk_expl.clone(),
            risk_signals: Some(RiskSignalsDetail {
                fan_variance: risk.fan_variance,
                activity_churn: risk.activity_churn,
                cross_repo_risk: risk.cross_repo_risk,
                status_risk: risk.status_risk,
                composite_risk: risk.composite_risk,
                explanation: risk_expl,
            }),
        };
        let score = b.pagerank
            + b.betweenness
            + b.blocker_ratio
            + b.staleness
            + b.priority_boost
            + b.time_to_impact
            + b.urgency
            + b.risk;

        // Generate human-readable reasons with emoji prefixes (Go parity:
        // GenerateTriageReasons in triage.go).  The emoji + phrasing must
        // match Go byte-for-byte so golden diffs converge.
        let mut reasons = Vec::new();

        // 1. Unblock cascade (highest priority — most actionable).
        //    Edge direction: u -> v means u depends on v (v blocks u).
        //    So in_degree(v) = how many issues are blocked by v = unblocks count.
        let unblocks = if idx == usize::MAX {
            0usize
        } else {
            inputs.g.in_degree(idx)
        };
        // Collect IDs of issues that this one unblocks (for the list suffix).
        // predecessors_slice returns nodes that have edges TO this node,
        // i.e., issues whose blocking dependency is this one.
        let unblocked_ids: Vec<String> = if idx != usize::MAX {
            inputs
                .g
                .predecessors_slice(idx)
                .iter()
                .filter_map(|&n| inputs.g.node_id(n).map(|s| s.to_string()))
                .collect()
        } else {
            Vec::new()
        };
        if unblocks >= 3 {
            let list = if unblocked_ids.len() <= 3 {
                unblocked_ids.join(", ")
            } else {
                format!(
                    "{}, {}, +{} more",
                    unblocked_ids[0],
                    unblocked_ids[1],
                    unblocked_ids.len() - 2
                )
            };
            reasons.push(format!(
                "🎯 Completing this unblocks {unblocks} downstream issues ({list})"
            ));
        } else if unblocks > 0 {
            let list = unblocked_ids.join(", ");
            reasons.push(format!("🔓 Unblocks {unblocks} item(s): {list}"));
        }

        // 2. Graph metrics (bottleneck / centrality).
        if bw_norm > 0.5 {
            reasons.push(format!(
                "🔀 Critical path bottleneck (betweenness: {:.0}%)",
                bw_norm * 100.0
            ));
        }
        if pr_norm > 0.3 {
            reasons.push(format!(
                "📊 High centrality in dependency graph (PageRank: {:.0}%)",
                pr_norm * 100.0
            ));
        }

        // 3. Staleness alert — compute actual days from updated_at (Go parity).
        //    Use seconds to avoid jiff Unit::Day quirks, then divide.
        let days_stale = if let Some(raw) = issue.updated_at.as_deref() {
            match raw.parse::<jiff::Timestamp>() {
                Ok(t) => {
                    let secs = (inputs.now - t).total(jiff::Unit::Second).unwrap_or(0.0);
                    (secs / 86400.0) as i64
                }
                Err(_) => 0,
            }
        } else {
            0
        };
        if days_stale > 14 {
            reasons.push(format!(
                "🕐 No activity in {days_stale} days - may need review"
            ));
        } else if days_stale > 7 {
            reasons.push(format!("📅 Last updated {days_stale} days ago"));
        }

        // Go sets this when the issue has open blockers (triage.go:1704);
        // it takes precedence over the plain start-work hint.
        let mut blocked_action_hint: Option<String> = None;

        // 4. Quick-win identification. Go decides this from the triage
        //    factor `QuickWinBoost > 0.05` (triage.go:1759), which is only
        //    known in the triage layer; the old local approximation
        //    (`unblocks > 0 && priority_norm >= 0.5`) fired for every
        //    mid-chain issue. The triage layer appends the reason and the
        //    action hint together, sharing one predicate.

        // 5. Quick-win reason. Go emits it here, before the claim-status
        //    reason (triage.go:1605 then 1625), so the ordering matters; the
        //    predicate itself (QuickWinBoost > 0.05) lives in the triage layer
        //    and is applied there, in position.
        // 6. Claim status — Go parity: isOpenStatus guard.
        //    Go inserts the quick-win reason just above this one
        //    (triage.go:1605, then 1625), so the two are recorded in that
        //    order; the quick-win predicate is applied in the triage layer,
        //    which splices it in ahead of the claim-status entry.
        if issue.status.is_open() && issue.assignee.is_empty() {
            reasons.push("✅ Currently unclaimed - available for work".to_string());
        }

        // 7. Blocked-by reason (Go triage.go:1696-1705). Go gates purely on
        //     having open blockers, whatever the status; the earlier
        //     `!is_open` guard dropped it for open-but-blocked issues, which
        //     is most of a dependency chain. Ancestor-epic parity (#2):
        //     inherited parent-child blockers surface via the shared helper.
        let by_id_map: std::collections::HashMap<&str, &Issue> =
            inputs.issues.iter().map(|i| (i.id.as_str(), i)).collect();
        let blocker_ids: Vec<String> = crate::blocker_chain::open_blockers(&by_id_map, &issue.id);
        if blocker_ids.len() == 1 {
            reasons.push(format!(
                "⏳ Blocked by {} - complete that first",
                blocker_ids[0]
            ));
            blocked_action_hint = Some(format!("Work on {} first to unblock this", blocker_ids[0]));
        } else if blocker_ids.len() > 1 {
            reasons.push(format!(
                "⏳ Blocked by {} items - need to clear dependencies",
                blocker_ids.len()
            ));
            blocked_action_hint = Some(format!("Work on {} first to unblock this", blocker_ids[0]));
        }

        // 6. Urgency label signal — Go parity: Priority <= 1 (P0/P1 only).
        if issue.priority <= 1 {
            let prio_label = format!("P{}", issue.priority);
            reasons.push(format!(
                "🚨 High priority ({prio_label}) - prioritize this work"
            ));
        }

        if reasons.is_empty() {
            reasons.push("Open and actionable".to_string());
        }

        results.push(IssueImpact {
            id: issue.id.clone(),
            title: issue.title.clone(),
            issue_type: issue.issue_type.clone(),
            status: issue.status.as_str().to_string(),
            priority: issue.priority,
            labels: issue.labels.clone(),
            score,
            breakdown: b,
            action: blocked_action_hint.unwrap_or_else(|| action_hint(issue)),
            reasons,
            // Filled in by the triage layer, which owns the unblocks map;
            // the impact scorer has no dependency graph context.
            unblocks_ids: Vec::new(),
            blocked_by: Vec::new(),
            // Tracker route is resolved by the CLI layer, which owns the
            // source path; the analysis layer has no origin to build it from.
            actions: None,
            claimable: is_claimable(issue),
        });
    }
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.id.cmp(&b.id))
    });
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue_with_repo(id: &str, repo: &str) -> Issue {
        Issue {
            id: id.to_string(),
            content_hash: String::new(),
            title: String::new(),
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
            dependencies: vec![],
            comments: vec![],
            source_repo: repo.to_string(),
        }
    }

    fn dep(
        issue_id: &str,
        target: &str,
        ty: bv_core::model::DependencyType,
    ) -> bv_core::model::Dependency {
        bv_core::model::Dependency {
            issue_id: issue_id.to_string(),
            depends_on_id: target.to_string(),
            depends_on_legacy: String::new(),
            target_id_legacy: String::new(),
            r#type: ty,
            created_at: None,
            created_by: String::new(),
        }
    }

    #[test]
    fn cross_repo_risk_is_zero_without_dependencies() {
        // Regression: a stub returned a flat 0.2 for any non-empty
        // `source_repo`, inflating composite_risk by 0.2*0.20 = 0.04 and
        // every weighted score by 0.004 on dependency-free issues. Go
        // (pkg/analysis/risk.go computeCrossRepoRisk) returns 0 as soon as
        // `len(issue.Dependencies) == 0`.
        use bv_core::model::DependencyType;
        let issue = issue_with_repo("A", "beads_viewer_rust");
        assert_eq!(compute_cross_repo_risk(&issue, &[]), 0.0);

        // A non-blocking dependency is not counted in the denominator.
        let mut with_related = issue.clone();
        with_related
            .dependencies
            .push(dep("A", "B", DependencyType::Related));
        assert_eq!(compute_cross_repo_risk(&with_related, &[]), 0.0);

        // No `source_repo` short-circuits even with a blocking dep present.
        let mut no_repo = issue_with_repo("A", "");
        no_repo
            .dependencies
            .push(dep("A", "B", DependencyType::Blocks));
        assert_eq!(compute_cross_repo_risk(&no_repo, &[]), 0.0);
    }

    #[test]
    fn cross_repo_risk_is_ratio_of_cross_repo_blocking_deps() {
        use bv_core::model::DependencyType;
        let same = issue_with_repo("B", "repo-a");
        let other = issue_with_repo("C", "repo-b");

        let mut target = issue_with_repo("A", "repo-a");
        target.dependencies = vec![
            // blocking -> same repo: not cross
            dep("A", "B", DependencyType::Blocks),
            // blocking -> different repo: cross
            dep("A", "C", DependencyType::Blocks),
        ];
        let all = vec![target.clone(), same, other];
        assert_eq!(compute_cross_repo_risk(&target, &all), 0.5);
    }

    #[test]
    fn staleness_caps_at_thirty_days() {
        let now = jiff::Timestamp::now();
        let old = (now - jiff::Span::new().hours(60 * 24)).to_string();
        assert_eq!(compute_staleness(Some(&old), &now), 1.0);
        let fresh = (now - jiff::Span::new().hours(12)).to_string();
        let s = compute_staleness(Some(&fresh), &now);
        assert!((s - 0.5 / 30.0).abs() < 0.01);
        assert_eq!(compute_staleness(None, &now), 0.5);
    }

    #[test]
    fn priority_boost_matches_go_table() {
        assert_eq!(compute_priority_boost(0), 1.0);
        assert_eq!(compute_priority_boost(1), 0.75);
        assert_eq!(compute_priority_boost(2), 0.5);
        assert_eq!(compute_priority_boost(3), 0.25);
        assert_eq!(compute_priority_boost(4), 0.0);
    }

    #[test]
    fn urgency_label_weights_and_decay_cap() {
        let now = jiff::Timestamp::now();
        let crit = vec!["critical".to_string()];
        let fresh_created = (now - jiff::Span::new().hours(1)).to_string();
        let u = compute_urgency(&crit, Some(&fresh_created), &now);
        assert!(u >= 1.0); // label alone hits cap
        let no_labels: Vec<String> = vec![];
        let ancient = (now - jiff::Span::new().hours(365 * 24)).to_string();
        let u2 = compute_urgency(&no_labels, Some(&ancient), &now);
        // decay approaches but never exceeds 0.5
        assert!(u2 < 0.51 && u2 > 0.45);
    }

    #[test]
    fn time_to_impact_depth_weighted_over_time() {
        // deep chain + no estimate(median 60m): depth 10 -> 1.0*0.7 + (1-60/480)*0.3 ≈ 0.86
        let s = compute_time_to_impact(10.0, None, 60);
        assert!((s - (1.0 * 0.7 + (1.0 - 60.0 / 480.0) * 0.3)).abs() < 1e-9);
    }
}

/// Explainable scoring: human-readable reasons for a recommendation.
#[derive(Debug, Clone, Serialize)]
pub struct ScoreReason {
    pub reason: String,
    pub component: String,
    pub value: f64,
}

/// Generate reasons from a breakdown for a specific issue.
use crate::scoring::{ScoreBreakdown, WEIGHT_PAGE_RANK};

pub fn explain_score(
    breakdown: &ScoreBreakdown,
    blockers: usize,
    days_stale: f64,
) -> Vec<ScoreReason> {
    let mut reasons = Vec::new();
    if breakdown.pagerank > 0.15 {
        reasons.push(ScoreReason {
            reason: format!(
                "high pagerank ({:.2})",
                breakdown.pagerank / WEIGHT_PAGE_RANK
            ),
            component: "pagerank".into(),
            value: breakdown.pagerank,
        });
    }
    if breakdown.betweenness > 0.10 {
        reasons.push(ScoreReason {
            reason: "high betweenness (bridge node)".into(),
            component: "betweenness".into(),
            value: breakdown.betweenness,
        });
    }
    if blockers > 0 {
        reasons.push(ScoreReason {
            reason: format!("blocks {blockers} issues"),
            component: "blocker_ratio".into(),
            value: breakdown.blocker_ratio,
        });
    }
    if breakdown.staleness > 0.03 {
        reasons.push(ScoreReason {
            reason: format!("stale {days_stale:.0} days"),
            component: "staleness".into(),
            value: breakdown.staleness,
        });
    }
    if breakdown.priority_boost > 0.05 {
        reasons.push(ScoreReason {
            reason: "high priority".into(),
            component: "priority_boost".into(),
            value: breakdown.priority_boost,
        });
    }
    reasons
}
