//! Orphan-commit detection — port of Go `pkg/correlation/orphan.go` plus the
//! two lookups it drives: `reverse.go`'s [`find_orphan_commits`] (the bounded
//! commit walk and the correlated/orphan split) and `file_index.go`'s
//! `BuildFileIndex`/`LookupByFile` (file → beads evidence).
//!
//! An orphan is a commit the correlation index never linked to a bead, inside
//! the *same* window that index covers (`source: "history_index"`), that also
//! changed something outside `.beads/`. Each orphan is then scored by four
//! heuristics — timing, files, message, author — whose signal weights are Go's
//! verbatim.

use crate::history::{self, HistoryOptions, HistoryReport};
use jiff::Timestamp;
use regex::Regex;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::LazyLock;

/// One message pattern + its contribution weight (Go orphanMessagePatterns).
struct MessagePattern {
    re: Regex,
    weight: i32,
}

static MESSAGE_PATTERNS: LazyLock<Vec<MessagePattern>> = LazyLock::new(|| {
    vec![
        MessagePattern {
            re: Regex::new(r"\b(fix|fixes|fixed)\b").unwrap(),
            weight: 10,
        },
        MessagePattern {
            re: Regex::new(r"\b(close|closes|closed)\b").unwrap(),
            weight: 10,
        },
        MessagePattern {
            re: Regex::new(r"\b(resolve|resolves|resolved)\b").unwrap(),
            weight: 10,
        },
        MessagePattern {
            re: Regex::new(r"\b(implement|implements|implemented)\b").unwrap(),
            weight: 8,
        },
        MessagePattern {
            re: Regex::new(r"\b(add|adds|added)\b").unwrap(),
            weight: 5,
        },
        MessagePattern {
            re: Regex::new(r"#\d+").unwrap(),
            weight: 15,
        },
        // lowercase JIRA-style (message is lowercased before matching)
        MessagePattern {
            re: Regex::new(r"\b[a-z]{2,5}-\d+\b").unwrap(),
            weight: 20,
        },
        MessagePattern {
            re: Regex::new(r"\bbv-[a-z0-9]+\b").unwrap(),
            weight: 25,
        },
        MessagePattern {
            re: Regex::new(r"\bbeads?[-_]?\d+\b").unwrap(),
            weight: 25,
        },
    ]
});

fn message_patterns() -> &'static [MessagePattern] {
    &MESSAGE_PATTERNS
}

/// Go `orphanBeadIDPattern`: bv-XXXX.. case-insensitive ID extraction.
pub fn extract_bv_ids(message_lower: &str) -> Vec<String> {
    let re = Regex::new(r"(?i)\bbv-([a-z0-9]{4,8})\b").unwrap();
    re.captures_iter(message_lower)
        .map(|c| format!("bv-{}", c[1].to_lowercase()))
        .collect()
}

/// Go `checkMessage`: total pattern weight capped at 35.
pub fn message_suspicion(message: &str) -> (i32, Vec<String>) {
    let msg = message.to_lowercase();
    let mut total = 0i32;
    let mut details = Vec::new();
    for p in message_patterns() {
        if let Some(m) = p.re.find(&msg) {
            total += p.weight;
            details.push(m.as_str().to_string());
        }
    }
    let cap = total.min(35);
    (cap, details)
}

/// Suspicion signal weights for non-message factors (Go checkTiming/checkFiles).
pub const TIMING_MATCH_WEIGHT: i32 = 30;
pub const FILE_OVERLAP_BASE_WEIGHT: i32 = 25;
pub const MENTIONED_BEAD_SCORE: i32 = 35;
pub const AUTHOR_NEARBY_WEIGHT: i32 = 15;

/// Go's per-hit weight for a registered `--id-pattern` in `checkMessage`
/// (orphan.go:455) — the same 25 the `bv-` / `bead-` built-ins carry.
const CUSTOM_ID_PATTERN_WEIGHT: i32 = 25;

/// Go `orphanUsageHints` — documents how the payload is computed.
const ORPHAN_USAGE_HINTS: &[&str] = &[
    "window: the non-merge commits scanned; source=history_index means the same bounded walk the correlation index (co_committed, explicit_id, temporal_author) covered, so an orphan is a code commit in that window no strategy linked",
    "stats.total_commits counts window commits that changed something outside .beads/; stats.beads_only_commits are the bookkeeping commits (e.g. `br sync`) skipped as neither orphans nor correlated, so window.commits = total_commits + beads_only_commits",
    "a high orphan_ratio usually means code and tracker updates land in separate commits; only explicit bead ids in messages, temporal author overlap, or confirm feedback can link those code commits",
    "suspicion_score (0-100, capped) sums signal weights: timing=30 per bead whose claimed→closed window contains the commit, files=25 per (file, bead) pair where a linked bead touched the same path, message<=35 for bead-like patterns in the subject, author=15 per bead the same author worked on within +/-7 days",
    "probable_beads.confidence adds 35 when the message names the bead id; the top 3 beads are kept; candidates below --orphans-min-score are dropped",
    "link a candidate with --robot-confirm-correlation SHA:BEAD (or exclude a false link with --robot-reject-correlation); both shape later history reports",
];

// ---------------------------------------------------------------------------
// Output types (Go orphan.go)
// ---------------------------------------------------------------------------

/// Go `OrphanSignal` — why a commit might be orphaned.
pub const SIGNAL_TIMING: &str = "timing";
pub const SIGNAL_FILES: &str = "files";
pub const SIGNAL_MESSAGE: &str = "message";
pub const SIGNAL_AUTHOR: &str = "author";

/// Go `OrphanSignalHit`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OrphanSignalHit {
    pub signal: &'static str,
    pub details: String,
    pub weight: i32,
}

/// Go `ProbableBead`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProbableBead {
    pub bead_id: String,
    pub bead_title: String,
    pub bead_status: String,
    pub confidence: i32,
    pub reasons: Vec<String>,
}

/// Go `OrphanCandidate`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OrphanCandidate {
    pub sha: String,
    pub short_sha: String,
    pub message: String,
    pub author: String,
    pub author_email: String,
    pub timestamp: String,
    /// `omitempty` in Go: a commit with no surviving paths emits no key.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    pub suspicion_score: i32,
    pub probable_beads: Vec<ProbableBead>,
    pub signals: Vec<OrphanSignalHit>,
}

