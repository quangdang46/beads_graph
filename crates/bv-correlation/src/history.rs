//! History report — port of Go `pkg/correlation`'s `Correlator` pipeline as
//! `pkg/correlation/correlator.go` + `extractor.go` + `cocommit.go` +
//! `explicit.go` + `temporal.go` + `scorer.go` assemble it for `--robot-history`.
//!
//! Go splits generation in two: `extractHistoryArtifact` runs the three
//! correlation strategies over committed history alone (the expensive half),
//! then `assembleReport` folds in the *current* bead set (titles, statuses,
//! stats, index). This module keeps that split: [`HistoryArtifact`] is the
//! HEAD-only half, [`assemble_report`] the working-tree half, and
//! [`build_history_report`] runs both.
//!
//! Strategy precedence for a (commit, bead) pair is co_committed, then
//! explicit_id, then temporal_author; a SHA matched by several strategies
//! keeps every method in `methods` with the highest-confidence method as
//! `method` and the scorer's combined confidence.

use crate::extractor::{extract, BeadEvent, EventType, ExtractOptions};
use crate::scorer::{combine_confidence, Method};
use regex::Regex;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::process::Command;
use std::time::Instant;

/// Go `FileChange`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileChange {
    pub path: String,
    /// A=added, M=modified, D=deleted, R=renamed.
    pub action: String,
    pub insertions: i64,
    pub deletions: i64,
}

/// Go `CorrelatedCommit` in its public report shape. `bead_id` is Go's
/// `json:"-"` internal linking state: kept here, never serialized.
#[derive(Debug, Clone, Serialize)]
pub struct HistoryCommit {
    #[serde(skip)]
    pub bead_id: String,
    pub sha: String,
    pub short_sha: String,
    pub message: String,
    pub author: String,
    pub author_email: String,
    pub timestamp: String,
    pub files: Vec<FileChange>,
    /// Primary (highest-confidence) method.
    pub method: &'static str,
    /// Every method that matched this (commit, bead) pair.
    pub methods: Vec<String>,
    pub confidence: f64,
    pub reason: String,
    #[serde(skip_serializing_if = "is_false")]
    pub confirmed: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl HistoryCommit {
    /// Go `CorrelatedCommit.AllMethods` — every method that matched this
    /// (commit, bead) pair, falling back to the primary `method` when the
    /// multi-method list is empty.
    pub fn all_methods(&self) -> Vec<&str> {
        if !self.methods.is_empty() {
            return self.methods.iter().map(String::as_str).collect();
        }
        if self.method.is_empty() {
            return Vec::new();
        }
        vec![self.method]
    }
}

/// Go `BeadMilestones` — pointers into the bead's own event slice.
#[derive(Debug, Clone, Default, Serialize)]
pub struct BeadMilestones {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<BeadEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claimed: Option<BeadEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed: Option<BeadEvent>,
    /// Most recent if multiple.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reopened: Option<BeadEvent>,
}

/// Go `CycleTime`. Go marshals `time.Duration` as an integer nanosecond count.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CycleTime {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claim_to_close: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub create_to_close: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub create_to_claim: Option<i64>,
}

/// Go `BeadHistory`.
#[derive(Debug, Clone, Serialize)]
pub struct BeadHistory {
    pub bead_id: String,
    pub title: String,
    pub status: String,
    /// Always an array: `buildHistories` seeds it, Go marshals `[]` not null.
    pub events: Vec<BeadEvent>,
    pub milestones: BeadMilestones,
    /// Null (not `[]`) when the bead has no commits — Go's nil slice.
    pub commits: Option<Vec<HistoryCommit>>,
    /// Null when the bead was never closed.
    pub cycle_time: Option<CycleTime>,
    pub last_author: String,
}

/// Go `HistoryWindow` — the commit window the index covers.
#[derive(Debug, Clone, Serialize)]
pub struct HistoryWindow {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub revision: String,
    pub limit: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    pub commits: i64,
}

/// Go `StrategyRun` — which correlation strategies ran and what they cost.
#[derive(Debug, Clone, Serialize)]
pub struct StrategyRun {
    pub name: &'static str,
    pub ran: bool,
    pub duration_ms: f64,
    /// Raw (commit, bead) pairs produced before assembly filtered unknown beads.
    pub candidates: i64,
}

/// Go `FeedbackApplied`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct FeedbackApplied {
    pub confirmed: i64,
    pub rejected: i64,
    pub ignored: i64,
}

/// Go `HistoryStats`.
#[derive(Debug, Clone, Serialize)]
pub struct HistoryStats {
    pub total_beads: i64,
    pub beads_with_commits: i64,
    pub total_commits: i64,
    pub unique_authors: i64,
    pub avg_commits_per_bead: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_cycle_time_days: Option<f64>,
    /// Count per correlation method (a multi-method commit counts once per method).
    pub method_distribution: BTreeMap<String, i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategies: Option<Vec<StrategyRun>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feedback_applied: Option<FeedbackApplied>,
}

/// Go `CommitIndex` — SHA -> sorted bead IDs.
pub type CommitIndex = BTreeMap<String, Vec<String>>;

/// Go `HistoryReport` (`causal_history` is present only for an explicitly
/// requested `--robot-causality` target, so it is omitted from every other
/// report).
#[derive(Debug, Clone, Serialize)]
pub struct HistoryReport {
    pub generated_at: String,
    pub data_hash: String,
    pub git_range: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub latest_commit_sha: String,
    pub window: HistoryWindow,
    pub stats: HistoryStats,
    pub histories: BTreeMap<String, BeadHistory>,
    pub commit_index: CommitIndex,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub causal_history: Option<crate::causality::CausalHistory>,
}

/// The minimal bead fields the report embeds (Go `BeadInfo`).
#[derive(Debug, Clone)]
pub struct BeadInfo {
    pub id: String,
    pub title: String,
    pub status: String,
}

/// Go `CorrelatorOptions` (the fields `--robot-history` can set).
#[derive(Debug, Clone, Default)]
pub struct HistoryOptions {
    /// Resolved commit to walk from (empty = HEAD).
    pub revision: String,
    /// Filter to a single bead ID (empty = all).
    pub bead_id: String,
    /// Only commits/events at or after this instant (RFC3339, as resolved by
    /// the caller from `--history-since`).
    pub since: Option<String>,
    pub until: Option<String>,
    /// Max commits to process (0 = no limit).
    pub limit: i64,
    /// Go `CorrelatorOptions.CausalityBeadID` — retain committed constraint
    /// snapshots for this one bead. Empty for every other report: the retained
    /// history is the expensive half of `--robot-causality` and nothing else
    /// consumes it.
    pub causality_bead_id: String,
}

// ---------------------------------------------------------------------------
// Bounded commit walk (Go commit_walk.go)
// ---------------------------------------------------------------------------

/// `git log --format` used by the walk: 0x1e record separator, NUL-separated
/// header fields, full body, 0x1f terminator, then `--name-only` paths.
const COMMIT_WALK_FORMAT: &str = "%x1e%H%x00%aI%x00%an%x00%ae%x00%s%x00%b%x1f";

/// Go `walkedCommit` — metadata plus every changed path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkedCommit {
    pub sha: String,
    pub timestamp: String,
    pub author: String,
    pub author_email: String,
    pub subject: String,
    pub body: String,
    pub files: Vec<String>,
}

impl WalkedCommit {
    /// Go `walkedCommit.message` — the text the explicit-ID matcher scans.
    pub fn message(&self) -> String {
        if self.body.is_empty() {
            self.subject.clone()
        } else {
            format!("{}\n{}", self.subject, self.body)
        }
    }

    /// Go `walkedCommit.touchesBeadsDir`.
    pub fn touches_beads_dir(&self) -> bool {
        self.files.iter().any(|f| f.starts_with(".beads/"))
    }

    /// Go `walkedCommit.beadsOnly` — every changed path lives under `.beads/`, so
    /// the commit is tracker bookkeeping rather than a code commit. A commit with
    /// no paths at all is *not* beads-only; it counts as a code commit whose
    /// only defect is having no files.
    pub fn beads_only(&self) -> bool {
        !self.files.is_empty() && self.files.iter().all(|f| f.starts_with(".beads/"))
    }
}

/// Go `appendHistoryFilters` — time/revision bounds and the commit cap.
fn append_history_filters(args: &mut Vec<String>, opts: &HistoryOptions) {
    if let Some(since) = &opts.since {
        args.push(format!("--since={since}"));
    }
    if let Some(until) = &opts.until {
        args.push(format!("--until={until}"));
    }
    if opts.limit > 0 {
        args.push(format!("-n{}", opts.limit));
    }
    if !opts.revision.is_empty() {
        args.push(opts.revision.clone());
    }
}

