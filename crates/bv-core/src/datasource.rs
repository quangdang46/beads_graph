//! Smart multi-source datasource — port of Go `internal/datasource`
//! (v0.25.0, commit 18afafa): `source.go` discovery, `select.go`
//! freshness ordering, `load.go` fused validate-and-load with fallback,
//! `sqlite.go`/`jsonl.go` readers, and the `loader.FindJSONLPath` /
//! `PrepareBeadsDirForRead` helpers they call.
//!
//! Go's `LoadIssues(repoPath)` contract this mirrors:
//! 1. `BEADS_DB` naming a concrete file wins outright (`ExplicitBeadsDBSource`).
//! 2. `GetBeadsDir` resolves BEADS_DB-dir / BEADS_DIR / `.beads` / worktree
//!    main-repo, honouring `.beads/redirect` chains (already in
//!    [`crate::discovery::get_beads_dir`]).
//! 3. bd/Dolt workspaces route through `bd export` ([`load_bd_workspace`]).
//! 4. `loadSmart` discovers `beads.db` + the preferred JSONL + worktree
//!    JSONLs, orders by ModTime desc / priority desc, and tries each until
//!    one validates-and-loads; rejected candidates are remembered as
//!    authority warnings on the winner.
//! 5. When every candidate fails, `loadLegacyJSONL` re-reads the preferred
//!    JSONL tolerantly (no error-rate gate) and the result is marked stale
//!    with `"source selection failed before fallback: …"`.
//!
//! `.beads/metadata.json`'s `database`/`jsonl_export` declarations are NOT
//! consulted here — Go only honours them in `loadIssuesFromBeadsDir` /
//! `countIssuesInBeadsDir` (cmd/bv/main.go:6341,6416), the pages auto-source
//! path, ported below as [`load_issues_from_beads_dir`]. Issue-origin binding
//! reads the same file via [`crate::tracker::resolve_issue_origin`].

use crate::discovery::{
    get_beads_dir, go_getwd, go_path_error, resolve_beads_dir_pub, BEADS_DB_ENV,
};
use crate::loader::{parse_issues_with_options, ParseOptions, ParseStats};
use crate::model::Issue;
use crate::sqlite::{validate_source, SqliteReader};
use crate::tracker::is_bd_workspace;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

// ---------------------------------------------------------------------------
// Types (Go: internal/datasource/source.go, load.go)
// ---------------------------------------------------------------------------

/// Go `SourceType` string values, kept as the wire strings because
/// `source_kind` lands verbatim in robot JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// Go's zero-value `SourceType` (`""`) — only ever appears on an empty
    /// `LoadResult`, where the authority maps it to `"unknown"`.
    Unknown,
    /// `"sqlite"` — priority 100.
    Sqlite,
    /// `"jsonl_worktree"` — priority 80.
    JsonlWorktree,
    /// `"jsonl_local"` — priority 50.
    JsonlLocal,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::Unknown => "",
            SourceKind::Sqlite => "sqlite",
            SourceKind::JsonlWorktree => "jsonl_worktree",
            SourceKind::JsonlLocal => "jsonl_local",
        }
    }

    fn priority(self) -> i64 {
        match self {
            SourceKind::Unknown => 0,
            SourceKind::Sqlite => 100,
            SourceKind::JsonlWorktree => 80,
            SourceKind::JsonlLocal => 50,
        }
    }
}

/// Go `DataSource`.
#[derive(Debug, Clone)]
pub struct DataSource {
    pub kind: SourceKind,
    pub path: PathBuf,
    pub mod_time: Option<SystemTime>,
    pub size: u64,
}

impl DataSource {
    fn priority(&self) -> i64 {
        self.kind.priority()
    }

    /// Go `sortByFreshnessThenPriority`: ModTime desc, ties by priority desc.
    /// Go's `sort.Slice` is unstable; sources that tie on BOTH keys are
    /// interchangeable for selection, so a stable sort is equivalent here.
    fn freshness_cmp(&self, other: &DataSource) -> std::cmp::Ordering {
        match (self.mod_time, other.mod_time) {
            (Some(a), Some(b)) if a != b => b.cmp(&a), // newest first
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            _ => other.priority().cmp(&self.priority()),
        }
    }
}

/// Go `LoadReport` (internal/datasource/load.go:25-46).
#[derive(Debug, Default, Clone)]
pub struct LoadReport {
    pub path: String,
    /// Unique issue lines that parsed and validated.
    pub valid: usize,
    /// Malformed/failed-validation/duplicate records dropped.
    pub errors: usize,
    /// Recognized non-issue `_type` records.
    pub skipped: usize,
    /// Up to 10 skip reasons, in file order.
    pub warnings: Vec<String>,
    pub warning_count: usize,
    /// Failed related-record reads (SQLite labels/deps/comments, bad times).
    pub read_errors: usize,
    /// Rejected-candidate / preferred-source-failure reasons.
    pub authority_warnings: Vec<String>,
    pub stale: bool,
    /// Tombstoned ids from the same successful read.
    pub tombstone_ids: Vec<String>,
}

