//! Drift detection — port of Go `pkg/drift` (Calculator + Result + exit
//! codes) and `pkg/baseline` snapshot format v1.

use bv_core::model::{Issue, Status};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    #[default]
    Info,
    Critical,
    Warning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AlertType {
    #[default]
    NewCycle,
    DensityGrowth,
    NodeCountChange,
    EdgeCountChange,
    BlockedIncrease,
    ActionableChange,
    PagerankChange,
    StaleIssue,
    BlockingCascade,
    HighImpactUnblock,
}

/// Go's `omitempty` on a float64 field: absent, or present-but-zero, both leave
/// the key out of the payload (pkg/drift/drift.go:75-77).
fn is_absent_or_zero(v: &Option<f64>) -> bool {
    v.is_none_or(|x| x == 0.0)
}

/// Go's `omitempty` on an int field — `UnblocksCount` / `DownstreamPrioritySum`
/// (pkg/drift/drift.go:95-96) — behaves exactly like the float64 case: a
/// present-but-zero value still leaves the key out.
fn is_absent_or_zero_i64(v: &Option<i64>) -> bool {
    v.is_none_or(|x| x == 0)
}

/// One drift alert — Go `drift.Alert` (pkg/drift/drift.go:71-97).
///
/// The field order below is Go's struct declaration order and is load-bearing:
/// serde emits keys in declaration order, and the frozen corpus carries that
/// same order (`golden/xl_2500____robot_alerts.json` blocking_cascade entry:
/// type, severity, message, details, issue_id, detected_at, labels,
/// suggested_action, unblocks_count, downstream_priority_sum). Reordering
/// these fields would change output bytes even though every value still
/// compared equal after key sorting.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Alert {
    #[serde(rename = "type")]
    pub alert_type: AlertType,
    pub severity: Severity,
    pub message: String,
    // Go declares these as plain float64 with `omitempty`
    // (pkg/drift/drift.go:75-77), so a present-but-zero value is omitted. The
    // Option shape is kept so bv-tui and other callers are untouched.
    #[serde(default, skip_serializing_if = "is_absent_or_zero")]
    #[serde(rename = "baseline_value")]
    pub baseline_val: Option<f64>,
    #[serde(default, skip_serializing_if = "is_absent_or_zero")]
    #[serde(rename = "current_value")]
    pub current_val: Option<f64>,
    #[serde(default, skip_serializing_if = "is_absent_or_zero")]
    pub delta: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub issue_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detected_at: Option<String>,
    // Go carries the flagged issue's labels so `--alert-label` can filter on
    // them (pkg/drift/drift.go:86, populated at :607/:702/:950/:996/:1090).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    /// Go populates this on every alert (e.g. drift.go:560); the Rust struct
    /// previously dropped it, so alerts serialized without the field.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub suggested_action: String,
    #[serde(default, skip_serializing_if = "is_absent_or_zero_i64")]
    pub unblocks_count: Option<i64>,
    #[serde(default, skip_serializing_if = "is_absent_or_zero_i64")]
    pub downstream_priority_sum: Option<i64>,
}

/// Baseline stats snapshot — Go `baseline.GraphStats` (baseline.go:47-56).
///
/// The JSON names are Go's, not Rust's: Go writes `open_count`/`closed_count`/
/// `blocked_count`/`actionable_count` where this struct used to write `open`,
/// `closed`, `blocked` and `actionable`. Go has no `pagerank` field on
/// GraphStats at all — the top-N PageRank lives in a sibling `top_metrics`
/// object (baseline.go:38-40, :59-61) — so `pagerank` is skipped in both
/// directions and populated from `top_metrics` by the reader. Serialising it
/// under `stats` produced a document Go silently ignored, which made every
/// current entry look like it had "entered top".
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct BaselineStats {
    pub node_count: usize,
    pub edge_count: usize,
    pub density: f64,
    // The `alias` on each count keeps baselines this port wrote before the
    // schema was corrected readable. Go has no such alias: it ignores the old
    // key and reads zero, which is why an old file looks like drift to it.
    #[serde(rename = "open_count", alias = "open")]
    pub open: usize,
    #[serde(rename = "closed_count", alias = "closed")]
    pub closed: usize,
    #[serde(rename = "blocked_count", alias = "blocked")]
    pub blocked: usize,
    pub cycle_count: usize,
    #[serde(rename = "actionable_count", alias = "actionable")]
    pub actionable: usize,
    /// Top PageRank per issue id. Not part of `stats` in Go's schema; see the
    /// type note. `skip` keeps it out of the written document and out of the
    /// `stats` object on read, so it can only arrive via `top_metrics`.
    #[serde(skip, default)]
    pub pagerank: BTreeMap<String, f64>,
}

/// Go `baseline.MetricItem` (baseline.go:77-80).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricItem {
    pub id: String,
    pub value: f64,
}

/// Go `baseline.TopMetrics` (baseline.go:59-75), in declaration order. Only
/// `pagerank` participates in a drift check today; the rest are stored so a
/// baseline Go writes round-trips through this port unchanged.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct TopMetrics {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pagerank: Vec<MetricItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub betweenness: Vec<MetricItem>,
    #[serde(
        rename = "critical_path",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub critical_path: Vec<MetricItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hubs: Vec<MetricItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authorities: Vec<MetricItem>,
}

