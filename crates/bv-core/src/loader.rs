//! Tolerant JSONL loader — port of Go `pkg/loader` parse semantics
//! (loader.go:584-830, recordTypeOf, normalizeLoadedIssue).
//!
//! Contract: byte-equivalent behavior with the Go serial path — same BOM
//! strip, same 10MB line cap, same `_type` dispatch, same warning texts,
//! same ParseStats accounting.

use crate::model::{Dependency, DependencyType, Issue, Status, ValidationError};
use serde::Deserialize;
use std::io::Read;

/// Go: `DefaultMaxBufferSize` = 10MB per-line cap.
pub const DEFAULT_MAX_BUFFER_SIZE: usize = 1024 * 1024 * 10;

/// Parallel-parse gates (Go: parallelParseMinBytes / parallelParseMinLines).
pub const PARALLEL_PARSE_MIN_BYTES: u64 = 4 * 1024 * 1024;
pub const PARALLEL_PARSE_MIN_LINES: usize = 512;

/// Go: `ParseStats`.
///
/// Not `Copy`: it carries the deleted ids Go keeps in `TombstoneIDs`, and a
/// caller that only wants a count should call [`ParseStats::tombstones`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ParseStats {
    pub valid: usize,
    pub errors: usize,
    pub skipped: usize,
    /// Records that parsed cleanly but describe a *deleted* issue, and so were
    /// kept out of every view. Go: `LoadResult.TombstoneIDs`
    /// (`pkg/workspace/loader.go:521`), surfaced as the envelope's
    /// `source_authority.tombstones`.
    ///
    /// The ids are kept, not just the count: Go's file-level digest folds each
    /// deleted record back in as a bare `{id, status: tombstone}` stub
    /// (`main.go:7354-7356`), so a caller that wants to describe the *file*
    /// rather than the analysed set needs to know which ones went missing.
    ///
    /// These still count towards `valid`: a tombstone is a well-formed record,
    /// not a parse failure, and Go runs the split after parsing. Only `visible`
    /// excludes them.
    pub tombstone_ids: Vec<String>,
}

impl ParseStats {
    /// Go: `ErrorRate()` — errors / (valid+errors), 0 when no accounted lines.
    pub fn error_rate(&self) -> f64 {
        let total = self.valid + self.errors;
        if total == 0 {
            0.0
        } else {
            self.errors as f64 / total as f64
        }
    }

    /// How many records were dropped as deleted issues.
    pub fn tombstones(&self) -> usize {
        self.tombstone_ids.len()
    }

    /// The records that reach analysis: everything that parsed, less the
    /// deleted ones Go routes into `TombstoneIDs`.
    pub fn visible(&self) -> usize {
        self.valid.saturating_sub(self.tombstones())
    }
}

/// What the datasource layer turns into envelope `load_stats`
/// (Go: datasource.LoadReport). Warnings capped at 10.
#[derive(Debug, Default, Clone)]
pub struct LoadReport {
    pub path: String,
    pub valid: usize,
    pub errors: usize,
    pub skipped: usize,
    pub warnings: Vec<String>,
}

pub const MAX_LOAD_REPORT_WARNINGS: usize = 10;

/// Record classification (Go: `recordTypeOf`).
enum RecordType {
    Issue,
    Skippable, // memory/sprint/forecast/burndown/ignore + unknown
}

fn record_type_of(line: &[u8]) -> RecordType {
    // Fast path: no `_type` key at all -> issue (pre-v1.0 shape).
    if !line.windows(7).any(|w| w == b"\"_type\"") {
        return RecordType::Issue;
    }
    #[derive(Deserialize)]
    struct Probe {
        #[serde(rename = "_type", default)]
        r#type: String,
    }
    match serde_json::from_slice::<Probe>(line) {
        Ok(p) => match p.r#type.as_str() {
            "" | "issue" => RecordType::Issue,
            "memory" | "sprint" | "forecast" | "burndown" | "ignore" => RecordType::Skippable,
            _ => RecordType::Skippable, // unknown types: silent skip + Skipped++
        },
        // Undecodable discriminator: fall through to issue parser so the
        // malformed-JSON warning fires at the usual site.
        Err(_) => RecordType::Issue,
    }
}

