//! Snapshot diff — port of Go `pkg/analysis/diff.go` (bv v0.25.0).
//!
//! `--robot-diff` is a before/after *graph* diff, not an issue-list diff: it
//! builds a `Snapshot` for the historical revision and one for the live tree,
//! runs the two-phase analyzer over each, and reports what changed between
//! them (issues, cycles, metric deltas, and a health trend).
//!
//! This is a sibling of [`crate::diff`], which is the older v0.20.0 flat
//! issue-list comparison the TUI time-travel view still consumes. The two
//! shapes are unrelated, so both live side by side.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use bv_core::model::{Issue, Status};
use serde::{Serialize, Serializer};

use crate::analyzer::{self, AnalysisBudget};

/// Go's zero `time.Time` as `encoding/json` renders it. The historical side
/// of a `--diff-since` snapshot is built with `NewSnapshotAt(issues,
/// time.Time{}, rev)`, so `from_timestamp` is always this value.
pub const GO_ZERO_TIME: &str = "0001-01-01T00:00:00Z";

/// The state of the issue graph at a point in time.
pub struct Snapshot {
    /// RFC3339 string; Go carries a `time.Time` and serializes it directly.
    pub timestamp: String,
    /// Git SHA or ref. Omitted from the diff JSON when empty (`omitempty`).
    pub revision: String,
    pub issues: Vec<Issue>,
    /// `None` mirrors Go's nil `GraphStats.cycles` (skipped or timed out).
    pub cycles: Option<Vec<Vec<String>>>,
    pub page_rank: Option<BTreeMap<String, f64>>,
    pub betweenness: Option<BTreeMap<String, f64>>,
    pub total_count: usize,
    pub open_count: usize,
    pub closed_count: usize,
    pub blocked_count: usize,
}

impl Snapshot {
    /// Go: `NewSnapshotAt(issues, timestamp, revision)` — analyze, then count.
    pub fn new(issues: Vec<Issue>, timestamp: String, revision: String) -> Self {
        let graph = Arc::new(analyzer::build_graph(&issues));
        let phase1 = analyzer::analyze_phase1(&graph);
        let budget = AnalysisBudget {
            density: phase1.density,
            ..AnalysisBudget::default()
        };
        // Go's `Analyze()` is `AnalyzeAsync` + `WaitForPhase2`; the blocking
        // variant is the same computation without the goroutine.
        let (_status, phase2) = analyzer::analyze_phase2_blocking(graph, &budget);

        let mut snap = Self {
            timestamp,
            revision,
            issues,
            cycles: phase2.cycles,
            page_rank: phase2.page_rank,
            betweenness: phase2.betweenness.map(drop_zero_scores),
            total_count: 0,
            open_count: 0,
            closed_count: 0,
            blocked_count: 0,
        };
        snap.compute_counts();
        snap
    }

    /// Go: `Snapshot.computeCounts` — blocked counts as open, and every
    /// non-closed status (deferred, draft, pinned, hooked, review) is open
    /// work. Only `closed`/`tombstone` are closed.
    fn compute_counts(&mut self) {
        self.total_count = self.issues.len();
        for issue in &self.issues {
            match issue.status {
                Status::Closed | Status::Tombstone => self.closed_count += 1,
                Status::Blocked => {
                    self.blocked_count += 1;
                    self.open_count += 1;
                }
                _ => self.open_count += 1,
            }
        }
    }
}

/// The differences between two snapshots.
#[derive(Debug, Clone, Serialize)]
pub struct SnapshotDiff {
    pub from_timestamp: String,
    pub to_timestamp: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub from_revision: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub to_revision: String,
    #[serde(serialize_with = "serialize_null_if_empty")]
    pub new_issues: Vec<Issue>,
    #[serde(serialize_with = "serialize_null_if_empty")]
    pub closed_issues: Vec<Issue>,
    #[serde(serialize_with = "serialize_null_if_empty")]
    pub removed_issues: Vec<Issue>,
    #[serde(serialize_with = "serialize_null_if_empty")]
    pub reopened_issues: Vec<Issue>,
    #[serde(serialize_with = "serialize_null_if_empty")]
    pub modified_issues: Vec<ModifiedIssue>,
    #[serde(serialize_with = "serialize_null_if_empty")]
    pub new_cycles: Vec<Vec<String>>,
    #[serde(serialize_with = "serialize_null_if_empty")]
    pub resolved_cycles: Vec<Vec<String>>,
    pub metric_deltas: MetricDeltas,
    pub summary: DiffSummary,
}

