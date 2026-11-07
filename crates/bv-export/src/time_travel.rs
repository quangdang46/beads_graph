//! Time-travel history for the pages export — port of Go `generateHistoryForExport`
//! (`cmd/bv/main.go:7043-7205`) and the two types it returns
//! (`cmd/bv/main.go:7024-7040`), gated in Go by `--pages-include-history`
//! (`cmd/bv/main.go:3179-3195`).
//!
//! The pages bundle's `data/history.json` drives the time-travel scrubber, so
//! this is not decoration: the flag defaults to **on** in Go
//! (`cmd/bv/main.go:1624`) and the Rust exporter wrote no history file at all,
//! leaving the scrubber with nothing to play.
//!
//! The shape is not a re-export of [`bv_correlation::history::HistoryReport`]
//! and must not become one. The timeline is built from *recorded issue
//! events* only — never from inferred code correlations and never from an
//! issue's status today. An issue can be reopened, and editing a closed record
//! must not make it visible again, so `BeadEvent::Modified` contributes
//! nothing to any of the three bead lists (Go `cmd/bv/main.go:7135-7148`).
//!
//! Ordering is Git *ancestry* order, not timestamp order, and the distinction
//! is load-bearing: the same commit can carry two author dates, and sorting by
//! date would replay a close before its creation. Go recovers ancestry with a
//! second `git log` (below); this module does the same.

use bv_core::model::Sprint;
use bv_correlation::extractor::EventType;
use bv_correlation::history::HistoryReport;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

use std::path::Path;

/// How many commits the ancestry walk considers — Go hard-codes `500` at
/// `cmd/bv/main.go:7161` and again as the correlation `Limit` at `:7084`.
const HISTORY_LIMIT: usize = 500;

/// Go `TimeTravelHistory` (`cmd/bv/main.go:7024-7030`).
#[derive(Debug, Clone, Serialize)]
pub struct TimeTravelHistory {
    pub generated_at: String,
    /// Go builds this by appending to a nil slice, so an empty timeline
    /// marshals as `null`, not `[]`. The field carries no `omitempty`, so the
    /// difference is visible in the file; see [`serialize_commits`].
    #[serde(serialize_with = "serialize_commits")]
    pub commits: Vec<TimeTravelCommit>,
    /// Beads observed unresolved before their first retained event — Go
    /// `cmd/bv/main.go:7183-7191`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub initial_beads: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sprints: Vec<Sprint>,
}

/// Go's `var commits []TimeTravelCommit` starts nil and is only ever grown by
/// `append`, so "no commits" is `null` in Go's JSON while "a commit with no
/// beads" is an object with its `omitempty` lists absent. Both are reproduced
/// here.
fn serialize_commits<S: serde::Serializer>(
    commits: &[TimeTravelCommit],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    if commits.is_empty() {
        serializer.serialize_none()
    } else {
        serializer.collect_seq(commits)
    }
}

/// Go `TimeTravelCommit` (`cmd/bv/main.go:7033-7040`).
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct TimeTravelCommit {
    pub sha: String,
    pub date: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub message: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub beads_added: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub beads_closed: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub beads_removed: Vec<String>,
}