/// Intermediate issue shape: every field raw/tolerant, mirroring how Go's
/// string-typed model accepts any status/type before validation.
#[derive(Deserialize)]
struct RawIssue {
    #[serde(default)]
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    design: String,
    #[serde(default, rename = "acceptance_criteria")]
    acceptance_criteria: String,
    #[serde(default)]
    notes: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    priority: i32,
    #[serde(default, rename = "issue_type")]
    issue_type: String,
    #[serde(default)]
    assignee: String,
    #[serde(default, rename = "estimated_minutes")]
    estimated_minutes: Option<i64>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default, rename = "due_date")]
    due_date: Option<String>,
    #[serde(default, rename = "defer_until")]
    defer_until: Option<String>,
    #[serde(default, rename = "closed_at")]
    closed_at: Option<String>,
    #[serde(default, rename = "external_ref")]
    external_ref: Option<String>,
    #[serde(default, rename = "compaction_level")]
    compaction_level: i64,
    #[serde(default, rename = "compacted_at")]
    compacted_at: Option<String>,
    #[serde(default, rename = "compacted_at_commit")]
    compacted_at_commit: Option<String>,
    #[serde(default, rename = "original_size")]
    original_size: i64,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    dependencies: Vec<RawDependency>,
    #[serde(default)]
    comments: Vec<crate::model::Comment>,
    #[serde(default, rename = "source_repo")]
    source_repo: String,
}

#[derive(Deserialize)]
struct RawDependency {
    #[serde(default)]
    issue_id: String,
    #[serde(default, rename = "depends_on_id")]
    depends_on_id: String,
    #[serde(default, rename = "depends_on")]
    depends_on_legacy: String,
    #[serde(default, rename = "target_id")]
    target_id_legacy: String,
    #[serde(default)]
    r#type: String,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    created_by: String,
}

/// Options (subset used by Phase 1 serial path).
#[derive(Debug, Default, Clone)]
pub struct ParseOptions {
    /// Per-line cap in bytes; 0 -> DEFAULT_MAX_BUFFER_SIZE. Env
    /// BV_MAX_LINE_SIZE_MB maps here at the CLI layer.
    pub buffer_size: Option<usize>,
}

/// Parse JSONL from a reader. Tolerant: malformed/invalid lines are skipped
/// with warnings; recognized non-issue `_type` records silently count as
/// skipped; over-long lines are discarded with a warning.
pub fn parse_issues_with_options(
    reader: &mut impl Read,
    opts: &ParseOptions,
    mut warn: impl FnMut(&str),
) -> Result<(Vec<Issue>, ParseStats), std::io::Error> {
    let max_capacity = opts.buffer_size.unwrap_or(DEFAULT_MAX_BUFFER_SIZE);
    let mut data = Vec::new();
    reader.read_to_end(&mut data)?;

    let mut issues = Vec::new();
    let mut stats = ParseStats::default();
    let mut line_num = 0usize;
    for mut line in data.split(|&b| b == b'\n') {
        line_num += 1;
        // Trim trailing \r (CRLF files).
        if line.last() == Some(&b'\r') {
            line = &line[..line.len() - 1];
        }
        if line.is_empty() {
            continue;
        }
        if line.len() > max_capacity {
            warn(&format!(
                "skipping line {line_num}: line too long (exceeds {max_capacity} bytes)"
            ));
            // Accounted like Go: not an error, not valid — just skipped line.
            continue;
        }
        if line_num == 1 {
            line = strip_bom(line);
        }
        process_line(line, line_num, &mut issues, &mut stats, &mut warn);
    }

    // Go `pkg/workspace/loader.go:519-527` walks the freshly parsed records
    // and routes every `status: "tombstone"` into `TombstoneIDs`, keeping the
    // rest as the visible set. The split happens *after* parsing — a tombstone
    // is a valid record — so `stats.valid` above still counts them.
    //
    // Doing this here rather than at each call site is what makes it total:
    // `load_issues_from_repo`, `Repository::load_from_git` (`--as-of`) and the
    // standalone loader in `bv` all come through this function, and every one of
    // them used to hand tombstones to the analyzers. They then reached
    // `issue_count`, `--robot-history`, the sqlite export and — worst of all —
    // the `data_hash`, which the compatibility contract pins byte-for-byte.
    if issues.iter().any(|i| i.status.is_tombstone()) {
        let mut visible = Vec::with_capacity(issues.len());
        for issue in issues {
            if issue.status.is_tombstone() {
                stats.tombstone_ids.push(issue.id.clone());
            } else {
                visible.push(issue);
            }
        }
        issues = visible;
    }

    Ok((issues, stats))
}