impl TopMetrics {
    /// Go `buildMetricItems(metrics, 10)` (main.go:5242-5258): value
    /// descending, then the top N. An empty map yields no items at all, which
    /// is what `omitempty` then drops from the document.
    pub fn from_map(items: &BTreeMap<String, f64>, limit: usize) -> Vec<MetricItem> {
        if items.is_empty() {
            return Vec::new();
        }
        let mut items: Vec<MetricItem> = items
            .iter()
            .map(|(id, value)| MetricItem {
                id: id.clone(),
                value: *value,
            })
            .collect();
        items.sort_by(|a, b| {
            b.value
                .partial_cmp(&a.value)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        items.truncate(limit);
        items
    }

    /// The `pagerank` list as the id -> value map the drift calculator reads.
    pub fn pagerank_map(&self) -> BTreeMap<String, f64> {
        self.pagerank
            .iter()
            .map(|m| (m.id.clone(), m.value))
            .collect()
    }
}

/// Per-label staleness threshold overrides (Go `LabelConfig`, bv-167).
#[derive(Debug, Clone, Default)]
pub struct LabelConfig {
    pub stale_warning_days: Option<u32>,
    pub stale_critical_days: Option<u32>,
    pub in_progress_stale_multiplier: Option<f64>,
}

/// Drift configuration thresholds (Go DefaultConfig).
#[derive(Debug, Clone)]
pub struct DriftConfig {
    pub density_warning_pct: f64,
    pub density_info_pct: f64,
    pub node_growth_info_pct: f64,
    pub edge_growth_info_pct: f64,
    pub blocked_increase_threshold: i64,
    pub actionable_decrease_warning_pct: f64,
    pub actionable_increase_info_pct: f64,
    pub pagerank_change_warning_pct: f64,
    /// Days since last update before a stale warning is emitted.
    pub stale_warning_days: u32,
    /// Days since last update before a stale critical alert is emitted.
    pub stale_critical_days: u32,
    /// Multiplier applied to staleness thresholds for in-progress items.
    /// <1 tightens thresholds (items age faster).
    pub in_progress_stale_multiplier: f64,
    /// Minimum unblocks count for an info-level BlockingCascade alert.
    pub blocking_cascade_info_threshold: i64,
    /// Minimum unblocks count for a warning-level BlockingCascade alert.
    pub blocking_cascade_warning_threshold: i64,
    /// An actionable issue that unblocks at least this many downstream items
    /// is a `high_impact_unblock` candidate, provided one of them is at least
    /// as urgent as [`DriftConfig::high_impact_priority_max`]. Go default 3
    /// (pkg/drift/config.go:120).
    pub high_impact_unblock_min: i64,
    /// Most urgent priority (P0=0 … P4=4) that still counts as "high impact"
    /// downstream. Go default 1, i.e. P0 or P1
    /// (pkg/drift/config.go:121).
    pub high_impact_priority_max: i32,
    /// Graph-size cap for the whole-graph proactive checks. Go default 2000
    /// (pkg/drift/config.go:125); above it the checks are skipped and the
    /// reason is reported so silence is not mistaken for health.
    pub proactive_max_issues: usize,
    /// Alert types that are disabled and should not generate alerts (bv-167).
    pub disabled_alerts: Vec<String>,
    /// Per-label staleness overrides (bv-167).
    pub label_overrides: BTreeMap<String, LabelConfig>,
}

impl Default for DriftConfig {
    fn default() -> Self {
        DriftConfig {
            density_warning_pct: 50.0,
            density_info_pct: 20.0,
            node_growth_info_pct: 25.0,
            edge_growth_info_pct: 25.0,
            blocked_increase_threshold: 5,
            actionable_decrease_warning_pct: 30.0,
            actionable_increase_info_pct: 20.0,
            pagerank_change_warning_pct: 50.0,
            stale_warning_days: 14,
            stale_critical_days: 30,
            in_progress_stale_multiplier: 0.5,
            proactive_max_issues: 2000,
            blocking_cascade_info_threshold: 3,
            blocking_cascade_warning_threshold: 5,
            high_impact_unblock_min: 3,
            high_impact_priority_max: 1,
            disabled_alerts: Vec::new(),
            label_overrides: BTreeMap::new(),
        }
    }
}

impl DriftConfig {
    /// Returns true if the given alert type is in the disabled list (bv-167).
    pub fn is_alert_disabled(&self, alert_type: &str) -> bool {
        self.disabled_alerts.iter().any(|d| d == alert_type)
    }

    /// Resolve staleness thresholds for an issue based on its labels (bv-167).
    /// If multiple labels have overrides, the tightest (smallest) non-zero
    /// thresholds among them are used.  Unset values inherit the global default.
    pub fn get_staleness_thresholds(&self, labels: &[String]) -> (u32, u32, f64) {
        let applicable: Vec<&LabelConfig> = labels
            .iter()
            .filter_map(|l| self.label_overrides.get(l))
            .collect();

        if applicable.is_empty() {
            return (
                self.stale_warning_days,
                self.stale_critical_days,
                self.in_progress_stale_multiplier,
            );
        }

        let resolve = |lc: &LabelConfig| -> (u32, u32, f64) {
            let w = lc.stale_warning_days.unwrap_or(self.stale_warning_days);
            let c = lc.stale_critical_days.unwrap_or(self.stale_critical_days);
            let m = lc
                .in_progress_stale_multiplier
                .unwrap_or(self.in_progress_stale_multiplier);
            (w, c, m)
        };

        let (mut warn, mut crit, mut mult) = resolve(applicable[0]);
        for lc in &applicable[1..] {
            let (w, c, m) = resolve(lc);
            if w < warn {
                warn = w;
            }
            if c < crit {
                crit = c;
            }
            if m < mult {
                mult = m;
            }
        }
        (warn, crit, mult)
    }
}

/// One alert type that `calculate` did not run, with the reason
/// (Go `SkippedCheck`, pkg/drift/drift.go:117).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkippedCheck {
    #[serde(rename = "type")]
    pub check_type: String,
    pub reason: String,
}

/// Complete drift analysis result (Go `Result`).
#[derive(Debug, Default, Serialize)]
pub struct DriftResult {
    pub has_drift: bool,
    pub alerts: Vec<Alert>,
    /// Alert types that were not evaluated, and why (Go `SkippedChecks`,
    /// pkg/drift/drift.go:111). Emitted so a skipped check is never read as a
    /// clean one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped_checks: Vec<SkippedCheck>,
    pub critical_count: usize,
    pub warning_count: usize,
    pub info_count: usize,
}

impl DriftResult {
    /// CI contract: exit 0=OK, 1=critical, 2=warning.
    pub fn exit_code(&self) -> u8 {
        if self.critical_count > 0 {
            1
        } else if self.warning_count > 0 {
            2
        } else {
            0
        }
    }