/// Go `walkCommits` — one metadata-only `git log --no-merges --name-only` over
/// the last `limit` commits, returned oldest-first. The single window the
/// explicit-ID and temporal strategies (and the orphan detector) agree on.
pub fn walk_commits(repo: &Path, opts: &HistoryOptions) -> Result<Vec<WalkedCommit>, String> {
    let mut args: Vec<String> = vec![
        "log".into(),
        "--no-merges".into(),
        "--name-only".into(),
        "--no-color".into(),
        format!("--format={COMMIT_WALK_FORMAT}"),
    ];
    append_history_filters(&mut args, opts);

    let out = Command::new("git")
        .args(&args)
        .current_dir(repo)
        .output()
        .map_err(|e| format!("spawning git log walk: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git log walk failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let mut commits = parse_commit_walk(&out.stdout)?;
    // git log emits newest first; strategies want chronological order.
    commits.reverse();
    Ok(commits)
}

/// Go `parseCommitWalk`.
fn parse_commit_walk(out: &[u8]) -> Result<Vec<WalkedCommit>, String> {
    let mut commits = Vec::new();
    for record in out.split(|b| *b == 0x1e) {
        if record.iter().all(|b| b.is_ascii_whitespace()) {
            continue;
        }
        let end = record.iter().position(|b| *b == 0x1f).ok_or_else(|| {
            format!(
                "parsing git log walk: record without terminator: {:.80?}",
                record
            )
        })?;
        let header = String::from_utf8_lossy(&record[..end]).to_string();
        // Go splits on NUL into at most 6 parts so a NUL inside the body of a
        // header (impossible for git metadata) cannot shift the fields.
        let mut parts = header.splitn(6, '\0');
        let mut next = || parts.next().unwrap_or_default().to_string();
        let sha = next();
        let timestamp = next();
        let author = next();
        let author_email = next();
        let subject = next();
        let body = next().trim_end_matches('\n').to_string();
        if parts.next().is_some() || sha.is_empty() {
            return Err(format!(
                "parsing git log walk: malformed header: {header:.80}"
            ));
        }
        let files = String::from_utf8_lossy(&record[end + 1..])
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        commits.push(WalkedCommit {
            sha,
            timestamp,
            author,
            author_email,
            subject,
            body,
            files,
        });
    }
    Ok(commits)
}

// ---------------------------------------------------------------------------
// Co-commit file extraction (Go cocommit.go)
// ---------------------------------------------------------------------------

/// Go `codeFileExtensions`.
const CODE_FILE_EXTENSIONS: &[&str] = &[
    "go", "py", "js", "ts", "jsx", "tsx", "rs", "java", "kt", "swift", "c", "cpp", "h", "hpp",
    "rb", "php", "cs", "scala", "yaml", "yml", "json", "toml", "md", "sql", "sh", "bash", "zsh",
];

/// Go `excludedPaths`.
const EXCLUDED_PATHS: &[&str] = &[
    ".beads/",
    ".bv/",
    ".git/",
    "node_modules/",
    "vendor/",
    "__pycache__/",
    ".venv/",
    "venv/",
    "dist/",
    "build/",
    ".next/",
];

/// Go `excludePathspecArgs` — exclude the noisy directories inside git itself
/// so the line-stat pass never diffs the multi-MB beads blob.
pub(crate) fn exclude_pathspec_args() -> Vec<String> {
    let mut args = vec!["--".to_string(), ".".to_string()];
    for prefix in EXCLUDED_PATHS {
        args.push(format!(
            ":(exclude,glob){}/**",
            prefix.trim_end_matches('/')
        ));
    }
    args
}

fn batch_log_args(diff_flag: &str, shas: &[String]) -> Vec<String> {
    let mut args = vec![
        "log".to_string(),
        "--no-walk=unsorted".to_string(),
        diff_flag.to_string(),
        "--no-color".to_string(),
        format!("--format={}", GIT_LOG_HEADER_FORMAT),
    ];
    args.extend(shas.iter().cloned());
    args.extend(exclude_pathspec_args());
    args
}

/// The header format shared by the batched co-commit logs (Go gitlog.go).
const GIT_LOG_HEADER_FORMAT: &str = "%H%x00%aI%x00%an%x00%ae%x00%s";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct LineStats {
    insertions: i64,
    deletions: i64,
}

/// Go `CoCommitExtractor.primeBatch` + `getFilesChanged`/`getLineStats`: two
/// batched `git log --no-walk=unsorted` passes (name-status and numstat are
/// mutually exclusive in one invocation) so N commits cost two processes
/// instead of 2N.
#[derive(Debug, Default)]
struct CoCommitCache {
    files: HashMap<String, Vec<FileChange>>,
    stats: HashMap<String, HashMap<String, LineStats>>,
}

impl CoCommitCache {
    fn prime(&mut self, repo: &Path, shas: &[String]) -> Result<(), String> {
        let want: Vec<String> = {
            let mut seen = BTreeSet::new();
            shas.iter()
                .filter(|s| !s.is_empty() && !self.files.contains_key(*s))
                .filter(|s| seen.insert((*s).clone()))
                .cloned()
                .collect()
        };
        if want.is_empty() {
            return Ok(());
        }
        let files_out = run_git_batch(repo, "--name-status", &want)?;
        let stats_out = run_git_batch(repo, "--numstat", &want)?;
        for (sha, payload) in files_out {
            self.files.insert(sha, parse_name_status(&payload));
        }
        for (sha, payload) in stats_out {
            self.stats.insert(sha, parse_numstat(&payload));
        }
        // A SHA whose diff is empty under the exclude pathspecs is absent from
        // both streams; record the empty result so it is not re-requested.
        for sha in want {
            self.files.entry(sha.clone()).or_default();
            self.stats.entry(sha).or_default();
        }
        Ok(())
    }

    /// Go `ExtractCoCommittedFiles` — code files only, with line stats.
    fn code_files(&self, sha: &str) -> Vec<FileChange> {
        let empty = Vec::new();
        let files = self.files.get(sha).unwrap_or(&empty);
        let stats = self.stats.get(sha);
        let mut out = Vec::with_capacity(files.len());
        for f in files {
            if !is_code_file(&f.path) || is_excluded_path(&f.path) {
                continue;
            }
            let mut f = f.clone();
            if let Some(s) = stats.and_then(|m| m.get(&f.path)) {
                f.insertions = s.insertions;
                f.deletions = s.deletions;
            }
            out.push(f);
        }
        out
    }
}

fn run_git_batch(
    repo: &Path,
    diff_flag: &str,
    shas: &[String],
) -> Result<Vec<(String, String)>, String> {
    let args = batch_log_args(diff_flag, shas);
    let out = Command::new("git")
        .args(&args)
        .current_dir(repo)
        .output()
        .map_err(|e| format!("spawning git log batch: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git log batch failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(for_each_commit_chunk(&out.stdout))
}

/// Go `forEachCommitChunk` — split a `git log --format=<header>` stream into
/// per-commit chunks, yielding (sha, diff payload after the header line).
///
/// A commit header starts at a hex run — 40 chars for SHA-1 repos, 64 for
/// SHA-256 repos — immediately followed by NUL. The run must be *exactly*
/// the SHA width: a longer hex run is message/author text, not a header.
fn for_each_commit_chunk(out: &[u8]) -> Vec<(String, String)> {
    let mut chunks = Vec::new();
    let mut starts: Vec<usize> = Vec::new();
    let mut i = 0usize;
    while i < out.len() {
        if !out[i].is_ascii_hexdigit() {
            i += 1;
            continue;
        }
        let mut run_end = i;
        while run_end < out.len() && out[run_end].is_ascii_hexdigit() {
            run_end += 1;
        }
        let run_len = run_end - i;
        if (run_len == 40 || run_len == 64) && out.get(run_end) == Some(&0) {
            starts.push(i);
        }
        i = run_end.max(i + 1);
    }
    for (idx, &start) in starts.iter().enumerate() {
        let end = starts.get(idx + 1).copied().unwrap_or(out.len());
        let chunk = &out[start..end];
        let Some(nl) = chunk.iter().position(|b| *b == b'\n') else {
            continue;
        };
        let Some(z) = chunk[..nl].iter().position(|b| *b == 0) else {
            continue;
        };
        let sha = String::from_utf8_lossy(&chunk[..z]).to_string();
        chunks.push((sha, String::from_utf8_lossy(&chunk[nl + 1..]).to_string()));
    }
    chunks
}

/// Go `parseNameStatus`.
pub(crate) fn parse_name_status(payload: &str) -> Vec<FileChange> {
    let mut files = Vec::new();
    for line in payload.lines() {
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 2 {
            continue;
        }
        let mut action = parts[0];
        let mut path = parts[1];
        // Renames: "R100\told\tnew" — keep the new name, collapse the score.
        if parts.len() == 3 && action.starts_with('R') {
            path = parts[2];
            action = "R";
        }
        if action.len() > 1 {
            action = &action[..1];
        }
        files.push(FileChange {
            path: path.to_string(),
            action: action.to_string(),
            insertions: 0,
            deletions: 0,
        });
    }
    files
}

/// Go `parseNumstat`.
fn parse_numstat(payload: &str) -> HashMap<String, LineStats> {
    let mut stats = HashMap::new();
    for line in payload.lines() {
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 3 {
            continue;
        }
        // Binary files show "-" instead of counts.
        let insertions = parts[0].parse::<i64>().unwrap_or(0);
        let deletions = parts[1].parse::<i64>().unwrap_or(0);
        let path = extract_new_path(parts[2]);
        stats.insert(
            path,
            LineStats {
                insertions,
                deletions,
            },
        );
    }
    stats
}

/// Go `extractNewPath` — resolve git's rename notation in numstat output.
fn extract_new_path(path: &str) -> String {
    if path.contains('{') {
        // "pkg/{old => new}/file.go"
        let fixed = replace_rename_braces(path);
        return fixed.replace("//", "/");
    }
    if let Some(idx) = path.find(" => ") {
        return path[idx + 4..].to_string();
    }
    path.to_string()
}

/// Go's `\{[^}]* => ([^}]*)\}` → `$1` applied with `ReplaceAllString`: every
/// `{old => new}` group in the path collapses, not just the first.
fn replace_rename_braces(path: &str) -> String {
    let mut result = path.to_string();
    while let Some(next) = replace_first_rename_brace(&result) {
        if next == result {
            break;
        }
        result = next;
    }
    result
}

/// Collapse the first `{old => new}` group, or return `None` when the path
/// has no (further) complete group.
fn replace_first_rename_brace(path: &str) -> Option<String> {
    let start = path.find('{')?;
    let arrow = path[start..].find(" => ")?;
    let after = start + arrow + 4;
    let close_rel = path[after..].find('}')?;
    let close = after + close_rel;
    Some(format!(
        "{}{}{}",
        &path[..start],
        &path[after..close],
        &path[close + 1..]
    ))
}

/// Go `isCodeFile`.
fn is_code_file(path: &str) -> bool {
    let path = path
        .strip_prefix('"')
        .and_then(|p| p.strip_suffix('"'))
        .unwrap_or(path);
    let lower = path.to_lowercase();
    let Some(dot) = lower.rfind('.') else {
        return false;
    };
    CODE_FILE_EXTENSIONS.contains(&&lower[dot + 1..])
}

/// Go `isExcludedPath` — direct prefix, or a nested occurrence at a
/// directory boundary.
pub(crate) fn is_excluded_path(path: &str) -> bool {
    if EXCLUDED_PATHS.iter().any(|p| path.starts_with(p)) {
        return true;
    }
    EXCLUDED_PATHS
        .iter()
        .filter(|p| p.ends_with('/'))
        .any(|p| path.contains(&format!("/{p}")))
}

pub(crate) fn short_sha(sha: &str) -> String {
    if sha.len() > 7 {
        sha[..7].to_string()
    } else {
        sha.to_string()
    }
}

fn contains_bead_id(text: &str, bead_id: &str) -> bool {
    !bead_id.is_empty() && text.to_lowercase().contains(&bead_id.to_lowercase())
}

/// Go `allTestFiles`.
fn all_test_files(files: &[FileChange]) -> bool {
    if files.is_empty() {
        return false;
    }
    const TEST_PATTERNS: &[&str] = &[
        "_test.go", ".test.js", ".test.ts", ".spec.js", ".spec.ts", "_test.py", "test_",
    ];
    files.iter().all(|f| {
        let lower = f.path.to_lowercase();
        TEST_PATTERNS.iter().any(|p| lower.contains(p))
    })
}

/// Go `CoCommitExtractor.calculateConfidence`.
fn co_commit_confidence(event: &BeadEvent, files: &[FileChange]) -> f64 {
    let mut confidence = 0.95f64;
    if contains_bead_id(&event.commit_msg, &event.bead_id) {
        confidence += 0.04;
    }
    if files.len() > 20 {
        confidence -= 0.10;
    }
    if all_test_files(files) {
        confidence -= 0.05;
    }
    confidence.clamp(0.0, 1.0)
}

/// Go `CoCommitExtractor.generateReason`.
fn co_commit_reason(event: &BeadEvent, files: &[FileChange]) -> String {
    let mut parts = vec![format!(
        "Co-committed with bead status change to {}",
        event.event_type.as_str()
    )];
    if contains_bead_id(&event.commit_msg, &event.bead_id) {
        parts.push("commit message references bead ID".to_string());
    }
    if files.len() > 20 {
        parts.push(format!("large commit ({} files)", files.len()));
    }
    if all_test_files(files) {
        parts.push("contains only test files".to_string());
    }
    parts.join("; ")
}

/// Go `CoCommitExtractor.ExtractAllCoCommits` — claimed/closed events only,
/// and only when the same commit changed at least one code file.
fn extract_all_co_commits(
    repo: &Path,
    events: &[BeadEvent],
    cache: &mut CoCommitCache,
) -> Result<Vec<HistoryCommit>, String> {
    let shas: Vec<String> = events
        .iter()
        .filter(|e| matches!(e.event_type, EventType::Claimed | EventType::Closed))
        .map(|e| e.commit_sha.clone())
        .collect();
    cache.prime(repo, &shas)?;

    let mut commits = Vec::new();
    for event in events {
        if !matches!(event.event_type, EventType::Claimed | EventType::Closed) {
            continue;
        }
        let files = cache.code_files(&event.commit_sha);
        if files.is_empty() {
            continue;
        }
        commits.push(HistoryCommit {
            bead_id: event.bead_id.clone(),
            sha: event.commit_sha.clone(),
            short_sha: short_sha(&event.commit_sha),
            message: event.commit_msg.clone(),
            author: event.author.clone(),
            author_email: event.author_email.clone(),
            timestamp: event.timestamp.clone(),
            confidence: co_commit_confidence(event, &files),
            reason: co_commit_reason(event, &files),
            files,
            method: Method::CoCommitted.as_str(),
            methods: vec![Method::CoCommitted.as_str().to_string()],
            confirmed: false,
        });
    }
    Ok(commits)
}

// ---------------------------------------------------------------------------
// Explicit-ID strategy (Go explicit.go)
// ---------------------------------------------------------------------------

/// Go `builtinPatterns` — evaluated in this exact order, which decides both
/// which match wins for a repeated ID and the `totalMatches` penalty.
struct IdRegexes {
    patterns: Vec<(Regex, u8)>,
}

fn builtin_patterns() -> Vec<(Regex, u8)> {
    // The tag is the pattern's index, used only to keep the order readable.
    let raw: Vec<Regex> = vec![
        Regex::new(r"\[([A-Za-z]+-\d+)\]").unwrap(),
        Regex::new(r"(?i)closes?:?\s*#?([A-Za-z]+-\d+)").unwrap(),
        Regex::new(r"(?i)fix(?:es|ed)?:?\s*#?([A-Za-z]+-\d+)").unwrap(),
        Regex::new(r"(?i)refs?:?\s*#?([A-Za-z]+-\d+)").unwrap(),
        Regex::new(r"(?i)resolves?:?\s*#?([A-Za-z]+-\d+)").unwrap(),
        Regex::new(r"(?i)beads?[-_](\d+)\b").unwrap(),
        Regex::new(r"(?i)bv[-_](\d+)\b").unwrap(),
        Regex::new(r"\b([A-Z]{2,10}-\d+)\b").unwrap(),
    ];
    raw.into_iter()
        .enumerate()
        .map(|(i, r)| (r, i as u8))
        .collect()
}

impl IdRegexes {
    /// Go `NewExplicitMatcher` — `DefaultPatterns()`, i.e. the built-ins followed
    /// by whatever `--id-pattern` registered (#188). Built fresh per extraction
    /// so a registration made at CLI startup is always picked up.
    fn new() -> Self {
        let mut patterns = builtin_patterns();
        let next = patterns.len() as u8;
        patterns.extend(
            crate::explicit::custom_id_patterns()
                .into_iter()
                .enumerate()
                .map(|(i, r)| (r, next.wrapping_add(i as u8))),
        );
        Self { patterns }
    }

    /// Go `ExtractIDsFromMessage` — patterns in order, first match per
    /// normalized ID wins.
    fn extract(&self, message: &str) -> Vec<IdMatch> {
        let mut matches = Vec::new();
        let mut seen = BTreeSet::new();
        for (re, _) in &self.patterns {
            for caps in re.captures_iter(message) {
                let Some(m) = caps.get(0) else { continue };
                // Prefer capture group 1; fall back to the whole match.
                let raw = caps
                    .get(1)
                    .map(|g| g.as_str())
                    .filter(|g| !g.is_empty())
                    .unwrap_or_else(|| m.as_str());
                if raw.is_empty() {
                    continue;
                }
                let id = normalize_bead_id(raw);
                if seen.insert(id.clone()) {
                    matches.push(IdMatch {
                        id,
                        match_type: classify_match(m.as_str()),
                    });
                }
            }
        }
        matches
    }
}

/// Go `IDMatch` (only the fields the report path consumes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdMatch {
    pub id: String,
    pub match_type: &'static str,
}

/// Go `normalizeBeadID`.
pub(crate) fn normalize_bead_id(id: &str) -> String {
    if !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()) {
        return format!("bv-{id}");
    }
    id.to_lowercase()
}

/// Go `classifyMatch`.
pub(crate) fn classify_match(raw: &str) -> &'static str {
    let lower = raw.to_lowercase();
    if lower.contains("close") {
        "closes"
    } else if lower.contains("fix") {
        "fixes"
    } else if lower.contains("ref") {
        "refs"
    } else if lower.contains("resolve") {
        "resolves"
    } else if raw.starts_with('[') && raw.ends_with(']') {
        "bracket"
    } else if lower.starts_with("bead") || lower.starts_with("bv") {
        "bead"
    } else {
        "generic"
    }
}

/// Go `CalculateConfidence`.
fn explicit_confidence(match_type: &str, total_matches: usize) -> f64 {
    let mut base = 0.90f64;
    match match_type {
        "closes" | "fixes" | "resolves" => base += 0.05,
        "bracket" => base += 0.02,
        "refs" => base += 0.01,
        "bead" => base += 0.03,
        _ => {}
    }
    if total_matches > 1 {
        base -= 0.02 * (total_matches - 1) as f64;
    }
    base.clamp(0.70, 0.99)
}

/// Go `ExplicitMatcher.MatchWalkedCommits` + `CreateCorrelatedCommit`: one
/// candidate per (commit, id) in walk order, keeping only commits that changed
/// at least one code file (a bookkeeping-only commit naming a bead is not a
/// code change). Unknown IDs are *not* filtered here — the artifact is a pure
/// function of HEAD, and assembly drops pairs whose bead is gone.
fn extract_explicit_commits(
    repo: &Path,
    walk: &[WalkedCommit],
    bead_filter: &str,
    cache: &mut CoCommitCache,
) -> Result<Vec<HistoryCommit>, String> {
    let regexes = IdRegexes::new();
    let mut matches: Vec<(IdMatch, f64, &WalkedCommit)> = Vec::new();
    for wc in walk {
        let ids = regexes.extract(&wc.message());
        if ids.is_empty() {
            continue;
        }
        let total = ids.len();
        for id in ids {
            if !bead_filter.is_empty() && !id.id.eq_ignore_ascii_case(bead_filter) {
                continue;
            }
            let confidence = explicit_confidence(id.match_type, total);
            matches.push((id, confidence, wc));
        }
    }
    if matches.is_empty() {
        return Ok(Vec::new());
    }
    let shas: Vec<String> = matches.iter().map(|(_, _, wc)| wc.sha.clone()).collect();
    cache.prime(repo, &shas)?;

    let mut commits = Vec::with_capacity(matches.len());
    for (id, confidence, wc) in matches {
        let files = cache.code_files(&wc.sha);
        if files.is_empty() {
            continue;
        }
        commits.push(HistoryCommit {
            bead_id: id.id.clone(),
            sha: wc.sha.clone(),
            short_sha: short_sha(&wc.sha),
            message: wc.subject.clone(),
            author: wc.author.clone(),
            author_email: wc.author_email.clone(),
            timestamp: wc.timestamp.clone(),
            files,
            method: Method::ExplicitId.as_str(),
            methods: vec![Method::ExplicitId.as_str().to_string()],
            confidence,
            reason: format!(
                "Commit message explicitly references {} ({})",
                id.id, id.match_type
            ),
            confirmed: false,
        });
    }
    Ok(commits)
}

// ---------------------------------------------------------------------------
// Temporal-author strategy (Go temporal.go)
// ---------------------------------------------------------------------------

/// Go `TemporalWindow`.
#[derive(Debug, Clone)]
struct TemporalWindow {
    bead_id: String,
    author: String,
    author_email: String,
    start: String,
    end: String,
    active_beads: usize,
}

/// Go `temporalCandidate` — the artifact-side (unscored) form.
#[derive(Debug, Clone)]
pub struct TemporalCandidate {
    bead_id: String,
    sha: String,
    message: String,
    author: String,
    author_email: String,
    timestamp: String,
    files: Vec<FileChange>,
    window_author: String,
    window_start: String,
    window_end: String,
    active_beads: usize,
}

pub(crate) fn parse_ts(s: &str) -> Option<jiff::Timestamp> {
    s.parse::<jiff::Timestamp>().ok()
}

/// Nanoseconds between two RFC3339 instants (Go `time.Duration` marshalling).
fn duration_ns(from: &str, to: &str) -> Option<i64> {
    let a = parse_ts(from)?;
    let b = parse_ts(to)?;
    i64::try_from(b.as_nanosecond() - a.as_nanosecond()).ok()
}

/// Go `temporalWindowsFromEvents` — one claimed→closed window per bead, with
/// `active_beads` = how many beads the same claimant had claimed and still open
/// at any point during the window (itself included). Never consults the wall
/// clock, so the artifact stays deterministic.
fn temporal_windows_from_events(events: &[BeadEvent], bead_filter: &str) -> Vec<TemporalWindow> {
    let mut by_bead: BTreeMap<&str, Vec<&BeadEvent>> = BTreeMap::new();
    for e in events {
        by_bead.entry(e.bead_id.as_str()).or_default().push(e);
    }
    let mut spans: HashMap<&str, (String, String, Option<String>)> = HashMap::new();
    let mut windows = Vec::new();
    for (id, evs) in &by_bead {
        let owned: Vec<BeadEvent> = evs.iter().map(|e| (*e).clone()).collect();
        let m = get_bead_milestones(&owned);
        let Some(claimed) = &m.claimed else { continue };
        let email = claimed.author_email.clone();
        let closed_ts = m.closed.as_ref().map(|c| c.timestamp.clone());
        spans.insert(
            id,
            (email.clone(), claimed.timestamp.clone(), closed_ts.clone()),
        );
        if !bead_filter.is_empty() && *id != bead_filter {
            continue;
        }
        let Some(closed) = &m.closed else { continue };
        windows.push(TemporalWindow {
            bead_id: (*id).to_string(),
            author: claimed.author.clone(),
            author_email: email,
            start: claimed.timestamp.clone(),
            end: closed.timestamp.clone(),
            active_beads: 0,
        });
    }
    // Concurrency pass: a span overlaps the window when it started before the
    // window ended and (if it closed) closed after the window started.
    let span_snapshot: Vec<(&str, &str, &str, Option<&str>)> = spans
        .iter()
        .map(|(id, (email, start, end))| (*id, email.as_str(), start.as_str(), end.as_deref()))
        .collect();
    for w in &mut windows {
        let mut concurrent = 0usize;
        for (_, email, start, end) in &span_snapshot {
            if *email != w.author_email {
                continue;
            }
            if !ts_before(start, &w.end) {
                continue;
            }
            if let Some(end) = end {
                if !ts_after(end, &w.start) {
                    continue;
                }
            }
            concurrent += 1;
        }
        w.active_beads = concurrent;
    }
    windows
}

fn ts_before(a: &str, b: &str) -> bool {
    match (parse_ts(a), parse_ts(b)) {
        (Some(a), Some(b)) => a < b,
        // Unparseable timestamps: Go's time.Time zero value compares as "not
        // after", so treat an unknown start as before the window end.
        _ => true,
    }
}

fn ts_after(a: &str, b: &str) -> bool {
    match (parse_ts(a), parse_ts(b)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// Go `extractPathHints` — file paths, package roots and component keywords
/// from a bead title.
fn extract_path_hints(title: &str) -> Vec<String> {
    static HINT: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = HINT.get_or_init(|| {
        Regex::new(concat!(
            r"(?i)\b(?:",
            r"(?:[a-z_][a-z0-9_]*(?:/[a-z_][a-z0-9_]*)+(?:\.[a-z]+)?)|",
            r"(?:pkg|src|lib|internal|cmd|app)/[a-z_][a-z0-9_]*|",
            r"(?:auth|login|user|api|db|database|config|service|handler|controller|model|view|component|util|helper|test|tests)",
            r")\b"
        ))
        .unwrap()
    });
    let mut seen = BTreeSet::new();
    let mut hints = Vec::new();
    for m in re.find_iter(&title.to_lowercase()) {
        let m = m.as_str().to_lowercase();
        if seen.insert(m.clone()) {
            hints.push(m);
        }
    }
    hints
}

fn paths_match_hints(files: &[FileChange], hints: &[String]) -> bool {
    files.iter().any(|f| {
        let lower = f.path.to_lowercase();
        hints.iter().any(|hint| lower.contains(hint.as_str()))
    })
}

/// Go `temporalConfidence` — base 0.50 ± factors, clamped to [0.20, 0.85].
fn temporal_confidence(
    active_beads: usize,
    window_duration_ns: i64,
    files: &[FileChange],
    hints: &[String],
) -> f64 {
    let mut base = 0.50f64;
    if active_beads <= 1 {
        base += 0.20;
    } else if active_beads == 2 {
        base += 0.10;
    } else if active_beads > 3 {
        base -= 0.10;
    }
    const HOUR: i64 = 3600 * 1_000_000_000;
    if window_duration_ns < 4 * HOUR {
        base += 0.10;
    } else if window_duration_ns < 24 * HOUR {
        base += 0.05;
    } else if window_duration_ns > 7 * 24 * HOUR {
        base -= 0.15;
    } else if window_duration_ns > 3 * 24 * HOUR {
        base -= 0.05;
    }
    if !hints.is_empty() && paths_match_hints(files, hints) {
        base += 0.15;
    }
    base.clamp(0.20, 0.85)
}

/// Go `temporalReason`.
fn temporal_reason(
    window_author: &str,
    active_beads: usize,
    window_duration_ns: i64,
    files: &[FileChange],
    hints: &[String],
) -> String {
    let mut parts = vec![format!(
        "Commit by {window_author} during bead's active window"
    )];
    const HOUR: i64 = 3600 * 1_000_000_000;
    if window_duration_ns < 4 * HOUR {
        parts.push("short window (<4h)".to_string());
    } else if window_duration_ns > 7 * 24 * HOUR {
        parts.push(format!(
            "long window ({}d)",
            window_duration_ns / (24 * HOUR)
        ));
    }
    if active_beads <= 1 {
        parts.push("author had only this bead active".to_string());
    } else if active_beads > 3 {
        parts.push(format!("author had {active_beads} beads active"));
    }
    if !hints.is_empty() && paths_match_hints(files, hints) {
        parts.push("file paths match bead title keywords".to_string());
    }
    parts.join("; ")
}

/// Go `commitInWindow` — author match (email preferred) inside [start, end].
/// Both bounds are inclusive, as in Go's `Before`/`After` guards.
fn commit_in_window(w: &TemporalWindow, wc: &WalkedCommit) -> bool {
    if ts_before(&wc.timestamp, &w.start) || ts_after(&wc.timestamp, &w.end) {
        return false;
    }
    if !w.author_email.is_empty() {
        return wc.author_email.eq_ignore_ascii_case(&w.author_email);
    }
    if !w.author.is_empty() {
        return wc.author.eq_ignore_ascii_case(&w.author);
    }
    false
}

/// Go `extractTemporalCandidates` — for every window, the walked commits by
/// the claimant inside it that did not touch `.beads/` (those belong to the
/// co-commit strategy) and changed at least one code file.
fn extract_temporal_candidates(
    repo: &Path,
    events: &[BeadEvent],
    walk: &[WalkedCommit],
    bead_filter: &str,
    cache: &mut CoCommitCache,
) -> Result<Vec<TemporalCandidate>, String> {
    let windows = temporal_windows_from_events(events, bead_filter);
    if windows.is_empty() {
        return Ok(Vec::new());
    }
    let mut pairs: Vec<(&TemporalWindow, &WalkedCommit)> = Vec::new();
    let mut shas: Vec<String> = Vec::new();
    let mut queued = BTreeSet::new();
    for w in &windows {
        for wc in walk {
            if wc.touches_beads_dir() || !commit_in_window(w, wc) {
                continue;
            }
            pairs.push((w, wc));
            if queued.insert(wc.sha.clone()) {
                shas.push(wc.sha.clone());
            }
        }
    }
    if pairs.is_empty() {
        return Ok(Vec::new());
    }
    cache.prime(repo, &shas)?;

    let mut candidates = Vec::with_capacity(pairs.len());
    for (w, wc) in pairs {
        let files = cache.code_files(&wc.sha);
        if files.is_empty() {
            continue;
        }
        candidates.push(TemporalCandidate {
            bead_id: w.bead_id.clone(),
            sha: wc.sha.clone(),
            message: wc.subject.clone(),
            author: wc.author.clone(),
            author_email: wc.author_email.clone(),
            timestamp: wc.timestamp.clone(),
            files,
            window_author: w.author.clone(),
            window_start: w.start.clone(),
            window_end: w.end.clone(),
            active_beads: w.active_beads,
        });
    }
    Ok(candidates)
}

/// Go `finalizeTemporalCandidate` — score against the bead's *current* title.
fn finalize_temporal_candidate(cand: &TemporalCandidate, title: &str) -> HistoryCommit {
    let active = if cand.active_beads == 0 {
        1
    } else {
        cand.active_beads
    };
    let dur = duration_ns(&cand.window_start, &cand.window_end).unwrap_or(0);
    let hints = extract_path_hints(title);
    HistoryCommit {
        bead_id: cand.bead_id.clone(),
        sha: cand.sha.clone(),
        short_sha: short_sha(&cand.sha),
        message: cand.message.clone(),
        author: cand.author.clone(),
        author_email: cand.author_email.clone(),
        timestamp: cand.timestamp.clone(),
        files: cand.files.clone(),
        method: Method::TemporalAuthor.as_str(),
        methods: vec![Method::TemporalAuthor.as_str().to_string()],
        confidence: temporal_confidence(active, dur, &cand.files, &hints),
        reason: temporal_reason(&cand.window_author, active, dur, &cand.files, &hints),
        confirmed: false,
    }
}

// ---------------------------------------------------------------------------
// Milestones, cycle time, merge
// ---------------------------------------------------------------------------

/// Go `GetBeadMilestones` — first created, first claimed, latest closed,
/// latest reopened.
pub fn get_bead_milestones(events: &[BeadEvent]) -> BeadMilestones {
    let mut m = BeadMilestones::default();
    for e in events {
        match e.event_type {
            EventType::Created if m.created.is_none() => m.created = Some(e.clone()),
            EventType::Claimed if m.claimed.is_none() => m.claimed = Some(e.clone()),
            EventType::Closed => m.closed = Some(e.clone()),
            EventType::Reopened => m.reopened = Some(e.clone()),
            _ => {}
        }
    }
    m
}

/// Go `CalculateCycleTime` — nanosecond durations; `None` when never closed.
pub fn calculate_cycle_time(m: &BeadMilestones) -> Option<CycleTime> {
    let closed = m.closed.as_ref()?;
    let mut ct = CycleTime::default();
    if let Some(claimed) = &m.claimed {
        ct.claim_to_close = duration_ns(&claimed.timestamp, &closed.timestamp);
    }
    if let Some(created) = &m.created {
        ct.create_to_close = duration_ns(&created.timestamp, &closed.timestamp);
        if let Some(claimed) = &m.claimed {
            ct.create_to_claim = duration_ns(&created.timestamp, &claimed.timestamp);
        }
    }
    Some(ct)
}

/// Go `dedupCommits` — first occurrence per SHA wins.
fn dedup_commits(commits: &[HistoryCommit]) -> Vec<HistoryCommit> {
    let mut seen = BTreeSet::new();
    commits
        .iter()
        .filter(|c| seen.insert(c.sha.clone()))
        .cloned()
        .collect()
}

/// Go `Scorer.CombineReasons`.
fn combine_reasons(signals: &[(Method, f64, String)]) -> String {
    if signals.is_empty() {
        return String::new();
    }
    if signals.len() == 1 {
        return signals[0].2.clone();
    }
    let mut sorted: Vec<&(Method, f64, String)> = signals.iter().collect();
    sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let parts: Vec<String> = sorted
        .iter()
        .map(|(_, conf, reason)| format!("{reason} ({:.0}%)", conf * 100.0))
        .collect();
    format!("Multiple signals: {}", parts.join("; "))
}

fn method_of(name: &str) -> Method {
    match name {
        "co_committed" => Method::CoCommitted,
        "explicit_id" => Method::ExplicitId,
        _ => Method::TemporalAuthor,
    }
}

/// Go `mergeCorrelatedCommits` — per-strategy lists in precedence order merged
/// into one chronological list. A SHA matched by several strategies keeps
/// every method, the highest-confidence method as primary, the combined
/// confidence/reason, and the union of their files.
fn merge_correlated_commits(strategies: &[Vec<HistoryCommit>]) -> Vec<HistoryCommit> {
    let mut order: Vec<String> = Vec::new();
    let mut by_sha: HashMap<String, Vec<HistoryCommit>> = HashMap::new();
    for list in strategies {
        for cc in dedup_commits(list) {
            if !by_sha.contains_key(&cc.sha) {
                order.push(cc.sha.clone());
            }
            by_sha.entry(cc.sha.clone()).or_default().push(cc);
        }
    }
    if order.is_empty() {
        return Vec::new();
    }
    let mut merged = Vec::with_capacity(order.len());
    for sha in order {
        let group = by_sha.remove(&sha).unwrap_or_default();
        let mut result = group[0].clone();
        result.methods = vec![group[0].method.to_string()];
        if group.len() > 1 {
            let signals: Vec<(Method, f64, String)> = group
                .iter()
                .map(|cc| (method_of(cc.method), cc.confidence, cc.reason.clone()))
                .collect();
            let mut seen_files: BTreeSet<String> = BTreeSet::new();
            let mut files: Vec<FileChange> = Vec::new();
            let mut best = 0usize;
            for (i, cc) in group.iter().enumerate() {
                if i > 0 {
                    result.methods.push(cc.method.to_string());
                }
                if cc.confidence > group[best].confidence {
                    best = i;
                }
                for f in &cc.files {
                    if seen_files.insert(f.path.clone()) {
                        files.push(f.clone());
                    }
                }
            }
            result.method = group[best].method;
            result.confidence =
                combine_confidence(&signals.iter().map(|(m, c, _)| (*m, *c)).collect::<Vec<_>>());
            result.reason = combine_reasons(&signals);
            result.files = files;
        }
        merged.push(result);
    }
    // Stable sort: ties keep strategy precedence (co_committed first).
    merged.sort_by(|a, b| ts_cmp(&a.timestamp, &b.timestamp));
    merged
}

fn ts_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    match (parse_ts(a), parse_ts(b)) {
        (Some(a), Some(b)) => a.cmp(&b),
        _ => a.cmp(b),
    }
}

// ---------------------------------------------------------------------------
// Artifact + assembly
// ---------------------------------------------------------------------------

/// Go `historyArtifact` — the HEAD-only half of report generation.
#[derive(Debug, Default)]
pub struct HistoryArtifact {
    pub events: Vec<BeadEvent>,
    /// co_committed
    pub commits: Vec<HistoryCommit>,
    /// explicit_id
    pub explicit: Vec<HistoryCommit>,
    /// temporal_author, scored at assembly
    pub temporal: Vec<TemporalCandidate>,
    pub walked_commits: i64,
    pub strategies: Vec<StrategyRun>,
    /// Go `extractCausalHistory` output, retained only when a causality target
    /// was requested.
    pub causal_history: Option<crate::causality::CausalHistory>,
}

/// Go `durationMS` — microsecond precision, so a strategy's reported cost has
/// the same three-decimal shape as Go's.
fn duration_ms(start: Instant) -> f64 {
    start.elapsed().as_micros() as f64 / 1000.0
}

/// Go `extractHistoryArtifact` — the three strategies over committed history.
pub fn extract_history_artifact(
    repo: &Path,
    opts: &HistoryOptions,
    beads_file: Option<&str>,
) -> Result<HistoryArtifact, String> {
    let mut art = HistoryArtifact::default();
    let extract_opts = ExtractOptions {
        revision: opts.revision.clone(),
        bead_id: Some(opts.bead_id.clone()).filter(|s| !s.is_empty()),
        since: opts.since.clone(),
        until: opts.until.clone(),
        limit: opts.limit.max(0) as usize,
        beads_file: beads_file.map(str::to_string),
    };

    // Strategy 1: co_committed.
    let start = Instant::now();
    let events = extract(repo, &extract_opts).map_err(|e| format!("extracting events: {e}"))?;
    let mut cache = CoCommitCache::default();
    let commits = extract_all_co_commits(repo, &events, &mut cache)
        .map_err(|e| format!("extracting co-commits: {e}"))?;
    art.events = events;
    art.commits = commits;
    art.strategies.push(StrategyRun {
        name: Method::CoCommitted.as_str(),
        ran: true,
        duration_ms: duration_ms(start),
        candidates: art.commits.len() as i64,
    });

    // One bounded metadata walk feeds the remaining strategies and the window.
    let walk = walk_commits(repo, opts).map_err(|e| format!("walking commits: {e}"))?;
    art.walked_commits = walk.len() as i64;

    // Strategy 2: explicit_id.
    let start = Instant::now();
    let explicit = extract_explicit_commits(repo, &walk, &opts.bead_id, &mut cache)
        .map_err(|e| format!("extracting explicit-id commits: {e}"))?;
    art.explicit = explicit;
    art.strategies.push(StrategyRun {
        name: Method::ExplicitId.as_str(),
        ran: true,
        duration_ms: duration_ms(start),
        candidates: art.explicit.len() as i64,
    });

    // Strategy 3: temporal_author.
    let start = Instant::now();
    let temporal = extract_temporal_candidates(repo, &art.events, &walk, &opts.bead_id, &mut cache)
        .map_err(|e| format!("extracting temporal candidates: {e}"))?;
    art.temporal = temporal;
    art.strategies.push(StrategyRun {
        name: Method::TemporalAuthor.as_str(),
        ran: true,
        duration_ms: duration_ms(start),
        candidates: art.temporal.len() as i64,
    });

    // Go `Correlator.GenerateReport` runs the causal walk only for an
    // explicitly requested target. It is deliberately a *separate* walk from
    // the one above: it must see records that did not change the target.
    if !opts.causality_bead_id.is_empty() {
        // The target's own `-G` filter must not apply here.
        let causal_opts = ExtractOptions {
            bead_id: None,
            ..extract_opts.clone()
        };
        let beads_rel = crate::extractor::resolve_beads_path(repo, beads_file);
        art.causal_history = Some(
            crate::extractor_snapshot::extract_causal_history(
                repo,
                &opts.causality_bead_id,
                &causal_opts,
                &beads_rel,
            )
            .map_err(|e| format!("extracting causal history: {e}"))?,
        );
    }

    Ok(art)
}

/// Go `buildHistories` — one entry per bead, seeded from the bead list so
/// beads with no history still appear with empty events and no commits.
fn build_histories(
    beads: &[BeadInfo],
    events: &[BeadEvent],
    commits: &[HistoryCommit],
) -> BTreeMap<String, BeadHistory> {
    let mut histories: BTreeMap<String, BeadHistory> = beads
        .iter()
        .map(|b| {
            (
                b.id.clone(),
                BeadHistory {
                    bead_id: b.id.clone(),
                    title: b.title.clone(),
                    status: b.status.clone(),
                    events: Vec::new(),
                    milestones: BeadMilestones::default(),
                    commits: None,
                    cycle_time: None,
                    last_author: String::new(),
                },
            )
        })
        .collect();

    let mut events_by_bead: BTreeMap<&str, Vec<BeadEvent>> = BTreeMap::new();
    for e in events {
        events_by_bead
            .entry(e.bead_id.as_str())
            .or_default()
            .push(e.clone());
    }
    let mut commits_by_bead: BTreeMap<&str, Vec<HistoryCommit>> = BTreeMap::new();
    for c in commits {
        if !c.bead_id.is_empty() {
            commits_by_bead
                .entry(c.bead_id.as_str())
                .or_default()
                .push(c.clone());
        }
    }

    for (bead_id, history) in histories.iter_mut() {
        if let Some(evs) = events_by_bead.get(bead_id.as_str()) {
            history.events = evs.clone();
        }
        if let Some(cs) = commits_by_bead.get(bead_id.as_str()) {
            let deduped = dedup_commits(cs);
            if !deduped.is_empty() {
                history.commits = Some(deduped);
            }
        }
        history.milestones = get_bead_milestones(&history.events);
        history.cycle_time = calculate_cycle_time(&history.milestones);
        history.last_author = last_author(history.commits.as_deref(), &history.events);
    }
    histories
}

fn last_author(commits: Option<&[HistoryCommit]>, events: &[BeadEvent]) -> String {
    if let Some(cs) = commits {
        if let Some(last) = cs.last() {
            return last.author.clone();
        }
    }
    events.last().map(|e| e.author.clone()).unwrap_or_default()
}

/// Go `mergeStrategyCommits` — fold explicit-ID and temporal candidates into
/// the co-commit histories, dropping pairs whose bead is not in the current set.
fn merge_strategy_commits(
    histories: &mut BTreeMap<String, BeadHistory>,
    beads: &[BeadInfo],
    art: &HistoryArtifact,
) {
    let titles: BTreeMap<&str, &str> = beads
        .iter()
        .map(|b| (b.id.as_str(), b.title.as_str()))
        .collect();

    let mut explicit_by_bead: BTreeMap<String, Vec<HistoryCommit>> = BTreeMap::new();
    let mut explicit_beads_by_sha: HashMap<&str, BTreeSet<&str>> = HashMap::new();
    for cc in &art.explicit {
        if !histories.contains_key(&cc.bead_id) {
            continue;
        }
        explicit_by_bead
            .entry(cc.bead_id.clone())
            .or_default()
            .push(cc.clone());
        explicit_beads_by_sha
            .entry(cc.sha.as_str())
            .or_default()
            .insert(cc.bead_id.as_str());
    }

    let mut temporal_by_bead: BTreeMap<String, Vec<HistoryCommit>> = BTreeMap::new();
    for cand in &art.temporal {
        if !histories.contains_key(&cand.bead_id) {
            continue;
        }
        // A commit naming a *different* existing bead belongs to that bead:
        // the explicit signal outranks "same author, same window".
        if let Some(beads_for_sha) = explicit_beads_by_sha.get(cand.sha.as_str()) {
            if !beads_for_sha.contains(cand.bead_id.as_str()) {
                continue;
            }
        }
        let title = titles.get(cand.bead_id.as_str()).copied().unwrap_or("");
        temporal_by_bead
            .entry(cand.bead_id.clone())
            .or_default()
            .push(finalize_temporal_candidate(cand, title));
    }

    let empty: Vec<HistoryCommit> = Vec::new();
    for (bead_id, history) in histories.iter_mut() {
        let co = history.commits.clone().unwrap_or_default();
        let merged = merge_correlated_commits(&[
            co,
            explicit_by_bead
                .get(bead_id)
                .cloned()
                .unwrap_or_else(|| empty.clone()),
            temporal_by_bead
                .get(bead_id)
                .cloned()
                .unwrap_or_else(|| empty.clone()),
        ]);
        history.commits = if merged.is_empty() {
            None
        } else {
            Some(merged)
        };
        history.last_author = last_author(history.commits.as_deref(), &history.events);
    }
}

/// Go `BuildCommitIndex` — SHA -> sorted, deduplicated bead IDs.
pub fn build_commit_index(histories: &BTreeMap<String, BeadHistory>) -> CommitIndex {
    let mut seen: HashMap<String, BTreeSet<String>> = HashMap::new();
    for (bead_id, history) in histories {
        for commit in history.commits.iter().flatten() {
            seen.entry(commit.sha.clone())
                .or_default()
                .insert(bead_id.clone());
        }
    }
    seen.into_iter()
        .map(|(sha, beads)| (sha, beads.into_iter().collect()))
        .collect()
}

/// Go `calculateStats` — note `avg_commits_per_bead` divides *unique* commits
/// by beads-with-commits, and the method distribution counts a multi-method
/// commit once per method.
///
/// `feedback` is Go's `*FeedbackApplied`: `None` when no feedback store was
/// attached (the field is then omitted from the JSON), `Some` once one was —
/// including a store that matched nothing, which still reports three zeros.
pub fn calculate_stats(
    histories: &BTreeMap<String, BeadHistory>,
    strategies: Vec<StrategyRun>,
    feedback: Option<FeedbackApplied>,
) -> HistoryStats {
    let mut stats = HistoryStats {
        total_beads: histories.len() as i64,
        beads_with_commits: 0,
        total_commits: 0,
        unique_authors: 0,
        avg_commits_per_bead: 0.0,
        avg_cycle_time_days: None,
        method_distribution: BTreeMap::new(),
        strategies: Some(strategies),
        feedback_applied: feedback,
    };
    let mut authors: BTreeSet<&str> = BTreeSet::new();
    let mut unique_commits: BTreeSet<&str> = BTreeSet::new();
    let mut cycle_times: Vec<i64> = Vec::new();

    for history in histories.values() {
        let commits = history.commits.as_deref().unwrap_or(&[]);
        if !commits.is_empty() {
            stats.beads_with_commits += 1;
        }
        for commit in commits {
            unique_commits.insert(commit.sha.as_str());
            authors.insert(commit.author.as_str());
            for method in commit.all_methods() {
                *stats
                    .method_distribution
                    .entry(method.to_string())
                    .or_default() += 1;
            }
            if commit.confirmed {
                *stats
                    .method_distribution
                    .entry("confirmed_by_feedback".to_string())
                    .or_default() += 1;
            }
        }
        for event in &history.events {
            authors.insert(event.author.as_str());
        }
        if let Some(ct) = history.cycle_time.as_ref().and_then(|c| c.claim_to_close) {
            cycle_times.push(ct);
        }
    }

    stats.total_commits = unique_commits.len() as i64;
    stats.unique_authors = authors.len() as i64;
    if stats.beads_with_commits > 0 {
        stats.avg_commits_per_bead = stats.total_commits as f64 / stats.beads_with_commits as f64;
    }
    if !cycle_times.is_empty() {
        let total: i64 = cycle_times.iter().sum();
        // Go: total.Hours() / 24 / count.
        let avg_days = (total as f64 / 3_600_000_000_000.0) / 24.0 / cycle_times.len() as f64;
        stats.avg_cycle_time_days = Some(avg_days);
    }
    stats
}

/// Go `hashBeads` — a 12-hex-char fingerprint over the bead fields the report
/// embeds (id, title, status), sorted so input order does not matter.
pub fn hash_beads(beads: &[BeadInfo]) -> String {
    use sha2::{Digest, Sha256};
    if beads.is_empty() {
        // Go: sha256 of the empty string, first 12 hex chars.
        return hex_prefix12(&Sha256::digest([]));
    }
    let mut entries: Vec<String> = beads
        .iter()
        .map(|b| format!("{}\u{0}{}\u{0}{}", b.id, b.title, b.status))
        .collect();
    entries.sort();
    let mut hasher = Sha256::new();
    for entry in entries {
        hasher.update(entry.as_bytes());
    }
    hex_prefix12(&hasher.finalize())
}

fn hex_prefix12(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(12);
    for b in bytes.iter().take(6) {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Go `describeGitRange` — `at <rev>, since <date>, until <date>, limit N commits`.
pub fn describe_git_range(opts: &HistoryOptions) -> String {
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
        parts.push(format!("limit {} commits", opts.limit));
    }
    if parts.is_empty() {
        return "all history".to_string();
    }
    parts.join(", ")
}

fn date_part(ts: &str) -> String {
    ts.split(['T', ' ']).next().unwrap_or(ts).to_string()
}

/// Go `findLatestCommitSHA` — newest timestamp across events and co-commits.
fn find_latest_commit_sha(events: &[BeadEvent], commits: &[HistoryCommit]) -> String {
    let mut latest: Option<jiff::Timestamp> = None;
    let mut latest_sha = String::new();
    let mut consider = |ts: &str, sha: &str| {
        if let Some(t) = parse_ts(ts) {
            if latest.map(|l| t > l).unwrap_or(true) {
                latest = Some(t);
                latest_sha = sha.to_string();
            }
        }
    };
    for e in events {
        consider(&e.timestamp, &e.commit_sha);
    }
    for c in commits {
        consider(&c.timestamp, &c.sha);
    }
    latest_sha
}

/// Go `(*Correlator).applyFeedback` (correlator.go:701-748) — consult the
/// store for every (commit, bead) pair in `histories`:
///
/// - `reject` drops the commit from that bead, and hence from the commit index
///   and the stats built afterwards;
/// - `confirm` pins the confidence to 1.0, marks the commit confirmed, and
///   appends `; confirmed by feedback (<by>)` to its reason;
/// - `ignore` leaves the commit untouched but is counted.
///
/// Runs *before* anything derives from the commit lists. Go returns `nil` when
/// no store is attached; the caller turns that into `None` here, which is the
/// only difference between the two `FeedbackApplied` shapes.
pub fn apply_feedback(
    histories: &mut BTreeMap<String, BeadHistory>,
    all: &std::collections::HashMap<(String, String), crate::feedback::CorrelationFeedback>,
) -> FeedbackApplied {
    let mut applied = FeedbackApplied::default();
    for (bead_id, history) in histories.iter_mut() {
        let Some(commits) = history.commits.as_ref() else {
            continue;
        };
        if commits.is_empty() {
            continue;
        }
        let mut kept: Vec<HistoryCommit> = Vec::with_capacity(commits.len());
        let mut changed = false;
        for commit in commits {
            let Some(fb) = all.get(&(commit.sha.clone(), bead_id.clone())) else {
                kept.push(commit.clone());
                continue;
            };
            match fb.feedback_type.as_str() {
                "reject" => {
                    applied.rejected += 1;
                    changed = true;
                    continue;
                }
                "confirm" => {
                    applied.confirmed += 1;
                    changed = true;
                    let mut commit = commit.clone();
                    commit.confidence = 1.0;
                    commit.confirmed = true;
                    commit.reason = format!(
                        "{}; confirmed by feedback ({})",
                        commit.reason, fb.feedback_by
                    );
                    kept.push(commit);
                }
                "ignore" => {
                    applied.ignored += 1;
                    kept.push(commit.clone());
                }
                // Go's switch has no default arm, so an unrecognised type is
                // counted in nothing and the commit is kept as-is.
                _ => kept.push(commit.clone()),
            }
        }
        if !changed {
            continue;
        }
        // Go recomputes LastAuthor from the surviving commits, falling back to
        // the last event when a rejection emptied the list.
        history.last_author = kept
            .last()
            .map(|c| c.author.clone())
            .or_else(|| history.events.last().map(|e| e.author.clone()))
            .unwrap_or_default();
        history.commits = Some(kept);
    }
    applied
}

/// Go `assembleReport` — the working-tree half: histories, index, stats, and
/// the scalar metadata the envelope and output struct carry.
#[allow(clippy::too_many_arguments)]
pub fn assemble_report(
    beads: &[BeadInfo],
    opts: &HistoryOptions,
    art: HistoryArtifact,
    generated_at: String,
) -> HistoryReport {
    assemble_report_with_feedback(beads, opts, art, generated_at, None)
}

/// Go `assembleReport` on a correlator built by `WithFeedbackStore` — the
/// variant every read path in `cmd/bv` uses, so a stored confirm/reject
/// actually shapes histories, the commit index and the stats.
#[allow(clippy::too_many_arguments)]
pub fn assemble_report_with_feedback(
    beads: &[BeadInfo],
    opts: &HistoryOptions,
    art: HistoryArtifact,
    generated_at: String,
    feedback_store: Option<&crate::feedback::FeedbackStore>,
) -> HistoryReport {
    let mut histories = build_histories(beads, &art.events, &art.commits);
    merge_strategy_commits(&mut histories, beads, &art);

    // Honor stored confirm/reject feedback before anything derives from the
    // commit lists (index, stats).
    let feedback_applied = feedback_store.map(|store| {
        let all = store.load_all();
        apply_feedback(&mut histories, &all)
    });

    if !opts.bead_id.is_empty() {
        // Case-insensitive, matching the `bead_filter` comparison in
        // `extract_explicit_commits` — the option is raw user input.
        histories.retain(|id, _| id.eq_ignore_ascii_case(&opts.bead_id));
    }

    let commit_index = build_commit_index(&histories);
    let stats = calculate_stats(&histories, art.strategies.clone(), feedback_applied);
    let git_range = describe_git_range(opts);
    let data_hash = hash_beads(beads);
    let mut latest_commit_sha = find_latest_commit_sha(&art.events, &art.commits);
    if !opts.revision.is_empty() {
        latest_commit_sha = opts.revision.clone();
    }

    HistoryReport {
        generated_at,
        data_hash,
        git_range,
        latest_commit_sha,
        window: HistoryWindow {
            revision: opts.revision.clone(),
            limit: opts.limit,
            since: opts.since.clone(),
            until: opts.until.clone(),
            commits: art.walked_commits,
        },
        stats,
        histories,
        commit_index,
        causal_history: art.causal_history,
    }
}

/// Go `Correlator.GenerateReport` — extract then assemble.
pub fn build_history_report(
    repo: &Path,
    beads: &[BeadInfo],
    opts: &HistoryOptions,
    beads_file: Option<&str>,
    generated_at: String,
) -> Result<HistoryReport, String> {
    let art = extract_history_artifact(repo, opts, beads_file)?;
    Ok(assemble_report(beads, opts, art, generated_at))
}

/// Go `NewCorrelator(dir, beadsPath).WithFeedbackStore(store).GenerateReportCached(...)`
/// — the correlator every read path in `cmd/bv` is built with
/// (robot_registry.go:2700-2714). `beads_file` is the resolved
/// `correlation_feedback.jsonl` path, threaded through unchanged.
#[allow(clippy::too_many_arguments)]
pub fn build_history_report_with_feedback(
    repo: &Path,
    beads: &[BeadInfo],
    opts: &HistoryOptions,
    beads_file: Option<&str>,
    generated_at: String,
    feedback_store: Option<&crate::feedback::FeedbackStore>,
) -> Result<HistoryReport, String> {
    let art = extract_history_artifact(repo, opts, beads_file)?;
    Ok(assemble_report_with_feedback(
        beads,
        opts,
        art,
        generated_at,
        feedback_store,
    ))
}

/// Go `Scorer.FilterHistoriesByConfidence` — drop commits below the floor, then
/// Go re-derives the commit index and `beads_with_commits` from what survived.
/// A non-positive floor is a no-op. Filtering to empty yields Go's nil slice,
/// which serializes as `null`.
pub fn filter_histories_by_confidence(report: &mut HistoryReport, min_confidence: f64) {
    if min_confidence <= 0.0 {
        return;
    }
    for history in report.histories.values_mut() {
        let Some(commits) = history.commits.take() else {
            continue;
        };
        let kept: Vec<HistoryCommit> = commits
            .into_iter()
            .filter(|c| c.confidence >= min_confidence)
            .collect();
        history.commits = if kept.is_empty() { None } else { Some(kept) };
    }
    report.commit_index = build_commit_index(&report.histories);
    report.stats.beads_with_commits = report
        .histories
        .values()
        .filter(|h| h.commits.as_ref().is_some_and(|c| !c.is_empty()))
        .count() as i64;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(bead: &str, ty: EventType, ts: &str, author: &str) -> BeadEvent {
        BeadEvent {
            bead_id: bead.to_string(),
            event_type: ty,
            timestamp: ts.to_string(),
            commit_sha: format!("sha-{bead}-{}", ts.len()),
            commit_msg: "msg".to_string(),
            author: author.to_string(),
            author_email: format!("{author}@example.com"),
            before: None,
            after: None,
            transition_observed: true,
        }
    }

    fn commit(bead: &str, sha: &str, method: Method, ts: &str, confidence: f64) -> HistoryCommit {
        HistoryCommit {
            bead_id: bead.to_string(),
            sha: sha.to_string(),
            short_sha: short_sha(sha),
            message: "msg".to_string(),
            author: "alice".to_string(),
            author_email: "alice@example.com".to_string(),
            timestamp: ts.to_string(),
            files: vec![FileChange {
                path: "a.rs".to_string(),
                action: "M".to_string(),
                insertions: 1,
                deletions: 0,
            }],
            method: method.as_str(),
            methods: vec![method.as_str().to_string()],
            confidence,
            reason: "because".to_string(),
            confirmed: false,
        }
    }

    #[test]
    fn walk_parses_records_oldest_first_after_reverse() {
        // Two records in git's newest-first order; the walker reverses them.
        let out = concat!(
            "\x1eSHA_B\x00 2026-01-02T00:00:00Z\x00bob\x00b@x\x00sub b\x00body b\x1f\nb.rs\n",
            "\x1eSHA_A\x00 2026-01-01T00:00:00Z\x00alice\x00a@x\x00sub a\x00\x1f\na.rs\n",
        );
        let mut commits = parse_commit_walk(out.as_bytes()).expect("parse");
        commits.reverse();
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].sha, "SHA_A");
        assert_eq!(commits[0].files, vec!["a.rs".to_string()]);
        assert_eq!(commits[0].message(), "sub a");
        assert_eq!(commits[1].message(), "sub b\nbody b");
    }

    #[test]
    fn name_status_collapses_rename_to_new_path() {
        let files = parse_name_status("M\ta.rs\nR100\told.go\tnew.go\nA\tb.rs\n");
        assert_eq!(files.len(), 3);
        assert_eq!(files[1].path, "new.go");
        assert_eq!(files[1].action, "R");
    }

    #[test]
    fn numstat_keeps_binary_files_at_zero() {
        let stats = parse_numstat("12\t3\ta.rs\n-\t-\tbin.png\n");
        assert_eq!(
            stats["a.rs"],
            LineStats {
                insertions: 12,
                deletions: 3
            }
        );
        assert_eq!(stats["bin.png"], LineStats::default());
    }

    #[test]
    fn extract_new_path_resolves_both_rename_notations() {
        assert_eq!(extract_new_path("pkg/{old => new}/f.go"), "pkg/new/f.go");
        assert_eq!(extract_new_path("old.go => new.go"), "new.go");
        assert_eq!(extract_new_path("plain.rs"), "plain.rs");
    }

    /// Go's `ReplaceAllString` collapses *every* `{old => new}` group, so a
    /// numstat path carrying two brace groups still resolves to the new names.
    #[test]
    fn extract_new_path_collapses_every_rename_brace_group() {
        assert_eq!(extract_new_path("{a => b}/{c => d}/f.go"), "b/d/f.go");
        assert_eq!(
            extract_new_path("pkg/{old => new}/sub/{x => y}/f.go"),
            "pkg/new/sub/y/f.go"
        );
    }

    /// `for_each_commit_chunk` accepts both SHA-1 (40-hex) and SHA-256
    /// (64-hex) commit headers.
    #[test]
    fn commit_chunks_accept_sha1_and_sha256_widths() {
        let sha1 = "a".repeat(40);
        let sha256 = "b".repeat(64);
        let mut out = Vec::new();
        out.extend_from_slice(sha1.as_bytes());
        out.push(0);
        out.extend_from_slice(b"2026-01-01T00:00:00Z\x00a\x00e\x00msg\n+diff\n");
        out.extend_from_slice(sha256.as_bytes());
        out.push(0);
        out.extend_from_slice(b"2026-01-02T00:00:00Z\x00a\x00e\x00msg\n+diff\n");
        let chunks = for_each_commit_chunk(&out);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].0, sha1);
        assert_eq!(chunks[1].0, sha256);
        assert!(chunks[0].1.contains("+diff"));
        assert!(chunks[1].1.contains("+diff"));
    }

    /// A hex run longer than the SHA width is message text, not a header.
    #[test]
    fn commit_chunks_reject_overlong_hex_runs() {
        let mut out = Vec::new();
        out.extend_from_slice("c".repeat(65).as_bytes());
        out.push(0);
        out.extend_from_slice(b"not a header\n");
        assert!(for_each_commit_chunk(&out).is_empty());
    }

    #[test]
    fn code_file_filter_matches_go_extension_set() {
        assert!(is_code_file("crates/bv/src/main.rs"));
        assert!(is_code_file("README.md"));
        assert!(!is_code_file("logo.png"));
        assert!(!is_code_file("Makefile"));
    }

    #[test]
    fn excluded_paths_match_prefix_and_nested() {
        assert!(is_excluded_path(".beads/issues.jsonl"));
        assert!(is_excluded_path("web/node_modules/x.js"));
        assert!(!is_excluded_path("src/building.rs"));
    }

    #[test]
    fn co_commit_confidence_penalties_match_go() {
        let event = ev("X-1", EventType::Closed, "2026-01-01T00:00:00Z", "alice");
        let one = vec![FileChange {
            path: "a.rs".to_string(),
            action: "M".to_string(),
            insertions: 0,
            deletions: 0,
        }];
        assert!((co_commit_confidence(&event, &one) - 0.95).abs() < 1e-9);
        let tests_only: Vec<FileChange> = ["a_test.go", "b_test.go"]
            .iter()
            .map(|p| FileChange {
                path: (*p).to_string(),
                action: "M".to_string(),
                insertions: 0,
                deletions: 0,
            })
            .collect();
        assert!((co_commit_confidence(&event, &tests_only) - 0.90).abs() < 1e-9);
        let shotgun: Vec<FileChange> = (0..25)
            .map(|i| FileChange {
                path: format!("f{i}.rs"),
                action: "M".to_string(),
                insertions: 0,
                deletions: 0,
            })
            .collect();
        assert!((co_commit_confidence(&event, &shotgun) - 0.85).abs() < 1e-9);
    }

    #[test]
    fn co_commit_reason_lists_qualifiers() {
        let mut event = ev("X-1", EventType::Claimed, "2026-01-01T00:00:00Z", "alice");
        event.commit_msg = "work on x-1".to_string();
        let files = vec![FileChange {
            path: "a.rs".to_string(),
            action: "M".to_string(),
            insertions: 0,
            deletions: 0,
        }];
        assert_eq!(
            co_commit_reason(&event, &files),
            "Co-committed with bead status change to claimed; commit message references bead ID"
        );
    }

    #[test]
    fn explicit_confidence_matches_go_table() {
        assert!((explicit_confidence("closes", 1) - 0.95).abs() < 1e-9);
        assert!((explicit_confidence("bracket", 1) - 0.92).abs() < 1e-9);
        assert!((explicit_confidence("refs", 1) - 0.91).abs() < 1e-9);
        assert!((explicit_confidence("bead", 1) - 0.93).abs() < 1e-9);
        assert!((explicit_confidence("generic", 1) - 0.90).abs() < 1e-9);
        // Two IDs cost 0.02, floored at 0.70.
        assert!((explicit_confidence("closes", 2) - 0.93).abs() < 1e-9);
        assert!((explicit_confidence("generic", 40) - 0.70).abs() < 1e-9);
    }

    #[test]
    fn id_extraction_prefers_first_pattern_and_normalizes() {
        let _serialized = crate::explicit::custom_patterns_lock();
        let re = IdRegexes::new();
        let ids = re.extract("closes PROJ-1 and beads-42");
        let got: Vec<&str> = ids.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(got, vec!["proj-1", "bv-42"]);
        assert_eq!(ids[0].match_type, "closes");
        assert_eq!(ids[1].match_type, "bead");
    }

    #[test]
    fn generic_pattern_does_not_truncate_hash_ids() {
        let _serialized = crate::explicit::custom_patterns_lock();
        let re = IdRegexes::new();
        assert!(re.extract("bv-8a4r").is_empty());
    }

    /// Go `TestCustomIDPatterns_*` (#188) through the `--robot-history` path:
    /// `NewExplicitMatcher` reads `DefaultPatterns()`, so a registered pattern
    /// lands in the same ordered list as the built-ins and feeds the same
    /// dedup + confidence handling.
    #[test]
    fn explicit_matcher_reads_registered_custom_patterns() {
        let _serialized = crate::explicit::custom_patterns_lock();

        crate::explicit::set_custom_id_patterns(Vec::new());
        assert!(IdRegexes::new()
            .extract("fix flush ordering (zzq-a1b2c)")
            .is_empty());

        crate::explicit::set_custom_id_patterns(vec![Regex::new(r"\bzzq-[a-z0-9]{5}\b").unwrap()]);
        let ids = IdRegexes::new().extract("fix flush ordering (zzq-a1b2c)");
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0].id, "zzq-a1b2c");
        assert_eq!(ids[0].match_type, "generic");
        assert!((explicit_confidence("generic", 1) - 0.90).abs() < 1e-9);

        // Capture group 1 wins over the whole match, and the built-ins still
        // take precedence for an ID they already name.
        crate::explicit::set_custom_id_patterns(vec![
            Regex::new(r"(?i)ticket\s+(zzt-[a-z]{3})\b").unwrap()
        ]);
        let ids = IdRegexes::new().extract("board polish, ticket ZZT-PBB done");
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0].id, "zzt-pbb");

        crate::explicit::set_custom_id_patterns(Vec::new());
    }

    #[test]
    fn milestones_keep_first_created_and_latest_closed() {
        let events = vec![
            ev("X-1", EventType::Created, "2026-01-01T00:00:00Z", "a"),
            ev("X-1", EventType::Claimed, "2026-01-02T00:00:00Z", "a"),
            ev("X-1", EventType::Closed, "2026-01-03T00:00:00Z", "a"),
            ev("X-1", EventType::Reopened, "2026-01-04T00:00:00Z", "a"),
            ev("X-1", EventType::Closed, "2026-01-05T00:00:00Z", "a"),
        ];
        let m = get_bead_milestones(&events);
        assert_eq!(
            m.created.as_ref().unwrap().timestamp,
            "2026-01-01T00:00:00Z"
        );
        assert_eq!(m.closed.as_ref().unwrap().timestamp, "2026-01-05T00:00:00Z");
        assert_eq!(
            m.reopened.as_ref().unwrap().timestamp,
            "2026-01-04T00:00:00Z"
        );
    }

    #[test]
    fn cycle_time_is_nanoseconds_and_none_without_close() {
        let events = vec![
            ev("X-1", EventType::Created, "2026-01-01T00:00:00Z", "a"),
            ev("X-1", EventType::Claimed, "2026-01-01T01:00:00Z", "a"),
            ev("X-1", EventType::Closed, "2026-01-01T02:00:00Z", "a"),
        ];
        let ct = calculate_cycle_time(&get_bead_milestones(&events)).expect("closed");
        assert_eq!(ct.claim_to_close, Some(3_600_000_000_000));
        assert_eq!(ct.create_to_close, Some(7_200_000_000_000));
        assert_eq!(ct.create_to_claim, Some(3_600_000_000_000));
        assert!(calculate_cycle_time(&BeadMilestones::default()).is_none());
    }

    #[test]
    fn merge_unions_methods_files_and_orders_chronologically() {
        let co = vec![commit(
            "X-1",
            "SHA_B",
            Method::CoCommitted,
            "2026-01-02T00:00:00Z",
            0.99,
        )];
        let mut ex = commit(
            "X-1",
            "SHA_A",
            Method::ExplicitId,
            "2026-01-01T00:00:00Z",
            0.90,
        );
        ex.files[0].path = "b.rs".to_string();
        let merged = merge_correlated_commits(&[co, vec![ex]]);
        assert_eq!(merged.len(), 2);
        // Chronological: SHA_A (explicit, earlier) first.
        assert_eq!(merged[0].sha, "SHA_A");
        assert_eq!(merged[0].method, "explicit_id");
        assert_eq!(merged[1].sha, "SHA_B");
        assert_eq!(merged[1].method, "co_committed");
    }

    #[test]
    fn merge_combines_confidence_and_reason_for_double_match() {
        let co = vec![commit(
            "X-1",
            "SHA",
            Method::CoCommitted,
            "2026-01-01T00:00:00Z",
            0.95,
        )];
        let mut ex = commit(
            "X-1",
            "SHA",
            Method::ExplicitId,
            "2026-01-01T00:00:00Z",
            0.90,
        );
        ex.files[0].path = "b.rs".to_string();
        let merged = merge_correlated_commits(&[co, vec![ex]]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].methods, vec!["co_committed", "explicit_id"]);
        assert_eq!(merged[0].method, "co_committed");
        assert_eq!(merged[0].files.len(), 2);
        assert!(merged[0].reason.starts_with("Multiple signals: "));
        assert!(merged[0].confidence > 0.95 && merged[0].confidence <= 0.99);
    }

    #[test]
    fn commit_index_sorts_and_deduplicates_bead_ids() {
        let mut histories = BTreeMap::new();
        for (id, sha) in [("B-2", "SHA"), ("A-1", "SHA")] {
            histories.insert(
                id.to_string(),
                BeadHistory {
                    bead_id: id.to_string(),
                    title: String::new(),
                    status: "open".to_string(),
                    events: Vec::new(),
                    milestones: BeadMilestones::default(),
                    commits: Some(vec![commit(
                        id,
                        sha,
                        Method::ExplicitId,
                        "2026-01-01T00:00:00Z",
                        0.9,
                    )]),
                    cycle_time: None,
                    last_author: String::new(),
                },
            );
        }
        let index = build_commit_index(&histories);
        assert_eq!(index["SHA"], vec!["A-1".to_string(), "B-2".to_string()]);
    }

    #[test]
    fn commit_in_window_matches_author_within_inclusive_bounds() {
        let w = TemporalWindow {
            bead_id: "X-1".into(),
            author: "Tran Quang Dang".into(),
            author_email: "dang@example.com".into(),
            start: "2026-08-23T00:25:54+07:00".into(),
            end: "2026-08-23T03:38:55+07:00".into(),
            active_beads: 9,
        };
        let wc = |ts: &str, name: &str, email: &str| WalkedCommit {
            sha: "s".into(),
            timestamp: ts.into(),
            author: name.into(),
            author_email: email.into(),
            subject: String::new(),
            body: String::new(),
            files: vec!["a.rs".into()],
        };
        let inside = wc(
            "2026-08-23T01:18:39+07:00",
            "Tran Quang Dang",
            "dang@example.com",
        );
        assert!(commit_in_window(&w, &inside), "inside the window");
        // Both bounds are inclusive.
        assert!(commit_in_window(
            &w,
            &wc(
                "2026-08-23T00:25:54+07:00",
                "Tran Quang Dang",
                "dang@example.com"
            )
        ));
        assert!(commit_in_window(
            &w,
            &wc(
                "2026-08-23T03:38:55+07:00",
                "Tran Quang Dang",
                "dang@example.com"
            )
        ));
        // Before the claim and after the close are both outside.
        assert!(!commit_in_window(
            &w,
            &wc(
                "2026-08-23T00:25:53+07:00",
                "Tran Quang Dang",
                "dang@example.com"
            )
        ));
        assert!(!commit_in_window(
            &w,
            &wc(
                "2026-08-23T03:38:56+07:00",
                "Tran Quang Dang",
                "dang@example.com"
            )
        ));
        // Right window, wrong author.
        assert!(!commit_in_window(
            &w,
            &wc(
                "2026-08-23T01:18:39+07:00",
                "Someone Else",
                "other@example.com"
            )
        ));
    }

    #[test]
    fn commit_in_window_falls_back_to_display_name_without_email() {
        let w = TemporalWindow {
            bead_id: "X-1".into(),
            author: "alice".into(),
            author_email: String::new(),
            start: "2026-01-01T00:00:00Z".into(),
            end: "2026-01-02T00:00:00Z".into(),
            active_beads: 1,
        };
        let wc = WalkedCommit {
            sha: "s".into(),
            timestamp: "2026-01-01T12:00:00Z".into(),
            author: "Alice".into(),
            author_email: "whatever@example.com".into(),
            subject: String::new(),
            body: String::new(),
            files: Vec::new(),
        };
        assert!(commit_in_window(&w, &wc), "case-insensitive name match");
    }

    #[test]
    fn temporal_windows_count_concurrent_beads_per_author() {
        let events = vec![
            ev("A-1", EventType::Claimed, "2026-01-01T00:00:00Z", "alice"),
            ev("A-1", EventType::Closed, "2026-01-03T00:00:00Z", "alice"),
            ev("B-2", EventType::Claimed, "2026-01-02T00:00:00Z", "alice"),
            ev("B-2", EventType::Closed, "2026-01-04T00:00:00Z", "alice"),
            // Different author, not concurrent for alice's window.
            ev("C-3", EventType::Claimed, "2026-01-01T00:00:00Z", "bob"),
            ev("C-3", EventType::Closed, "2026-01-05T00:00:00Z", "bob"),
        ];
        let windows = temporal_windows_from_events(&events, "");
        assert_eq!(windows.len(), 3);
        let a1 = windows.iter().find(|w| w.bead_id == "A-1").unwrap();
        let b2 = windows.iter().find(|w| w.bead_id == "B-2").unwrap();
        // A-1's window overlaps B-2 (started before A-1 closed, closed after
        // A-1 started): both windows see 2 concurrent beads.
        assert_eq!(a1.active_beads, 2);
        assert_eq!(b2.active_beads, 2);
        let c3 = windows.iter().find(|w| w.bead_id == "C-3").unwrap();
        assert_eq!(c3.active_beads, 1);
    }

    #[test]
    fn temporal_confidence_factors_match_go() {
        const HOUR: i64 = 3600 * 1_000_000_000;
        let no_hints: Vec<String> = Vec::new();
        // One bead, short window, no hint match: 0.50 + 0.20 + 0.10.
        assert!((temporal_confidence(1, 2 * HOUR, &[], &no_hints) - 0.80).abs() < 1e-9);
        // Many beads, long window: 0.50 - 0.10 - 0.15.
        assert!((temporal_confidence(5, 8 * 24 * HOUR, &[], &no_hints) - 0.25).abs() < 1e-9);
        // Hint match adds 0.15.
        let files = vec![FileChange {
            path: "src/auth/login.rs".to_string(),
            action: "M".to_string(),
            insertions: 0,
            deletions: 0,
        }];
        let hints = vec!["auth".to_string()];
        assert!(
            (temporal_confidence(1, 2 * HOUR, &files, &hints) - 0.85).abs() < 1e-9,
            "clamped at the 0.85 ceiling"
        );
    }

    #[test]
    fn path_hints_extract_components_and_dedupe() {
        let hints = extract_path_hints("Fix auth in pkg/api and the auth handler");
        assert!(hints.contains(&"auth".to_string()));
        assert!(hints.contains(&"pkg/api".to_string()));
        assert!(hints.contains(&"handler".to_string()));
        // "auth" appears twice in the title but is hinted once.
        assert_eq!(hints.iter().filter(|h| *h == "auth").count(), 1);
    }

    #[test]
    fn git_range_descriptions_match_go() {
        assert_eq!(
            describe_git_range(&HistoryOptions::default()),
            "all history"
        );
        let opts = HistoryOptions {
            limit: 500,
            since: Some("2026-01-02T03:04:05Z".to_string()),
            revision: "abc123".to_string(),
            ..Default::default()
        };
        assert_eq!(
            describe_git_range(&opts),
            "at abc123, since 2026-01-02, limit 500 commits"
        );
    }

    #[test]
    fn hash_beads_is_order_independent_and_empty_safe() {
        let a = BeadInfo {
            id: "A-1".into(),
            title: "A".into(),
            status: "open".into(),
        };
        let b = BeadInfo {
            id: "B-2".into(),
            title: "B".into(),
            status: "closed".into(),
        };
        assert_eq!(
            hash_beads(&[a.clone(), b.clone()]),
            hash_beads(&[b, a.clone()])
        );
        assert_eq!(hash_beads(&[]).len(), 12);
        assert_eq!(hash_beads(&[a]).len(), 12);
    }

    #[test]
    fn stats_average_unique_commits_over_beads_with_commits() {
        let mut histories = BTreeMap::new();
        for (id, shas) in [("A-1", vec!["S1", "S2"]), ("B-2", vec!["S1"])] {
            histories.insert(
                id.to_string(),
                BeadHistory {
                    bead_id: id.to_string(),
                    title: String::new(),
                    status: "open".to_string(),
                    events: Vec::new(),
                    milestones: BeadMilestones::default(),
                    commits: Some(
                        shas.iter()
                            .map(|s| {
                                commit(id, s, Method::CoCommitted, "2026-01-01T00:00:00Z", 0.99)
                            })
                            .collect(),
                    ),
                    cycle_time: None,
                    last_author: String::new(),
                },
            );
        }
        // A third bead with no commits at all.
        histories.insert(
            "C-3".to_string(),
            BeadHistory {
                bead_id: "C-3".to_string(),
                title: String::new(),
                status: "open".to_string(),
                events: Vec::new(),
                milestones: BeadMilestones::default(),
                commits: None,
                cycle_time: None,
                last_author: String::new(),
            },
        );
        let stats = calculate_stats(&histories, Vec::new(), None);
        assert_eq!(stats.total_beads, 3);
        assert_eq!(stats.beads_with_commits, 2);
        assert_eq!(
            stats.total_commits, 2,
            "S1 shared by both beads counts once"
        );
        assert!((stats.avg_commits_per_bead - 1.0).abs() < 1e-9);
        assert_eq!(stats.method_distribution["co_committed"], 3);
    }

    #[test]
    fn bead_filter_keeps_only_the_requested_history() {
        let beads = vec![
            BeadInfo {
                id: "A-1".into(),
                title: "A".into(),
                status: "open".into(),
            },
            BeadInfo {
                id: "B-2".into(),
                title: "B".into(),
                status: "open".into(),
            },
        ];
        let art = HistoryArtifact {
            events: vec![ev("A-1", EventType::Created, "2026-01-01T00:00:00Z", "a")],
            ..Default::default()
        };
        let opts = HistoryOptions {
            bead_id: "A-1".to_string(),
            ..Default::default()
        };
        let report = assemble_report(&beads, &opts, art, "2026-01-01T00:00:00Z".to_string());
        assert_eq!(report.histories.len(), 1);
        assert!(report.histories.contains_key("A-1"));
    }
}