/// Go `LoadResult`.
#[derive(Debug)]
pub struct LoadResult {
    /// Visible issues (tombstones removed).
    pub issues: Vec<Issue>,
    pub source: DataSource,
    pub report: LoadReport,
}

impl Default for LoadResult {
    fn default() -> Self {
        LoadResult {
            issues: Vec::new(),
            source: DataSource {
                kind: SourceKind::Unknown,
                path: PathBuf::new(),
                mod_time: None,
                size: 0,
            },
            report: LoadReport::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Go env.Robot.Bool() — BV_ROBOT truthy: "1","true","yes","on".
// ---------------------------------------------------------------------------

fn env_robot() -> bool {
    matches!(
        std::env::var("BV_ROBOT")
            .map(|v| v.trim().to_lowercase())
            .as_deref(),
        Ok("1") | Ok("true") | Ok("yes") | Ok("on")
    )
}

/// Go `loader.MaxLineSizeFromEnv` -> bytes; 0 means "use the default".
fn max_line_size_from_env() -> usize {
    let raw = std::env::var("BV_MAX_LINE_SIZE_MB").unwrap_or_default();
    let raw = raw.trim();
    if raw.is_empty() {
        return 0;
    }
    let mb: i64 = match raw.parse() {
        Ok(v) => v,
        Err(_) => return 0,
    };
    if mb <= 0 {
        return 0;
    }
    let mb = mb.min(1024);
    (mb * 1024 * 1024) as usize
}

// ---------------------------------------------------------------------------
// loadRecorder — buffers per-line warnings; only the selected source replays
// them to stderr, and only outside robot mode (load.go:75-128).
// ---------------------------------------------------------------------------

struct LoadRecorder {
    path: String,
    stats: ParseStats,
    warnings: Vec<String>, // capped at 10 for the report
    warning_count: usize,
    pending: Vec<String>, // every warning, replayed only when selected
    robot: bool,
}

impl LoadRecorder {
    fn new(path: &Path) -> Self {
        LoadRecorder {
            path: path.to_string_lossy().into_owned(),
            stats: ParseStats::default(),
            warnings: Vec::new(),
            warning_count: 0,
            pending: Vec::new(),
            robot: env_robot(),
        }
    }

    fn warn(&mut self, msg: &str) {
        self.warning_count += 1;
        if self.warnings.len() < 10 {
            self.warnings.push(msg.to_string());
        }
        if !self.robot {
            self.pending.push(msg.to_string());
        }
    }

    /// Go `finish(issues, selected)`: replay pending warnings, build report.
    /// `tombstone_ids` comes from the parse stats — `parse_issues_with_options`
    /// already performs Go's post-parse tombstone split.
    fn finish(self, selected: bool) -> LoadReport {
        if selected {
            for msg in &self.pending {
                eprintln!("Warning: {msg}");
            }
        }
        LoadReport {
            path: self.path,
            valid: self.stats.valid,
            errors: self.stats.errors,
            skipped: self.stats.skipped,
            warnings: self.warnings,
            warning_count: self.warning_count,
            tombstone_ids: self.stats.tombstone_ids.clone(),
            ..Default::default()
        }
    }
}

// ---------------------------------------------------------------------------
// JSONL file open — Go `openIssuesFile` (loader.go:884-912), minus the
// sameFileSnapshot retry loop (a TOCTOU guard with no observable contract).
// ---------------------------------------------------------------------------

fn open_issues_file(path: &Path) -> Result<std::fs::File, String> {
    let p = path.to_string_lossy();
    let info = match std::fs::metadata(path) {
        Ok(i) => i,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("no beads issues found at {p}"));
        }
        Err(e) => {
            return Err(format!(
                "failed to inspect issues file {p}: {}",
                go_path_error("stat", &p, &e)
            ));
        }
    };
    if !info.is_file() {
        return Err(format!("issues path is not a regular file: {p}"));
    }
    std::fs::File::open(path).map_err(|e| {
        format!(
            "failed to open issues file: {}",
            go_path_error("open", &p, &e)
        )
    })
}

/// One JSONL read: recorder (with buffered warnings + stats), the visible
/// issues, and either a Go-shaped error string or nothing.
struct JsonlRead {
    rec: LoadRecorder,
    issues: Vec<Issue>,
    stats: ParseStats,
    error: Option<String>,
}

/// Go `loader.LoadIssuesFromFileWithOptions` minus tombstone re-filtering —
/// `parse_issues_with_options` already performs Go's post-parse tombstone
/// split, so `issues` here IS the visible set and `stats.tombstone_ids`
/// carries the hidden ids, exactly what `LoadReport.TombstoneIDs` wants.
fn read_jsonl_file(path: &Path) -> JsonlRead {
    let mut rec = LoadRecorder::new(path);
    let file = match open_issues_file(path) {
        Ok(f) => f,
        Err(e) => {
            return JsonlRead {
                rec,
                issues: Vec::new(),
                stats: ParseStats::default(),
                error: Some(e),
            };
        }
    };
    let mut reader = std::io::BufReader::new(file);
    let n = max_line_size_from_env();
    let opts = ParseOptions {
        buffer_size: if n == 0 { None } else { Some(n) },
    };
    // Warnings are collected during the parse (Go's WarningHandler fires
    // mid-stream) and folded into the recorder afterwards — order preserved.
    let mut collected: Vec<String> = Vec::new();
    let parsed =
        parse_issues_with_options(&mut reader, &opts, |msg| collected.push(msg.to_string()));
    for msg in &collected {
        rec.warn(msg);
    }
    match parsed {
        Ok((issues, stats)) => {
            rec.stats = stats.clone();
            JsonlRead {
                rec,
                issues,
                stats,
                error: None,
            }
        }
        Err(e) => JsonlRead {
            rec,
            issues: Vec::new(),
            stats: ParseStats::default(),
            error: Some(e.to_string()),
        },
    }
}

// ---------------------------------------------------------------------------
// Explicit BEADS_DB (load.go:264-315)
// ---------------------------------------------------------------------------

/// Go `explicitBeadsDBFileType`: extension -> (kind, ok).
fn explicit_beads_db_file_type(path: &str) -> Option<SourceKind> {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "db" | "sqlite" | "sqlite3" => Some(SourceKind::Sqlite),
        "jsonl" => Some(SourceKind::JsonlLocal),
        _ => None,
    }
}