    fn push(&mut self, alert: Alert) {
        self.has_drift = true;
        match alert.severity {
            Severity::Critical => self.critical_count += 1,
            Severity::Warning => self.warning_count += 1,
            Severity::Info => self.info_count += 1,
        }
        self.alerts.push(alert);
    }
}

fn pct_change(baseline: f64, current: f64) -> Option<f64> {
    if baseline == 0.0 {
        return None;
    }
    Some(((current - baseline) / baseline) * 100.0)
}

/// Parse an ISO-8601 timestamp string to seconds since Unix epoch.
/// Returns `None` on parse failure.
fn parse_ts_secs(s: &str) -> Option<f64> {
    s.parse::<jiff::Timestamp>()
        .ok()
        .map(|ts| ts.as_millisecond() as f64 / 1000.0)
}

/// Emit StaleIssue alerts for open/in_progress issues whose last update is
/// beyond the configured staleness thresholds.  Mirrors Go
/// `Calculator.checkStaleness`.
fn check_staleness(
    result: &mut DriftResult,
    cfg: &DriftConfig,
    issues: &[Issue],
    now: jiff::Timestamp,
) {
    if cfg.is_alert_disabled("stale_issue") || issues.is_empty() {
        return;
    }
    let now_secs = now.as_millisecond() as f64 / 1000.0;
    let now_str = now.to_string();

    for issue in issues {
        // Go skips only Closed and Tombstone.
        if matches!(
            issue.status,
            bv_core::model::Status::Closed | bv_core::model::Status::Tombstone
        ) {
            continue;
        }

        // Last activity = UpdatedAt (fallback CreatedAt when zero).
        let (last_active_raw, last_active_secs) = issue
            .updated_at
            .as_deref()
            .map(|s| (s, parse_ts_secs(s)))
            .or_else(|| issue.created_at.as_deref().map(|s| (s, parse_ts_secs(s))))
            .map(|(s, t)| (s, t.unwrap_or(0.0)))
            .unwrap_or(("", 0.0));
        if last_active_secs == 0.0 {
            continue;
        }

        let (warn_days, crit_days, in_progress_mult) = cfg.get_staleness_thresholds(&issue.labels);

        let mut warn_secs = warn_days as f64 * 86400.0;
        let mut crit_secs = crit_days as f64 * 86400.0;

        // Tighten thresholds for in-progress items (Go: in_progress_stale_multiplier).
        if issue.status == bv_core::model::Status::InProgress && in_progress_mult > 0.0 {
            warn_secs *= in_progress_mult;
            crit_secs *= in_progress_mult;
        }

        let inactive_secs = now_secs - last_active_secs;
        if inactive_secs < 0.0 {
            continue;
        }
        let inactive_days = inactive_secs / 86400.0;

        let severity = if inactive_secs >= crit_secs {
            Severity::Critical
        } else if inactive_secs >= warn_secs {
            Severity::Warning
        } else {
            continue;
        };

        let status_str = match issue.status {
            bv_core::model::Status::Open => "open",
            bv_core::model::Status::InProgress => "in_progress",
            bv_core::model::Status::Closed => "closed",
            bv_core::model::Status::Deferred => "deferred",
            _ => "other",
        };

        result.push(Alert {
            alert_type: AlertType::StaleIssue,
            severity,
            // Go drift.go:607 — the flagged issue's own labels, so
            // `--alert-label` can filter on them.
            labels: issue.labels.clone(),
            suggested_action:
                "Update, close, or re-triage the issue; stale work hides real priorities".into(),
            message: format!("Issue {} inactive for {:.0} days", issue.id, inactive_days),
            baseline_val: None,
            // Go leaves BaselineVal/CurrentVal unset (omitempty → absent).
            current_val: None,
            delta: None,
            details: vec![
                format!("status={status_str}"),
                format!("last_update={last_active_raw}"),
            ],
            issue_id: issue.id.clone(),
            label: String::new(),
            detected_at: Some(now_str.clone()),
            unblocks_count: None,
            downstream_priority_sum: None,
        });
    }
}

/// Compute the set of issue IDs that `issue_id` would unblock if completed.
///
/// An issue is considered "unblocked" by completing `issue_id` when:
/// 1. It depends on `issue_id` via a blocking dependency type.
/// 2. It is not already closed-like.
/// 3. It has no other open blockers besides `issue_id`.
///
/// This is a simplified port of Go `Analyzer.computeUnblocks` (plan.go:85).
fn compute_unblocks(issues: &[Issue], issue_id: &str) -> Vec<String> {
    let by_id: BTreeMap<&str, &Issue> = issues.iter().map(|i| (i.id.as_str(), i)).collect();

    let Some(blocker) = by_id.get(issue_id) else {
        return Vec::new();
    };

    // Find all non-closed issues that directly depend on this blocker.
    let dependents: Vec<&Issue> = issues
        .iter()
        .filter(|i| {
            !i.status.is_closed()
                && i.id != issue_id
                && i.dependencies
                    .iter()
                    .any(|d| d.r#type.is_blocking() && d.effective_depends_on() == blocker.id)
        })
        .collect();

    let mut unblocks = Vec::new();
    for dep_issue in dependents {
        // Check if this dependent has other open blockers besides `issue_id`.
        let still_blocked = dep_issue.dependencies.iter().any(|d| {
            if !d.r#type.is_blocking() {
                return false;
            }
            let target = d.effective_depends_on();
            if target == issue_id {
                return false; // The one we are completing.
            }
            // Check if that other blocker is still open.
            by_id
                .get(target)
                .is_some_and(|other| !other.status.is_closed())
        });
        if !still_blocked {
            unblocks.push(dep_issue.id.clone());
        }
    }
    unblocks.sort();
    unblocks
}

/// The actionable issues in the order Go emits the cascade/unblock families
/// from: `sortedByID(analyzer.GetActionableIssues())`
/// (pkg/drift/drift.go:668-676 and :925, ordered by `sortedByID` at :812-816).
///
/// `sortedByID` compares `out[i].ID < out[j].ID` — a byte-wise string
/// comparison, so "XL-1" < "XL-110" < "XL-14", NOT numeric order. The frozen
/// corpus carries exactly that order: `golden/xl_2500____robot_alerts.json`
/// lists blocking_cascade as XL-1, XL-110, XL-1237, XL-14, XL-186, … and
/// high_impact_unblock as XL-1, XL-1237, XL-14, XL-186, … . Appending alerts in
/// this iteration order is what reproduces it; a numeric sort of the alerts
/// does not.
///
/// Readiness here mirrors the existing `blocking_cascade` filter: closed and
/// deferred issues are skipped, as is anything still carrying an open blocker
/// (`blocker_chain::open_blockers`, which also propagates parent-child
/// blocking). Go spells the same contract `IsCandidate(id) &&
/// Readiness().ReadyAfter(id, now, nil)` (graph.go:2836-2842).
fn actionable_by_id<'a>(
    issues: &'a [Issue],
    by_id: &std::collections::HashMap<&str, &'a Issue>,
) -> Vec<&'a Issue> {
    let mut out: Vec<&Issue> = issues
        .iter()
        .filter(|i| !i.status.is_closed() && i.status != Status::Deferred)
        // Skip issues that have open blockers — they can't be completed yet.
        .filter(|i| crate::blocker_chain::open_blockers(by_id, &i.id).is_empty())
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Emit BlockingCascade alerts for issues whose completion would unblock
/// many downstream dependents.  Mirrors Go `Calculator.checkBlockingCascade`.
fn check_blocking_cascade(
    result: &mut DriftResult,
    cfg: &DriftConfig,
    issues: &[Issue],
    now: jiff::Timestamp,
) {
    if cfg.is_alert_disabled("blocking_cascade") || issues.is_empty() {
        return;
    }
    let info_thresh = cfg.blocking_cascade_info_threshold;
    let warn_thresh = cfg.blocking_cascade_warning_threshold;
    if info_thresh <= 0 && warn_thresh <= 0 {
        return;
    }
    let now_str = now.to_string();

    let by_id: BTreeMap<&str, &Issue> = issues.iter().map(|i| (i.id.as_str(), i)).collect();
    // Ancestor-epic parity (#2): inherited parent-child blocking gates this too.
    let by_id_map: std::collections::HashMap<&str, &Issue> =
        issues.iter().map(|i| (i.id.as_str(), i)).collect();

    // Go appends one alert per qualifying actionable issue while walking that
    // list, so the alert order IS the iteration order.
    for issue in actionable_by_id(issues, &by_id_map) {
        let unblocked = compute_unblocks(issues, &issue.id);
        let count = unblocked.len() as i64;
        if count == 0 {
            continue;
        }

        let severity = if warn_thresh > 0 && count >= warn_thresh {
            Severity::Warning
        } else if info_thresh > 0 && count < info_thresh {
            continue;
        } else {
            Severity::Info
        };

        // Downstream priority sum for urgency scoring (bv-165).
        // Lower priority values = higher importance (P0=critical, P4=backlog).
        let priority_sum: i64 = unblocked
            .iter()
            .filter_map(|id| by_id.get(id.as_str()))
            .map(|i| i.priority as i64)
            .sum();

        result.push(Alert {
            alert_type: AlertType::BlockingCascade,
            severity,
            labels: issue.labels.clone(),
            suggested_action:
                "Prioritize this issue: closing it releases the listed downstream items".into(),
            message: format!(
                "Completing {} unblocks {} downstream item(s)",
                issue.id, count
            ),
            baseline_val: None,
            current_val: None,
            delta: None,
            details: unblocked,
            issue_id: issue.id.clone(),
            label: String::new(),
            detected_at: Some(now_str.clone()),
            unblocks_count: Some(count),
            downstream_priority_sum: Some(priority_sum),
        });
    }
}

