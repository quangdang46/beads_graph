//! Related-work discovery — port of Go `pkg/correlation/related.go`.
//!
//! Four independent detectors each produce a scored, human-explained
//! shortlist for one target bead. They run in a fixed order and share a `seen`
//! set, so a bead is attributed to the first detector that claimed it and never
//! double-counted across categories (Go related.go:108-137).
//!
//! Ordering is a total order everywhere: every detector sorts by
//! `(relevance desc, bead_id asc)`, so output does not depend on hash-map
//! iteration order.

use crate::file_index::{is_closed_history_status, normalize_path, FileLookup};
use crate::history::{self, BeadHistory, HistoryReport};
use jiff::Timestamp;
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

/// How two beads are related.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationType {
    /// The beads touch the same files.
    FileOverlap,
    /// The beads share commits.
    CommitOverlap,
    /// The beads sit in the same dependency cluster.
    DependencyCluster,
    /// The beads are active in the same time window.
    Concurrent,
}

/// A bead related to a target bead.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RelatedWorkBead {
    pub bead_id: String,
    pub title: String,
    pub status: String,
    pub relation_type: RelationType,
    /// 0-100 score.
    pub relevance: i64,
    /// Human-readable explanation.
    pub reason: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shared_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shared_commits: Vec<String>,
}

/// Every related bead, grouped by relationship type.
///
/// The four category fields are `Option` because Go's struct tags carry no
/// `omitempty` and each finder returns a *nil* slice from its main path
/// (`var results []RelatedWorkBead`, related.go:173/245/367/425) while its
/// early return hands back a non-nil `[]RelatedWorkBead{}` (related.go:150).
/// `encoding/json` renders nil as `null` and empty-non-nil as `[]`, so the
/// distinction is observable: a bead with no related work serializes with
/// `"concurrent": null`, not `"concurrent": []`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RelatedWorkResult {
    pub target_bead_id: String,
    pub target_title: String,
    pub file_overlap: Option<Vec<RelatedWorkBead>>,
    pub commit_overlap: Option<Vec<RelatedWorkBead>>,
    pub dependency_cluster: Option<Vec<RelatedWorkBead>>,
    pub concurrent: Option<Vec<RelatedWorkBead>>,
    pub total_related: usize,
    pub generated_at: String,
}

impl RelatedWorkResult {
    /// A category list with Go's nil accumulator read as empty. Go's `len`
    /// and `range` over a nil slice both behave as zero-length, so this is
    /// what every Go call site of the four categories sees.
    pub fn category(list: &Option<Vec<RelatedWorkBead>>) -> &[RelatedWorkBead] {
        list.as_deref().unwrap_or(&[])
    }
}

/// Collapse a finished finder result the way Go's nil slice does: an empty
/// accumulator is `None` (JSON `null`), anything else is `Some`.
fn nil_when_empty(results: Vec<RelatedWorkBead>) -> Option<Vec<RelatedWorkBead>> {
    if results.is_empty() {
        None
    } else {
        Some(results)
    }
}

/// Go `RelatedWorkOptions`.
#[derive(Debug, Clone)]
pub struct RelatedWorkOptions {
    /// Minimum relevance score (0-100) to include.
    pub min_relevance: i64,
    /// Maximum results per category (0 = unlimited).
    pub max_results: usize,
    /// Time window for concurrent detection.
    pub concurrency_window: Duration,
    /// Include closed beads in results.
    pub include_closed: bool,
    /// Pre-built file lookup; one is built from the report when absent.
    pub file_lookup: Option<FileLookup>,
    /// BeadID → the IDs it depends on.
    pub dependency_graph: Option<BTreeMap<String, Vec<String>>>,
}

/// Go `DefaultRelatedWorkOptions` — note the struct literal, not `Default`:
/// Go's zero value would silently disable the relevance floor and the
/// concurrency window.
pub fn default_related_work_options() -> RelatedWorkOptions {
    RelatedWorkOptions {
        min_relevance: 20,
        max_results: 10,
        // 1 week.
        concurrency_window: Duration::from_secs(7 * 24 * 60 * 60),
        include_closed: false,
        file_lookup: None,
        dependency_graph: None,
    }
}