/// Go `SourceFromFile` — `(source, ok, err)`.
fn source_from_file(db_path: &str) -> Result<(Option<DataSource>, bool), String> {
    if db_path.trim().is_empty() {
        return Ok((None, false));
    }
    let p = Path::new(db_path);
    let stat = std::fs::metadata(p).ok();
    if let Some(info) = &stat {
        if info.is_dir() {
            return Ok((None, false));
        }
    }
    // os.Stat error other than NotExist -> hard error.
    if stat.is_none() {
        match std::fs::metadata(p) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(format!(
                    "stat {db_path}: {}",
                    go_path_error("stat", db_path, &e)
                ));
            }
            _ => {}
        }
    }
    let Some(kind) = explicit_beads_db_file_type(db_path) else {
        if stat.is_some() {
            return Err(format!("unsupported BEADS_DB file type: {db_path}"));
        }
        return Ok((None, false));
    };
    Ok((
        Some(DataSource {
            kind,
            path: p.to_path_buf(),
            mod_time: stat.as_ref().and_then(|m| m.modified().ok()),
            size: stat.as_ref().map(|m| m.len()).unwrap_or(0),
        }),
        true,
    ))
}

/// Go `ExplicitBeadsDBSource`.
fn explicit_beads_db_source() -> Result<Option<DataSource>, String> {
    let env_db = std::env::var(BEADS_DB_ENV).unwrap_or_default();
    let (source, _ok) = source_from_file(&env_db)?;
    Ok(source)
}

// ---------------------------------------------------------------------------
// Discovery (source.go:93-396)
// ---------------------------------------------------------------------------

/// Go `DiscoverSources` with `ValidateAfterDiscovery: false`. `warn` receives
/// discovery warnings (merge artifacts) only in non-robot mode — Go passes a
/// nil WarningHandler under robot, and the fused loader never prints
/// rejected-candidate noise.
fn discover_sources(
    beads_dir: &Path,
    repo_path: &Path,
    warn: &mut dyn FnMut(&str),
) -> Vec<DataSource> {
    let mut sources = Vec::new();

    // SQLite: <beadsDir>/beads.db
    let db_path = beads_dir.join("beads.db");
    if let Ok(info) = std::fs::metadata(&db_path) {
        sources.push(DataSource {
            kind: SourceKind::Sqlite,
            path: db_path,
            mod_time: info.modified().ok(),
            size: info.len(),
        });
    }

    // Local JSONL: only the single preferred export is a candidate
    // (source.go:244-296). A failed preferred-path resolution yields zero
    // JSONL candidates but does NOT fail discovery — sqlite/worktree
    // candidates still compete.
    if let Ok(preferred) = find_jsonl_path_go(beads_dir, warn) {
        if preferred.is_file() {
            if let Ok(info) = std::fs::metadata(&preferred) {
                sources.push(DataSource {
                    kind: SourceKind::JsonlLocal,
                    path: preferred,
                    mod_time: info.modified().ok(),
                    size: info.len(),
                });
            }
        }
    }

    // Worktree JSONLs: <git-dir>/beads-worktrees/*/issues.jsonl
    sources.extend(discover_worktree_sources(repo_path, warn));

    sources
}

