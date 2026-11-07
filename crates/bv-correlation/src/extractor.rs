//! Bead-event extraction from git history — port of Go
//! `pkg/correlation/extractor.go` legacy `git log -p --unified=0 --follow`
//! path. Event classification and status transitions match Go exactly.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

/// Lifecycle event types (Go EventType).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EventType {
    Created,
    Claimed,
    Closed,
    Reopened,
    Modified,
    /// The bead record disappeared from the history source. Go emits this for
    /// `hadOld && !hasNew` in `parseDiff`; it records removal, not completion.
    Deleted,
}

impl EventType {
    pub fn as_str(self) -> &'static str {
        match self {
            EventType::Created => "created",
            EventType::Claimed => "claimed",
            EventType::Closed => "closed",
            EventType::Reopened => "reopened",
            EventType::Modified => "modified",
            EventType::Deleted => "deleted",
        }
    }
}

/// Go `HistoricalDependency` — the dependency edge as it appeared inside a
/// committed bead record (owned by the extraction, not the live issue set).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoricalDependency {
    #[serde(rename = "depends_on_id")]
    pub depends_on_id: String,
    #[serde(rename = "type")]
    pub dep_type: String,
}

/// Go `HistoricalIssueState` — the decision evidence a committed record
/// carries. `dependencies` is omitted when the record had none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoricalIssueState {
    pub id: String,
    pub status: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<HistoricalDependency>,
}

/// One bead lifecycle event (Go `BeadEvent`).
#[derive(Debug, Clone, Serialize)]
pub struct BeadEvent {
    pub bead_id: String,
    #[serde(rename = "event_type")]
    pub event_type: EventType,
    /// RFC3339 timestamp of the commit.
    pub timestamp: String,
    pub commit_sha: String,
    #[serde(rename = "commit_message")]
    pub commit_msg: String,
    pub author: String,
    pub author_email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<HistoricalIssueState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<HistoricalIssueState>,
    /// Go: the per-commit record transition parsed cleanly. Full-source
    /// authority is a separate concern (causal history).
    pub transition_observed: bool,
}

/// Minimal bead state snapshot parsed from +/- diff lines (Go beadSnapshot).
#[derive(Debug, Clone)]
pub struct BeadSnapshot {
    pub id: String,
    pub status: String,
    pub title: String,
    pub dependencies: Vec<HistoricalDependency>,
}

impl BeadSnapshot {
    /// Go `beadSnapshot.historicalState`.
    pub fn historical_state(&self) -> HistoricalIssueState {
        HistoricalIssueState {
            id: self.id.clone(),
            status: self.status.clone(),
            title: self.title.clone(),
            dependencies: self.dependencies.clone(),
        }
    }
}

/// The `git log --format` header shared by every extraction path
/// (Go `gitLogHeaderFormat`).
pub const GIT_LOG_HEADER_FORMAT: &str = "%H%x00%aI%x00%an%x00%ae%x00%s";

/// One commit's metadata, parsed out of a `--format` header line. Shared by the
/// patch and snapshot extraction paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitHeader {
    pub sha: String,
    pub timestamp: String,
    pub author: String,
    pub author_email: String,
    pub message: String,
}

impl CommitHeader {
    /// Split a NUL-separated header line into its five fields. Returns `None`
    /// when the line is not a well-formed header, which is how the patch path
    /// tells a commit boundary from diff content.
    pub fn parse(line: &[u8]) -> Option<Self> {
        let text = String::from_utf8_lossy(line);
        let mut parts = text.split('\0');
        let sha = parts.next()?.to_string();
        // 40 hex chars for SHA-1 repos, 64 for SHA-256 repos.
        if !(sha.len() == 40 || sha.len() == 64) || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let timestamp = parts.next()?.to_string();
        let author = parts.next()?.to_string();
        let author_email = parts.next()?.to_string();
        let message = parts.next()?.to_string();
        Some(Self {
            sha,
            timestamp,
            author,
            author_email,
            message,
        })
    }
}