/// Go `FindRelatedWorkAt` — related beads for `target_id`, evaluated at the
/// caller-owned instant `now`.
///
/// Returns `None` when the target is not in the report, matching Go's `nil`.
/// The zero instant is valid and is preserved as given.
pub fn find_related_work_at(
    report: &HistoryReport,
    target_id: &str,
    opts: &RelatedWorkOptions,
    now: Timestamp,
) -> Option<RelatedWorkResult> {
    let target = report.histories.get(target_id)?;

    // Go builds the file lookup once and threads it through the file-overlap
    // detector; the caller may supply a pre-built one.
    let owned_lookup;
    let file_lookup = match &opts.file_lookup {
        Some(lookup) => lookup,
        None => {
            owned_lookup = FileLookup::new(report);
            &owned_lookup
        }
    };

    // The target's own files and commits seed the two overlap detectors.
    let mut target_files: HashSet<String> = HashSet::new();
    let mut target_commits: HashSet<String> = HashSet::new();
    for commit in target.commits.iter().flatten() {
        target_commits.insert(commit.sha.clone());
        for file in &commit.files {
            target_files.insert(normalize_path(&file.path));
        }
    }

    // A bead belongs to the first detector that claimed it.
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(target_id.to_string());

    let file_overlap = find_file_overlap(report, &target_files, file_lookup, opts, &seen);
    seen.extend(file_overlap.iter().flatten().map(|b| b.bead_id.clone()));

    let commit_overlap = find_commit_overlap(report, target_id, &target_commits, opts, &seen);
    seen.extend(commit_overlap.iter().flatten().map(|b| b.bead_id.clone()));

    // The dependency detector is skipped entirely without a graph; Go leaves
    // the field at its `[]RelatedWorkBead{}` initializer (related.go:87).
    let dependency_cluster = if opts.dependency_graph.is_some() {
        let found = find_dependency_cluster(report, target_id, opts, &seen);
        seen.extend(found.iter().flatten().map(|b| b.bead_id.clone()));
        found
    } else {
        Some(Vec::new())
    };

    let concurrent = find_concurrent(report, target_id, target, opts, &seen, now);

    Some(RelatedWorkResult {
        target_bead_id: target_id.to_string(),
        target_title: target.title.clone(),
        total_related: file_overlap.iter().flatten().count()
            + commit_overlap.iter().flatten().count()
            + dependency_cluster.iter().flatten().count()
            + concurrent.iter().flatten().count(),
        file_overlap,
        commit_overlap,
        dependency_cluster,
        concurrent,
        generated_at: rfc3339_seconds(now),
    })
}

/// Go's `RelatedWorkResult.GeneratedAt` is a `time.Time`, and `encoding/json`
/// marshals `time.Time` as RFC3339 at **second** precision. `Timestamp`'s
/// `Display` keeps the sub-second component, so the raw `to_string()` renders
/// `2026-08-22T14:07:01.790741Z` where Go renders `2026-08-22T14:07:01Z`.
fn rfc3339_seconds(now: Timestamp) -> String {
    Timestamp::from_second(now.as_second())
        .map(|ts| ts.to_string())
        .unwrap_or_else(|_| now.to_string())
}

/// Go `findFileOverlap` — beads touching the same files as the target.
///
/// `relevance` is the share of the target's files the bead also touched, as a
/// percentage. Closed beads join the candidate pool only when
/// [`RelatedWorkOptions::include_closed`] is set.
///
/// Go's signature also takes the target ID (related.go:147) and never reads it;
/// excluding the target is the caller's job, via the `seen` set.
fn find_file_overlap(
    report: &HistoryReport,
    target_files: &HashSet<String>,
    file_lookup: &FileLookup,
    opts: &RelatedWorkOptions,
    seen: &HashSet<String>,
) -> Option<Vec<RelatedWorkBead>> {
    if target_files.is_empty() {
        return Some(Vec::new());
    }

    // beadID → shared files. Iteration order here is the hash-set's, but the
    // per-bead file lists are sorted and the results are ordered below, so the
    // output is deterministic regardless.
    let mut overlap_count: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for file in target_files {
        let lookup = file_lookup.lookup_by_file(file);
        for reference in &lookup.open_beads {
            if seen.contains(&reference.bead_id) {
                continue;
            }
            overlap_count
                .entry(reference.bead_id.clone())
                .or_default()
                .push(file.clone());
        }
        if opts.include_closed {
            for reference in &lookup.closed_beads {
                if seen.contains(&reference.bead_id) {
                    continue;
                }
                overlap_count
                    .entry(reference.bead_id.clone())
                    .or_default()
                    .push(file.clone());
            }
        }
    }

    let total_target_files = target_files.len() as i64;
    let mut results: Vec<RelatedWorkBead> = overlap_count
        .into_iter()
        .filter_map(|(bead_id, mut shared_files)| {
            let history = report.histories.get(&bead_id)?;
            if should_skip_related_status(&history.status, opts.include_closed) {
                return None;
            }
            let shared = shared_files.len() as i64;
            let mut relevance = shared * 100 / total_target_files;
            if relevance > 100 {
                relevance = 100;
            }
            if relevance < opts.min_relevance {
                return None;
            }
            shared_files.sort();
            Some(RelatedWorkBead {
                bead_id,
                title: history.title.clone(),
                status: history.status.clone(),
                relation_type: RelationType::FileOverlap,
                relevance,
                reason: format_file_overlap_reason(shared, total_target_files),
                shared_files: limit_strings(shared_files, 5),
                shared_commits: Vec::new(),
            })
        })
        .collect();

    sort_related_results(&mut results);
    truncate_to_max(&mut results, opts.max_results);
    nil_when_empty(results)
}