/// Go `discoverWorktreeSources` (source.go:298-396).
fn discover_worktree_sources(repo_path: &Path, _warn: &mut dyn FnMut(&str)) -> Vec<DataSource> {
    let repo = if repo_path.as_os_str().is_empty() {
        go_getwd()
    } else {
        repo_path.to_path_buf()
    };
    let out = match Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(&repo)
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(), // not a git repository
    };
    let mut git_dir = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string());
    if !git_dir.is_absolute() {
        git_dir = repo.join(git_dir);
    }
    let worktrees_dir = git_dir.join("beads-worktrees");
    let entries = match std::fs::read_dir(&worktrees_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut names: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.path())
        .collect();
    names.sort();
    let mut sources = Vec::new();
    for wt in names {
        let jsonl = wt.join("issues.jsonl");
        if let Ok(info) = std::fs::metadata(&jsonl) {
            sources.push(DataSource {
                kind: SourceKind::JsonlWorktree,
                path: jsonl,
                mod_time: info.modified().ok(),
                size: info.len(),
            });
        }
    }
    sources
}

// ---------------------------------------------------------------------------
// FindJSONLPath — Go pkg/loader/loader.go:679-768, verbatim semantics:
// preferred-name order (NOT size), empty files accepted, merge-artifact
// warning, bd-workspace hard error.
// ---------------------------------------------------------------------------

/// Go `FindJSONLPathWithWarnings`. Returns the chosen path or a Go-shaped
/// error string.
pub fn find_jsonl_path_go(beads_dir: &Path, warn: &mut dyn FnMut(&str)) -> Result<PathBuf, String> {
    let dir = beads_dir.to_string_lossy().into_owned();
    let entries = match std::fs::read_dir(beads_dir) {
        Ok(e) => e,
        Err(e) => {
            return Err(format!(
                "failed to read beads directory: {}",
                go_path_error("open", &dir, &e)
            ));
        }
    };
    // os.ReadDir sorts by filename.
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());

    let mut candidates: Vec<String> = Vec::new();
    let mut merge_artifacts: Vec<String> = Vec::new();
    for e in entries {
        if e.file_type().map(|t| t.is_dir()).unwrap_or(true) {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".jsonl") {
            continue;
        }
        if name.contains(".backup")
            || name.contains(".orig")
            || name.contains(".merge")
            || name == "deletions.jsonl"
        {
            continue;
        }
        if name.starts_with("beads.left") || name.starts_with("beads.right") {
            merge_artifacts.push(name);
            continue;
        }
        candidates.push(name);
    }
    if !merge_artifacts.is_empty() {
        warn(&format!(
            "Merge artifact files detected: {}. Clean them up before relying on the JSONL view.",
            merge_artifacts.join(", ")
        ));
    }
    if candidates.is_empty() {
        return Err(format!("no beads JSONL file found in {dir}"));
    }

    let is_bd = is_bd_workspace(beads_dir);
    // Go's preferred list is identical for bd and non-bd workspaces today.
    let preferred: [&str; 3] = ["issues.jsonl", "beads.jsonl", "beads.base.jsonl"];
    for pref in preferred {
        for name in &candidates {
            if name == pref {
                let path = beads_dir.join(name);
                let p = path.to_string_lossy().into_owned();
                let info = match std::fs::metadata(&path) {
                    Ok(i) => i,
                    Err(e) => {
                        return Err(format!(
                            "inspect beads JSONL file {p}: {}",
                            go_path_error("stat", &p, &e)
                        ));
                    }
                };
                // Empty exports represent empty projects — accepted.
                if info.is_file() {
                    return Ok(path);
                }
            }
        }
    }
    if is_bd {
        let issues = beads_dir.join("issues.jsonl");
        return Err(format!(
            "no compatibility JSONL found at {}; run 'bd export -o .beads/issues.jsonl'",
            issues.display()
        ));
    }
    Err(format!(
        "no beads JSONL file found in {dir} (expected {}; use an explicit file override for other names)",
        preferred.join(", ")
    ))
}