/// What changed in an issue between the two snapshots.
#[derive(Debug, Clone, Serialize)]
pub struct ModifiedIssue {
    pub issue_id: String,
    pub title: String,
    pub changes: Vec<FieldChange>,
}

/// A single field change. Long text fields report `(modified)` on both sides
/// rather than inlining the whole body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FieldChange {
    pub field: &'static str,
    pub old_value: String,
    pub new_value: String,
}

/// Changes in the graph-level metrics. `total_edges` and `component_count`
/// are declared by Go but never assigned, so they are always 0 — kept for
/// schema parity.
#[derive(Debug, Clone, Serialize)]
pub struct MetricDeltas {
    pub total_issues: i64,
    pub open_issues: i64,
    pub closed_issues: i64,
    pub blocked_issues: i64,
    pub total_edges: i64,
    pub cycle_count: i64,
    pub component_count: i64,
    pub avg_pagerank: f64,
    pub avg_betweenness: f64,
}

/// Quick overview of the diff, including the health trend.
#[derive(Debug, Clone, Serialize)]
pub struct DiffSummary {
    pub total_changes: i64,
    pub issues_added: i64,
    pub issues_closed: i64,
    pub issues_removed: i64,
    pub issues_reopened: i64,
    pub issues_modified: i64,
    pub cycles_introduced: i64,
    pub cycles_resolved: i64,
    pub net_issue_change: i64,
    pub health_trend: &'static str,
}

/// Go's nil slice marshals to `null`, and every list in `CompareSnapshots` is
/// grown by `append` onto a nil slice — so an empty result is always `null`,
/// never `[]`.
fn serialize_null_if_empty<T, S>(value: &[T], serializer: S) -> Result<S::Ok, S::Error>
where
    T: Serialize,
    S: Serializer,
{
    if value.is_empty() {
        serializer.serialize_none()
    } else {
        serializer.collect_seq(value.iter())
    }
}

/// Go: `CompareSnapshots(from, to)`.
pub fn compare_snapshots(from: &Snapshot, to: &Snapshot) -> SnapshotDiff {
    let from_map: BTreeMap<&str, &Issue> = from.issues.iter().map(|i| (i.id.as_str(), i)).collect();
    let to_map: BTreeMap<&str, &Issue> = to.issues.iter().map(|i| (i.id.as_str(), i)).collect();

    let mut new_issues = Vec::new();
    let mut closed_issues = Vec::new();
    let mut removed_issues = Vec::new();
    let mut reopened_issues = Vec::new();
    let mut modified_issues = Vec::new();

    for (id, to_issue) in &to_map {
        let Some(from_issue) = from_map.get(id) else {
            new_issues.push((*to_issue).clone());
            continue;
        };

        let changes = detect_changes(from_issue, to_issue);

        // A closed-like transition is reported in its own list, never both.
        let is_status_change =
            if !is_closed_like(from_issue.status) && is_closed_like(to_issue.status) {
                closed_issues.push((*to_issue).clone());
                true
            } else if is_closed_like(from_issue.status) && !is_closed_like(to_issue.status) {
                reopened_issues.push((*to_issue).clone());
                true
            } else {
                false
            };

        // On a status transition, drop the `status` entry so the lists stay
        // disjoint; other field edits still land in `modified_issues`.
        if is_status_change {
            let other: Vec<FieldChange> = changes
                .into_iter()
                .filter(|c| c.field != "status")
                .collect();
            if !other.is_empty() {
                modified_issues.push(ModifiedIssue {
                    issue_id: (*id).to_string(),
                    title: to_issue.title.clone(),
                    changes: other,
                });
            }
        } else if !changes.is_empty() {
            modified_issues.push(ModifiedIssue {
                issue_id: (*id).to_string(),
                title: to_issue.title.clone(),
                changes,
            });
        }
    }

    for (id, from_issue) in &from_map {
        if !to_map.contains_key(id) {
            removed_issues.push((*from_issue).clone());
        }
    }

    new_issues.sort_by(|a, b| a.id.cmp(&b.id));
    closed_issues.sort_by(|a, b| a.id.cmp(&b.id));
    removed_issues.sort_by(|a, b| a.id.cmp(&b.id));
    reopened_issues.sort_by(|a, b| a.id.cmp(&b.id));
    modified_issues.sort_by(|a, b| a.issue_id.cmp(&b.issue_id));

    let (new_cycles, resolved_cycles) = compare_cycles(from.cycles.as_ref(), to.cycles.as_ref());
    let metric_deltas = calculate_metric_deltas(from, to);
    let summary = calculate_summary(
        &new_issues,
        &closed_issues,
        &removed_issues,
        &reopened_issues,
        &modified_issues,
        &new_cycles,
        &resolved_cycles,
        metric_deltas.blocked_issues,
    );

    SnapshotDiff {
        from_timestamp: from.timestamp.clone(),
        to_timestamp: to.timestamp.clone(),
        from_revision: from.revision.clone(),
        to_revision: to.revision.clone(),
        new_issues,
        closed_issues,
        removed_issues,
        reopened_issues,
        modified_issues,
        new_cycles,
        resolved_cycles,
        metric_deltas,
        summary,
    }
}