/// Go `OrphanWindow` — which commits orphan detection considered.
#[derive(Debug, Clone, Serialize)]
pub struct OrphanWindow {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub revision: String,
    pub commits: i64,
    pub limit: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    pub source: String,
}

/// Go `OrphanReportStats`.
#[derive(Debug, Clone, Serialize)]
pub struct OrphanReportStats {
    pub total_commits: i64,
    pub correlated_count: i64,
    pub orphan_count: i64,
    pub beads_only_commits: i64,
    pub candidate_count: i64,
    pub orphan_ratio: f64,
    pub avg_suspicion_score: f64,
}

/// Go `OrphanReport`.
#[derive(Debug, Clone, Serialize)]
pub struct OrphanReport {
    pub generated_at: String,
    pub data_hash: String,
    pub git_range: String,
    pub window: OrphanWindow,
    pub stats: OrphanReportStats,
    pub candidates: Vec<OrphanCandidate>,
    /// BeadID → commit SHAs. `BTreeMap` so encoding matches Go's sorted map keys.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub by_bead: BTreeMap<String, Vec<String>>,
    pub usage_hints: Vec<String>,
}

/// Go `OrphanCommit` — a walked commit with no bead linkage.
#[derive(Debug, Clone)]
struct OrphanCommit {
    sha: String,
    short_sha: String,
    message: String,
    author: String,
    author_email: String,
    timestamp: String,
    files: Vec<String>,
}

// ---------------------------------------------------------------------------
// File -> beads index (Go file_index.go)
// ---------------------------------------------------------------------------
//
// The index itself lives in `file_index.rs`, which ports Go's `FileLookup`
// in full (build, exact + prefix lookup, glob, co-change). This module used to
// carry its own reduced copy implementing only the exact-match arm; it is
// gone, so the crate has a single implementation of the index.

use crate::file_index::FileLookup;

// ---------------------------------------------------------------------------
// Detector (Go orphan.go)
// ---------------------------------------------------------------------------

/// Go `TemporalWindow` as the orphan detector uses it: the bead's active span.
#[derive(Debug, Clone)]
struct BeadWindow {
    title: String,
    start: Timestamp,
    end: Timestamp,
}

/// Go `probableBeadBuilder`.
#[derive(Debug, Default, Clone)]
struct ProbableBeadBuilder {
    title: String,
    status: String,
    score: i32,
    reasons: Vec<String>,
}

/// Go `normalizeStatus`.
fn normalize_status(status: &str) -> String {
    status.trim().to_lowercase()
}

/// Go `isClosedHistoryStatus`.
fn is_closed_history_status(status: &str) -> bool {
    let normalized = normalize_status(status);
    normalized == "closed" || normalized == "tombstone"
}

/// Go `orphanActivityWindowEnd` — the close instant for a closed bead, else the
/// caller's reference instant.
fn orphan_activity_window_end(history: &history::BeadHistory, now: Timestamp) -> Timestamp {
    if is_closed_history_status(&history.status) {
        if let Some(closed) = &history.milestones.closed {
            if let Some(ts) = history::parse_ts(&closed.timestamp) {
                return ts;
            }
        }
    }
    now
}

/// Nanoseconds in the author heuristic's ±7-day slack (Go `7 * 24 * time.Hour`).
const WEEK_NS: i128 = 7 * 24 * 60 * 60 * 1_000_000_000;

/// Go `OrphanDetector`.
pub struct OrphanDetector<'a> {
    repo: &'a Path,
    report: &'a HistoryReport,
    file_lookup: FileLookup,
    bead_windows: BTreeMap<String, BeadWindow>,
    author_beads: BTreeMap<String, Vec<String>>,
}

impl<'a> OrphanDetector<'a> {
    /// Go `newOrphanDetector` — index the report's bead windows and the
    /// author→beads map. `now` is the caller-owned reference instant used for
    /// open windows and for the report's own timestamp.
    pub fn new(repo: &'a Path, report: &'a HistoryReport, now: Timestamp) -> Self {
        let mut bead_windows = BTreeMap::new();
        let mut author_beads: BTreeMap<String, Vec<String>> = BTreeMap::new();

        for (bead_id, h) in &report.histories {
            if let Some(claimed) = &h.milestones.claimed {
                let Some(mut start) = history::parse_ts(&claimed.timestamp) else {
                    continue;
                };
                let end = orphan_activity_window_end(h, now);
                if !is_closed_history_status(&h.status) {
                    if let Some(reopened) = &h.milestones.reopened {
                        if let Some(rs) = history::parse_ts(&reopened.timestamp) {
                            if rs > start {
                                start = rs;
                            }
                        }
                    }
                }
                bead_windows.insert(
                    bead_id.clone(),
                    BeadWindow {
                        title: h.title.clone(),
                        start,
                        end,
                    },
                );
            }

            if !h.last_author.is_empty() {
                for commit in h.commits.iter().flatten() {
                    if commit.author_email.is_empty() {
                        continue;
                    }
                    let list = author_beads.entry(commit.author_email.clone()).or_default();
                    if !list.iter().any(|b| b == bead_id) {
                        list.push(bead_id.clone());
                    }
                }
            }
        }
        for beads in author_beads.values_mut() {
            beads.sort();
        }

        Self {
            repo,
            report,
            file_lookup: FileLookup::new(report),
            bead_windows,
            author_beads,
        }
    }

    /// Go `scoreMentionedBead` — credit the bead whose ID the message names.
    /// Case-insensitive, and ambiguous (case-colliding) IDs are ignored.
    fn score_mentioned_bead(
        &self,
        bead_scores: &mut BTreeMap<String, ProbableBeadBuilder>,
        bead_id: &str,
    ) {
        let mut matches: Vec<&str> = self
            .report
            .histories
            .keys()
            .filter(|id| id.eq_ignore_ascii_case(bead_id))
            .map(|s| s.as_str())
            .collect();
        if matches.len() != 1 {
            return;
        }
        let key = matches.remove(0).to_string();
        let builder = bead_scores
            .entry(key.clone())
            .or_insert_with(|| ProbableBeadBuilder {
                title: self.report.histories[&key].title.clone(),
                status: self.report.histories[&key].status.clone(),
                ..Default::default()
            });
        builder.score += MENTIONED_BEAD_SCORE;
        builder
            .reasons
            .push("bead ID mentioned in commit message".to_string());
    }