// ---------------------------------------------------------------------------
// Readers (load.go:386-467)
// ---------------------------------------------------------------------------

/// Go `loadAndValidateJSONL` (load.go:410-454): fused parse, error-rate gate,
/// and zero-valid gate. `report` always describes THIS candidate's read; only
/// the selected candidate replays buffered warnings to stderr.
fn load_and_validate_jsonl(source: &DataSource) -> (LoadResult, Option<String>) {
    let read = read_jsonl_file(&source.path);
    let JsonlRead {
        rec,
        issues,
        stats,
        error,
    } = read;
    if let Some(e) = error {
        // Go: `rec.finish(all, false)` then return err — issues nil.
        let report = rec.finish(false);
        return (
            LoadResult {
                issues: Vec::new(),
                source: source.clone(),
                report,
            },
            Some(e),
        );
    }

    // Error-rate gate (validate.go: MaxJSONLErrorRate = 0.10).
    let rate = stats.error_rate();
    if rate > 0.10 {
        let report = rec.finish(false);
        let msg = format!(
            "{}: too many errors: {:.1}% (max {:.1}%)",
            source.path.display(),
            rate * 100.0,
            10.0
        );
        return (
            LoadResult {
                issues: Vec::new(),
                source: source.clone(),
                report,
            },
            Some(msg),
        );
    }
    // Zero-valid gate: records existed but none were issues.
    if stats.valid == 0 && stats.errors + stats.skipped > 0 {
        let report = rec.finish(false);
        let msg = format!(
            "{}: no issue records ({} non-issue/error lines, 0 valid issues)",
            source.path.display(),
            stats.errors + stats.skipped
        );
        return (
            LoadResult {
                issues: Vec::new(),
                source: source.clone(),
                report,
            },
            Some(msg),
        );
    }

    // Selected: finish(true) replays the buffered warnings to stderr.
    let report = rec.finish(true);
    (
        LoadResult {
            issues,
            source: source.clone(),
            report,
        },
        None,
    )
}

/// Go `loadAndValidate` dispatch (load.go:386-403).
fn load_and_validate(source: &DataSource) -> (LoadResult, Option<String>) {
    match source.kind {
        SourceKind::Sqlite => {
            if let Err(e) = validate_source(&source.path.to_string_lossy()) {
                return (
                    LoadResult {
                        issues: Vec::new(),
                        source: source.clone(),
                        report: LoadReport {
                            path: source.path.to_string_lossy().into_owned(),
                            ..Default::default()
                        },
                    },
                    Some(e),
                );
            }
            load_from_source(source)
        }
        SourceKind::JsonlLocal | SourceKind::JsonlWorktree => load_and_validate_jsonl(source),
        // Go `loadAndValidate` default arm (load.go:400-402).
        SourceKind::Unknown => (
            LoadResult {
                issues: Vec::new(),
                source: source.clone(),
                report: LoadReport {
                    path: source.path.to_string_lossy().into_owned(),
                    ..Default::default()
                },
            },
            Some("unsupported source type: ".to_string()),
        ),
    }
}

/// Go `LoadFromSource` (load.go:458-467) + `NewReader` dispatch — used for
/// explicit `BEADS_DB` sources where NO validation gate applies.
fn load_from_source(source: &DataSource) -> (LoadResult, Option<String>) {
    match source.kind {
        SourceKind::Sqlite => {
            let path = source.path.to_string_lossy().into_owned();
            let mut reader = match SqliteReader::open(&path) {
                Ok(r) => r,
                Err(e) => {
                    return (
                        LoadResult {
                            issues: Vec::new(),
                            source: source.clone(),
                            report: LoadReport {
                                path: path.clone(),
                                ..Default::default()
                            },
                        },
                        Some(format!("failed to open source {path}: {e}")),
                    );
                }
            };
            match reader.load_issues() {
                Ok(load) => (
                    LoadResult {
                        issues: load.issues,
                        source: source.clone(),
                        report: sqlite_report_to_load(&load.report),
                    },
                    None,
                ),
                Err(e) => {
                    let report = sqlite_report_to_load(&reader_report_fallback(&mut reader));
                    (
                        LoadResult {
                            issues: Vec::new(),
                            source: source.clone(),
                            report,
                        },
                        Some(e),
                    )
                }
            }
        }
        SourceKind::JsonlLocal | SourceKind::JsonlWorktree => {
            // JSONLReader.LoadIssues: tolerant parse, tombstone filter,
            // report — no error-rate gate (explicit source is authoritative).
            match parse_jsonl_reader(source) {
                Ok((issues, report)) => (
                    LoadResult {
                        issues,
                        source: source.clone(),
                        report,
                    },
                    None,
                ),
                Err(pair) => {
                    let (report, e) = *pair;
                    (
                        LoadResult {
                            issues: Vec::new(),
                            source: source.clone(),
                            report,
                        },
                        Some(e),
                    )
                }
            }
        }
        // Go `NewReader` default arm (reader.go:44-46).
        SourceKind::Unknown => (
            LoadResult {
                issues: Vec::new(),
                source: source.clone(),
                report: LoadReport {
                    path: source.path.to_string_lossy().into_owned(),
                    ..Default::default()
                },
            },
            Some("unsupported source type: ".to_string()),
        ),
    }
}