/// Go: `isClosedLikeStatus` — `closed` or `tombstone`.
fn is_closed_like(status: Status) -> bool {
    matches!(status, Status::Closed | Status::Tombstone)
}

/// Drop zero-valued scores from a betweenness map.
///
/// gonum's `network.Betweenness` only returns nodes that actually score above
/// zero, so Go's `GraphStats.Betweenness()` map holds 25 entries for a
/// 46-node graph. Rust's analyzer populates every node, which would change
/// the denominator of `avgMapValue` and make `avg_betweenness` diverge. The
/// zero entries carry no information for the average, so dropping them
/// reproduces Go's map exactly. PageRank needs no such filter — gonum returns
/// a score for every node there.
fn drop_zero_scores(scores: BTreeMap<String, f64>) -> BTreeMap<String, f64> {
    scores.into_iter().filter(|(_, v)| *v != 0.0).collect()
}

/// Go: `detectChanges`. Field order is the wire order of `changes[]`.
fn detect_changes(from: &Issue, to: &Issue) -> Vec<FieldChange> {
    let mut changes = Vec::new();

    if from.title != to.title {
        changes.push(FieldChange {
            field: "title",
            old_value: from.title.clone(),
            new_value: to.title.clone(),
        });
    }
    if from.status != to.status {
        changes.push(FieldChange {
            field: "status",
            old_value: from.status.as_str().to_string(),
            new_value: to.status.as_str().to_string(),
        });
    }
    if from.priority != to.priority {
        changes.push(FieldChange {
            field: "priority",
            old_value: priority_string(from.priority),
            new_value: priority_string(to.priority),
        });
    }
    if from.assignee != to.assignee {
        changes.push(FieldChange {
            field: "assignee",
            old_value: from.assignee.clone(),
            new_value: to.assignee.clone(),
        });
    }
    if from.issue_type != to.issue_type {
        changes.push(FieldChange {
            field: "type",
            old_value: from.issue_type.clone(),
            new_value: to.issue_type.clone(),
        });
    }

    // Long text bodies report only that they changed.
    for (field, old, new) in [
        ("description", &from.description, &to.description),
        ("design", &from.design, &to.design),
        (
            "acceptance_criteria",
            &from.acceptance_criteria,
            &to.acceptance_criteria,
        ),
        ("notes", &from.notes, &to.notes),
    ] {
        if old != new {
            changes.push(FieldChange {
                field,
                old_value: "(modified)".to_string(),
                new_value: "(modified)".to_string(),
            });
        }
    }

    let from_deps = dependency_set(&from.dependencies);
    let to_deps = dependency_set(&to.dependencies);
    if from_deps != to_deps {
        changes.push(FieldChange {
            field: "dependencies",
            old_value: format_deps(&from_deps),
            new_value: format_deps(&to_deps),
        });
    }

    let from_labels: BTreeSet<&str> = from.labels.iter().map(String::as_str).collect();
    let to_labels: BTreeSet<&str> = to.labels.iter().map(String::as_str).collect();
    if from_labels != to_labels {
        changes.push(FieldChange {
            field: "labels",
            old_value: format_labels(&from.labels),
            new_value: format_labels(&to.labels),
        });
    }

    changes
}