/// Go `findCommitOverlap` — beads sharing commits with the target.
fn find_commit_overlap(
    report: &HistoryReport,
    target_id: &str,
    target_commits: &HashSet<String>,
    opts: &RelatedWorkOptions,
    seen: &HashSet<String>,
) -> Option<Vec<RelatedWorkBead>> {
    if target_commits.is_empty() {
        return Some(Vec::new());
    }

    let mut shared_count: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for sha in target_commits {
        let Some(bead_ids) = report.commit_index.get(sha) else {
            continue;
        };
        for bead_id in bead_ids {
            if seen.contains(bead_id) || bead_id == target_id {
                continue;
            }
            let shas = shared_count.entry(bead_id.clone()).or_default();
            if !shas.iter().any(|existing| existing == sha) {
                shas.push(sha.clone());
            }
        }
    }

    let total_target_commits = target_commits.len() as i64;
    let mut results: Vec<RelatedWorkBead> = shared_count
        .into_iter()
        .filter_map(|(bead_id, mut shared_shas)| {
            let history = report.histories.get(&bead_id)?;
            if should_skip_related_status(&history.status, opts.include_closed) {
                return None;
            }
            let shared = shared_shas.len() as i64;
            let mut relevance = shared * 100 / total_target_commits;
            if relevance > 100 {
                relevance = 100;
            }
            if relevance < opts.min_relevance {
                return None;
            }
            shared_shas.sort();
            Some(RelatedWorkBead {
                bead_id,
                title: history.title.clone(),
                status: history.status.clone(),
                relation_type: RelationType::CommitOverlap,
                relevance,
                reason: format_commit_overlap_reason(shared, total_target_commits),
                shared_files: Vec::new(),
                shared_commits: limit_strings(shorten_shas(&shared_shas), 5),
            })
        })
        .collect();

    sort_related_results(&mut results);
    truncate_to_max(&mut results, opts.max_results);
    nil_when_empty(results)
}

/// Go `findDependencyCluster` — beads within two hops of the target in the
/// dependency graph.
fn find_dependency_cluster(
    report: &HistoryReport,
    target_id: &str,
    opts: &RelatedWorkOptions,
    seen: &HashSet<String>,
) -> Option<Vec<RelatedWorkBead>> {
    let Some(graph) = &opts.dependency_graph else {
        return Some(Vec::new());
    };

    // beadID → hop distance.
    let mut cluster: BTreeMap<&str, u8> = BTreeMap::new();
    if let Some(deps) = graph.get(target_id) {
        for dep_id in deps {
            if !seen.contains(dep_id) {
                cluster.insert(dep_id.as_str(), 1);
            }
        }
    }
    // Reverse dependencies: things that depend on the target.
    for (bead_id, deps) in graph {
        if seen.contains(bead_id) {
            continue;
        }
        if deps.iter().any(|dep| dep == target_id) {
            cluster.entry(bead_id.as_str()).or_insert(1);
        }
    }
    // Second hop: dependencies of the first hop.
    let first_hop: Vec<&str> = cluster.keys().copied().collect();
    for hop_id in first_hop {
        let Some(deps) = graph.get(hop_id) else {
            continue;
        };
        for dep_id in deps {
            if !seen.contains(dep_id) && dep_id != target_id {
                cluster.entry(dep_id.as_str()).or_insert(2);
            }
        }
    }

    let mut results: Vec<RelatedWorkBead> = cluster
        .into_iter()
        .filter_map(|(bead_id, hops)| {
            let history = report.histories.get(bead_id)?;
            if should_skip_related_status(&history.status, opts.include_closed) {
                return None;
            }
            // Direct dependencies (1 hop) score 80, indirect (2 hops) 40.
            let (relevance, reason) = if hops == 2 {
                (40, "Indirect dependency (2 hops)")
            } else {
                (80, "Direct dependency")
            };
            if relevance < opts.min_relevance {
                return None;
            }
            Some(RelatedWorkBead {
                bead_id: bead_id.to_string(),
                title: history.title.clone(),
                status: history.status.clone(),
                relation_type: RelationType::DependencyCluster,
                relevance,
                reason: reason.to_string(),
                shared_files: Vec::new(),
                shared_commits: Vec::new(),
            })
        })
        .collect();

    sort_related_results(&mut results);
    truncate_to_max(&mut results, opts.max_results);
    nil_when_empty(results)
}

/// Go `findConcurrent` — beads active in the same time window as the target.
///
/// The target's window is widened by [`RelatedWorkOptions::concurrency_window`]
/// on both sides for the *qualification* test; a candidate qualifies when its
/// window overlaps the widened one. The overlap itself — the relevance bonus
/// and the "N days of overlapping activity" reason — is measured against the
/// target's own `[start, end]` span, so slack-region activity cannot inflate
/// the score past what real concurrency earns.
fn find_concurrent(
    report: &HistoryReport,
    target_id: &str,
    target: &BeadHistory,
    opts: &RelatedWorkOptions,
    seen: &HashSet<String>,
    now: Timestamp,
) -> Option<Vec<RelatedWorkBead>> {
    let Some((target_start, target_end)) = activity_window(target, now) else {
        return Some(Vec::new());
    };

    // Widened by the concurrency window on both sides.
    let slack = nanos_of(opts.concurrency_window);
    let window_start = target_start.as_nanosecond() - slack;
    let window_end = target_end.as_nanosecond() + slack;
    let target_duration = target_end.as_nanosecond() - target_start.as_nanosecond();

    let mut results: Vec<RelatedWorkBead> = Vec::new();
    for (bead_id, history) in &report.histories {
        if bead_id == target_id || seen.contains(bead_id) {
            continue;
        }
        if should_skip_related_status(&history.status, opts.include_closed) {
            continue;
        }
        let Some((bead_start, bead_end)) = activity_window(history, now) else {
            continue;
        };
        let bead_start = bead_start.as_nanosecond();
        let bead_end = bead_end.as_nanosecond();
        if bead_start > window_end || bead_end < window_start {
            continue;
        }
        // Overlap is measured against the target's real span, not the widened
        // qualification window — a candidate touching only the slack earns
        // the flat 30, never the scaled bonus.
        let target_start_ns = target_start.as_nanosecond();
        let target_end_ns = target_end.as_nanosecond();
        let overlap_duration =
            (bead_end.min(target_end_ns) - bead_start.max(target_start_ns)).max(0);

        let mut relevance = 30;
        if target_duration > 0 {
            let overlap_pct = (overlap_duration as f64 / target_duration as f64 * 50.0) as i64;
            relevance += overlap_pct;
            if relevance > 100 {
                relevance = 100;
            }
        }
        if relevance < opts.min_relevance {
            continue;
        }
        results.push(RelatedWorkBead {
            bead_id: bead_id.clone(),
            title: history.title.clone(),
            status: history.status.clone(),
            relation_type: RelationType::Concurrent,
            relevance,
            reason: format_concurrent_reason(overlap_duration),
            shared_files: Vec::new(),
            shared_commits: Vec::new(),
        });
    }

    sort_related_results(&mut results);
    truncate_to_max(&mut results, opts.max_results);
    nil_when_empty(results)
}