/// `parse_jsonl_reader` outcome: the parsed set plus its report on success,
/// or the failed candidate's report plus the Go-shaped error string.
type JsonlReaderOutcome = Result<(Vec<Issue>, LoadReport), Box<(LoadReport, String)>>;

/// JSONLReader.LoadIssues equivalent: parse once, `finish(selected = ok)`,
/// tombstones already split by the parser. No error-rate gate — an explicit
/// `BEADS_DB`/`--db` source is authoritative.
fn parse_jsonl_reader(source: &DataSource) -> JsonlReaderOutcome {
    let read = read_jsonl_file(&source.path);
    let JsonlRead {
        rec, issues, error, ..
    } = read;
    match error {
        Some(e) => {
            let report = rec.finish(false);
            Err(Box::new((report, e)))
        }
        None => {
            let report = rec.finish(true);
            Ok((issues, report))
        }
    }
}

fn sqlite_report_to_load(r: &crate::sqlite::SqliteReport) -> LoadReport {
    LoadReport {
        path: r.path.clone(),
        valid: r.valid,
        errors: r.errors,
        skipped: 0,
        warnings: r.warnings.clone(),
        warning_count: r.warning_count,
        read_errors: r.read_errors,
        tombstone_ids: r.tombstone_ids.clone(),
        ..Default::default()
    }
}

/// The reader stores its report even when `load_issues` errors (Go sets
/// `r.report` before returning the error). Expose the last report.
fn reader_report_fallback(reader: &mut SqliteReader) -> crate::sqlite::SqliteReport {
    reader.last_report().clone()
}

// ---------------------------------------------------------------------------
// loadSmart / LoadIssues / LoadIssuesFromDir (load.go:148-379)
// ---------------------------------------------------------------------------

/// Result of `loadSmart`: the winning/last `LoadResult` plus the smart-path
/// error (empty on success) — Go returns `(LoadResult, error)` where the
/// result is meaningful even on failure (last rejected candidate).
pub struct SmartOutcome {
    pub result: LoadResult,
    pub error: Option<String>,
}

/// Go `loadSmart` (load.go:329-379).
fn load_smart(beads_dir: &Path, repo_path: &Path, warn: &mut dyn FnMut(&str)) -> SmartOutcome {
    let mut sources = discover_sources(beads_dir, repo_path, warn);
    if sources.is_empty() {
        return SmartOutcome {
            result: LoadResult::default(),
            error: Some("no valid sources discovered".to_string()),
        };
    }
    sources.sort_by(|a, b| a.freshness_cmp(b));

    let mut last_err: Option<String> = None;
    let mut last_result = LoadResult::default();
    let mut rejected: Vec<String> = Vec::new();
    for source in &sources {
        let (mut result, err) = load_and_validate(source);
        if let Some(e) = err {
            last_err = Some(e.clone());
            last_result = LoadResult {
                issues: result.issues,
                source: result.source,
                report: result.report,
            };
            if rejected.len() < 10 {
                rejected.push(format!("rejected source {}: {e}", source.path.display()));
            }
            continue;
        }
        result.report.authority_warnings = rejected;
        result.report.stale = !result.report.authority_warnings.is_empty();
        return SmartOutcome {
            result,
            error: None,
        };
    }
    if let Some(e) = last_err {
        last_result.report.authority_warnings = rejected;
        return SmartOutcome {
            result: last_result,
            error: Some(format!("no valid sources discovered: {e}")),
        };
    }
    SmartOutcome {
        result: LoadResult::default(),
        error: Some("no valid sources discovered".to_string()),
    }
}

/// Go `loadLegacyJSONL` (load.go:186-208): tolerant parse, NO gates.
fn load_legacy_jsonl(beads_dir: &Path) -> (LoadResult, Option<String>) {
    let mut silent = |_: &str| {};
    let jsonl_path = match find_jsonl_path_go(beads_dir, &mut silent) {
        Ok(p) => p,
        Err(e) => {
            return (LoadResult::default(), Some(e));
        }
    };
    let source = DataSource {
        kind: SourceKind::JsonlLocal,
        path: jsonl_path.clone(),
        mod_time: std::fs::metadata(&jsonl_path)
            .ok()
            .and_then(|m| m.modified().ok()),
        size: std::fs::metadata(&jsonl_path).map(|m| m.len()).unwrap_or(0),
    };
    // loadLegacyJSONL uses the recorder with finish(issues, err==nil) — the
    // same shape as JSONLReader.LoadIssues.
    let (result, err) = load_from_source(&source);
    (result, err)
}