    /// Go `checkTiming` — commit lands inside a bead's active window.
    fn check_timing(
        &self,
        candidate: &mut OrphanCandidate,
        timestamp: Timestamp,
        bead_scores: &mut BTreeMap<String, ProbableBeadBuilder>,
    ) {
        for (bead_id, window) in &self.bead_windows {
            if timestamp <= window.start || timestamp >= window.end {
                continue;
            }
            candidate.signals.push(OrphanSignalHit {
                signal: SIGNAL_TIMING,
                details: format!("Commit during {bead_id} active period"),
                weight: TIMING_MATCH_WEIGHT,
            });
            let builder =
                bead_scores
                    .entry(bead_id.clone())
                    .or_insert_with(|| ProbableBeadBuilder {
                        title: window.title.clone(),
                        // "If we're here, it was active".
                        status: "in_progress".to_string(),
                        ..Default::default()
                    });
            builder.score += TIMING_MATCH_WEIGHT;
            builder
                .reasons
                .push("commit during active timeframe".to_string());
        }
    }

    /// Go `checkFiles` — commit touches a path a linked bead also touched.
    fn check_files(
        &self,
        candidate: &mut OrphanCandidate,
        bead_scores: &mut BTreeMap<String, ProbableBeadBuilder>,
    ) {
        if candidate.files.is_empty() {
            return;
        }
        for file in &candidate.files {
            let lookup = self.file_lookup.lookup_by_file(file);
            for r in lookup.open_beads.into_iter().chain(lookup.closed_beads) {
                candidate.signals.push(OrphanSignalHit {
                    signal: SIGNAL_FILES,
                    details: format!("Touches {file} (linked to {})", r.bead_id),
                    weight: FILE_OVERLAP_BASE_WEIGHT,
                });
                let builder =
                    bead_scores
                        .entry(r.bead_id.clone())
                        .or_insert_with(|| ProbableBeadBuilder {
                            title: r.title.clone(),
                            status: r.status.clone(),
                            ..Default::default()
                        });
                builder.score += FILE_OVERLAP_BASE_WEIGHT;
                builder.reasons.push(format!("touches file {file}"));
            }
        }
    }

    /// Go `checkMessage` — bead-like wording plus any bead ID the subject names.
    fn check_message(
        &self,
        candidate: &mut OrphanCandidate,
        bead_scores: &mut BTreeMap<String, ProbableBeadBuilder>,
    ) {
        let msg = candidate.message.to_lowercase();
        let (mut total, mut details) = message_suspicion(&candidate.message);
        // Custom ID patterns (--id-pattern, #188) count as strong ID signals,
        // matched against the original-case message since the pattern is
        // user-supplied, and they join the same weight total and detail list the
        // built-in patterns feed (Go orphan.go:449-457).
        for re in custom_id_patterns() {
            if let Some(m) = re.find(&candidate.message) {
                total += CUSTOM_ID_PATTERN_WEIGHT;
                details.push(m.as_str().to_string());
            }
        }
        if total > 0 {
            candidate.signals.push(OrphanSignalHit {
                signal: SIGNAL_MESSAGE,
                details: format!("Message patterns: {}", details.join(", ")),
                weight: total.min(35),
            });
        }

        let re = Regex::new(r"(?i)\bbv-([a-z0-9]{4,8})\b").unwrap();
        for caps in re.captures_iter(&msg) {
            if let Some(m) = caps.get(1) {
                self.score_mentioned_bead(
                    bead_scores,
                    &format!("bv-{}", m.as_str()).to_lowercase(),
                );
            }
        }

        for id in custom_id_matches(&candidate.message) {
            self.score_mentioned_bead(bead_scores, &id);
        }
    }

    /// Go `checkAuthor` — same author worked on a bead within ±7 days.
    fn check_author(
        &self,
        candidate: &mut OrphanCandidate,
        timestamp: Timestamp,
        bead_scores: &mut BTreeMap<String, ProbableBeadBuilder>,
    ) {
        if candidate.author_email.is_empty() {
            return;
        }
        let Some(bead_ids) = self.author_beads.get(&candidate.author_email) else {
            return;
        };
        let ts = timestamp.as_nanosecond();
        for bead_id in bead_ids {
            let Some(window) = self.bead_windows.get(bead_id) else {
                continue;
            };
            let start = window.start.as_nanosecond() - WEEK_NS;
            let end = window.end.as_nanosecond() + WEEK_NS;
            if ts <= start || ts >= end {
                continue;
            }
            candidate.signals.push(OrphanSignalHit {
                signal: SIGNAL_AUTHOR,
                details: format!("Author worked on {bead_id} around this time"),
                weight: AUTHOR_NEARBY_WEIGHT,
            });
            let builder = bead_scores.entry(bead_id.clone()).or_insert_with(|| {
                let history = self.report.histories.get(bead_id);
                ProbableBeadBuilder {
                    title: history
                        .map(|h| h.title.clone())
                        .unwrap_or_else(|| window.title.clone()),
                    status: history
                        .map(|h| h.status.clone())
                        .unwrap_or_else(|| "unknown".to_string()),
                    ..Default::default()
                }
            });
            builder.score += AUTHOR_NEARBY_WEIGHT;
            builder
                .reasons
                .push("same author worked on bead nearby".to_string());
        }
    }