/// Failures from building the timeline.
#[derive(Debug, thiserror::Error)]
pub enum TimeTravelError {
    /// `cmd/bv/main.go:7162-7165` — the ancestry walk failed.
    #[error("ordering timeline commits: {0}")]
    Order(#[source] std::io::Error),
    /// `cmd/bv/main.go:7170-7172`.
    #[error("timeline commit {0} missing from source history")]
    CommitNotInHistory(String),
    /// `cmd/bv/main.go:7192-7195`.
    #[error("loading timeline sprints: {0}")]
    Sprints(String),
}

/// Go `generateHistoryForExport` (`cmd/bv/main.go:7043-7205`), taking the
/// already-built correlation report rather than producing one.
///
/// Go generates the report itself; this crate cannot, because the report is
/// assembled by `bv-correlation` and already exists by the time the pages
/// exporter runs. Splitting it this way keeps the expensive walk where the
/// rest of the CLI's history handling lives and leaves this module with the
/// part that is genuinely export-specific.
///
/// * `repo` — the working directory the git commands run in.
/// * `beads_file` — the JSONL path, passed to the ancestry walk as a pathspec
///   so rename-following matches the extractor.
/// * `generated_at` — Go's `robotNow()`, formatted RFC3339.
pub fn generate_history_for_export(
    repo: &Path,
    beads_file: &str,
    report: &HistoryReport,
    generated_at: &str,
) -> Result<TimeTravelHistory, TimeTravelError> {
    let order = commit_ancestry_order(repo, beads_file)?;
    let (mut commits, commit_shas) = group_events_by_commit(report);

    for commit in &commits {
        // Go `cmd/bv/main.go:7170-7172` — a commit the extractors saw but the
        // ancestry walk did not means the two disagree about the history, and
        // silently dropping it would desynchronise the scrubber.
        if !order.contains_key(commit.sha.as_str()) {
            return Err(TimeTravelError::CommitNotInHistory(commit.sha.clone()));
        }
    }
    // Ancestry order, not timestamp order — cmd/bv/main.go:7175-7177.
    commits.sort_by_key(|c| order[c.sha.as_str()]);

    let initial_beads = initial_beads(report, &commit_shas, &order)?;

    let mut sprints = bv_core::sprint::load_sprints(repo).map_err(TimeTravelError::Sprints)?;
    sort_sprints(&mut sprints);

    Ok(TimeTravelHistory {
        generated_at: generated_at.to_string(),
        commits,
        initial_beads,
        sprints,
    })
}

/// Go `cmd/bv/main.go:7113-7155`: fold every recorded event into one entry per
/// commit, with the three bead lists sorted.
///
/// Returns the commits plus the set of SHAs they cover, which
/// [`initial_beads`] needs to decide which events are on the timeline at all.
fn group_events_by_commit(report: &HistoryReport) -> (Vec<TimeTravelCommit>, BTreeSet<String>) {
    // `BTreeMap` rather than Go's map so the pre-sort iteration is
    // deterministic; Go relies on the later `sort.Strings` on each list to
    // reach the same result, so the two agree either way.
    let mut commit_map: BTreeMap<&str, TimeTravelCommit> = BTreeMap::new();
    for (bead_id, history) in &report.histories {
        for event in &history.events {
            // Go `cmd/bv/main.go:7115`: an unplaced event contributes nothing.
            if event.commit_sha.is_empty() || is_go_zero_time(&event.timestamp) {
                continue;
            }
            let commit = commit_map
                .entry(event.commit_sha.as_str())
                .or_insert_with(|| TimeTravelCommit {
                    sha: event.commit_sha.clone(),
                    date: event.timestamp.clone(),
                    message: event.commit_msg.clone(),
                    ..TimeTravelCommit::default()
                });
            match event.event_type {
                EventType::Created | EventType::Reopened => {
                    commit.beads_added.push(bead_id.clone());
                    // A record can be created already closed (a reopen of a
                    // tombstone, an import); Go checks `After`, not the type.
                    if event.after.as_ref().is_some_and(|after| {
                        let status = after.status.trim();
                        status.eq_ignore_ascii_case("closed")
                            || status.eq_ignore_ascii_case("tombstone")
                    }) {
                        commit.beads_closed.push(bead_id.clone());
                    }
                }
                EventType::Closed => commit.beads_closed.push(bead_id.clone()),
                EventType::Deleted => commit.beads_removed.push(bead_id.clone()),
                // Claimed and Modified are not timeline events.
                EventType::Claimed | EventType::Modified => {}
            }
        }
    }

    for commit in commit_map.values_mut() {
        // Go `cmd/bv/main.go:7153-7155`.
        commit.beads_added.sort();
        commit.beads_closed.sort();
        commit.beads_removed.sort();
    }
    let shas: BTreeSet<String> = commit_map.keys().map(|s| (*s).to_string()).collect();
    (commit_map.into_values().collect(), shas)
}

/// Go `cmd/bv/main.go:7180-7191`: the beads that were already open when their
/// oldest retained event happened, so the animation can show them before the
/// first commit that mentions them.
fn initial_beads(
    report: &HistoryReport,
    commit_shas: &BTreeSet<String>,
    order: &BTreeMap<String, i64>,
) -> Result<Vec<String>, TimeTravelError> {
    let mut initial = Vec::new();
    for (bead_id, history) in &report.histories {
        // The oldest retained event for this bead, in ancestry order.
        let first = history
            .events
            .iter()
            .filter(|e| commit_shas.contains(&e.commit_sha))
            .min_by_key(|e| order[&e.commit_sha]);
        let Some(first) = first else { continue };
        // Go `cmd/bv/main.go:7186` — a later observation or today's status
        // cannot fill in an unknown boundary, so an unparsed transition or a
        // missing "before" snapshot leaves the start state unknown.
        if !first.transition_observed {
            continue;
        }
        let Some(before) = &first.before else {
            continue;
        };
        let status = before.status.trim().to_lowercase();
        if !status.is_empty() && status != "closed" && status != "tombstone" {
            initial.push(bead_id.clone());
        }
    }
    initial.sort();
    Ok(initial)
}

/// Go `cmd/bv/main.go:7157-7169`.
///
/// The walk deliberately mirrors the extractor's: `--topo-order` for ancestry,
/// `--follow` so a renamed JSONL still resolves, and `--reverse` is *not* used
/// because it interferes with following the old name. The rank is negated so
/// that ascending sort is newest-first, which is the playback order.
fn commit_ancestry_order(
    repo: &Path,
    beads_file: &str,
) -> Result<BTreeMap<String, i64>, TimeTravelError> {
    let output = std::process::Command::new("git")
        .args([
            "log",
            "--format=%H",
            "--topo-order",
            "--follow",
            "-n",
            &HISTORY_LIMIT.to_string(),
            "HEAD",
            "--",
            beads_file,
        ])
        .current_dir(repo)
        .output()
        .map_err(TimeTravelError::Order)?;

    if !output.status.success() {
        // Go surfaces `cmd.Run`'s error, which for a non-zero exit is
        // "exit status N"; the stderr text is the useful part.
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.lines().next().unwrap_or("").trim();
        let e = if detail.is_empty() {
            std::io::Error::other(format!(
                "exit status {}",
                output.status.code().unwrap_or(-1)
            ))
        } else {
            std::io::Error::other(detail.to_string())
        };
        return Err(TimeTravelError::Order(e));
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let mut order = BTreeMap::new();
    for (i, sha) in text.split_whitespace().enumerate() {
        // Go `cmd/bv/main.go:7167`: `order[sha] = -i`.
        order.insert(sha.to_string(), -(i as i64));
    }
    Ok(order)
}

/// Go `cmd/bv/main.go:7196-7201`: by start date, ties broken by ID.
///
/// A missing `start_date` is Go's zero `time.Time`, which sorts before every
/// real date — and `None` orders before `Some` here, so that falls out.
fn sort_sprints(sprints: &mut [Sprint]) {
    sprints.sort_by(|a, b| {
        sprint_start(a)
            .cmp(&sprint_start(b))
            .then_with(|| a.id.cmp(&b.id))
    });
}

fn sprint_start(sprint: &Sprint) -> Option<jiff::Timestamp> {
    // bv-core's own sprint code parses dates with `model::parse_ts`, so the
    // same helper is used here to keep the two agreeing on what a
    // `start_date` is.
    bv_core::model::parse_ts(&sprint.start_date)
}

/// Go's `time.Time.IsZero()` against a textual field.
///
/// IsZero is exact: the instant `0001-01-01T00:00:00Z` and nothing else.
/// Prefix-matching the date would also swallow `0001-01-01T00:00:01Z` (one
/// second later, not zero) and any offset-shifted spelling.
fn is_go_zero_time(raw: &str) -> bool {
    if raw.is_empty() {
        return true;
    }
    match raw.parse::<jiff::Timestamp>() {
        Ok(ts) => ts == go_zero_instant(),
        // Unparseable text is not the zero time; Go's parse would already
        // have rejected the record upstream.
        Err(_) => false,
    }
}

/// The instant Go's zero `time.Time` denotes, parsed once per call site —
/// cheap enough that a `LazyLock` would be ceremony.
fn go_zero_instant() -> jiff::Timestamp {
    "0001-01-01T00:00:00Z"
        .parse()
        .expect("Go zero time is a valid RFC 3339 instant")
}

#[cfg(test)]
mod tests {
    use super::*;
    use bv_correlation::extractor::{BeadEvent, HistoricalIssueState};
    use bv_correlation::history::{BeadHistory, HistoryStats, HistoryWindow};

    fn event(
        bead: &str,
        ty: EventType,
        sha: &str,
        timestamp: &str,
        before: Option<&str>,
        after: Option<&str>,
    ) -> BeadEvent {
        BeadEvent {
            bead_id: bead.to_string(),
            event_type: ty,
            timestamp: timestamp.to_string(),
            commit_sha: sha.to_string(),
            commit_msg: format!("commit {sha}"),
            author: "dev".to_string(),
            author_email: "dev@example.com".to_string(),
            before: before.map(|s| HistoricalIssueState {
                id: bead.to_string(),
                status: s.to_string(),
                title: bead.to_string(),
                dependencies: Vec::new(),
            }),
            after: after.map(|s| HistoricalIssueState {
                id: bead.to_string(),
                status: s.to_string(),
                title: bead.to_string(),
                dependencies: Vec::new(),
            }),
            transition_observed: true,
        }
    }

    fn report(histories: Vec<(&str, Vec<BeadEvent>)>) -> HistoryReport {
        let mut map = BTreeMap::new();
        for (id, events) in histories {
            map.insert(
                id.to_string(),
                BeadHistory {
                    bead_id: id.to_string(),
                    title: id.to_string(),
                    status: "open".to_string(),
                    events,
                    milestones: Default::default(),
                    commits: None,
                    cycle_time: None,
                    last_author: "dev".to_string(),
                },
            );
        }
        HistoryReport {
            generated_at: "2024-01-15T10:00:00Z".to_string(),
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
            histories: map,
            commit_index: BTreeMap::new(),
            causal_history: None,
        }
    }

    #[test]
    fn events_map_onto_the_three_bead_lists() {
        // cmd/bv/main.go:7135-7148
        let report = report(vec![(
            "bd-1",
            vec![
                event(
                    "bd-1",
                    EventType::Created,
                    "sha-a",
                    "2024-01-01T00:00:00Z",
                    None,
                    Some("open"),
                ),
                event(
                    "bd-1",
                    EventType::Claimed,
                    "sha-b",
                    "2024-01-02T00:00:00Z",
                    Some("open"),
                    Some("in_progress"),
                ),
                event(
                    "bd-1",
                    EventType::Closed,
                    "sha-c",
                    "2024-01-03T00:00:00Z",
                    Some("in_progress"),
                    Some("closed"),
                ),
                event(
                    "bd-1",
                    EventType::Modified,
                    "sha-d",
                    "2024-01-04T00:00:00Z",
                    Some("closed"),
                    Some("closed"),
                ),
                event(
                    "bd-1",
                    EventType::Deleted,
                    "sha-e",
                    "2024-01-05T00:00:00Z",
                    Some("closed"),
                    None,
                ),
            ],
        )]);
        let (commits, shas) = group_events_by_commit(&report);
        // Every commit that carried an event appears, including the one whose
        // only event is a Modified — Go creates the entry from the event
        // before deciding what the event means.
        assert_eq!(commits.len(), 5);
        assert_eq!(shas.len(), 5);

        let by_sha = |sha: &str| commits.iter().find(|c| c.sha == sha).unwrap();
        assert_eq!(by_sha("sha-a").beads_added, ["bd-1"]);
        assert!(by_sha("sha-a").beads_closed.is_empty());

        // Claimed and Modified populate no list at all.
        assert!(by_sha("sha-b").beads_added.is_empty());
        assert!(by_sha("sha-b").beads_closed.is_empty());
        assert!(by_sha("sha-b").beads_removed.is_empty());
        assert!(by_sha("sha-d").beads_added.is_empty());
        assert!(by_sha("sha-d").beads_closed.is_empty());
        assert!(by_sha("sha-d").beads_removed.is_empty());

        assert_eq!(by_sha("sha-c").beads_closed, ["bd-1"]);
        assert!(by_sha("sha-c").beads_added.is_empty());
        assert_eq!(by_sha("sha-e").beads_removed, ["bd-1"]);

        // The commit's date and message come from the first event that
        // created the entry.
        assert_eq!(by_sha("sha-a").date, "2024-01-01T00:00:00Z");
        assert_eq!(by_sha("sha-a").message, "commit sha-a");
    }

    #[test]
    fn a_record_created_closed_lands_in_both_lists() {
        // Go checks `After.Status` on a Created/Reopened event, so an import
        // that lands an issue already closed is added *and* closed. The
        // comparison is case-insensitive and trims.
        //
        // Note the bead list is keyed by the *history map key*, not by
        // `BeadEvent.bead_id` — Go iterates `for beadID, history := range
        // report.Histories` — so the two beads need separate entries.
        let report = report(vec![
            (
                "bd-1",
                vec![event(
                    "bd-1",
                    EventType::Created,
                    "sha-a",
                    "2024-01-01T00:00:00Z",
                    None,
                    Some("  TOMBSTONE "),
                )],
            ),
            (
                "bd-2",
                vec![event(
                    "bd-2",
                    EventType::Reopened,
                    "sha-a",
                    "2024-01-01T00:00:00Z",
                    None,
                    Some("Closed"),
                )],
            ),
        ]);
        let (commits, _) = group_events_by_commit(&report);
        assert_eq!(commits[0].beads_added, ["bd-1", "bd-2"]);
        assert_eq!(commits[0].beads_closed, ["bd-1", "bd-2"]);
    }

    #[test]
    fn unplaced_events_contribute_nothing() {
        // Go `cmd/bv/main.go:7115` skips an empty SHA or a zero timestamp
        // before the event is even associated with a commit.
        let report = report(vec![(
            "bd-1",
            vec![
                event(
                    "bd-1",
                    EventType::Created,
                    "",
                    "2024-01-01T00:00:00Z",
                    None,
                    Some("open"),
                ),
                event("bd-1", EventType::Created, "sha-z", "", None, Some("open")),
                event(
                    "bd-1",
                    EventType::Created,
                    "sha-y",
                    "0001-01-01T00:00:00Z",
                    None,
                    Some("open"),
                ),
            ],
        )]);
        let (commits, shas) = group_events_by_commit(&report);
        assert!(commits.is_empty());
        assert!(shas.is_empty());
    }

    #[test]
    fn a_bead_whose_start_state_is_unknown_or_closed_is_not_initial() {
        // cmd/bv/main.go:7186-7191 — the decision reads `first.Before.Status`,
        // the state the bead was in *before* its oldest retained event. A bead
        // that was already closed then was never part of the working set.
        let already_closed = report(vec![(
            "bd-closed",
            vec![event(
                "bd-closed",
                EventType::Reopened,
                "sha-a",
                "2024-01-01T00:00:00Z",
                Some(" CLOSED "),
                Some("open"),
            )],
        )]);
        let already_tombstoned = report(vec![(
            "bd-tomb",
            vec![event(
                "bd-tomb",
                EventType::Reopened,
                "sha-a",
                "2024-01-01T00:00:00Z",
                Some("tombstone"),
                Some("open"),
            )],
        )]);
        // No `before` snapshot means the start state is unknown, so the bead is
        // not claimed as initially open either.
        let unknown_first = report(vec![(
            "bd-unknown",
            vec![event(
                "bd-unknown",
                EventType::Created,
                "sha-a",
                "2024-01-01T00:00:00Z",
                None,
                Some("open"),
            )],
        )]);
        // An unparsed transition is also unknown.
        let unparsed = {
            let mut r = report(vec![(
                "bd-unparsed",
                vec![event(
                    "bd-unparsed",
                    EventType::Created,
                    "sha-a",
                    "2024-01-01T00:00:00Z",
                    Some("open"),
                    Some("open"),
                )],
            )]);
            r.histories.get_mut("bd-unparsed").unwrap().events[0].transition_observed = false;
            r
        };
        let open_first = report(vec![(
            "bd-open",
            vec![event(
                "bd-open",
                EventType::Created,
                "sha-a",
                "2024-01-01T00:00:00Z",
                Some("in_progress"),
                Some("open"),
            )],
        )]);

        let order = BTreeMap::from([("sha-a".to_string(), 0)]);
        let shas = BTreeSet::from(["sha-a".to_string()]);
        for (report, expected) in [
            (&already_closed, vec![]),
            (&already_tombstoned, vec![]),
            (&unknown_first, vec![]),
            (&unparsed, vec![]),
            (&open_first, vec!["bd-open"]),
        ] {
            let initial = initial_beads(report, &shas, &order).unwrap();
            assert_eq!(initial, expected);
        }
    }

    #[test]
    fn initial_beads_picks_the_oldest_retained_event() {
        // cmd/bv/main.go:7181-7185 — the *oldest* event decides the start
        // state, so a bead that was open when it was created stays initial
        // even though its latest event closed it.
        let report = report(vec![(
            "bd-1",
            vec![
                event(
                    "bd-1",
                    EventType::Created,
                    "sha-old",
                    "2024-01-01T00:00:00Z",
                    Some("open"),
                    Some("open"),
                ),
                event(
                    "bd-1",
                    EventType::Closed,
                    "sha-new",
                    "2024-02-01T00:00:00Z",
                    Some("open"),
                    Some("closed"),
                ),
            ],
        )]);
        // Ancestry order: sha-new is rank 0 (newest), sha-old is rank -1.
        let order = BTreeMap::from([("sha-new".to_string(), 0), ("sha-old".to_string(), -1)]);
        let shas = BTreeSet::from(["sha-new".to_string(), "sha-old".to_string()]);
        assert_eq!(initial_beads(&report, &shas, &order).unwrap(), ["bd-1"]);
    }

    #[test]
    fn sprints_sort_by_start_date_then_id() {
        // cmd/bv/main.go:7196-7201
        let sprint = |id: &str, start: Option<&str>| Sprint {
            id: id.to_string(),
            name: id.to_string(),
            start_date: start.map(str::to_string),
            end_date: None,
            bead_ids: Vec::new(),
            velocity_target: None,
            created_at: None,
            updated_at: None,
        };
        let mut sprints = vec![
            sprint("b", Some("2024-02-01T00:00:00Z")),
            sprint("a", Some("2024-02-01T00:00:00Z")),
            sprint("c", Some("2024-01-01T00:00:00Z")),
            sprint("z", None),
        ];
        sort_sprints(&mut sprints);
        let ids: Vec<&str> = sprints.iter().map(|s| s.id.as_str()).collect();
        // A missing start date is Go's zero time, so it sorts first.
        assert_eq!(ids, ["z", "c", "a", "b"]);
    }

    #[test]
    fn empty_commits_marshal_as_null_like_go() {
        // cmd/bv/main.go:7151 — `var commits []TimeTravelCommit` is nil when no
        // commit carried an event, and `json:"commits"` has no omitempty.
        let history = TimeTravelHistory {
            generated_at: "2024-01-15T10:00:00Z".to_string(),
            commits: Vec::new(),
            initial_beads: Vec::new(),
            sprints: Vec::new(),
        };
        assert_eq!(
            serde_json::to_string(&history).unwrap(),
            r#"{"generated_at":"2024-01-15T10:00:00Z","commits":null}"#
        );
    }

    #[test]
    fn a_commit_with_no_beads_omits_its_lists() {
        // Go's `omitempty` on all three lists plus the message.
        let history = TimeTravelHistory {
            generated_at: "2024-01-15T10:00:00Z".to_string(),
            commits: vec![TimeTravelCommit {
                sha: "abc".to_string(),
                date: "2024-01-01T00:00:00Z".to_string(),
                message: String::new(),
                beads_added: Vec::new(),
                beads_closed: Vec::new(),
                beads_removed: Vec::new(),
            }],
            initial_beads: Vec::new(),
            sprints: Vec::new(),
        };
        assert_eq!(
            serde_json::to_string(&history).unwrap(),
            r#"{"generated_at":"2024-01-15T10:00:00Z","commits":[{"sha":"abc","date":"2024-01-01T00:00:00Z"}]}"#
        );
    }

    #[test]
    fn go_zero_time_detection() {
        assert!(is_go_zero_time(""));
        assert!(is_go_zero_time("0001-01-01T00:00:00Z"));
        assert!(!is_go_zero_time("2024-01-01T00:00:00Z"));
        // Same date, different instant — IsZero is exact, not a prefix match.
        assert!(!is_go_zero_time("0001-01-01T00:00:01Z"));
        // ...but the same instant spelled with an offset still is zero:
        // 01:00:00+01:00 is 00:00:00Z.
        assert!(is_go_zero_time("0001-01-01T01:00:00+01:00"));
    }
}