fn strip_bom(line: &[u8]) -> &[u8] {
    if line.starts_with(&[0xEF, 0xBB, 0xBF]) {
        &line[3..]
    } else {
        line
    }
}

fn process_line(
    line: &[u8],
    line_num: usize,
    issues: &mut Vec<Issue>,
    stats: &mut ParseStats,
    warn: &mut impl FnMut(&str),
) {
    match record_type_of(line) {
        RecordType::Skippable => {
            stats.skipped += 1;
            return;
        }
        RecordType::Issue => {}
    }

    let raw: RawIssue = match serde_json::from_slice(line) {
        Ok(r) => r,
        Err(e) => {
            stats.errors += 1;
            warn(&format!("skipping malformed JSON on line {line_num}: {e}"));
            return;
        }
    };

    let parent_id = raw.id.clone();
    let deps: Vec<Dependency> = raw
        .dependencies
        .iter()
        .map(|d| d.to_owned().into_dependency_owned(&parent_id))
        .collect();

    let issue = match raw.into_issue_with_deps(deps) {
        Ok(i) => i,
        Err(e) => {
            stats.errors += 1;
            warn(&format!("skipping invalid issue on line {line_num}: {e}"));
            return;
        }
    };
    stats.valid += 1;
    issues.push(issue);
}

// Small helper trait shim to keep RawDependency conversion ergonomic.
trait IntoDependencyOwned {
    fn into_dependency_owned(self, parent_id: &str) -> Dependency;
}
impl IntoDependencyOwned for &RawDependency {
    fn into_dependency_owned(self, parent_id: &str) -> Dependency {
        let issue_id = if self.issue_id.is_empty() {
            parent_id.to_string()
        } else {
            self.issue_id.clone()
        };
        Dependency {
            issue_id,
            depends_on_id: self.depends_on_id.clone(),
            depends_on_legacy: self.depends_on_legacy.clone(),
            target_id_legacy: self.target_id_legacy.clone(),
            r#type: DependencyType::parse(&self.r#type),
            created_at: self.created_at.clone(),
            created_by: self.created_by.clone(),
        }
    }
}

impl RawIssue {
    fn into_issue_with_deps(self, deps: Vec<Dependency>) -> Result<Issue, ValidationError> {
        let status_trimmed = self.status.trim().to_lowercase();
        let status = match status_trimmed.as_str() {
            "open" => Status::Open,
            "in_progress" => Status::InProgress,
            "blocked" => Status::Blocked,
            "deferred" => Status::Deferred,
            "draft" => Status::Draft,
            "pinned" => Status::Pinned,
            "hooked" => Status::Hooked,
            "review" => Status::Review,
            "closed" => Status::Closed,
            "tombstone" => Status::Tombstone,
            other => return Err(ValidationError::InvalidStatus(other.to_string())),
        };
        if self.id.is_empty() {
            return Err(ValidationError::MissingField("id"));
        }
        if self.title.is_empty() {
            return Err(ValidationError::MissingField("title"));
        }
        // Go's loader validates no dependency type: `pkg/loader/loader.go:1859`
        // only interns the string. Rejecting a record here would drop the whole
        // bead whenever br wrote a type this build does not recognise — and
        // `waits-for` and `conditional-blocks` are real Go blocking constants,
        // not typos. `DependencyType::is_blocking` is where a type earns the
        // right to gate readiness.
        if let (Some(c), Some(u)) = (&self.created_at, &self.updated_at) {
            if let (Ok(c), Ok(u)) = (c.parse::<jiff::Timestamp>(), u.parse::<jiff::Timestamp>()) {
                if u < c {
                    return Err(ValidationError::InvertedTimestamps);
                }
            }
        }
        Ok(Issue {
            id: self.id,
            content_hash: String::new(),
            title: self.title,
            description: self.description,
            design: self.design,
            acceptance_criteria: self.acceptance_criteria,
            notes: self.notes,
            status,
            priority: self.priority,
            issue_type: self.issue_type,
            assignee: self.assignee,
            estimated_minutes: self.estimated_minutes,
            created_at: self.created_at,
            updated_at: self.updated_at,
            due_date: self.due_date,
            defer_until: self.defer_until,
            closed_at: self.closed_at,
            external_ref: self.external_ref,
            compaction_level: self.compaction_level,
            compacted_at: self.compacted_at,
            compacted_at_commit: self.compacted_at_commit,
            original_size: self.original_size,
            labels: self.labels,
            dependencies: deps,
            comments: self.comments,
            source_repo: self.source_repo,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &str) -> (Vec<Issue>, ParseStats, Vec<String>) {
        let mut warnings = Vec::new();
        let mut rdr = input.as_bytes();
        let (issues, stats) = parse_issues_with_options(&mut rdr, &ParseOptions::default(), |w| {
            warnings.push(w.to_string())
        })
        .unwrap();
        (issues, stats, warnings)
    }

    #[test]
    fn parses_basic_issue() {
        let (issues, stats, _) = parse(
            r#"{"id":"A-1","title":"T","status":"open","priority":2,"issue_type":"task","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}"#,
        );
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].id, "A-1");
        assert_eq!(stats.valid, 1);
        assert_eq!(stats.errors, 0);
    }