    /// Go `analyzeOrphan` — run the four heuristics and score the commit.
    fn analyze_orphan(&self, orphan: &OrphanCommit) -> Result<OrphanCandidate, String> {
        let mut candidate = OrphanCandidate {
            sha: orphan.sha.clone(),
            short_sha: orphan.short_sha.clone(),
            message: orphan.message.clone(),
            author: orphan.author.clone(),
            author_email: orphan.author_email.clone(),
            timestamp: orphan.timestamp.clone(),
            files: orphan.files.clone(),
            suspicion_score: 0,
            probable_beads: Vec::new(),
            signals: Vec::new(),
        };

        // The walk already carries the changed paths, so no per-commit git show
        // is needed — unless every path was excluded, in which case the walk
        // yields a nil list and Go falls back to asking git. The fallback's
        // pathspec only excludes the *root* .beads/, .bv/, … directories, so a
        // nested path like `tests/fixtures/x/.beads/y` comes back here even
        // though the walk filtered it out.
        if candidate.files.is_empty() {
            candidate.files = self.get_commit_files(&orphan.sha)?;
        }
        let timestamp = history::parse_ts(&orphan.timestamp);
        let mut bead_scores: BTreeMap<String, ProbableBeadBuilder> = BTreeMap::new();

        if let Some(ts) = timestamp {
            self.check_timing(&mut candidate, ts, &mut bead_scores);
        }
        self.check_files(&mut candidate, &mut bead_scores);
        self.check_message(&mut candidate, &mut bead_scores);
        if let Some(ts) = timestamp {
            self.check_author(&mut candidate, ts, &mut bead_scores);
        }

        for (bead_id, builder) in &bead_scores {
            if builder.score > 0 {
                candidate.probable_beads.push(ProbableBead {
                    bead_id: bead_id.clone(),
                    bead_title: builder.title.clone(),
                    bead_status: builder.status.clone(),
                    confidence: builder.score.min(100),
                    reasons: builder.reasons.clone(),
                });
            }
        }

        // Confidence desc, bead ID asc; reasons and signals lexicographic.
        candidate.probable_beads.sort_by(|a, b| {
            b.confidence
                .cmp(&a.confidence)
                .then_with(|| a.bead_id.cmp(&b.bead_id))
        });
        for bead in candidate.probable_beads.iter_mut() {
            bead.reasons.sort();
        }
        candidate.signals.sort_by(|a, b| {
            a.signal
                .cmp(b.signal)
                .then_with(|| a.details.cmp(&b.details))
                .then_with(|| a.weight.cmp(&b.weight))
        });
        candidate.probable_beads.truncate(3);

        candidate.suspicion_score = candidate
            .signals
            .iter()
            .map(|s| s.weight)
            .sum::<i32>()
            .min(100);
        Ok(candidate)
    }