/// Options controlling extraction (Go `ExtractOptions`).
#[derive(Debug, Clone, Default)]
pub struct ExtractOptions {
    /// Resolved commit to walk from (empty = HEAD).
    pub revision: String,
    /// Only commits touching this bead ID (git -G filter).
    pub bead_id: Option<String>,
    /// Only commits at or after this instant (RFC3339).
    pub since: Option<String>,
    /// Only commits at or before this instant (RFC3339).
    pub until: Option<String>,
    /// Max commits to walk; 0 = unlimited.
    pub limit: usize,
    /// Path to the beads JSONL relative to repo root. Defaults to the
    /// first existing preferred name.
    pub beads_file: Option<String>,
}

/// Go `appendHistoryFilters` — the time/count bounds and the resolved revision,
/// shared by both extraction paths.
pub fn append_history_filters(args: &mut Vec<String>, opts: &ExtractOptions) {
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

pub const DEFAULT_BEADS_FILES: [&str; 3] = ["issues.jsonl", "beads.jsonl", "beads.base.jsonl"];

pub fn resolve_beads_path(repo: &Path, requested: Option<&str>) -> String {
    if let Some(p) = requested {
        return p.to_string();
    }
    for name in DEFAULT_BEADS_FILES {
        let candidate = repo.join(".beads").join(name);
        if candidate.exists() {
            // git log path is repo-relative
            return format!(".beads/{name}");
        }
    }
    ".beads/issues.jsonl".to_string()
}

/// Extract bead lifecycle events from the repository's git history.
///
/// Dispatches exactly as Go's `Extractor.Extract` does: a large followed blob
/// takes the snapshot path (per-commit JSONL record-multiset differences, no
/// textual patch), a small one takes the legacy `git log -p` path. The two are
/// byte-identical on every repo Go's differential test covers, but the snapshot
/// path additionally treats a pure record *reordering* as "no change" — so the
/// gate is a semantics decision, not only a speed one.
pub fn extract(repo: &Path, opts: &ExtractOptions) -> Result<Vec<BeadEvent>, String> {
    let beads_rel = resolve_beads_path(repo, opts.beads_file.as_deref());
    if crate::extractor_snapshot::prefer_snapshot_path(repo, &beads_rel) {
        return crate::extractor_snapshot::extract_via_snapshots(repo, opts, &beads_rel);
    }
    extract_via_git_log_patch(repo, opts, &beads_rel)
}

/// Go `extractViaGitLogPatch` — the legacy textual-patch path.
fn extract_via_git_log_patch(
    repo: &Path,
    opts: &ExtractOptions,
    beads_rel: &str,
) -> Result<Vec<BeadEvent>, String> {
    let mut args: Vec<String> = vec![
        "log".into(),
        "-p".into(),
        "--unified=0".into(),
        "--follow".into(),
        format!("--format={GIT_LOG_HEADER_FORMAT}"),
    ];
    append_history_filters(&mut args, opts);
    if let Some(id) = &opts.bead_id {
        args.push(format!("-G\"id\":\\s*\"{id}\""));
    }
    args.push("--".into());
    args.push(beads_rel.to_string());

    let out = Command::new("git")
        .args(&args)
        .current_dir(repo)
        .output()
        .map_err(|e| format!("spawning git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git log failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let mut events = parse_log_output(&text, opts.bead_id.as_deref());
    // Go `reverseEvents`: git log returns newest first, and every consumer of
    // `Extract` (milestones, cycle time, temporal windows, causality) reads a
    // chronological stream — `GetBeadMilestones` keeps the FIRST created and
    // claimed event, which newest-first would turn into the latest.
    events.reverse();
    Ok(events)
}

/// Parse combined `git log -p` output into events. Public for differential
/// testing against fixture logs.
pub fn parse_log_output(output: &str, filter_bead_id: Option<&str>) -> Vec<BeadEvent> {
    let mut all_events = Vec::new();

    // Split into commit sections by the header line pattern:
    // <sha>\x00<rfc3339>\x00<name>\x00<email>\x00<subject>
    // where <sha> is 40 hex chars on SHA-1 repos and 64 on SHA-256 repos.
    let mut current: Option<(CommitHeader, String)> = None;

    for line in output.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        // Header detection: the first NUL-separated field is a 40- or
        // 64-hex SHA followed by more fields.
        let bytes = line.as_bytes();
        let is_header = bytes.iter().position(|b| *b == 0).is_some_and(|nul| {
            (nul == 40 || nul == 64)
                && bytes[..nul].iter().all(|b| b.is_ascii_hexdigit())
                && bytes.len() > nul + 1
        });

        if is_header {
            if let Some((info, diff)) = current.take() {
                all_events.extend(parse_diff_text(&diff, &info, filter_bead_id));
            }
            if let Some(header) = CommitHeader::parse(bytes) {
                current = Some((header, String::new()));
            }
        } else if let Some((_, diff)) = current.as_mut() {
            diff.push_str(line);
            diff.push('\n');
        }
    }
    if let Some((info, diff)) = current.take() {
        all_events.extend(parse_diff_text(&diff, &info, filter_bead_id));
    }

    all_events
}

fn ignorable_metadata_line(line: &str) -> bool {
    match line.as_bytes().first() {
        None => true,
        Some(b'@') | Some(b'd') | Some(b'i') | Some(b'n') => true,
        // "diff --git", "index", "new file mode" all covered by first byte
        _ => false,
    }
}

/// Go `parseBeadJSON`: parse one committed record line. Only the fields the
/// event evidence needs are read; `dependencies` folds the legacy
/// `depends_on` / `target_id` target spellings into `depends_on_id` the same
/// way `bv_core::model::Dependency::effective_depends_on` does (Go
/// `Dependency.UnmarshalJSON`), while `type` is kept as the raw string the
/// record carried — Go's `DependencyType` is a plain string, so an absent
/// type stays `""` rather than normalizing to `"blocks"`.
pub fn parse_bead_json(json_str: &str) -> Option<BeadSnapshot> {
    #[derive(serde::Deserialize)]
    struct Partial {
        #[serde(default)]
        id: String,
        #[serde(default)]
        status: String,
        #[serde(default)]
        title: String,
        #[serde(default)]
        dependencies: Vec<RawDependency>,
    }
    /// The dependency edge as written in the committed record. Serde field
    /// names mirror `bv_core::model::Dependency` so the same legacy spellings
    /// are accepted; the fold order matches `effective_depends_on`.
    #[derive(serde::Deserialize)]
    struct RawDependency {
        #[serde(default, rename = "depends_on_id")]
        depends_on_id: String,
        #[serde(default, rename = "depends_on")]
        depends_on: String,
        #[serde(default, rename = "target_id")]
        target_id: String,
        #[serde(default, rename = "type")]
        dep_type: String,
    }
    impl RawDependency {
        fn effective_depends_on(&self) -> &str {
            if !self.depends_on_id.is_empty() {
                &self.depends_on_id
            } else if !self.depends_on.is_empty() {
                &self.depends_on
            } else {
                &self.target_id
            }
        }
    }
    let p: Partial = serde_json::from_str(json_str).ok()?;
    if p.id.is_empty() {
        return None;
    }
    // Canonical empty values survive the optional JSON field's cache round trip.
    let dependencies = if p.dependencies.is_empty() {
        Vec::new()
    } else {
        p.dependencies
            .into_iter()
            .map(|d| HistoricalDependency {
                depends_on_id: d.effective_depends_on().to_string(),
                dep_type: d.dep_type,
            })
            .collect()
    };
    Some(BeadSnapshot {
        id: p.id,
        status: p.status,
        title: p.title,
        dependencies,
    })
}

/// Go: `determineStatusEvent`.
pub fn determine_status_event(old_status: &str, new_status: &str) -> EventType {
    let old_s = old_status.trim().to_lowercase();
    let new_s = new_status.trim().to_lowercase();
    let was_closed = old_s == "closed" || old_s == "tombstone";
    match new_s.as_str() {
        "in_progress" => {
            if was_closed {
                EventType::Reopened
            } else {
                EventType::Claimed
            }
        }
        "closed" | "tombstone" => EventType::Closed,
        "open" => {
            if was_closed {
                EventType::Reopened
            } else {
                EventType::Modified
            }
        }
        _ => EventType::Modified,
    }
}

/// Go: `parseDiff` — collect +/- bead snapshots then emit sorted-ID events.
///
/// Go `parseDiff` for one commit's record diff. Public so the snapshot path can
/// feed it a synthesized `+{`/`-{` record diff.
///
/// Matches Go exactly on the three points that shape the emitted stream:
/// only `-{` / `+{` lines are considered (a `-` line that is not a JSON object
/// is neither a record nor evidence of a truncated parse), the affected bead
/// IDs are the *union* of old and new records iterated in sorted order, and
/// a record that vanished from the source emits a `deleted` event carrying
/// only its `before` state.
pub fn parse_diff_text(
    diff: &str,
    info: &CommitHeader,
    filter_bead_id: Option<&str>,
) -> Vec<BeadEvent> {
    // BTreeMap keeps deterministic sorted iteration (Go sorts explicitly).
    let mut old_beads: BTreeMap<String, BeadSnapshot> = BTreeMap::new();
    let mut new_beads: BTreeMap<String, BeadSnapshot> = BTreeMap::new();
    let mut complete = true;

    for line in diff.split('\n') {
        if ignorable_metadata_line(line) {
            continue;
        }
        if line.starts_with("-{") {
            let json = &line[1..];
            match parse_bead_json(json) {
                Some(snap) => {
                    if filter_bead_id.is_none_or(|f| snap.id == f) {
                        old_beads.insert(snap.id.clone(), snap);
                    }
                }
                None => complete = false,
            }
            continue;
        }
        if line.starts_with("+{") {
            let json = &line[1..];
            match parse_bead_json(json) {
                Some(snap) => {
                    if filter_bead_id.is_none_or(|f| snap.id == f) {
                        new_beads.insert(snap.id.clone(), snap);
                    }
                }
                None => complete = false,
            }
            continue;
        }
    }

    let mut seen: BTreeMap<&str, ()> = BTreeMap::new();
    for id in old_beads.keys().chain(new_beads.keys()) {
        seen.insert(id.as_str(), ());
    }

    let base = |bead_id: &str| BeadEvent {
        bead_id: bead_id.to_string(),
        event_type: EventType::Modified,
        timestamp: info.timestamp.clone(),
        commit_sha: info.sha.clone(),
        commit_msg: info.message.clone(),
        author: info.author.clone(),
        author_email: info.author_email.clone(),
        before: None,
        after: None,
        transition_observed: complete,
    };

    let mut events = Vec::new();
    for bead_id in seen.into_keys() {
        let old_snap = old_beads.get(bead_id);
        let new_snap = new_beads.get(bead_id);
        let mut event = base(bead_id);
        if let Some(old) = old_snap {
            event.before = Some(old.historical_state());
        }
        if let Some(new) = new_snap {
            event.after = Some(new.historical_state());
        }
        match (old_snap, new_snap) {
            (None, Some(_)) => {
                event.event_type = EventType::Created;
                events.push(event);
            }
            (Some(old), Some(new)) => {
                event.event_type = if old.status != new.status {
                    determine_status_event(&old.status, &new.status)
                } else {
                    EventType::Modified
                };
                events.push(event);
            }
            (Some(_), None) => {
                event.event_type = EventType::Deleted;
                events.push(event);
            }
            (None, None) => unreachable!("seen is the union of both maps"),
        }
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_LOG: &str = concat!(
        "abc123def4567890abc123def4567890abc12344\x00",
        "2026-08-22T10:00:00Z\x00",
        "Tran Quang Dang\x00",
        "dev@example.com\x00",
        "beads: close phase1a\n"
    );

    /// `CommitHeader::parse` accepts both SHA widths: 40 hex (SHA-1) and
    /// 64 hex (SHA-256).
    #[test]
    fn commit_header_accepts_sha1_and_sha256() {
        let sha1 = "a".repeat(40);
        let sha256 = "b".repeat(64);
        for sha in [&sha1, &sha256] {
            let line = format!("{sha}\x002026-01-01T00:00:00Z\x00A\x00a@x\x00msg");
            let header = CommitHeader::parse(line.as_bytes()).expect("header");
            assert_eq!(header.sha, *sha);
        }
        // Wrong widths and non-hex are still rejected.
        assert!(CommitHeader::parse(b"abc\x00t\x00a\x00e\x00m").is_none());
        assert!(CommitHeader::parse(
            b"zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz\x00t\x00a\x00e\x00m"
        )
        .is_none());
    }

    /// The patch path's header-line detection must also recognize a 64-hex
    /// SHA-256 header, otherwise every line lands in the previous diff.
    #[test]
    fn parse_log_output_recognizes_sha256_headers() {
        let sha256 = "b".repeat(64);
        let log = format!(
            "{sha256}\x002026-08-22T10:00:00Z\x00A\x00a@x\x00beads: close x\n+{{\"id\":\"X-1\",\"title\":\"T\",\"status\":\"open\"}}\n"
        );
        let events = parse_log_output(&log, None);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].bead_id, "X-1");
        assert_eq!(events[0].commit_sha, sha256);
    }

    #[test]
    fn history_filters_are_appended_in_git_order() {
        let opts = ExtractOptions {
            revision: "abc123".into(),
            since: Some("2026-08-01T00:00:00Z".into()),
            until: Some("2026-09-01T00:00:00Z".into()),
            limit: 50,
            ..Default::default()
        };
        let mut args = vec!["log".to_string()];
        append_history_filters(&mut args, &opts);
        assert_eq!(
            args,
            vec![
                "log",
                "--since=2026-08-01T00:00:00Z",
                "--until=2026-09-01T00:00:00Z",
                "-n50",
                "abc123",
            ]
        );
        // No bounds, no filters.
        let mut empty = vec!["log".to_string()];
        append_history_filters(&mut empty, &ExtractOptions::default());
        assert_eq!(empty, vec!["log"]);
    }

    #[test]
    fn parses_created_event_from_added_line() {
        let log = format!(
            "{SAMPLE_LOG}{}",
            "+{\"id\":\"X-1\",\"title\":\"New\",\"status\":\"open\"}\n"
        );
        let events = parse_log_output(&log, None);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].bead_id, "X-1");
        assert_eq!(events[0].event_type, EventType::Created);
        assert_eq!(
            events[0].commit_sha,
            "abc123def4567890abc123def4567890abc12344"
        );
    }

    #[test]
    fn status_transition_open_to_in_progress_is_claimed() {
        let log = format!(
            "{SAMPLE_LOG}{}{}",
            "-{\"id\":\"X-1\",\"title\":\"T\",\"status\":\"open\"}\n",
            "+{\"id\":\"X-1\",\"title\":\"T\",\"status\":\"in_progress\"}\n"
        );
        let events = parse_log_output(&log, None);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, EventType::Claimed);
    }

    #[test]
    fn closed_to_open_is_reopened() {
        let log = format!(
            "{SAMPLE_LOG}{}{}",
            "-{\"id\":\"X-1\",\"title\":\"T\",\"status\":\"closed\"}\n",
            "+{\"id\":\"X-1\",\"title\":\"T\",\"status\":\"open\"}\n"
        );
        let events = parse_log_output(&log, None);
        assert_eq!(events[0].event_type, EventType::Reopened);
    }

    #[test]
    fn same_status_change_is_modified() {
        let log = format!(
            "{SAMPLE_LOG}{}{}",
            "-{\"id\":\"X-1\",\"title\":\"Old title\",\"status\":\"open\"}\n",
            "+{\"id\":\"X-1\",\"title\":\"New title\",\"status\":\"open\"}\n"
        );
        let events = parse_log_output(&log, None);
        assert_eq!(events[0].event_type, EventType::Modified);
    }

    #[test]
    fn metadata_lines_ignored() {
        let log = format!(
            "{SAMPLE_LOG}{}{}{}{}",
            "diff --git a/.beads/issues.jsonl b/.beads/issues.jsonl\n",
            "index abc..def 100644\n",
            "@@ -1 +1 @@\n",
            "+{\"id\":\"Y-9\",\"title\":\"N\",\"status\":\"open\"}\n"
        );
        let events = parse_log_output(&log, None);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].bead_id, "Y-9");
    }

    #[test]
    fn filter_restricts_to_bead() {
        let log = format!(
            "{SAMPLE_LOG}{}{}",
            "+{\"id\":\"A-1\",\"title\":\"A\",\"status\":\"open\"}\n",
            "+{\"id\":\"B-2\",\"title\":\"B\",\"status\":\"open\"}\n"
        );
        let events = parse_log_output(&log, Some("B-2"));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].bead_id, "B-2");
    }

    #[test]
    fn deletion_only_produces_deleted_event() {
        // Go `parseDiff`: hadOld && !hasNew emits EventDeleted carrying the
        // before state and no after state.
        let log = format!(
            "{SAMPLE_LOG}{}",
            "-{\"id\":\"GONE\",\"title\":\"x\",\"status\":\"open\"}\n"
        );
        let events = parse_log_output(&log, None);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].bead_id, "GONE");
        assert_eq!(events[0].event_type, EventType::Deleted);
        assert!(events[0].before.is_some());
        assert!(events[0].after.is_none());
    }

    #[test]
    fn created_event_carries_after_state_with_dependencies() {
        let log = format!(
            "{SAMPLE_LOG}{}\n",
            r#"+{"id":"X-9","title":"T","status":"open","dependencies":[{"depends_on_id":"X-1","type":"blocks"}]}"#
        );
        let events = parse_log_output(&log, None);
        assert_eq!(events.len(), 1);
        assert!(events[0].before.is_none());
        let after = events[0].after.as_ref().expect("after state");
        assert_eq!(after.title, "T");
        assert_eq!(
            after.dependencies,
            vec![HistoricalDependency {
                depends_on_id: "X-1".to_string(),
                dep_type: "blocks".to_string(),
            }]
        );
    }

    /// Legacy `depends_on` / `target_id` spellings fold into `depends_on_id`
    /// (Go `Dependency.UnmarshalJSON`), and an absent `type` stays `""` —
    /// Go's `DependencyType` is a plain string, not normalized to "blocks".
    #[test]
    fn legacy_dependency_spellings_fold_and_untyped_stays_empty() {
        let snap = parse_bead_json(
            r#"{"id":"X-1","status":"open","dependencies":[
                {"depends_on":"X-2"},
                {"target_id":"X-3","type":"related"},
                {"depends_on_id":"X-4","type":""}
            ]}"#,
        )
        .expect("snapshot");
        assert_eq!(
            snap.dependencies,
            vec![
                HistoricalDependency {
                    depends_on_id: "X-2".to_string(),
                    dep_type: String::new(),
                },
                HistoricalDependency {
                    depends_on_id: "X-3".to_string(),
                    dep_type: "related".to_string(),
                },
                HistoricalDependency {
                    depends_on_id: "X-4".to_string(),
                    dep_type: String::new(),
                },
            ]
        );
    }

    #[test]
    fn after_state_without_dependencies_omits_the_key() {
        let log = format!(
            "{SAMPLE_LOG}{}\n",
            r#"+{"id":"X-8","title":"T","status":"open"}"#
        );
        let events = parse_log_output(&log, None);
        let json = serde_json::to_value(&events[0]).expect("serialize");
        let after = &json["after"];
        assert!(after.get("dependencies").is_none(), "{after}");
    }

    #[test]
    fn non_record_diff_line_does_not_break_completeness() {
        // A `-plain text` line is neither a record nor evidence of a truncated
        // parse, so the sibling record's transition stays observable.
        let log = format!(
            "{SAMPLE_LOG}{}{}{}",
            "-plain text removal\n",
            "-{\"id\":\"X-7\",\"title\":\"T\",\"status\":\"open\"}\n",
            "+{\"id\":\"X-7\",\"title\":\"T\",\"status\":\"in_progress\"}\n"
        );
        let events = parse_log_output(&log, None);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, EventType::Claimed);
        assert!(events[0].transition_observed);
    }

    #[test]
    fn mixed_event_order_is_sorted_by_bead_id() {
        let log = format!(
            "{SAMPLE_LOG}{}{}{}",
            "+{\"id\":\"B-2\",\"title\":\"B\",\"status\":\"open\"}\n",
            "+{\"id\":\"A-1\",\"title\":\"A\",\"status\":\"open\"}\n",
            "-{\"id\":\"C-3\",\"title\":\"C\",\"status\":\"open\"}\n"
        );
        let events = parse_log_output(&log, None);
        let ids: Vec<&str> = events.iter().map(|e| e.bead_id.as_str()).collect();
        assert_eq!(ids, vec!["A-1", "B-2", "C-3"]);
    }
}