/// Emit HighImpactUnblock alerts — `blocking_cascade`'s priority-aware sibling.
/// Port of Go `Calculator.checkHighImpactUnblock` (pkg/drift/drift.go:915-957).
///
/// An actionable issue qualifies when it unblocks at least
/// `high_impact_unblock_min` items AND at least one of those is at priority
/// `<= high_impact_priority_max`; two or more such downstream items escalate
/// the alert from info to warning. Go runs this check with no
/// `expensiveCheckAllowed` guard, so `proactive_max_issues` does not suppress
/// it (contrast `checkPotentialDuplicate` at :1015-1018).
fn check_high_impact_unblock(
    result: &mut DriftResult,
    cfg: &DriftConfig,
    issues: &[Issue],
    now: jiff::Timestamp,
) {
    if cfg.is_alert_disabled("high_impact_unblock") || issues.is_empty() {
        return;
    }
    let min_unblocks = cfg.high_impact_unblock_min;
    if min_unblocks <= 0 {
        return;
    }
    let max_priority = cfg.high_impact_priority_max;
    let now_str = now.to_string();

    let by_id: BTreeMap<&str, &Issue> = issues.iter().map(|i| (i.id.as_str(), i)).collect();
    let by_id_map: std::collections::HashMap<&str, &Issue> =
        issues.iter().map(|i| (i.id.as_str(), i)).collect();

    for issue in actionable_by_id(issues, &by_id_map) {
        let unblocks = compute_unblocks(issues, &issue.id);
        if (unblocks.len() as i64) < min_unblocks {
            continue;
        }
        // `issueMap[id]` in Go: a downstream id with no row is not urgent.
        let mut urgent: Vec<String> = unblocks
            .iter()
            .filter(|id| {
                by_id
                    .get(id.as_str())
                    .is_some_and(|downstream| downstream.priority <= max_priority)
            })
            .cloned()
            .collect();
        if urgent.is_empty() {
            continue;
        }
        urgent.sort();

        let severity = if urgent.len() >= 2 {
            Severity::Warning
        } else {
            Severity::Info
        };

        result.push(Alert {
            alert_type: AlertType::HighImpactUnblock,
            severity,
            // Go drift.go:948-955 — BaselineVal/CurrentVal/Delta stay zero, so
            // `omitempty` leaves all three out of the payload.
            message: format!(
                "Completing {} unblocks {} item(s), {} of them at P{} or higher",
                issue.id,
                unblocks.len(),
                urgent.len(),
                max_priority
            ),
            suggested_action: "Schedule this issue next; it releases high-priority downstream work"
                .into(),
            baseline_val: None,
            current_val: None,
            delta: None,
            details: urgent,
            issue_id: issue.id.clone(),
            labels: issue.labels.clone(),
            label: String::new(),
            detected_at: Some(now_str.clone()),
            unblocks_count: Some(unblocks.len() as i64),
            // Go never sets DownstreamPrioritySum here; 0 is omitted either way.
            downstream_priority_sum: None,
        });
    }
}