    /// Go `OrphanDetector.getCommitFiles` — one `git show --name-status` for a
    /// commit the metadata walk could not describe, filtered by the same
    /// root-directory pathspec the co-commit extractor uses.
    fn get_commit_files(&self, sha: &str) -> Result<Vec<String>, String> {
        let mut args = vec![
            "show".to_string(),
            "--name-status".to_string(),
            "--format=".to_string(),
            sha.to_string(),
        ];
        args.extend(history::exclude_pathspec_args());
        let out = std::process::Command::new("git")
            .args(&args)
            .current_dir(self.repo)
            .output()
            .map_err(|e| format!("get files changed: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "get files changed: git show failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let payload = String::from_utf8_lossy(&out.stdout);
        Ok(history::parse_name_status(&payload)
            .into_iter()
            .map(|f| f.path)
            .collect())
    }
}

/// Go `CustomIDPatterns` — the `--id-pattern` registrations (Go
/// `SetCustomIDPatterns`, main.go:1782). Empty exactly as it is in Go when the
/// flag is absent.
fn custom_id_patterns() -> Vec<Regex> {
    crate::explicit::custom_id_patterns()
}

/// Go `checkMessage`'s custom-pattern pass: the bead IDs a message names via a
/// registered `--id-pattern`, taking capture group 1 when the pattern has one
/// and the whole match otherwise. Go hands the *raw* group to
/// `scoreMentionedBead` here — unlike the explicit matcher, which lowercases
/// through `normalizeBeadID` — and the bead lookup is case-insensitive either
/// way (see `custom_id_pattern_capture_group_one_is_the_id`).
fn custom_id_matches(message: &str) -> Vec<String> {
    let mut out = Vec::new();
    for re in custom_id_patterns() {
        for caps in re.captures_iter(message) {
            let id = caps
                .get(1)
                .map(|m| m.as_str().to_string())
                .unwrap_or_else(|| caps[0].to_string());
            if !id.is_empty() {
                out.push(id);
            }
        }
    }
    out
}

/// Go `formatGitRange` — the orphan report's own (shorter) range spelling, which
/// differs from the history report's `describeGitRange`: no "commits" suffix.
fn format_git_range(opts: &HistoryOptions) -> String {
    if opts.revision.is_empty() && opts.since.is_none() && opts.until.is_none() && opts.limit == 0 {
        return "all history".to_string();
    }
    let mut parts: Vec<String> = Vec::new();
    if !opts.revision.is_empty() {
        parts.push(format!("at {}", opts.revision));
    }
    if let Some(since) = &opts.since {
        parts.push(format!("since {}", date_part(since)));
    }
    if let Some(until) = &opts.until {
        parts.push(format!("until {}", date_part(until)));
    }
    if opts.limit > 0 {
        parts.push(format!("limit {}", opts.limit));
    }
    if parts.is_empty() {
        return "all history".to_string();
    }
    parts.join(", ")
}

fn date_part(ts: &str) -> String {
    ts.split(['T', ' ']).next().unwrap_or(ts).to_string()
}

/// Go `ReverseLookup.FindOrphanCommits` — walk the report's window and split it
/// into beads-only bookkeeping, correlated code, and orphan code commits.
fn find_orphan_commits(
    repo: &Path,
    report: &HistoryReport,
) -> Result<(Vec<OrphanCommit>, OrphanWindow, OrphanReportStats), String> {
    let opts = HistoryOptions {
        revision: report.window.revision.clone(),
        limit: report.window.limit,
        since: report.window.since.clone(),
        until: report.window.until.clone(),
        ..Default::default()
    };
    let walk = history::walk_commits(repo, &opts)?;

    let mut orphans = Vec::new();
    let mut total = 0i64;
    let mut correlated = 0i64;
    let mut beads_only = 0i64;
    for wc in &walk {
        if wc.beads_only() {
            beads_only += 1;
            continue;
        }
        total += 1;
        if report.commit_index.contains_key(&wc.sha) {
            correlated += 1;
            continue;
        }
        let files = wc
            .files
            .iter()
            .filter(|f| !history::is_excluded_path(f))
            .cloned()
            .collect();
        orphans.push(OrphanCommit {
            sha: wc.sha.clone(),
            short_sha: history::short_sha(&wc.sha),
            message: wc.subject.clone(),
            author: wc.author.clone(),
            author_email: wc.author_email.clone(),
            timestamp: wc.timestamp.clone(),
            files,
        });
    }

    let orphan_count = orphans.len() as i64;
    let stats = OrphanReportStats {
        total_commits: total,
        correlated_count: correlated,
        orphan_count,
        beads_only_commits: beads_only,
        candidate_count: 0,
        orphan_ratio: if total > 0 {
            orphan_count as f64 / total as f64
        } else {
            0.0
        },
        avg_suspicion_score: 0.0,
    };
    let window = OrphanWindow {
        revision: opts.revision.clone(),
        commits: walk.len() as i64,
        limit: opts.limit,
        since: opts.since.clone(),
        until: opts.until.clone(),
        source: "history_index".to_string(),
    };
    Ok((orphans, window, stats))
}

/// Go `OrphanDetector.DetectOrphans` — score every orphan the correlation index
/// failed to link and rank the survivors.
pub fn detect_orphans(
    repo: &Path,
    report: &HistoryReport,
    now: Timestamp,
    generated_at: String,
) -> Result<OrphanReport, String> {
    let detector = OrphanDetector::new(repo, report, now);
    let (orphans, window, stats) = find_orphan_commits(repo, report)?;
    let git_range = format_git_range(&HistoryOptions {
        revision: window.revision.clone(),
        limit: window.limit,
        since: window.since.clone(),
        until: window.until.clone(),
        ..Default::default()
    });

    let mut candidates: Vec<OrphanCandidate> = Vec::with_capacity(orphans.len());
    let mut by_bead: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut total_suspicion = 0i64;
    let mut candidate_count = 0i64;

    for orphan in &orphans {
        let candidate = detector
            .analyze_orphan(orphan)
            .map_err(|e| format!("analyzing orphan commit {}: {e}", orphan.sha))?;
        if candidate.suspicion_score <= 0 {
            continue;
        }
        total_suspicion += candidate.suspicion_score as i64;
        candidate_count += 1;
        for bead in &candidate.probable_beads {
            // Short SHAs, the canonical form `filter_by_min_score` rebuilds
            // the index with — detect and filter must emit the same payload.
            by_bead
                .entry(bead.bead_id.clone())
                .or_default()
                .push(candidate.short_sha.clone());
        }
        candidates.push(candidate);
    }

    // Score desc, SHA asc.
    candidates.sort_by(|a, b| {
        b.suspicion_score
            .cmp(&a.suspicion_score)
            .then_with(|| a.sha.cmp(&b.sha))
    });

    let mut final_stats = stats;
    final_stats.candidate_count = candidate_count;
    if !candidates.is_empty() {
        final_stats.avg_suspicion_score = total_suspicion as f64 / candidates.len() as f64;
    }

    Ok(OrphanReport {
        generated_at,
        data_hash: report.data_hash.clone(),
        git_range,
        window,
        stats: final_stats,
        candidates,
        by_bead,
        usage_hints: ORPHAN_USAGE_HINTS.iter().map(|s| s.to_string()).collect(),
    })
}

/// Go `filterOrphanReportByMinScore` — drop weak candidates, then re-derive the
/// bead index and the two stats that depend on what survived. The index is
/// rebuilt from the *short* SHAs, which is what the payload reports.
pub fn filter_by_min_score(report: &mut OrphanReport, min_score: i32) {
    let mut filtered: Vec<OrphanCandidate> = Vec::with_capacity(report.candidates.len());
    let mut by_bead: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut total_suspicion = 0i64;

    for candidate in report.candidates.drain(..) {
        if candidate.suspicion_score < min_score {
            continue;
        }
        total_suspicion += candidate.suspicion_score as i64;
        for bead in &candidate.probable_beads {
            by_bead
                .entry(bead.bead_id.clone())
                .or_default()
                .push(candidate.short_sha.clone());
        }
        filtered.push(candidate);
    }

    report.candidates = filtered;
    report.by_bead = by_bead;
    report.stats.candidate_count = report.candidates.len() as i64;
    report.stats.avg_suspicion_score = if report.candidates.is_empty() {
        0.0
    } else {
        total_suspicion as f64 / report.candidates.len() as f64
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extractor::BeadEvent;
    use crate::history::{
        build_commit_index, BeadHistory, BeadMilestones, FileChange, HistoryCommit, HistoryStats,
        HistoryWindow, StrategyRun,
    };
    use std::collections::BTreeMap;

    fn ts(s: &str) -> Timestamp {
        history::parse_ts(s).unwrap()
    }

    fn commit(sha: &str, bead: &str, timestamp: &str, path: &str) -> HistoryCommit {
        HistoryCommit {
            bead_id: bead.to_string(),
            sha: sha.to_string(),
            short_sha: history::short_sha(sha),
            message: "fix: something".to_string(),
            author: "Ada".to_string(),
            author_email: "ada@example.com".to_string(),
            timestamp: timestamp.to_string(),
            files: vec![FileChange {
                path: path.to_string(),
                action: "M".to_string(),
                insertions: 3,
                deletions: 1,
            }],
            method: "co_committed",
            methods: vec!["co_committed".to_string()],
            confidence: 0.9,
            reason: "co-committed".to_string(),
            confirmed: false,
        }
    }

    fn event(bead: &str, timestamp: &str) -> BeadEvent {
        BeadEvent {
            bead_id: bead.to_string(),
            event_type: crate::extractor::EventType::Claimed,
            commit_sha: format!("{bead}-claim"),
            commit_msg: String::new(),
            author: "Ada".to_string(),
            author_email: "ada@example.com".to_string(),
            timestamp: timestamp.to_string(),
            before: None,
            after: None,
            transition_observed: true,
        }
    }

    /// A synthetic report with one closed bead (`bv-alpha12`) whose window
    /// covers 2026-01-10..2026-01-20 and whose one linked commit touched
    /// `src/app.rs`.
    fn synthetic_report() -> HistoryReport {
        let bead_id = "bv-alpha12".to_string();
        let mut histories = BTreeMap::new();
        histories.insert(
            bead_id.clone(),
            BeadHistory {
                bead_id: bead_id.clone(),
                title: "Alpha work".to_string(),
                status: "closed".to_string(),
                events: vec![event(&bead_id, "2026-01-10T00:00:00Z")],
                milestones: BeadMilestones {
                    created: None,
                    claimed: Some(event(&bead_id, "2026-01-10T00:00:00Z")),
                    closed: Some(event(&bead_id, "2026-01-20T00:00:00Z")),
                    reopened: None,
                },
                commits: Some(vec![commit(
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    &bead_id,
                    "2026-01-11T00:00:00Z",
                    "src/app.rs",
                )]),
                cycle_time: None,
                last_author: "Ada".to_string(),
            },
        );
        let commit_index = build_commit_index(&histories);
        HistoryReport {
            generated_at: "2026-01-25T00:00:00Z".to_string(),
            data_hash: "deadbeefcafe".to_string(),
            git_range: "limit 500".to_string(),
            latest_commit_sha: String::new(),
            window: HistoryWindow {
                revision: String::new(),
                limit: 500,
                since: None,
                until: None,
                commits: 1,
            },
            stats: HistoryStats {
                total_beads: 1,
                beads_with_commits: 1,
                total_commits: 1,
                unique_authors: 1,
                avg_commits_per_bead: 1.0,
                avg_cycle_time_days: None,
                method_distribution: BTreeMap::new(),
                strategies: Some(vec![StrategyRun {
                    name: "co_committed",
                    ran: true,
                    duration_ms: 0.0,
                    candidates: 1,
                }]),
                feedback_applied: None,
            },
            histories,
            commit_index,
            causal_history: None,
        }
    }

    fn orphan_commit(
        sha: &str,
        message: &str,
        timestamp: &str,
        files: &[&str],
        author_email: &str,
    ) -> OrphanCommit {
        OrphanCommit {
            sha: sha.to_string(),
            short_sha: history::short_sha(sha),
            message: message.to_string(),
            author: "Grace".to_string(),
            author_email: author_email.to_string(),
            timestamp: timestamp.to_string(),
            files: files.iter().map(|f| f.to_string()).collect(),
        }
    }

    #[test]
    fn fix_keyword_scores_ten() {
        let (score, details) = message_suspicion("fix the login bug");
        assert_eq!(score, 10);
        assert_eq!(details, vec!["fix"]);
    }

    #[test]
    fn bv_pattern_scores_twenty_five() {
        let (score, _) = message_suspicion("update bv-abc123 handling");
        // "bv-abc123" matches bv-pattern(25); also lowercase JIRA-style? no —
        // bv- has digits+letters; [a-z]{2,5}-\d+ needs dash-digits. Only bv hit.
        assert!(score >= 25);
    }

    #[test]
    fn multiple_patterns_accumulate_capped_at_35() {
        let (score, _) = message_suspicion("fix and close #123");
        // fix=10 + close=10 + #123=15 = 35 → capped at 35
        assert_eq!(score, 35);
    }

    #[test]
    fn plain_message_scores_zero() {
        let (score, details) = message_suspicion("refactor internal helpers");
        assert_eq!(score, 0);
        assert!(details.is_empty());
    }

    #[test]
    fn extracts_bv_ids_case_insensitive() {
        let ids = extract_bv_ids("resolves BV-ab12cd34 quickly");
        assert_eq!(ids, vec!["bv-ab12cd34"]);
    }

    /// A one-bead report whose ID is in a custom tracker's format, so the
    /// `--id-pattern` path is the only thing that can name it.
    fn custom_id_report() -> HistoryReport {
        let bead_id = "zzq-a1b2c".to_string();
        let mut histories = BTreeMap::new();
        histories.insert(
            bead_id.clone(),
            BeadHistory {
                bead_id: bead_id.clone(),
                title: "Flush ordering".to_string(),
                status: "open".to_string(),
                events: vec![event(&bead_id, "2026-01-10T00:00:00Z")],
                milestones: BeadMilestones {
                    created: None,
                    claimed: Some(event(&bead_id, "2026-01-10T00:00:00Z")),
                    closed: None,
                    reopened: None,
                },
                commits: Some(vec![]),
                cycle_time: None,
                last_author: "Ada".to_string(),
            },
        );
        let commit_index = build_commit_index(&histories);
        HistoryReport {
            generated_at: "2026-01-25T00:00:00Z".to_string(),
            data_hash: "deadbeefcafe".to_string(),
            git_range: "limit 500".to_string(),
            latest_commit_sha: String::new(),
            window: HistoryWindow {
                revision: String::new(),
                limit: 500,
                since: None,
                until: None,
                commits: 0,
            },
            stats: HistoryStats {
                total_beads: 1,
                beads_with_commits: 0,
                total_commits: 0,
                unique_authors: 0,
                avg_commits_per_bead: 0.0,
                avg_cycle_time_days: None,
                method_distribution: BTreeMap::new(),
                strategies: None,
                feedback_applied: None,
            },
            histories,
            commit_index,
            causal_history: None,
        }
    }

    fn message_only_candidate(message: &str) -> OrphanCandidate {
        OrphanCandidate {
            sha: "abc123def456".to_string(),
            short_sha: "abc123d".to_string(),
            message: message.to_string(),
            author: "Ada".to_string(),
            author_email: "ada@example.com".to_string(),
            timestamp: "2026-01-12T00:00:00Z".to_string(),
            files: vec!["src/app.rs".to_string()],
            suspicion_score: 0,
            probable_beads: Vec::new(),
            signals: Vec::new(),
        }
    }

    /// Go `TestOrphanDetector_CustomIDPatternMatchesProbableBead` (#188): a
    /// registered `--id-pattern` adds its 25 to the message weight *and* credits
    /// the bead it names, so a tracker whose IDs carry no numeric suffix still
    /// links. No built-in pattern here can see `zzq-a1b2c` — only the
    /// registration can.
    #[test]
    fn custom_id_pattern_scores_message_and_names_the_bead() {
        let _serialized = crate::explicit::custom_patterns_lock();
        crate::explicit::set_custom_id_patterns(vec![Regex::new(r"\bzzq-[a-z0-9]{5}\b").unwrap()]);

        let report = custom_id_report();
        let now = ts("2026-01-25T00:00:00Z");
        let detector = OrphanDetector::new(Path::new("."), &report, now);
        let mut candidate = message_only_candidate("fix flush ordering for zzq-a1b2c");
        let mut bead_scores: BTreeMap<String, ProbableBeadBuilder> = BTreeMap::new();
        detector.check_message(&mut candidate, &mut bead_scores);

        // "fix" (10) + the custom hit (25) = 35, at the cap.
        assert_eq!(candidate.signals.len(), 1);
        assert_eq!(candidate.signals[0].signal, SIGNAL_MESSAGE);
        assert_eq!(candidate.signals[0].weight, 35);
        assert_eq!(
            candidate.signals[0].details,
            "Message patterns: fix, zzq-a1b2c"
        );

        let builder = bead_scores
            .get("zzq-a1b2c")
            .expect("custom-pattern bead ID is scored");
        assert_eq!(builder.score, MENTIONED_BEAD_SCORE);
    }

    /// With no registration the same message scores only its built-in "fix"
    /// signal and names no bead — Go's behavior when `--id-pattern` is absent.
    #[test]
    fn no_custom_id_pattern_leaves_custom_formats_unseen() {
        let _serialized = crate::explicit::custom_patterns_lock();
        crate::explicit::set_custom_id_patterns(Vec::new());

        let report = custom_id_report();
        let now = ts("2026-01-25T00:00:00Z");
        let detector = OrphanDetector::new(Path::new("."), &report, now);
        let mut candidate = message_only_candidate("fix flush ordering for zzq-a1b2c");
        let mut bead_scores: BTreeMap<String, ProbableBeadBuilder> = BTreeMap::new();
        detector.check_message(&mut candidate, &mut bead_scores);

        assert_eq!(candidate.signals.len(), 1);
        assert_eq!(candidate.signals[0].weight, 10);
        assert!(bead_scores.is_empty());
    }

    /// A pattern with a capture group yields group 1, per Go's `checkMessage`
    /// custom pass (orphan.go:479-489). Go hands the raw group to
    /// `scoreMentionedBead` without normalizing — unlike the explicit matcher,
    /// which lowercases through `normalizeBeadID` — and the bead lookup is
    /// case-insensitive either way.
    #[test]
    fn custom_id_pattern_capture_group_one_is_the_id() {
        let _serialized = crate::explicit::custom_patterns_lock();
        crate::explicit::set_custom_id_patterns(vec![
            Regex::new(r"(?i)ticket\s+(zzt-[a-z]{3})\b").unwrap()
        ]);

        assert_eq!(
            custom_id_matches("board polish, ticket ZZT-PBB done"),
            vec!["ZZT-PBB"]
        );
    }

    #[test]
    fn walks_the_same_window_the_correlation_index_covered() {
        let report = synthetic_report();
        let window = &report.window;
        let opts = HistoryOptions {
            revision: window.revision.clone(),
            limit: window.limit,
            since: window.since.clone(),
            until: window.until.clone(),
            ..Default::default()
        };
        assert_eq!(window.limit, 500);
        assert_eq!(format_git_range(&opts), "limit 500");
    }

    /// The whole classification pipeline, exercised without a git repository:
    /// build the detector from a synthetic report and score hand-built orphan
    /// commits the way `find_orphan_commits` would hand them over. Every orphan
    /// below carries a non-empty file list, so the per-commit git fallback the
    /// detector keeps for fileless commits is never reached.
    fn score_through_detector(
        orphans: &[OrphanCommit],
        report: &HistoryReport,
        now: Timestamp,
    ) -> Vec<OrphanCandidate> {
        // `checkMessage` reads the process-global `--id-pattern` registry, so
        // serialize against any sibling test that registers patterns.
        let _serialized = crate::explicit::custom_patterns_lock();
        let detector = OrphanDetector::new(Path::new("."), report, now);
        let mut scored: Vec<OrphanCandidate> = orphans
            .iter()
            .map(|o| detector.analyze_orphan(o).expect("scoring orphan"))
            .filter(|c| c.suspicion_score > 0)
            .collect();
        scored.sort_by(|a, b| {
            b.suspicion_score
                .cmp(&a.suspicion_score)
                .then_with(|| a.sha.cmp(&b.sha))
        });
        scored
    }

    #[test]
    fn timing_and_file_and_author_signals_accumulate_and_cap_at_100() {
        let report = synthetic_report();
        let now = ts("2026-01-25T00:00:00Z");
        // Commit lands inside bv-alpha12's window (timing 30), touches the same
        // file the bead touched (files 25), and shares the author (author 15).
        let orphan = orphan_commit(
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "chore: tidy up",
            "2026-01-12T00:00:00Z",
            &["src/app.rs"],
            "ada@example.com",
        );
        let scored = score_through_detector(&[orphan], &report, now);
        assert_eq!(scored.len(), 1);
        let c = &scored[0];
        assert_eq!(c.suspicion_score, 30 + 25 + 15);
        assert_eq!(c.probable_beads.len(), 1);
        let bead = &c.probable_beads[0];
        assert_eq!(bead.bead_id, "bv-alpha12");
        assert_eq!(bead.bead_title, "Alpha work");
        // The timing heuristic runs first and seeds the status, so a bead the
        // commit merely overlaps in time reports as in_progress, not closed.
        assert_eq!(bead.bead_status, "in_progress");
        assert_eq!(bead.confidence, 70);
        // Reasons are sorted, and the timing heuristic seeds the status.
        assert_eq!(
            bead.reasons,
            vec![
                "commit during active timeframe",
                "same author worked on bead nearby",
                "touches file src/app.rs",
            ]
        );
        // Signals are sorted by signal name, then details.
        let signals: Vec<&str> = c.signals.iter().map(|s| s.signal).collect();
        let mut sorted = signals.clone();
        sorted.sort();
        assert_eq!(signals, sorted);
    }

    #[test]
    fn a_commit_outside_every_window_scores_zero_and_is_dropped() {
        let report = synthetic_report();
        let now = ts("2026-01-25T00:00:00Z");
        // Well after the bead closed (past the author's ±7-day slack too), no
        // shared file, and a message with no bead-like pattern: nothing fires.
        let orphan = orphan_commit(
            "cccccccccccccccccccccccccccccccccccccccc",
            "chore: tidy up",
            "2026-06-01T00:00:00Z",
            &["src/other.rs"],
            "ada@example.com",
        );
        let scored = score_through_detector(&[orphan], &report, now);
        assert!(scored.is_empty());
    }

    #[test]
    fn naming_a_bead_id_raises_confidence_not_the_suspicion_score() {
        let report = synthetic_report();
        let now = ts("2026-01-25T00:00:00Z");
        // Outside the window, no shared file. The subject trips the `bv-…`
        // message pattern (25, a signal) and names the bead (35, credited to
        // probable_beads.confidence only — Go's scoreMentionedBead adds no
        // signal of its own).
        let orphan = orphan_commit(
            "dddddddddddddddddddddddddddddddddddddddd",
            "update bv-alpha12 notes",
            "2026-06-01T00:00:00Z",
            &["docs/readme.md"],
            "ada@example.com",
        );
        let scored = score_through_detector(&[orphan], &report, now);
        assert_eq!(scored.len(), 1);
        let c = &scored[0];
        assert_eq!(c.suspicion_score, 25);
        assert_eq!(c.probable_beads[0].bead_id, "bv-alpha12");
        // Status comes from the history, since no earlier heuristic seeded it.
        assert_eq!(c.probable_beads[0].bead_status, "closed");
        assert_eq!(c.probable_beads[0].confidence, 35);
        assert_eq!(
            c.probable_beads[0].reasons,
            vec!["bead ID mentioned in commit message"]
        );
        assert_eq!(
            c.signals
                .iter()
                .filter(|s| s.signal == SIGNAL_MESSAGE)
                .map(|s| s.weight)
                .sum::<i32>(),
            25
        );
    }

    #[test]
    fn suspicion_score_is_capped_at_one_hundred() {
        let report = synthetic_report();
        let now = ts("2026-01-25T00:00:00Z");
        // A commit touching the same file repeatedly over-counts file evidence
        // (2 files x 25) on top of timing + author; the sum must clamp to 100.
        let orphan = orphan_commit(
            "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
            "fix: repair src/app.rs",
            "2026-01-12T00:00:00Z",
            &["src/app.rs", "src/app.rs"],
            "ada@example.com",
        );
        let scored = score_through_detector(&[orphan], &report, now);
        let raw: i32 = scored[0].signals.iter().map(|s| s.weight).sum();
        assert!(raw > 100, "expected uncapped sum above 100, got {raw}");
        assert_eq!(scored[0].suspicion_score, 100);
    }

    #[test]
    fn min_score_filter_rebuilds_by_bead_with_short_shas() {
        let mut report = OrphanReport {
            generated_at: "2026-01-25T00:00:00Z".to_string(),
            data_hash: "deadbeefcafe".to_string(),
            git_range: "limit 500".to_string(),
            window: OrphanWindow {
                revision: String::new(),
                commits: 10,
                limit: 500,
                since: None,
                until: None,
                source: "history_index".to_string(),
            },
            stats: OrphanReportStats {
                total_commits: 8,
                correlated_count: 1,
                orphan_count: 7,
                beads_only_commits: 2,
                candidate_count: 7,
                orphan_ratio: 7.0 / 8.0,
                avg_suspicion_score: 0.0,
            },
            candidates: vec![
                OrphanCandidate {
                    sha: "1111111111111111111111111111111111111111".to_string(),
                    short_sha: "1111111".to_string(),
                    message: "one".to_string(),
                    author: "A".to_string(),
                    author_email: "a@example.com".to_string(),
                    timestamp: "2026-01-01T00:00:00Z".to_string(),
                    files: vec![],
                    suspicion_score: 90,
                    probable_beads: vec![ProbableBead {
                        bead_id: "bv-alpha12".to_string(),
                        bead_title: "Alpha".to_string(),
                        bead_status: "closed".to_string(),
                        confidence: 90,
                        reasons: vec!["x".to_string()],
                    }],
                    signals: vec![],
                },
                OrphanCandidate {
                    sha: "2222222222222222222222222222222222222222".to_string(),
                    short_sha: "2222222".to_string(),
                    message: "two".to_string(),
                    author: "B".to_string(),
                    author_email: "b@example.com".to_string(),
                    timestamp: "2026-01-01T00:00:00Z".to_string(),
                    files: vec![],
                    suspicion_score: 10,
                    probable_beads: vec![ProbableBead {
                        bead_id: "bv-beta34".to_string(),
                        bead_title: "Beta".to_string(),
                        bead_status: "open".to_string(),
                        confidence: 10,
                        reasons: vec!["y".to_string()],
                    }],
                    signals: vec![],
                },
            ],
            by_bead: BTreeMap::new(),
            usage_hints: vec![],
        };

        filter_by_min_score(&mut report, 30);

        assert_eq!(report.candidates.len(), 1);
        assert_eq!(report.candidates[0].short_sha, "1111111");
        assert_eq!(report.stats.candidate_count, 1);
        assert_eq!(report.stats.avg_suspicion_score, 90.0);
        // Only the surviving candidate's bead survives, keyed by short SHA.
        assert_eq!(report.by_bead.len(), 1);
        assert_eq!(report.by_bead["bv-alpha12"], vec!["1111111".to_string()]);
    }

    #[test]
    fn beads_only_commits_are_classified_as_bookkeeping() {
        // A commit whose every path lives under .beads/ is a `br sync`, not an
        // orphan; one with no paths at all is not beads-only.
        let tracker_only = history::WalkedCommit {
            sha: "f".repeat(40),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            author: "A".to_string(),
            author_email: "a@example.com".to_string(),
            subject: "chore: sync beads".to_string(),
            body: String::new(),
            files: vec![".beads/issues.jsonl".to_string()],
        };
        assert!(tracker_only.beads_only());
        assert!(tracker_only.touches_beads_dir());

        let empty = history::WalkedCommit {
            files: Vec::new(),
            ..tracker_only.clone()
        };
        assert!(!empty.beads_only());
        assert!(!empty.touches_beads_dir());

        let code = history::WalkedCommit {
            files: vec![".beads/issues.jsonl".to_string(), "src/main.rs".to_string()],
            ..tracker_only
        };
        assert!(!code.beads_only());
        assert!(code.touches_beads_dir());
    }

    #[test]
    fn excluded_paths_are_dropped_from_a_candidates_files() {
        // .beads/, node_modules/ and the rest never reach the file heuristic.
        assert!(history::is_excluded_path(".beads/issues.jsonl"));
        assert!(history::is_excluded_path("web/node_modules/x/y.js"));
        assert!(!history::is_excluded_path("crates/bv/src/main.rs"));
    }
}