/// Shared tail of `LoadIssues`/`LoadIssuesFromDir` (load.go:169-179,220-231):
/// smart first, legacy fallback with stale marking.
fn load_with_fallback(
    beads_dir: &Path,
    repo_path: &Path,
    warn: &mut dyn FnMut(&str),
) -> (LoadResult, Option<String>) {
    let smart = load_smart(beads_dir, repo_path, warn);
    if smart.error.is_none() {
        return (smart.result, None);
    }
    let smart_err = smart.error.clone().unwrap();
    let (mut fallback, err) = load_legacy_jsonl(beads_dir);
    if fallback.source.path != smart.result.source.path
        && !smart.result.source.path.as_os_str().is_empty()
    {
        fallback.report.stale = true;
        fallback.report.authority_warnings.push(format!(
            "source selection failed before fallback: {smart_err}"
        ));
    }
    (fallback, err)
}

/// Go `LoadIssues(repoPath)` (load.go:148-179). Returns `(result, error)`
/// exactly like Go: the `LoadResult` is meaningful even on failure (it is the
/// last rejected candidate or the empty legacy result), and callers build the
/// failed source report from it.
pub fn load_issues(repo_path: &Path) -> (LoadResult, Option<String>) {
    match explicit_beads_db_source() {
        Err(e) => return (LoadResult::default(), Some(e)),
        Ok(Some(source)) => return load_from_source(&source),
        Ok(None) => {}
    }
    let beads_dir = match get_beads_dir(repo_path) {
        Ok(d) => d,
        Err(e) => return (LoadResult::default(), Some(e.to_string())),
    };
    if is_bd_workspace(&beads_dir) {
        return load_bd_workspace(&beads_dir, repo_path);
    }
    load_with_fallback(&beads_dir, repo_path, &mut |m| {
        if !env_robot() {
            eprintln!("Warning: {m}");
        }
    })
}

/// Go `LoadIssuesFromDir(beadsDir)` (load.go:213-232).
pub fn load_issues_from_dir(beads_dir: &Path) -> (LoadResult, Option<String>) {
    let beads_dir = match resolve_beads_dir_pub(beads_dir) {
        Ok(d) => d,
        Err(e) => return (LoadResult::default(), Some(e.to_string())),
    };
    load_issues_from_dir_inner(&beads_dir)
}

// ---------------------------------------------------------------------------
// bd/Dolt workspace (load.go:241-262 + loader.PrepareBeadsDirForRead)
// ---------------------------------------------------------------------------

/// Go `loadBDWorkspace` (load.go:241-262). Returns the `LoadResult` even on
/// failure — Go's `LoadResult{Report: LoadReport{Path: beadsDir,
/// AuthorityWarnings: …}}` still describes the attempted source.
fn load_bd_workspace(beads_dir: &Path, _repo_path: &Path) -> (LoadResult, Option<String>) {
    let mut authority_warnings: Vec<String> = Vec::new();
    let dir = beads_dir.to_string_lossy().into_owned();
    let mut warn = |msg: String| {
        authority_warnings.push(msg.clone());
        if !env_robot() {
            eprintln!("Warning: {msg}");
        }
    };
    let failed = |warnings: Vec<String>| LoadResult {
        issues: Vec::new(),
        source: DataSource {
            kind: SourceKind::JsonlLocal,
            path: PathBuf::new(),
            mod_time: None,
            size: 0,
        },
        report: LoadReport {
            path: dir.clone(),
            authority_warnings: warnings,
            ..Default::default()
        },
    };

    let issues_path = beads_dir.join("issues.jsonl");
    // PrepareBeadsDirForRead(beadsDir, refreshBDExport=true, warn):
    // refresh via `bd export -o`, fall back to an existing export with a
    // warning, hard-error when neither exists.
    match crate::bdcli::export_bd_issues_jsonl(beads_dir, &issues_path) {
        Ok(()) => {}
        Err(e) => {
            if issues_path.exists() {
                warn(format!(
                    "bd export failed, using existing issues.jsonl: {e}"
                ));
            } else {
                let result = failed(authority_warnings);
                return (
                    result,
                    Some(format!(
                        "bd/Dolt workspace detected at {dir}: failed to refresh bd compatibility JSONL (run 'bd export -o .beads/issues.jsonl'): {e}"
                    )),
                );
            }
        }
    }
    if !issues_path.exists() {
        let result = failed(authority_warnings);
        return (
            result,
            Some(format!(
                "bd/Dolt workspace detected at {dir}: no compatibility JSONL found at {}; run 'bd export -o .beads/issues.jsonl'",
                issues_path.display()
            )),
        );
    }

    let source = DataSource {
        kind: SourceKind::JsonlLocal,
        path: issues_path,
        mod_time: None,
        size: 0,
    };
    let (mut result, err) = load_and_validate_jsonl(&source);
    result.report.authority_warnings = authority_warnings;
    result.report.stale = !result.report.authority_warnings.is_empty();
    (result, err)
}