/// Go: `dependencySet` — keyed on `id:type` so a type change on the same edge
/// still registers as a difference.
fn dependency_set(deps: &[bv_core::model::Dependency]) -> BTreeSet<String> {
    deps.iter()
        .filter_map(|dep| {
            let id = dep.effective_depends_on();
            if id.is_empty() {
                None
            } else {
                Some(format!("{}:{}", id, dep.r#type.as_str()))
            }
        })
        .collect()
}

/// Go: `formatDeps`.
fn format_deps(deps: &BTreeSet<String>) -> String {
    if deps.is_empty() {
        return "(none)".to_string();
    }
    deps.iter().cloned().collect::<Vec<_>>().join(", ")
}

/// Go: `formatLabels` — sorts a copy; duplicates are preserved.
fn format_labels(labels: &[String]) -> String {
    if labels.is_empty() {
        return "(none)".to_string();
    }
    let mut sorted = labels.to_vec();
    sorted.sort();
    sorted.join(", ")
}

/// Go: `priorityString` — `"P0"`..`"P9"` in the single-digit branch, and plain
/// decimal formatting above that (`"P10"`, `"P-1"`). Both branches produce the
/// same string in Rust, so the split collapses.
fn priority_string(p: i64) -> String {
    format!("P{p}")
}

/// Go: `compareCycles` — matches cycles by their canonical rotation, so the
/// same cycle discovered at a different rotation counts as unchanged.
fn compare_cycles(
    from: Option<&Vec<Vec<String>>>,
    to: Option<&Vec<Vec<String>>>,
) -> (Vec<Vec<String>>, Vec<Vec<String>>) {
    let from_set = cycle_keys(from);
    let to_set = cycle_keys(to);

    let mut new_cycles: Vec<Vec<String>> = to_set
        .iter()
        .filter(|(key, _)| !from_set.contains_key(*key))
        .map(|(_, cycle)| cycle.clone())
        .collect();
    let mut resolved_cycles: Vec<Vec<String>> = from_set
        .iter()
        .filter(|(key, _)| !to_set.contains_key(*key))
        .map(|(_, cycle)| cycle.clone())
        .collect();

    new_cycles.sort_by_key(|c| normalize_cycle(c));
    resolved_cycles.sort_by_key(|c| normalize_cycle(c));

    (new_cycles, resolved_cycles)
}

fn cycle_keys(cycles: Option<&Vec<Vec<String>>>) -> BTreeMap<String, Vec<String>> {
    let mut set = BTreeMap::new();
    for cycle in cycles.into_iter().flatten() {
        set.insert(normalize_cycle(cycle), cycle.clone());
    }
    set
}

/// Go: `normalizeCycle` — rotate to start at the lexicographically smallest
/// member and join with `->`. The first minimum wins, matching Go's strict
/// `<` scan.
fn normalize_cycle(cycle: &[String]) -> String {
    if cycle.is_empty() {
        return String::new();
    }
    let mut min_idx = 0;
    for (i, id) in cycle.iter().enumerate() {
        if *id < cycle[min_idx] {
            min_idx = i;
        }
    }
    (0..cycle.len())
        .map(|i| cycle[(min_idx + i) % cycle.len()].as_str())
        .collect::<Vec<_>>()
        .join("->")
}

/// Go: `calculateMetricDeltas`.
fn calculate_metric_deltas(from: &Snapshot, to: &Snapshot) -> MetricDeltas {
    MetricDeltas {
        total_issues: to.total_count as i64 - from.total_count as i64,
        open_issues: to.open_count as i64 - from.open_count as i64,
        closed_issues: to.closed_count as i64 - from.closed_count as i64,
        blocked_issues: to.blocked_count as i64 - from.blocked_count as i64,
        // Go never assigns these two; they exist only for schema parity.
        total_edges: 0,
        component_count: 0,
        cycle_count: cycle_count(to) - cycle_count(from),
        avg_pagerank: avg_map_value(to.page_rank.as_ref()) - avg_map_value(from.page_rank.as_ref()),
        avg_betweenness: avg_map_value(to.betweenness.as_ref())
            - avg_map_value(from.betweenness.as_ref()),
    }
}

fn cycle_count(s: &Snapshot) -> i64 {
    s.cycles.as_ref().map_or(0, |c| c.len() as i64)
}

/// Go: `avgMapValue`. Sums in sorted-key order so floating-point results are
/// reproducible (`BTreeMap` already iterates sorted).
fn avg_map_value(m: Option<&BTreeMap<String, f64>>) -> f64 {
    let Some(m) = m else { return 0.0 };
    if m.is_empty() {
        return 0.0;
    }
    let mut sum = 0.0f64;
    for v in m.values() {
        sum += *v;
    }
    sum / m.len() as f64
}

/// Go: `calculateSummary(diff *SnapshotDiff)` — reads the tallies straight off
/// the diff it is summarizing.
///
/// The health score weights cycles more heavily than issue count: resolving a
/// cycle is worth +2 and introducing one −3, while closing a single issue is
/// +1. A net drop in blocked issues is +2; a rise is −1. The verdict needs a
/// score strictly above 1 to read "improving" and strictly below −1 to read
/// "degrading"; everything in between (including exactly ±1) is "stable".
#[allow(clippy::too_many_arguments)]
fn calculate_summary(
    new_issues: &[Issue],
    closed_issues: &[Issue],
    removed_issues: &[Issue],
    reopened_issues: &[Issue],
    modified_issues: &[ModifiedIssue],
    new_cycles: &[Vec<String>],
    resolved_cycles: &[Vec<String>],
    blocked_delta: i64,
) -> DiffSummary {
    let added = new_issues.len() as i64;
    let closed = closed_issues.len() as i64;
    let removed = removed_issues.len() as i64;
    let reopened = reopened_issues.len() as i64;
    let modified = modified_issues.len() as i64;
    let cycles_introduced = new_cycles.len() as i64;
    let cycles_resolved = resolved_cycles.len() as i64;

    let total_changes = added + closed + removed + reopened + modified;

    let mut score = 0i64;
    score += cycles_resolved * 2;
    score -= cycles_introduced * 3;
    score += closed;
    score -= reopened;
    if blocked_delta < 0 {
        score += 2;
    } else if blocked_delta > 0 {
        score -= 1;
    }

    let health_trend = if score > 1 {
        "improving"
    } else if score < -1 {
        "degrading"
    } else {
        "stable"
    };

    DiffSummary {
        total_changes,
        issues_added: added,
        issues_closed: closed,
        issues_removed: removed,
        issues_reopened: reopened,
        issues_modified: modified,
        cycles_introduced,
        cycles_resolved,
        net_issue_change: added - removed,
        health_trend,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bv_core::model::{Dependency, DependencyType};

    fn issue(id: &str, status: Status) -> Issue {
        Issue {
            id: id.into(),
            content_hash: String::new(),
            title: format!("Issue {id}"),
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
            labels: vec![],
            dependencies: vec![],
            comments: vec![],
            source_repo: String::new(),
        }
    }

    /// A snapshot without the analyzer, for testing the pure list/summary
    /// logic. Counts are filled the same way `compute_counts` fills them.
    fn plain_snapshot(issues: Vec<Issue>) -> Snapshot {
        let mut s = Snapshot {
            timestamp: GO_ZERO_TIME.to_string(),
            revision: String::new(),
            issues,
            cycles: None,
            page_rank: None,
            betweenness: None,
            total_count: 0,
            open_count: 0,
            closed_count: 0,
            blocked_count: 0,
        };
        s.compute_counts();
        s
    }

    #[test]
    fn counts_blocked_as_open_and_closed_like_as_closed() {
        let s = plain_snapshot(vec![
            issue("a", Status::Open),
            issue("b", Status::Blocked),
            issue("c", Status::Closed),
            issue("d", Status::Tombstone),
            issue("e", Status::Deferred),
        ]);
        assert_eq!(s.total_count, 5);
        assert_eq!(s.open_count, 3, "open + blocked + deferred");
        assert_eq!(s.closed_count, 2, "closed + tombstone");
        assert_eq!(s.blocked_count, 1);
    }

    #[test]
    fn partitions_new_closed_removed_reopened_modified() {
        let from = plain_snapshot(vec![
            issue("gone", Status::Open),   // removed
            issue("shut", Status::Open),   // closed
            issue("back", Status::Closed), // reopened
            issue("edit", Status::Open),   // title edit
            issue("stay", Status::Open),   // untouched
        ]);
        let mut edited = issue("edit", Status::Open);
        edited.title = "Issue EDIT".into();

        let mut closed = issue("shut", Status::Open);
        closed.status = Status::Closed;
        closed.title = "Issue SHUT".into();

        let to = plain_snapshot(vec![
            issue("fresh", Status::Open),
            closed,
            issue("back", Status::Open),
            edited,
            issue("stay", Status::Open),
        ]);

        let d = compare_snapshots(&from, &to);
        assert_eq!(ids(&d.new_issues), vec!["fresh"]);
        assert_eq!(ids(&d.closed_issues), vec!["shut"]);
        assert_eq!(ids(&d.removed_issues), vec!["gone"]);
        assert_eq!(ids(&d.reopened_issues), vec!["back"]);
        assert_eq!(
            d.modified_issues
                .iter()
                .map(|m| m.issue_id.as_str())
                .collect::<Vec<_>>(),
            vec!["edit", "shut"],
            "a closed issue with a title edit is both closed and modified"
        );

        // The status entry is stripped from the closed issue's field changes.
        let shut = d
            .modified_issues
            .iter()
            .find(|m| m.issue_id == "shut")
            .unwrap();
        assert_eq!(
            shut.changes,
            vec![FieldChange {
                field: "title",
                old_value: "Issue shut".into(),
                new_value: "Issue SHUT".into(),
            }]
        );
    }

    #[test]
    fn status_transition_without_other_edits_is_not_modified() {
        let from = plain_snapshot(vec![issue("a", Status::Closed)]);
        let to = plain_snapshot(vec![issue("a", Status::Open)]);
        let d = compare_snapshots(&from, &to);
        assert_eq!(d.reopened_issues.len(), 1);
        assert!(d.modified_issues.is_empty());
    }

    #[test]
    fn net_issue_change_is_added_minus_removed() {
        let from = plain_snapshot(vec![issue("x", Status::Open), issue("y", Status::Open)]);
        let to = plain_snapshot(vec![
            issue("x", Status::Open),
            issue("z", Status::Open),
            issue("w", Status::Open),
        ]);
        let d = compare_snapshots(&from, &to);
        assert_eq!(d.summary.issues_added, 2);
        assert_eq!(d.summary.issues_removed, 1);
        assert_eq!(d.summary.net_issue_change, 1);
        assert_eq!(d.summary.total_changes, 3);
    }

    /// Drive `calculate_summary` through the same synthetic issue sets the
    /// real comparator builds, so the health-score thresholds are exercised
    /// against the actual list plumbing rather than bare integers.
    /// Tuple is `(added, closed, removed, reopened, modified, introduced, resolved)`.
    fn trend_for(
        counts: (usize, usize, usize, usize, usize, usize, usize),
        blocked: i64,
    ) -> &'static str {
        let issue = |i: usize| issue(&format!("i{i}"), Status::Open);
        let (a, c, r, ro, m, ci, cr) = counts;
        calculate_summary(
            &(0..a).map(issue).collect::<Vec<_>>(),
            &(0..c).map(issue).collect::<Vec<_>>(),
            &(0..r).map(issue).collect::<Vec<_>>(),
            &(0..ro).map(issue).collect::<Vec<_>>(),
            &(0..m)
                .map(|i| ModifiedIssue {
                    issue_id: format!("m{i}"),
                    title: String::new(),
                    changes: vec![],
                })
                .collect::<Vec<_>>(),
            &(0..ci).map(|i| vec![format!("c{i}")]).collect::<Vec<_>>(),
            &(0..cr).map(|i| vec![format!("d{i}")]).collect::<Vec<_>>(),
            blocked,
        )
        .health_trend
    }

    #[test]
    fn health_trend_thresholds() {
        // score = closed - reopened + 2*cycles_resolved - 3*cycles_introduced
        assert_eq!(trend_for((0, 0, 0, 0, 0, 0, 0), 0), "stable", "score 0");
        assert_eq!(trend_for((0, 1, 0, 0, 0, 0, 0), 0), "stable", "score 1");
        assert_eq!(trend_for((0, 2, 0, 0, 0, 0, 0), 0), "improving", "score 2");
        assert_eq!(trend_for((0, 0, 0, 1, 0, 0, 0), 0), "stable", "score -1");
        assert_eq!(trend_for((0, 0, 0, 2, 0, 0, 0), 0), "degrading", "score -2");
        assert_eq!(trend_for((0, 0, 0, 0, 0, 1, 0), 0), "degrading", "score -3");
        assert_eq!(
            trend_for((0, 0, 0, 0, 0, 0, 1), 0),
            "improving",
            "one resolved cycle alone scores 2"
        );
    }

    #[test]
    fn blocked_delta_shifts_health_trend() {
        // 1 closed (+1) with one fewer blocked issue (+2) => score 3.
        assert_eq!(trend_for((0, 1, 0, 0, 0, 0, 0), -1), "improving");
        // 1 closed (+1) with one more blocked issue (-1) => score 0.
        assert_eq!(trend_for((0, 1, 0, 0, 0, 0, 0), 1), "stable");
        // 1 closed (+1), no blocked movement => score 1, still stable.
        assert_eq!(trend_for((0, 1, 0, 0, 0, 0, 0), 0), "stable");
    }

    #[test]
    fn cycles_compare_by_rotation_not_start_index() {
        let mut from = plain_snapshot(vec![]);
        from.cycles = Some(vec![vec!["a".into(), "b".into(), "c".into()]]);
        // Same cycle, rotated — should not register as new.
        let mut to = plain_snapshot(vec![]);
        to.cycles = Some(vec![vec!["b".into(), "c".into(), "a".into()]]);
        let d = compare_snapshots(&from, &to);
        assert!(d.new_cycles.is_empty());
        assert!(d.resolved_cycles.is_empty());
        assert_eq!(d.metric_deltas.cycle_count, 0);
    }

    #[test]
    fn cycles_introduced_and_resolved_are_reported_and_counted() {
        let mut from = plain_snapshot(vec![]);
        from.cycles = Some(vec![vec!["x".into(), "y".into()]]);
        let mut to = plain_snapshot(vec![]);
        to.cycles = Some(vec![vec!["p".into(), "q".into()]]);

        let d = compare_snapshots(&from, &to);
        assert_eq!(d.new_cycles, vec![vec!["p".to_string(), "q".to_string()]]);
        assert_eq!(
            d.resolved_cycles,
            vec![vec!["x".to_string(), "y".to_string()]]
        );
        assert_eq!(d.metric_deltas.cycle_count, 0, "one in, one out");
        assert_eq!(d.summary.cycles_introduced, 1);
        assert_eq!(d.summary.cycles_resolved, 1);
        // +2 resolved, -3 introduced => score -1 => stable.
        assert_eq!(d.summary.health_trend, "stable");
    }

    #[test]
    fn metric_deltas_count_edge_and_component_are_always_zero() {
        let from = plain_snapshot(vec![issue("a", Status::Open)]);
        let to = plain_snapshot(vec![issue("a", Status::Open), issue("b", Status::Blocked)]);
        let d = compare_snapshots(&from, &to);
        assert_eq!(d.metric_deltas.total_issues, 1);
        assert_eq!(d.metric_deltas.open_issues, 1);
        assert_eq!(d.metric_deltas.blocked_issues, 1);
        assert_eq!(d.metric_deltas.total_edges, 0);
        assert_eq!(d.metric_deltas.component_count, 0);
    }

    #[test]
    fn avg_map_value_sums_in_sorted_key_order() {
        let m: BTreeMap<String, f64> = [("b", 2.0), ("a", 1.0), ("c", 3.0)]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        assert_eq!(avg_map_value(Some(&m)), 2.0);
        assert_eq!(avg_map_value(None), 0.0);
        assert_eq!(avg_map_value(Some(&BTreeMap::new())), 0.0);
    }

    #[test]
    fn drop_zero_scores_matches_gonum_betweenness_map_shape() {
        let m: BTreeMap<String, f64> = [("a", 0.0), ("b", 4.0), ("c", 0.0), ("d", 8.0)]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        let kept = drop_zero_scores(m);
        assert_eq!(kept.len(), 2);
        assert_eq!(avg_map_value(Some(&kept)), 6.0);
    }

    #[test]
    fn betweenness_delta_ignores_zero_scoring_nodes() {
        // Same two nonzero scores on both sides, but the live side has more
        // zero-scoring nodes. Go averages only the nonzero entries, so the
        // delta is 0 — which is what keeps `avg_betweenness` at parity when a
        // revision only adds isolated issues.
        let mut from = plain_snapshot(vec![]);
        from.betweenness = Some(drop_zero_scores(BTreeMap::from([
            ("a".to_string(), 4.0),
            ("b".to_string(), 8.0),
        ])));
        let mut to = plain_snapshot(vec![]);
        to.betweenness = Some(drop_zero_scores(BTreeMap::from([
            ("a".to_string(), 4.0),
            ("b".to_string(), 8.0),
            ("c".to_string(), 0.0),
            ("d".to_string(), 0.0),
        ])));
        assert_eq!(
            compare_snapshots(&from, &to).metric_deltas.avg_betweenness,
            0.0
        );
    }

    fn dep(depends_on: &str, ty: DependencyType) -> Dependency {
        Dependency {
            issue_id: String::new(),
            depends_on_id: depends_on.into(),
            depends_on_legacy: String::new(),
            target_id_legacy: String::new(),
            r#type: ty,
            created_at: None,
            created_by: String::new(),
        }
    }

    #[test]
    fn detect_changes_reports_dependency_type_and_label_edits() {
        let mut from = issue("a", Status::Open);
        from.labels = vec!["beta".into(), "alpha".into()];
        from.dependencies = vec![dep("z", DependencyType::Blocks)];

        let mut to = issue("a", Status::Open);
        to.labels = vec!["alpha".into()];
        to.dependencies = vec![dep("z", DependencyType::Related)];

        let changes = detect_changes(&from, &to);
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].field, "dependencies");
        assert_eq!(changes[0].old_value, "z:blocks");
        assert_eq!(changes[0].new_value, "z:related");
        assert_eq!(changes[1].field, "labels");
        assert_eq!(changes[1].old_value, "alpha, beta", "sorted");
        assert_eq!(changes[1].new_value, "alpha");
    }

    #[test]
    fn dependency_set_folds_legacy_id_fields_and_skips_empty_targets() {
        let mut legacy = dep("", DependencyType::Blocks);
        legacy.depends_on_legacy = "q".into();
        let mut other_legacy = dep("", DependencyType::Blocks);
        other_legacy.target_id_legacy = "r".into();
        let set = dependency_set(&[
            dep("z", DependencyType::Blocks),
            legacy,
            other_legacy,
            dep("", DependencyType::Blocks),
        ]);
        assert_eq!(
            set,
            BTreeSet::from([
                "z:blocks".to_string(),
                "q:blocks".to_string(),
                "r:blocks".to_string()
            ])
        );
    }

    #[test]
    fn priority_strings_use_go_formatting() {
        assert_eq!(priority_string(0), "P0");
        assert_eq!(priority_string(4), "P4");
        assert_eq!(priority_string(9), "P9");
        assert_eq!(priority_string(10), "P10");
        assert_eq!(priority_string(-1), "P-1");
    }

    #[test]
    fn empty_lists_serialize_as_null_like_go_nil_slices() {
        let from = plain_snapshot(vec![]);
        let d = compare_snapshots(&from, &from);
        let v = serde_json::to_value(&d).unwrap();
        for key in [
            "new_issues",
            "closed_issues",
            "removed_issues",
            "reopened_issues",
            "modified_issues",
            "new_cycles",
            "resolved_cycles",
        ] {
            assert!(v[key].is_null(), "{key} should be null, got {}", v[key]);
        }
    }

    #[test]
    fn field_order_matches_go_wire_order() {
        let from = plain_snapshot(vec![]);
        let to = plain_snapshot(vec![issue("a", Status::Open)]);
        let d = compare_snapshots(&from, &to);
        let v = serde_json::to_value(&d).unwrap();
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec![
                "from_timestamp",
                "to_timestamp",
                "new_issues",
                "closed_issues",
                "removed_issues",
                "reopened_issues",
                "modified_issues",
                "new_cycles",
                "resolved_cycles",
                "metric_deltas",
                "summary",
            ],
            "absent revisions are omitted and the rest keep Go's order"
        );
    }

    #[test]
    fn revisions_are_omitted_when_empty() {
        let mut from = plain_snapshot(vec![]);
        from.revision = "abc123".into();
        // The live side is built with NewSnapshot, which never sets a revision.
        let to = plain_snapshot(vec![]);
        let v = serde_json::to_value(compare_snapshots(&from, &to)).unwrap();
        assert_eq!(v["from_revision"], "abc123");
        assert!(v.get("to_revision").is_none());
    }

    fn ids(issues: &[Issue]) -> Vec<&str> {
        issues.iter().map(|i| i.id.as_str()).collect()
    }
}