/// Run all drift checks between two snapshots.
///
/// `issues` is the current issue list for issue-level alerts (staleness,
/// blocking cascade).  Pass `&[]` to skip issue-level checks.
pub fn calculate(
    baseline: &BaselineStats,
    current: &BaselineStats,
    cfg: &DriftConfig,
    new_cycles: &[Vec<String>],
    issues: &[Issue],
    now: jiff::Timestamp,
) -> DriftResult {
    let mut r = DriftResult::default();

    // Go `expensiveCheckAllowed` (pkg/drift/drift.go:122) gates the two
    // whole-graph proactive checks and records why when it refuses, so a
    // silent skip is never mistaken for a clean result. Rust does not
    // implement `potential_duplicate` or `priority_mismatch` at all, so
    // recording them as skipped is truthful on every graph, not only large
    // ones — the reason text below matches Go's format exactly.
    for typ in ["potential_duplicate", "priority_mismatch"] {
        let limit = cfg.proactive_max_issues;
        if limit > 0 && issues.len() > limit {
            r.skipped_checks.push(SkippedCheck {
                check_type: typ.to_string(),
                reason: format!(
                    "{} issues exceed proactive_max_issues={}",
                    issues.len(),
                    limit
                ),
            });
        }
    }

    // Cycles: any NEW cycle is critical. Go `checkCycles`
    // (pkg/drift/drift.go:317-355) reports the COUNT, not the rendered list,
    // and carries the cycles in `details` joined with U+2192 ARROW.
    if !cfg.is_alert_disabled("new_cycle") && !new_cycles.is_empty() {
        let details: Vec<String> = new_cycles
            .iter()
            .map(|cycle| cycle.join(" \u{2192} "))
            .collect();
        r.push(Alert {
            alert_type: AlertType::NewCycle,
            severity: Severity::Critical,
            message: format!("{} new cycle(s) detected", new_cycles.len()),
            suggested_action:
                "Break the cycle by removing or reversing one dependency edge (bv --robot-suggest lists cycle-break candidates)"
                    .into(),
            // Go's BaselineVal is `len(c.baseline.Cycles)`
            // (pkg/drift/drift.go:346) — the number of cycles RECORDED IN THE
            // BASELINE SNAPSHOT, not `baseline.Stats.CycleCount`. `cycles` is a
            // top-level baseline.json key (pkg/baseline/baseline.go:43) that
            // `BaselineStats` never carries, and Go leaves the baseline's
            // `Cycles` nil whenever no baseline file exists
            // (cmd/bv/robot_registry.go:1158), so every golden here omits the
            // field. Emitting `baseline.cycle_count` instead would be a
            // different number Go never prints on this path.
            baseline_val: None,
            current_val: Some(current.cycle_count as f64),
            delta: Some(new_cycles.len() as f64),
            details,
            detected_at: Some(now.to_string()),
            ..Default::default()
        });
    }

    // Density growth.
    if baseline.density > 0.0 {
        if let Some(pct) = pct_change(baseline.density, current.density) {
            let sev = if pct >= cfg.density_warning_pct {
                Some(Severity::Warning)
            } else if pct >= cfg.density_info_pct {
                Some(Severity::Info)
            } else {
                None
            };
            if let Some(sev) = sev {
                r.push(Alert {
                    alert_type: AlertType::DensityGrowth,
                    severity: sev,
                    message: format!("Graph density increased by {pct:.1}%"),
                    baseline_val: Some(baseline.density),
                    current_val: Some(current.density),
                    delta: Some(current.density - baseline.density),
                    ..Default::default()
                });
            }
        }
    }

    // Node count change (info at threshold).
    if let Some(pct) = pct_change(baseline.node_count as f64, current.node_count as f64) {
        if pct.abs() >= cfg.node_growth_info_pct {
            let delta = current.node_count as i64 - baseline.node_count as i64;
            r.push(Alert {
                alert_type: AlertType::NodeCountChange,
                severity: Severity::Info,
                suggested_action:
                    "Confirm the graph change is intended (bv --robot-diff --diff-since <baseline commit> lists it)"
                        .into(),
                // Go drift.go:421 phrases this as a signed delta plus a
                // percentage, not as "from X to Y".
                message: format!("Node count changed by {delta:+} ({pct:.1}%)"),
                baseline_val: Some(baseline.node_count as f64),
                current_val: Some(current.node_count as f64),
                delta: Some(delta as f64),
                detected_at: Some(now.to_string()),
                ..Default::default()
            });
        }
    }

    // Edge count change (info at threshold).
    if let Some(pct) = pct_change(baseline.edge_count as f64, current.edge_count as f64) {
        if pct.abs() >= cfg.edge_growth_info_pct {
            r.push(Alert {
                alert_type: AlertType::EdgeCountChange,
                severity: Severity::Info,
                suggested_action:
                    "Review recently added or removed dependencies for accidental blockers"
                        .to_string(),
                // Go drift.go:438-447 phrases this as a delta plus a
                // percentage, not as "from X to Y".
                message: format!(
                    "Edge count changed by {delta} ({pct:.1}%)",
                    delta = current.edge_count as i64 - baseline.edge_count as i64
                ),
                baseline_val: Some(baseline.edge_count as f64),
                current_val: Some(current.edge_count as f64),
                delta: Some((current.edge_count as i64 - baseline.edge_count as i64) as f64),
                detected_at: Some(now.to_string()),
                ..Default::default()
            });
        }
    }

    // Blocked increase (warning at +N).
    let blocked_delta = current.blocked as i64 - baseline.blocked as i64;
    if blocked_delta >= cfg.blocked_increase_threshold {
        r.push(Alert {
            alert_type: AlertType::BlockedIncrease,
            severity: Severity::Warning,
            message: format!(
                "Blocked issues increased from {} to {} (+{blocked_delta})",
                baseline.blocked, current.blocked
            ),
            baseline_val: Some(baseline.blocked as f64),
            current_val: Some(current.blocked as f64),
            delta: Some(blocked_delta as f64),
            ..Default::default()
        });
    }

    // Actionable decrease (warning at -N%) / increase (info at +N%).
    if baseline.actionable > 0 {
        if let Some(pct) = pct_change(baseline.actionable as f64, current.actionable as f64) {
            if pct <= -cfg.actionable_decrease_warning_pct {
                // Go prints the magnitude of the drop, not the endpoints:
                // "decreased by %d (%.1f%%)" over -delta and -pct
                // (drift.go:494). bvr worded it as "from X to Y" and left the
                // percentage signed, so the same drop read
                // "...to 0 (-100.0%)" where Go reads "decreased by 2 (100.0%)".
                let delta = current.actionable as i64 - baseline.actionable as i64;
                r.push(Alert {
                    alert_type: AlertType::ActionableChange,
                    severity: Severity::Warning,
                    suggested_action:
                        "Fewer ready items means work is piling up behind blockers; unblock before starting new work"
                            .into(),
                    message: format!("Actionable issues decreased by {} ({:.1}%)", -delta, -pct),
                    baseline_val: Some(baseline.actionable as f64),
                    current_val: Some(current.actionable as f64),
                    delta: Some(delta as f64),
                    ..Default::default()
                });
            } else if pct >= cfg.actionable_increase_info_pct
                || pct <= -cfg.actionable_increase_info_pct
            {
                // Go's info branch is signed and covers decreases that did not
                // cross the warning threshold: "changed by %+d (%.1f%%)"
                // (drift.go:503). The `%+d` keeps the sign, which is why the
                // wording is "changed" rather than "increased" — bvr matched
                // the condition but not the message.
                let delta = current.actionable as i64 - baseline.actionable as i64;
                r.push(Alert {
                    alert_type: AlertType::ActionableChange,
                    severity: Severity::Info,
                    message: format!("Actionable issues changed by {delta:+} ({pct:.1}%)"),
                    baseline_val: Some(baseline.actionable as f64),
                    current_val: Some(current.actionable as f64),
                    delta: Some(delta as f64),
                    ..Default::default()
                });
            }
        }
    }

    // PageRank drift. Go (drift.go:520-565) collects every change into a
    // single aggregated warning rather than one alert per issue: entries that
    // left the top, entries whose value moved past the threshold, and entries
    // that newly entered. The message is the count and `details` carries the
    // sorted change strings.
    {
        let mut changes: Vec<String> = Vec::new();
        for (id, bl_val) in &baseline.pagerank {
            match current.pagerank.get(id) {
                None => changes.push(format!("{id} dropped from top")),
                Some(cur_val) if *bl_val > 0.0 => {
                    let pct = ((cur_val - bl_val) / bl_val) * 100.0;
                    if pct.abs() >= cfg.pagerank_change_warning_pct {
                        changes.push(format!("{id}: {pct:.1}% change"));
                    }
                }
                Some(_) => {}
            }
        }
        for id in current.pagerank.keys() {
            if !baseline.pagerank.contains_key(id) {
                changes.push(format!("{id} entered top"));
            }
        }
        if !changes.is_empty() {
            changes.sort();
            r.push(Alert {
                alert_type: AlertType::PagerankChange,
                severity: Severity::Warning,
                suggested_action:
                    "Re-check the priority of the issues whose structural importance moved".into(),
                message: format!("{} PageRank changes detected", changes.len()),
                details: changes,
                detected_at: Some(now.to_string()),
                ..Default::default()
            });
        }
    }

    // Staleness: check open/in_progress issues against per-label thresholds.
    check_staleness(&mut r, cfg, issues, now);

    // Blocking cascade: BFS downstream through blocked issues.
    check_blocking_cascade(&mut r, cfg, issues, now);

    // Go runs checkHighImpactUnblock immediately after checkBlockingCascade
    // (pkg/drift/drift.go:292), with only the non-gated checkVelocityDrop
    // between the graph families. The alert array is in generation order, so
    // this position is load-bearing: the frozen corpus carries every
    // blocking_cascade alert before any high_impact_unblock alert.
    check_high_impact_unblock(&mut r, cfg, issues, now);

    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(
        nodes: usize,
        edges: usize,
        density: f64,
        blocked: usize,
        actionable: usize,
    ) -> BaselineStats {
        BaselineStats {
            node_count: nodes,
            edge_count: edges,
            density,
            open: nodes,
            closed: 0,
            blocked,
            cycle_count: 0,
            actionable,
            pagerank: BTreeMap::new(),
        }
    }

    #[test]
    fn no_drift_on_identical_snapshots() {
        let s = snap(100, 120, 0.05, 3, 40);
        let r = calculate(
            &s,
            &s,
            &DriftConfig::default(),
            &[],
            &[],
            jiff::Timestamp::now(),
        );
        assert!(!r.has_drift);
        assert_eq!(r.exit_code(), 0);
    }

    #[test]
    fn new_cycle_is_critical_exit_one() {
        let s = snap(10, 10, 0.1, 0, 5);
        let r = calculate(
            &s,
            &s,
            &DriftConfig::default(),
            &[vec!["A".into(), "B".into(), "A".into()]],
            &[],
            jiff::Timestamp::now(),
        );
        assert!(r.has_drift);
        assert_eq!(r.critical_count, 1);
        assert_eq!(r.exit_code(), 1);
    }

    #[test]
    fn density_growth_warning_at_fifty_pct() {
        let base = snap(100, 120, 0.04, 0, 40);
        let cur = snap(100, 130, 0.064, 0, 40); // +60%
        let r = calculate(
            &base,
            &cur,
            &DriftConfig::default(),
            &[],
            &[],
            jiff::Timestamp::now(),
        );
        assert!(r
            .alerts
            .iter()
            .any(|a| a.alert_type == AlertType::DensityGrowth && a.severity == Severity::Warning));
        assert_eq!(r.exit_code(), 2);
    }

    #[test]
    fn density_growth_info_at_twenty_pct() {
        let base = snap(100, 120, 0.05, 0, 40);
        let cur = snap(100, 125, 0.0625, 0, 40); // +25% -> info band
        let r = calculate(
            &base,
            &cur,
            &DriftConfig::default(),
            &[],
            &[],
            jiff::Timestamp::now(),
        );
        assert!(r
            .alerts
            .iter()
            .any(|a| a.alert_type == AlertType::DensityGrowth && a.severity == Severity::Info));
        assert_eq!(r.exit_code(), 0); // info doesn't set exit code
    }

    #[test]
    fn blocked_increase_warning_at_plus_five() {
        let base = snap(100, 120, 0.05, 2, 40);
        let cur = snap(100, 120, 0.05, 8, 34); // +6 blocked; actionable -15% (<30 no warn)
        let r = calculate(
            &base,
            &cur,
            &DriftConfig::default(),
            &[],
            &[],
            jiff::Timestamp::now(),
        );
        assert!(r
            .alerts
            .iter()
            .any(|a| a.alert_type == AlertType::BlockedIncrease));
        assert_eq!(r.exit_code(), 2);
    }

    #[test]
    fn actionable_drop_warning_at_minus_thirty_pct() {
        let base = snap(100, 120, 0.05, 0, 40);
        let cur = snap(100, 120, 0.05, 0, 25); // -37.5%
        let r = calculate(
            &base,
            &cur,
            &DriftConfig::default(),
            &[],
            &[],
            jiff::Timestamp::now(),
        );
        assert!(r.alerts.iter().any(
            |a| a.alert_type == AlertType::ActionableChange && a.severity == Severity::Warning
        ));
    }

    #[test]
    fn pagerank_shift_warning() {
        let mut base = snap(10, 10, 0.1, 0, 5);
        base.pagerank.insert("X-1".into(), 0.10);
        let mut cur = snap(10, 10, 0.1, 0, 5);
        cur.pagerank.insert("X-1".into(), 0.20); // +100%
        let r = calculate(
            &base,
            &cur,
            &DriftConfig::default(),
            &[],
            &[],
            jiff::Timestamp::now(),
        );
        assert!(r
            .alerts
            .iter()
            .any(|a| a.alert_type == AlertType::PagerankChange));
    }

    // -- StaleIssue tests ---------------------------------------------------

    fn stale_issue(id: &str, status: bv_core::model::Status, updated: &str) -> Issue {
        Issue {
            id: id.to_string(),
            content_hash: String::new(),
            title: format!("issue {id}"),
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
            updated_at: Some(updated.into()),
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
            source_repo: String::new(),
        }
    }

    #[test]
    fn stale_open_issue_triggers_warning_after_fourteen_days() {
        let s = snap(10, 10, 0.1, 0, 5);
        // 20 days ago: exceeds 14-day warning but not 30-day critical.
        // Computed relative to `now` so the test doesn't rot as wall-clock moves.
        let now = jiff::Timestamp::now();
        let updated = (now - jiff::SignedDuration::from_secs(20 * 86400)).to_string();
        let issue = stale_issue("X-1", bv_core::model::Status::Open, &updated);
        let r = calculate(&s, &s, &DriftConfig::default(), &[], &[issue], now);
        assert!(
            r.alerts
                .iter()
                .any(|a| a.alert_type == AlertType::StaleIssue && a.severity == Severity::Warning),
            "expected stale warning for 20-day inactive issue"
        );
    }

    #[test]
    fn stale_open_issue_triggers_critical_after_thirty_days() {
        let s = snap(10, 10, 0.1, 0, 5);
        // 40 days ago: exceeds 30-day critical.
        // Computed relative to `now` so the test doesn't rot as wall-clock moves.
        let now = jiff::Timestamp::now();
        let updated = (now - jiff::SignedDuration::from_secs(40 * 86400)).to_string();
        let issue = stale_issue("X-2", bv_core::model::Status::Open, &updated);
        let r = calculate(&s, &s, &DriftConfig::default(), &[], &[issue], now);
        assert!(
            r.alerts
                .iter()
                .any(|a| a.alert_type == AlertType::StaleIssue && a.severity == Severity::Critical),
            "expected stale critical for 40-day inactive issue"
        );
    }

    #[test]
    fn in_progress_stale_uses_halved_thresholds() {
        let s = snap(10, 10, 0.1, 0, 5);
        // 8 days ago: below 14-day warning, but in_progress multiplier 0.5
        // makes effective warn = 7 days -> should trigger.
        // Computed relative to `now` so the test doesn't rot as wall-clock moves.
        let now = jiff::Timestamp::now();
        let updated = (now - jiff::SignedDuration::from_secs(8 * 86400)).to_string();
        let issue = stale_issue("X-3", bv_core::model::Status::InProgress, &updated);
        let r = calculate(&s, &s, &DriftConfig::default(), &[], &[issue], now);
        assert!(
            r.alerts
                .iter()
                .any(|a| a.alert_type == AlertType::StaleIssue),
            "expected stale alert for in_progress issue with halved thresholds"
        );
    }

    #[test]
    fn stale_issue_skipped_when_disabled() {
        let s = snap(10, 10, 0.1, 0, 5);
        let cfg = DriftConfig {
            disabled_alerts: vec!["stale_issue".into()],
            ..Default::default()
        };
        let issue = stale_issue("X-4", bv_core::model::Status::Open, "2026-07-01T00:00:00Z");
        let r = calculate(&s, &s, &cfg, &[], &[issue], jiff::Timestamp::now());
        assert!(
            !r.alerts
                .iter()
                .any(|a| a.alert_type == AlertType::StaleIssue),
            "stale_issue disabled -- no alert expected"
        );
    }

    #[test]
    fn closed_issue_not_flagged_stale() {
        let s = snap(10, 10, 0.1, 0, 5);
        let issue = stale_issue(
            "X-5",
            bv_core::model::Status::Closed,
            "2026-07-01T00:00:00Z",
        );
        let r = calculate(
            &s,
            &s,
            &DriftConfig::default(),
            &[],
            &[issue],
            jiff::Timestamp::now(),
        );
        assert!(
            !r.alerts
                .iter()
                .any(|a| a.alert_type == AlertType::StaleIssue),
            "closed issues must not trigger stale alerts"
        );
    }

    // -- BlockingCascade tests ----------------------------------------------

    fn casc_issue(id: &str, status: bv_core::model::Status, priority: i32) -> Issue {
        Issue {
            id: id.to_string(),
            content_hash: String::new(),
            title: format!("issue {id}"),
            description: String::new(),
            design: String::new(),
            acceptance_criteria: String::new(),
            notes: String::new(),
            status,
            priority,
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
            source_repo: String::new(),
        }
    }

    fn casc_dep(from: &str, to: &str) -> bv_core::model::Dependency {
        bv_core::model::Dependency {
            issue_id: from.to_string(),
            depends_on_id: to.to_string(),
            depends_on_legacy: String::new(),
            target_id_legacy: String::new(),
            r#type: bv_core::model::DependencyType::Blocks,
            created_at: None,
            created_by: String::new(),
        }
    }

    #[test]
    fn blocking_cascade_info_at_three_unblocks() {
        let s = snap(10, 10, 0.1, 0, 5);
        // Blocker X-1 is depended on by X-2, X-3, X-4 (3 dependents).
        let mut dependent2 = casc_issue("X-2", bv_core::model::Status::Open, 2);
        dependent2.dependencies.push(casc_dep("X-2", "X-1"));
        let mut dependent3 = casc_issue("X-3", bv_core::model::Status::Open, 3);
        dependent3.dependencies.push(casc_dep("X-3", "X-1"));
        let mut dependent4 = casc_issue("X-4", bv_core::model::Status::Open, 4);
        dependent4.dependencies.push(casc_dep("X-4", "X-1"));
        let blocker = casc_issue("X-1", bv_core::model::Status::Open, 1);
        let issues = vec![blocker, dependent2, dependent3, dependent4];

        let r = calculate(
            &s,
            &s,
            &DriftConfig::default(),
            &[],
            &issues,
            jiff::Timestamp::now(),
        );
        let cascade_alerts: Vec<_> = r
            .alerts
            .iter()
            .filter(|a| a.alert_type == AlertType::BlockingCascade)
            .collect();
        assert_eq!(
            cascade_alerts.len(),
            1,
            "expected one cascade alert for X-1"
        );
        assert_eq!(cascade_alerts[0].severity, Severity::Info);
        assert_eq!(cascade_alerts[0].unblocks_count, Some(3));
        assert_eq!(cascade_alerts[0].issue_id, "X-1");
    }

    #[test]
    fn blocking_cascade_warning_at_five_unblocks() {
        let s = snap(10, 10, 0.1, 0, 5);
        let blocker = casc_issue("X-1", bv_core::model::Status::Open, 1);
        let mut issues = vec![blocker];
        for i in 2..=6 {
            let mut dep = casc_issue(&format!("X-{i}"), bv_core::model::Status::Open, 2);
            dep.dependencies.push(casc_dep(&format!("X-{i}"), "X-1"));
            issues.push(dep);
        }
        let r = calculate(
            &s,
            &s,
            &DriftConfig::default(),
            &[],
            &issues,
            jiff::Timestamp::now(),
        );
        let cascade = r
            .alerts
            .iter()
            .find(|a| a.alert_type == AlertType::BlockingCascade);
        assert!(cascade.is_some(), "expected cascade alert");
        assert_eq!(cascade.unwrap().severity, Severity::Warning);
        assert_eq!(cascade.unwrap().unblocks_count, Some(5));
    }

    #[test]
    fn blocking_cascade_skipped_when_disabled() {
        let s = snap(10, 10, 0.1, 0, 5);
        let cfg = DriftConfig {
            disabled_alerts: vec!["blocking_cascade".into()],
            ..Default::default()
        };
        let blocker = casc_issue("X-1", bv_core::model::Status::Open, 1);
        let mut dep = casc_issue("X-2", bv_core::model::Status::Open, 2);
        dep.dependencies.push(casc_dep("X-2", "X-1"));
        let mut dep2 = casc_issue("X-3", bv_core::model::Status::Open, 3);
        dep2.dependencies.push(casc_dep("X-3", "X-1"));
        let mut dep3 = casc_issue("X-4", bv_core::model::Status::Open, 4);
        dep3.dependencies.push(casc_dep("X-4", "X-1"));
        let issues = vec![blocker, dep, dep2, dep3];

        let r = calculate(&s, &s, &cfg, &[], &issues, jiff::Timestamp::now());
        assert!(
            !r.alerts
                .iter()
                .any(|a| a.alert_type == AlertType::BlockingCascade),
            "blocking_cascade disabled -- no alert expected"
        );
    }

    #[test]
    fn closed_dependent_not_counted_in_cascade() {
        let s = snap(10, 10, 0.1, 0, 5);
        let blocker = casc_issue("X-1", bv_core::model::Status::Open, 1);
        let mut dep_open = casc_issue("X-2", bv_core::model::Status::Open, 2);
        dep_open.dependencies.push(casc_dep("X-2", "X-1"));
        let mut dep_closed = casc_issue("X-3", bv_core::model::Status::Closed, 3);
        dep_closed.dependencies.push(casc_dep("X-3", "X-1"));
        let issues = vec![blocker, dep_open, dep_closed];

        let r = calculate(
            &s,
            &s,
            &DriftConfig::default(),
            &[],
            &issues,
            jiff::Timestamp::now(),
        );
        // Only X-2 is open, so unblocks = 1, below info threshold (3).
        assert!(
            !r.alerts
                .iter()
                .any(|a| a.alert_type == AlertType::BlockingCascade),
            "closed dependents must not be counted"
        );
    }

    #[test]
    fn already_blocked_dependent_not_counted_in_cascade() {
        let s = snap(10, 10, 0.1, 0, 5);
        // X-1 and X-BOTH are blockers; X-2 depends on both.
        let blocker1 = casc_issue("X-1", bv_core::model::Status::Open, 1);
        let blocker2 = casc_issue("X-BOTH", bv_core::model::Status::Open, 1);
        let mut dep = casc_issue("X-2", bv_core::model::Status::Open, 2);
        dep.dependencies.push(casc_dep("X-2", "X-1"));
        dep.dependencies.push(casc_dep("X-2", "X-BOTH"));
        let issues = vec![blocker1, blocker2, dep];

        let r = calculate(
            &s,
            &s,
            &DriftConfig::default(),
            &[],
            &issues,
            jiff::Timestamp::now(),
        );
        // Completing X-1 still leaves X-2 blocked by X-BOTH, so unblocks = 0.
        assert!(
            !r.alerts
                .iter()
                .any(|a| a.alert_type == AlertType::BlockingCascade && a.issue_id == "X-1"),
            "X-2 is still blocked by X-BOTH -- X-1 must not appear"
        );
    }

    #[test]
    fn blocking_cascade_emits_in_bytewise_id_order_not_numeric() {
        // Go's `sortedByID` (pkg/drift/drift.go:812-816) compares IDs with `<`,
        // so "XL-110" and "XL-1237" sort BEFORE "XL-14". The frozen corpus
        // (golden/xl_2500____robot_alerts.json) carries exactly that order; a
        // numeric sort would interleave them differently and change bytes.
        let s = snap(10, 10, 0.1, 0, 5);
        let mut issues = Vec::new();
        for id in ["XL-1", "XL-14", "XL-110", "XL-1237"] {
            let blocker = casc_issue(id, bv_core::model::Status::Open, 1);
            for i in 0..3 {
                let dep_id = format!("{id}-d{i}");
                let mut dep = casc_issue(&dep_id, bv_core::model::Status::Open, 2);
                dep.dependencies.push(casc_dep(&dep_id, id));
                issues.push(dep);
            }
            issues.push(blocker);
        }
        let r = calculate(
            &s,
            &s,
            &DriftConfig::default(),
            &[],
            &issues,
            jiff::Timestamp::now(),
        );
        let order: Vec<&str> = r
            .alerts
            .iter()
            .filter(|a| a.alert_type == AlertType::BlockingCascade)
            .map(|a| a.issue_id.as_str())
            .collect();
        assert_eq!(order, vec!["XL-1", "XL-110", "XL-1237", "XL-14"]);
    }

    // -- HighImpactUnblock tests -------------------------------------------

    /// A blocker with `n` P1 dependents and `m` low-priority ones, so the
    /// unblocks count (`high_impact_unblock_min` = 3) and the urgent count
    /// (drives info vs warning) can be varied independently.
    fn high_impact_issue(id: &str, urgent: usize, routine: usize) -> Vec<Issue> {
        let mut issues = vec![casc_issue(id, bv_core::model::Status::Open, 1)];
        for i in 0..urgent {
            let dep_id = format!("{id}-u{i}");
            let mut dep = casc_issue(&dep_id, bv_core::model::Status::Open, 1);
            dep.dependencies.push(casc_dep(&dep_id, id));
            issues.push(dep);
        }
        for i in 0..routine {
            let dep_id = format!("{id}-r{i}");
            let mut dep = casc_issue(&dep_id, bv_core::model::Status::Open, 3);
            dep.dependencies.push(casc_dep(&dep_id, id));
            issues.push(dep);
        }
        issues
    }

    #[test]
    fn high_impact_unblock_warns_on_two_urgent_downstream_items() {
        let s = snap(10, 10, 0.1, 0, 5);
        // 3 unblocks, 2 of them P1 → warning (Go drift.go:940-943).
        let issues = high_impact_issue("H-1", 2, 1);
        let r = calculate(
            &s,
            &s,
            &DriftConfig::default(),
            &[],
            &issues,
            jiff::Timestamp::now(),
        );
        let alert = r
            .alerts
            .iter()
            .find(|a| a.alert_type == AlertType::HighImpactUnblock)
            .expect("expected a high_impact_unblock alert");
        assert_eq!(alert.severity, Severity::Warning);
        assert_eq!(alert.issue_id, "H-1");
        assert_eq!(alert.unblocks_count, Some(3));
        assert_eq!(alert.details, vec!["H-1-u0", "H-1-u1"]);
        assert_eq!(
            alert.message,
            "Completing H-1 unblocks 3 item(s), 2 of them at P1 or higher"
        );
        assert_eq!(
            alert.suggested_action,
            "Schedule this issue next; it releases high-priority downstream work"
        );
        // Go leaves BaselineVal/CurrentVal/Delta/DownstreamPrioritySum unset.
        assert_eq!(alert.baseline_val, None);
        assert_eq!(alert.current_val, None);
        assert_eq!(alert.delta, None);
        assert_eq!(alert.downstream_priority_sum, None);
    }

    #[test]
    fn high_impact_unblock_info_on_a_single_urgent_downstream_item() {
        let s = snap(10, 10, 0.1, 0, 5);
        // 3 unblocks, exactly 1 of them P1 → info.
        let issues = high_impact_issue("H-2", 1, 2);
        let r = calculate(
            &s,
            &s,
            &DriftConfig::default(),
            &[],
            &issues,
            jiff::Timestamp::now(),
        );
        let alert = r
            .alerts
            .iter()
            .find(|a| a.alert_type == AlertType::HighImpactUnblock)
            .expect("expected a high_impact_unblock alert");
        assert_eq!(alert.severity, Severity::Info);
        assert_eq!(alert.details, vec!["H-2-u0"]);
    }

    #[test]
    fn high_impact_unblock_skipped_when_no_downstream_is_urgent() {
        let s = snap(10, 10, 0.1, 0, 5);
        // 3 unblocks but every one is P3 — above high_impact_priority_max (1).
        let issues = high_impact_issue("H-3", 0, 3);
        let r = calculate(
            &s,
            &s,
            &DriftConfig::default(),
            &[],
            &issues,
            jiff::Timestamp::now(),
        );
        assert!(
            !r.alerts
                .iter()
                .any(|a| a.alert_type == AlertType::HighImpactUnblock),
            "no downstream at P1 or higher -- blocking_cascade still fires, unblock must not"
        );
        assert!(r
            .alerts
            .iter()
            .any(|a| a.alert_type == AlertType::BlockingCascade));
    }

    #[test]
    fn high_impact_unblock_below_min_unblocks_is_not_alerted() {
        let s = snap(10, 10, 0.1, 0, 5);
        // Only 2 unblocks, both P1 — under high_impact_unblock_min (3).
        let issues = high_impact_issue("H-4", 2, 0);
        let r = calculate(
            &s,
            &s,
            &DriftConfig::default(),
            &[],
            &issues,
            jiff::Timestamp::now(),
        );
        assert!(
            !r.alerts
                .iter()
                .any(|a| a.alert_type == AlertType::HighImpactUnblock),
            "2 unblocks is under the threshold of 3"
        );
    }

    #[test]
    fn high_impact_unblock_skipped_when_disabled() {
        let s = snap(10, 10, 0.1, 0, 5);
        let cfg = DriftConfig {
            disabled_alerts: vec!["high_impact_unblock".into()],
            ..Default::default()
        };
        let issues = high_impact_issue("H-5", 2, 1);
        let r = calculate(&s, &s, &cfg, &[], &issues, jiff::Timestamp::now());
        assert!(!r
            .alerts
            .iter()
            .any(|a| a.alert_type == AlertType::HighImpactUnblock));
    }

    #[test]
    fn high_impact_unblock_runs_after_blocking_cascade() {
        // Go's Calculate runs checkBlockingCascade then checkHighImpactUnblock
        // (pkg/drift/drift.go:291-292) and the alert array is in generation
        // order, so a qualifying issue must contribute cascade-then-unblock.
        let s = snap(10, 10, 0.1, 0, 5);
        let issues = high_impact_issue("H-6", 2, 1);
        let r = calculate(
            &s,
            &s,
            &DriftConfig::default(),
            &[],
            &issues,
            jiff::Timestamp::now(),
        );
        let kinds: Vec<AlertType> = r.alerts.iter().map(|a| a.alert_type).collect();
        let cascade_at = kinds
            .iter()
            .position(|k| *k == AlertType::BlockingCascade)
            .expect("cascade alert");
        let unblock_at = kinds
            .iter()
            .position(|k| *k == AlertType::HighImpactUnblock)
            .expect("unblock alert");
        assert!(
            cascade_at < unblock_at,
            "blocking_cascade must precede high_impact_unblock, got {kinds:?}"
        );
    }

    // -- new_cycle shape ----------------------------------------------------

    #[test]
    fn new_cycle_reports_count_and_arrow_separated_details() {
        let base = snap(10, 10, 0.1, 0, 5);
        let mut cur = snap(10, 10, 0.1, 0, 5);
        cur.cycle_count = 2;
        let r = calculate(
            &base,
            &cur,
            &DriftConfig::default(),
            &[vec!["A".into(), "B".into(), "A".into()], vec!["C".into()]],
            &[],
            jiff::Timestamp::now(),
        );
        let alert = r
            .alerts
            .iter()
            .find(|a| a.alert_type == AlertType::NewCycle)
            .expect("expected a new_cycle alert");
        assert_eq!(alert.message, "2 new cycle(s) detected");
        assert_eq!(alert.details, vec!["A \u{2192} B \u{2192} A", "C"]);
        assert_eq!(alert.current_val, Some(2.0));
        assert_eq!(alert.delta, Some(2.0));
        assert_eq!(alert.severity, Severity::Critical);
        assert_eq!(
            alert.suggested_action,
            "Break the cycle by removing or reversing one dependency edge (bv --robot-suggest lists cycle-break candidates)"
        );
    }

    #[test]
    fn new_cycle_skipped_when_disabled() {
        let s = snap(10, 10, 0.1, 0, 5);
        let cfg = DriftConfig {
            disabled_alerts: vec!["new_cycle".into()],
            ..Default::default()
        };
        let r = calculate(
            &s,
            &s,
            &cfg,
            &[vec!["A".into(), "B".into(), "A".into()]],
            &[],
            jiff::Timestamp::now(),
        );
        assert!(!r.alerts.iter().any(|a| a.alert_type == AlertType::NewCycle));
    }

    #[test]
    fn zero_downstream_priority_sum_is_omitted_like_go_omitempty() {
        // Go declares UnblocksCount/DownstreamPrioritySum as plain ints with
        // `omitempty` (pkg/drift/drift.go:95-96), so a present-but-zero value
        // still leaves the key out of the payload.
        let alert = Alert {
            alert_type: AlertType::BlockingCascade,
            severity: Severity::Info,
            message: "m".into(),
            unblocks_count: Some(3),
            downstream_priority_sum: Some(0),
            ..Default::default()
        };
        let json = serde_json::to_value(&alert).unwrap();
        assert_eq!(json["unblocks_count"], serde_json::json!(3));
        assert!(
            json.get("downstream_priority_sum").is_none(),
            "Go omits a zero downstream_priority_sum, got {json}"
        );
    }
}