// ---------------------------------------------------------------------------
// metadata.json preferred source — Go cmd/bv/main.go:6380-6440
// (pages auto-source path only; NOT part of LoadIssues).
// ---------------------------------------------------------------------------

struct BeadsMetadata {
    database: String,
    jsonl_export: String,
}

fn read_beads_metadata(beads_dir: &Path) -> Option<BeadsMetadata> {
    let data = std::fs::read(beads_dir.join("metadata.json")).ok()?;
    #[derive(serde::Deserialize)]
    struct Raw {
        #[serde(default)]
        database: String,
        #[serde(default, rename = "jsonl_export")]
        jsonl_export: String,
    }
    let raw: Raw = serde_json::from_slice(&data).ok()?;
    Some(BeadsMetadata {
        database: raw.database,
        jsonl_export: raw.jsonl_export,
    })
}

/// Go `metadataPreferredSource` (main.go:6385-6413).
pub fn metadata_preferred_source(beads_dir: &Path) -> Option<(PathBuf, SourceKind)> {
    let meta = read_beads_metadata(beads_dir)?;
    for (declared, kind) in [
        (&meta.database, SourceKind::Sqlite),
        (&meta.jsonl_export, SourceKind::JsonlLocal),
    ] {
        if declared.is_empty() {
            continue;
        }
        let mut path = PathBuf::from(declared);
        if !path.is_absolute() {
            path = beads_dir.join(path);
        }
        if let Ok(info) = std::fs::metadata(&path) {
            if !info.is_dir() {
                return Some((path, kind));
            }
        }
    }
    None
}

/// Go `loadIssuesFromBeadsDir` (main.go:6416-6438): explicit dir ->
/// ResolveBeadsDir -> bd workspace -> metadata-preferred source ->
/// LoadIssuesFromDir, with `preferred export source failed` staleness.
pub fn load_issues_from_beads_dir(beads_dir: &Path) -> (LoadResult, Option<String>) {
    let beads_dir = match resolve_beads_dir_pub(beads_dir) {
        Ok(d) => d,
        Err(e) => return (LoadResult::default(), Some(e.to_string())),
    };
    if is_bd_workspace(&beads_dir) {
        return load_issues_from_dir(&beads_dir);
    }
    let mut preferred_err: Option<String> = None;
    if let Some((path, kind)) = metadata_preferred_source(&beads_dir) {
        let source = DataSource {
            kind,
            path: path.clone(),
            mod_time: std::fs::metadata(&path)
                .ok()
                .and_then(|m| m.modified().ok()),
            size: std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
        };
        let (result, err) = load_from_source(&source);
        if err.is_none() {
            return (result, None);
        }
        preferred_err = err;
    }
    let (mut loaded, err) = load_issues_from_dir_inner(&beads_dir);
    if let Some(pe) = preferred_err {
        loaded.report.stale = true;
        loaded
            .report
            .authority_warnings
            .push(format!("preferred export source failed: {pe}"));
    }
    (loaded, err)
}

/// `LoadIssuesFromDir` body without the ResolveBeadsDir step (already done).
fn load_issues_from_dir_inner(beads_dir: &Path) -> (LoadResult, Option<String>) {
    if is_bd_workspace(beads_dir) {
        return load_bd_workspace(beads_dir, Path::new(""));
    }
    load_with_fallback(beads_dir, Path::new(""), &mut |m| {
        if !env_robot() {
            eprintln!("Warning: {m}");
        }
    })
}

/// The path freshness selection would try first inside `beads_dir` — for
/// consumers that only need a watch target (the TUI polls one file's mtime)
/// rather than a full load. Returns `None` when no candidate exists.
pub fn freshest_source_path(beads_dir: &Path, repo_path: &Path) -> Option<PathBuf> {
    let mut warn = |_: &str| {};
    let mut sources = discover_sources(beads_dir, repo_path, &mut warn);
    sources.sort_by(|a, b| a.freshness_cmp(b));
    sources.first().map(|s| s.path.clone())
}