    #[test]
    fn strips_bom_on_first_line() {
        let bom = "\u{feff}";
        let (issues, stats, _) = parse(
            format!(
                "{}{{\"id\":\"A-1\",\"title\":\"T\",\"status\":\"open\",\"issue_type\":\"task\"}}",
                bom
            )
            .as_str(),
        );
        assert_eq!(issues.len(), 1);
        assert_eq!(stats.valid, 1);
    }

    #[test]
    fn skips_malformed_lines_with_warning_and_error_count() {
        let (issues, stats, warnings) = parse(
            "{not json}\n{\"id\":\"A-1\",\"title\":\"T\",\"status\":\"open\",\"issue_type\":\"task\"}",
        );
        assert_eq!(issues.len(), 1);
        assert_eq!(stats.errors, 1);
        assert_eq!(stats.valid, 1);
        assert!(warnings[0].starts_with("skipping malformed JSON on line 1"));
    }

    #[test]
    fn skips_inverted_timestamps_as_invalid() {
        let (issues, stats, warnings) = parse(
            r#"{"id":"A-1","title":"T","status":"open","issue_type":"task","created_at":"2026-01-05T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}"#,
        );
        assert_eq!(issues.len(), 0);
        assert_eq!(stats.errors, 1);
        assert!(warnings[0].starts_with("skipping invalid issue on line 1"));
    }

    #[test]
    fn type_records_silently_skipped_counted() {
        let mem = r#"{"_type":"memory","content":"x"}"#;
        let sprint = r#"{"_type":"sprint","name":"s1"}"#;
        let unknown = r#"{"_type":"wat","x":1}"#;
        let issue = r#"{"id":"A-1","title":"T","status":"open","issue_type":"task"}"#;
        let (issues, stats, warnings) = parse(&format!("{mem}\n{sprint}\n{unknown}\n{issue}"));
        assert_eq!(issues.len(), 1);
        assert_eq!(stats.skipped, 3);
        assert_eq!(stats.errors, 0);
        assert!(warnings.is_empty());
    }

    #[test]
    fn normalizes_status_case_and_backfills_dep_issue_id() {
        let line = r#"{"id":"A-1","title":"T","status":" OPEN ","issue_type":"task","dependencies":[{"depends_on_id":"B-2","created_by":"x"}]}"#;
        let (issues, _, _) = parse(line);
        assert!(matches!(issues[0].status, Status::Open));
        assert_eq!(issues[0].dependencies[0].issue_id, "A-1");
        assert_eq!(issues[0].dependencies[0].effective_depends_on(), "B-2");
    }

    #[test]
    fn overlong_line_skipped_with_warning() {
        let big = format!(
            "{{\"id\":\"{}\",\"title\":\"T\",\"status\":\"open\",\"issue_type\":\"task\"}}",
            "X".repeat(300)
        );
        // cap below the big-line size
        let mut warnings = Vec::new();
        let opts = ParseOptions {
            buffer_size: Some(128),
        };
        let mut rdr = big.as_bytes();
        let (issues, stats) =
            parse_issues_with_options(&mut rdr, &opts, |w| warnings.push(w.to_string())).unwrap();
        assert_eq!(issues.len(), 0);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("line too long"));
        assert_eq!(stats.valid, 0);
    }

    #[test]
    fn differential_small_chain_loads_12() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/small_chain/.beads/issues.jsonl"
        );
        let raw = std::fs::read_to_string(path).unwrap();
        let (issues, stats, _) = parse(&raw);
        assert_eq!(issues.len(), 12);
        assert_eq!(
            stats,
            ParseStats {
                valid: 12,
                errors: 0,
                skipped: 0,
                tombstone_ids: Vec::new()
            }
        );
    }
}