/// The window a bead was active in: created (else first commit) through closed
/// (else `now`), clamped so the end is never before the start.
///
/// Returns `None` when there is no usable start, which is Go's zero-time guard.
/// A closed milestone whose timestamp does not parse falls back to `now`; the
/// report's own extractor only ever emits valid RFC3339.
fn activity_window(history: &BeadHistory, now: Timestamp) -> Option<(Timestamp, Timestamp)> {
    let start = history
        .milestones
        .created
        .as_ref()
        .and_then(|event| history::parse_ts(&event.timestamp))
        .or_else(|| {
            history
                .commits
                .iter()
                .flatten()
                .next()
                .and_then(|commit| history::parse_ts(&commit.timestamp))
        })?;
    let end = match &history.milestones.closed {
        Some(event) if is_closed_history_status(&history.status) => {
            history::parse_ts(&event.timestamp).unwrap_or(now)
        }
        _ => now,
    };
    Some((start, if end < start { start } else { end }))
}

fn nanos_of(duration: Duration) -> i128 {
    duration.as_nanos() as i128
}

/// Go `shouldSkipRelatedStatus` — tombstones are always dropped; closed beads
/// are dropped unless explicitly requested.
fn should_skip_related_status(status: &str, include_closed: bool) -> bool {
    let normalized = crate::file_index::normalize_status(status);
    normalized == "tombstone" || (!include_closed && normalized == "closed")
}

/// Go `sortRelatedResults` — relevance descending, bead ID breaking ties.
fn sort_related_results(results: &mut [RelatedWorkBead]) {
    results.sort_by(|a, b| {
        b.relevance
            .cmp(&a.relevance)
            .then_with(|| a.bead_id.cmp(&b.bead_id))
    });
}

fn truncate_to_max(results: &mut Vec<RelatedWorkBead>, max_results: usize) {
    if max_results > 0 && results.len() > max_results {
        results.truncate(max_results);
    }
}

/// Go `formatFileOverlapReason`.
fn format_file_overlap_reason(shared: i64, total: i64) -> String {
    let pct = shared * 100 / total;
    if shared == 1 {
        return "1 shared file".to_string();
    }
    format!(
        "{}{}",
        format_plural_related(shared, "shared file", "shared files"),
        format_pct_related(pct)
    )
}

/// Go `formatCommitOverlapReason`.
fn format_commit_overlap_reason(shared: i64, total: i64) -> String {
    let pct = shared * 100 / total;
    if shared == 1 {
        return "1 shared commit".to_string();
    }
    format!(
        "{}{}",
        format_plural_related(shared, "shared commit", "shared commits"),
        format_pct_related(pct)
    )
}

/// Go `formatConcurrentReason` — whole days of overlap, truncated.
fn format_concurrent_reason(overlap_ns: i128) -> String {
    const DAY_NS: i128 = 24 * 60 * 60 * 1_000_000_000;
    // Integer division truncates toward zero, as Go's int() conversion does.
    let days = (overlap_ns / DAY_NS) as i64;
    if days < 1 {
        return "Active in same time window".to_string();
    }
    format!(
        "{} of overlapping activity",
        format_plural_related(days, "day", "days")
    )
}

/// Go `formatPluralRelated`.
fn format_plural_related(n: i64, singular: &str, plural: &str) -> String {
    if n == 1 {
        format!("1 {singular}")
    } else {
        format!("{n} {plural}")
    }
}

/// Go `formatPctRelated` — a percentage suffix, omitted when zero.
fn format_pct_related(pct: i64) -> String {
    if pct <= 0 {
        String::new()
    } else {
        format!(" ({pct}%)")
    }
}

/// Go `limitStrings`.
fn limit_strings(values: Vec<String>, max: usize) -> Vec<String> {
    if values.len() <= max {
        values
    } else {
        values.into_iter().take(max).collect()
    }
}

/// Go `shortenSHAs` — full SHA down to its 7-character prefix.
fn shorten_shas(shas: &[String]) -> Vec<String> {
    shas.iter()
        .map(|sha| {
            if sha.chars().count() > 7 {
                sha.chars().take(7).collect()
            } else {
                sha.clone()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extractor::{BeadEvent, EventType};
    use crate::history::{
        BeadMilestones, FileChange, HistoryCommit, HistoryReport, HistoryStats, HistoryWindow,
    };
    use std::collections::BTreeMap;

    fn event(ts: &str) -> BeadEvent {
        BeadEvent {
            bead_id: String::new(),
            event_type: EventType::Created,
            timestamp: ts.to_string(),
            commit_sha: String::new(),
            commit_msg: String::new(),
            author: String::new(),
            author_email: String::new(),
            before: None,
            after: None,
            transition_observed: false,
        }
    }

    fn commit(sha: &str, ts: &str, files: &[&str]) -> HistoryCommit {
        HistoryCommit {
            bead_id: String::new(),
            sha: sha.to_string(),
            short_sha: sha.chars().take(7).collect(),
            message: String::new(),
            author: String::new(),
            author_email: String::new(),
            timestamp: ts.to_string(),
            files: files
                .iter()
                .map(|path| FileChange {
                    path: path.to_string(),
                    action: "M".into(),
                    insertions: 1,
                    deletions: 1,
                })
                .collect(),
            method: "explicit_id",
            methods: vec!["explicit_id".into()],
            confidence: 1.0,
            reason: String::new(),
            confirmed: false,
        }
    }

    fn history(
        bead_id: &str,
        title: &str,
        status: &str,
        created: Option<&str>,
        closed: Option<&str>,
        commits: Vec<HistoryCommit>,
    ) -> BeadHistory {
        BeadHistory {
            bead_id: bead_id.into(),
            title: title.into(),
            status: status.into(),
            events: Vec::new(),
            milestones: BeadMilestones {
                created: created.map(event),
                closed: closed.map(event),
                ..Default::default()
            },
            commits: Some(commits),
            cycle_time: None,
            last_author: String::new(),
        }
    }

    fn report(histories: BTreeMap<String, BeadHistory>) -> HistoryReport {
        HistoryReport {
            generated_at: "2026-01-01T00:00:00Z".into(),
            data_hash: String::new(),
            git_range: String::new(),
            latest_commit_sha: String::new(),
            window: HistoryWindow {
                revision: String::new(),
                limit: 0,
                since: None,
                until: None,
                commits: 0,
            },
            stats: HistoryStats {
                total_beads: 0,
                beads_with_commits: 0,
                total_commits: 0,
                unique_authors: 0,
                avg_commits_per_bead: 0.0,
                avg_cycle_time_days: None,
                method_distribution: BTreeMap::new(),
                strategies: None,
                feedback_applied: None,
            },
            commit_index: crate::history::build_commit_index(&histories),
            histories,
            causal_history: None,
        }
    }

    /// bd-1 and bd-2 share every file and one commit; bd-3 is closed and shares
    /// nothing; bd-4 is a tombstone that shares bd-1's file.
    fn sample_report() -> HistoryReport {
        let mut histories = BTreeMap::new();
        histories.insert(
            "bd-1".into(),
            history(
                "bd-1",
                "target",
                "open",
                Some("2026-01-01T00:00:00Z"),
                None,
                vec![
                    commit(
                        "aaaaaaaa1",
                        "2026-01-02T00:00:00Z",
                        &["src/a.rs", "src/b.rs"],
                    ),
                    commit("cccccccc1", "2026-01-03T00:00:00Z", &["src/a.rs"]),
                ],
            ),
        );
        histories.insert(
            "bd-2".into(),
            history(
                "bd-2",
                "sibling",
                "in_progress",
                Some("2026-01-01T00:00:00Z"),
                None,
                vec![commit(
                    "bbbbbbbb1",
                    "2026-01-02T00:00:00Z",
                    &["src/a.rs", "src/b.rs"],
                )],
            ),
        );
        histories.insert(
            "bd-3".into(),
            history(
                "bd-3",
                "closed elsewhere",
                "closed",
                Some("2025-12-01T00:00:00Z"),
                Some("2025-12-20T00:00:00Z"),
                vec![commit("dddddddd1", "2025-12-15T00:00:00Z", &["other/z.rs"])],
            ),
        );
        histories.insert(
            "bd-4".into(),
            history(
                "bd-4",
                "tombstoned",
                "tombstone",
                Some("2026-01-01T00:00:00Z"),
                None,
                vec![commit("eeeeeeee1", "2026-01-02T00:00:00Z", &["src/a.rs"])],
            ),
        );
        report(histories)
    }

    fn now() -> Timestamp {
        "2026-01-05T00:00:00Z".parse().unwrap()
    }

    #[test]
    fn default_options_match_go() {
        let opts = default_related_work_options();
        assert_eq!(opts.min_relevance, 20);
        assert_eq!(opts.max_results, 10);
        assert_eq!(
            opts.concurrency_window,
            Duration::from_secs(7 * 24 * 60 * 60)
        );
        assert!(!opts.include_closed);
        assert!(opts.file_lookup.is_none());
        assert!(opts.dependency_graph.is_none());
    }

    #[test]
    fn unknown_target_returns_none() {
        let report = sample_report();
        let opts = default_related_work_options();
        assert!(find_related_work_at(&report, "bd-missing", &opts, now()).is_none());
    }

    #[test]
    fn file_overlap_scores_full_relevance_for_an_identical_file_set() {
        let report = sample_report();
        let opts = default_related_work_options();
        let result = find_related_work_at(&report, "bd-1", &opts, now()).unwrap();
        let overlap = RelatedWorkResult::category(&result.file_overlap);
        assert_eq!(overlap.len(), 1, "only bd-2 shares files: {overlap:?}");
        assert_eq!(overlap[0].bead_id, "bd-2");
        assert_eq!(overlap[0].relation_type, RelationType::FileOverlap);
        // bd-2 touches both of bd-1's files, so relevance is 100.
        assert_eq!(overlap[0].relevance, 100);
        assert_eq!(overlap[0].shared_files, vec!["src/a.rs", "src/b.rs"]);
        assert_eq!(overlap[0].reason, "2 shared files (100%)");
    }

    #[test]
    fn generated_at_is_rfc3339_at_second_precision() {
        // Go marshals `GeneratedAt time.Time` through encoding/json, which
        // drops the sub-second component. A nanosecond-precision `now` must
        // still render as a whole second.
        let with_nanos: Timestamp = "2026-01-05T00:00:00.790741Z".parse().unwrap();
        let result = find_related_work_at(
            &sample_report(),
            "bd-1",
            &default_related_work_options(),
            with_nanos,
        )
        .unwrap();
        assert_eq!(result.generated_at, "2026-01-05T00:00:00Z");
    }

    #[test]
    fn tombstones_are_never_related_even_with_include_closed() {
        let report = sample_report();
        let mut opts = default_related_work_options();
        opts.include_closed = true;
        let result = find_related_work_at(&report, "bd-1", &opts, now()).unwrap();
        let every_bead = RelatedWorkResult::category(&result.file_overlap)
            .iter()
            .chain(RelatedWorkResult::category(&result.commit_overlap))
            .chain(RelatedWorkResult::category(&result.dependency_cluster))
            .chain(RelatedWorkResult::category(&result.concurrent))
            .map(|b| b.bead_id.as_str())
            .collect::<Vec<_>>();
        assert!(!every_bead.contains(&"bd-4"), "{every_bead:?}");
    }

    #[test]
    fn include_closed_adds_closed_beads_to_the_pool() {
        // bd-3 is closed and touches none of bd-1's files, so build a target
        // that does overlap it to observe the flag's effect.
        let mut histories = BTreeMap::new();
        histories.insert(
            "t-1".into(),
            history(
                "t-1",
                "target",
                "open",
                Some("2026-01-01T00:00:00Z"),
                None,
                vec![commit(
                    "11111111a",
                    "2026-01-02T00:00:00Z",
                    &["src/shared.rs"],
                )],
            ),
        );
        histories.insert(
            "c-1".into(),
            history(
                "c-1",
                "closed sibling",
                "closed",
                Some("2026-01-01T00:00:00Z"),
                Some("2026-01-03T00:00:00Z"),
                vec![commit(
                    "22222222b",
                    "2026-01-02T00:00:00Z",
                    &["src/shared.rs"],
                )],
            ),
        );
        let report = report(histories);

        let mut opts = default_related_work_options();
        let without = find_related_work_at(&report, "t-1", &opts, now()).unwrap();
        assert!(!RelatedWorkResult::category(&without.file_overlap)
            .iter()
            .chain(RelatedWorkResult::category(&without.concurrent))
            .any(|b| b.bead_id == "c-1"));

        opts.include_closed = true;
        let with = find_related_work_at(&report, "t-1", &opts, now()).unwrap();
        assert!(
            RelatedWorkResult::category(&with.file_overlap)
                .iter()
                .chain(RelatedWorkResult::category(&with.concurrent))
                .any(|b| b.bead_id == "c-1"),
            "include_closed should surface c-1"
        );
    }

    #[test]
    fn min_relevance_gates_candidates() {
        let report = sample_report();

        // Default floor of 20 keeps bd-2 (relevance 100).
        let result =
            find_related_work_at(&report, "bd-1", &default_related_work_options(), now()).unwrap();
        assert!(RelatedWorkResult::category(&result.file_overlap)
            .iter()
            .any(|b| b.bead_id == "bd-2"));

        // The gate is `relevance < min_relevance`, so a floor equal to a
        // candidate's score keeps it; 101 is what actually drops everything.
        let mut strict = default_related_work_options();
        strict.min_relevance = 100;
        let at_100 = find_related_work_at(&report, "bd-1", &strict, now()).unwrap();
        assert!(
            RelatedWorkResult::category(&at_100.file_overlap)
                .iter()
                .any(|b| b.bead_id == "bd-2"),
            "100 < 100 is false"
        );

        strict.min_relevance = 101;
        let over_100 = find_related_work_at(&report, "bd-1", &strict, now()).unwrap();
        assert!(
            RelatedWorkResult::category(&over_100.file_overlap).is_empty(),
            "{:?}",
            over_100.file_overlap
        );
        // Concurrency (relevance 30 base) drops at the default floor too.
        assert!(
            RelatedWorkResult::category(&over_100.concurrent).is_empty(),
            "{:?}",
            over_100.concurrent
        );
    }

    /// A candidate touching only the ±concurrency_window slack qualifies as
    /// concurrent but must not earn the scaled overlap bonus: relevance is
    /// measured against the target's own `[start, end]` span.
    #[test]
    fn concurrent_bonus_ignores_slack_only_overlap() {
        let mut histories = BTreeMap::new();
        histories.insert(
            "bd-1".into(),
            history(
                "bd-1",
                "target",
                "open",
                Some("2026-01-10T00:00:00Z"),
                None,
                vec![commit("aaaaaaaa1", "2026-01-11T00:00:00Z", &["src/a.rs"])],
            ),
        );
        // Closed 2026-01-05, five days before the target's start — inside the
        // default 7-day slack, outside the target's real window.
        histories.insert(
            "bd-slack".into(),
            history(
                "bd-slack",
                "slack neighbour",
                "closed",
                Some("2026-01-01T00:00:00Z"),
                Some("2026-01-05T00:00:00Z"),
                vec![commit("bbbbbbbb1", "2026-01-02T00:00:00Z", &["other/z.rs"])],
            ),
        );
        let report = report(histories);
        let mut opts = default_related_work_options();
        opts.include_closed = true;
        let result = find_related_work_at(
            &report,
            "bd-1",
            &opts,
            "2026-01-20T00:00:00Z".parse().unwrap(),
        )
        .unwrap();
        let concurrent = RelatedWorkResult::category(&result.concurrent);
        let slack = concurrent
            .iter()
            .find(|b| b.bead_id == "bd-slack")
            .expect("slack-only neighbour still qualifies");
        assert_eq!(slack.relevance, 30, "{slack:?}");
        assert_eq!(slack.reason, "Active in same time window");
    }

    #[test]
    fn commit_overlap_is_empty_when_no_commit_is_shared() {
        // bd-1 owns both of its commits, so the commit index maps them only to
        // bd-1 itself, which the detector skips.
        let report = sample_report();
        let result =
            find_related_work_at(&report, "bd-1", &default_related_work_options(), now()).unwrap();
        assert!(
            RelatedWorkResult::category(&result.commit_overlap).is_empty(),
            "{:?}",
            result.commit_overlap
        );
    }

    /// A report whose candidates deliberately share no files with the target,
    /// so the dependency detector is not starved by the file-overlap `seen`
    /// set. bd-1 depends directly on d-1 and d-3; d-1 in turn depends on d-2.
    fn dependency_report() -> HistoryReport {
        let mut histories = BTreeMap::new();
        histories.insert(
            "bd-1".into(),
            history(
                "bd-1",
                "target",
                "open",
                Some("2026-01-01T00:00:00Z"),
                None,
                vec![commit(
                    "1111111a",
                    "2026-01-02T00:00:00Z",
                    &["src/target.rs"],
                )],
            ),
        );
        histories.insert(
            "d-1".into(),
            history(
                "d-1",
                "direct",
                "open",
                Some("2026-01-01T00:00:00Z"),
                None,
                vec![commit("1111111b", "2026-01-02T00:00:00Z", &["other/x.rs"])],
            ),
        );
        histories.insert(
            "d-2".into(),
            history(
                "d-2",
                "indirect",
                "open",
                Some("2026-01-01T00:00:00Z"),
                None,
                vec![commit("1111111c", "2026-01-02T00:00:00Z", &["other/y.rs"])],
            ),
        );
        histories.insert(
            "d-3".into(),
            history(
                "d-3",
                "closed direct",
                "closed",
                Some("2026-01-01T00:00:00Z"),
                Some("2026-01-03T00:00:00Z"),
                vec![commit("1111111d", "2026-01-02T00:00:00Z", &["other/z.rs"])],
            ),
        );
        report(histories)
    }

    #[test]
    fn dependency_cluster_scores_one_hop_above_two() {
        let report = dependency_report();
        let mut opts = default_related_work_options();
        opts.dependency_graph = Some(BTreeMap::from([
            (
                "bd-1".to_string(),
                vec!["d-1".to_string(), "d-3".to_string()],
            ),
            ("d-1".to_string(), vec!["d-2".to_string()]),
        ]));
        let result = find_related_work_at(&report, "bd-1", &opts, now()).unwrap();
        let scored: Vec<(&str, i64, &str)> =
            RelatedWorkResult::category(&result.dependency_cluster)
                .iter()
                .map(|b| (b.bead_id.as_str(), b.relevance, b.reason.as_str()))
                .collect();
        assert!(
            scored.contains(&("d-1", 80, "Direct dependency")),
            "{scored:?}"
        );
        assert!(
            scored.contains(&("d-2", 40, "Indirect dependency (2 hops)")),
            "{scored:?}"
        );
        // d-3 is a closed direct dependency, so the status gate hides it.
        assert!(!scored.iter().any(|(id, _, _)| *id == "d-3"), "{scored:?}");
    }

    #[test]
    fn dependency_cluster_honors_include_closed() {
        let report = dependency_report();
        let mut opts = default_related_work_options();
        opts.include_closed = true;
        opts.dependency_graph = Some(BTreeMap::from([(
            "bd-1".to_string(),
            vec!["d-3".to_string()],
        )]));
        let result = find_related_work_at(&report, "bd-1", &opts, now()).unwrap();
        assert!(
            RelatedWorkResult::category(&result.dependency_cluster)
                .iter()
                .any(|b| b.bead_id == "d-3"),
            "{:?}",
            result.dependency_cluster
        );
    }

    #[test]
    fn dependency_cluster_includes_reverse_dependencies() {
        // bd-1 depends on nothing, but d-1 depends on bd-1.
        let report = dependency_report();
        let mut opts = default_related_work_options();
        opts.dependency_graph = Some(BTreeMap::from([(
            "d-1".to_string(),
            vec!["bd-1".to_string()],
        )]));
        let result = find_related_work_at(&report, "bd-1", &opts, now()).unwrap();
        assert_eq!(
            RelatedWorkResult::category(&result.dependency_cluster)
                .iter()
                .map(|b| (b.bead_id.as_str(), b.relevance, b.reason.as_str()))
                .collect::<Vec<_>>(),
            vec![("d-1", 80, "Direct dependency")]
        );
    }

    #[test]
    fn dependency_cluster_is_skipped_without_a_graph() {
        let report = sample_report();
        let result =
            find_related_work_at(&report, "bd-1", &default_related_work_options(), now()).unwrap();
        assert!(RelatedWorkResult::category(&result.dependency_cluster).is_empty());
    }

    #[test]
    fn total_related_sums_every_category() {
        let report = sample_report();
        let mut opts = default_related_work_options();
        opts.dependency_graph = Some(BTreeMap::from([(
            "bd-1".to_string(),
            vec!["bd-2".to_string()],
        )]));
        let result = find_related_work_at(&report, "bd-1", &opts, now()).unwrap();
        assert_eq!(
            result.total_related,
            RelatedWorkResult::category(&result.file_overlap).len()
                + RelatedWorkResult::category(&result.commit_overlap).len()
                + RelatedWorkResult::category(&result.dependency_cluster).len()
                + RelatedWorkResult::category(&result.concurrent).len()
        );
    }

    #[test]
    fn a_bead_is_claimed_by_only_the_first_detector() {
        let report = sample_report();
        let mut opts = default_related_work_options();
        opts.dependency_graph = Some(BTreeMap::from([(
            "bd-1".to_string(),
            vec!["bd-2".to_string()],
        )]));
        let result = find_related_work_at(&report, "bd-1", &opts, now()).unwrap();
        // bd-2 overlaps bd-1 on files, so the dependency detector must not
        // re-claim it.
        assert!(RelatedWorkResult::category(&result.file_overlap)
            .iter()
            .any(|b| b.bead_id == "bd-2"));
        assert!(
            !RelatedWorkResult::category(&result.dependency_cluster)
                .iter()
                .any(|b| b.bead_id == "bd-2"),
            "bd-2 already claimed by file_overlap"
        );
    }

    #[test]
    fn max_results_truncates_each_category() {
        let report = sample_report();
        let mut opts = default_related_work_options();
        opts.dependency_graph = Some(BTreeMap::from([
            ("bd-1".to_string(), vec!["bd-2".to_string()]),
            ("bd-2".to_string(), vec!["bd-3".to_string()]),
        ]));
        opts.include_closed = true;
        opts.max_results = 1;
        let result = find_related_work_at(&report, "bd-1", &opts, now()).unwrap();
        for category in [
            RelatedWorkResult::category(&result.file_overlap),
            RelatedWorkResult::category(&result.commit_overlap),
            RelatedWorkResult::category(&result.dependency_cluster),
            RelatedWorkResult::category(&result.concurrent),
        ] {
            assert!(category.len() <= 1, "{category:?}");
        }
    }

    #[test]
    fn reason_formatting_matches_go() {
        // Singular forms carry no percentage; plurals carry one.
        assert_eq!(format_file_overlap_reason(1, 3), "1 shared file");
        assert_eq!(format_file_overlap_reason(2, 4), "2 shared files (50%)");
        assert_eq!(format_file_overlap_reason(3, 3), "3 shared files (100%)");
        assert_eq!(format_commit_overlap_reason(1, 2), "1 shared commit");
        assert_eq!(format_commit_overlap_reason(2, 8), "2 shared commits (25%)");
        // Whole days, truncated; sub-day overlaps get the generic phrasing.
        assert_eq!(format_concurrent_reason(0), "Active in same time window");
        assert_eq!(
            format_concurrent_reason(23 * 60 * 60 * 1_000_000_000),
            "Active in same time window"
        );
        assert_eq!(
            format_concurrent_reason(24 * 60 * 60 * 1_000_000_000),
            "1 day of overlapping activity"
        );
        assert_eq!(
            format_concurrent_reason(49 * 60 * 60 * 1_000_000_000),
            "2 days of overlapping activity"
        );
    }

    #[test]
    fn should_skip_related_status_drops_tombstones_always() {
        assert!(should_skip_related_status("tombstone", false));
        assert!(should_skip_related_status(" TOMBSTONE ", false));
        assert!(should_skip_related_status("tombstone", true));
        assert!(should_skip_related_status("closed", false));
        assert!(!should_skip_related_status("closed", true));
        assert!(!should_skip_related_status("open", false));
    }
}
