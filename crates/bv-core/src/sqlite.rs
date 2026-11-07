//! SQLite reader — port of Go `internal/datasource/sqlite.go` (v0.25.0,
//! commit 18afafa), including its deliberate quirks:
//! - dependencies: only `depends_on_id` + type are selected (`created_at` /
//!   `created_by` stay zero — `loadDependencies`, sqlite.go:581-617)
//! - comments likewise carry no created-at unless the column parses
//! - the full-column query falls back to `loadIssuesSimple` when prepare
//!   fails (missing columns like `due_date` on br databases)
//! - `closed_at` and `source_repo` ARE selected by the Go query, and the
//!   v0.25.0 `data_hash` covers them
//!
//! Timestamp handling mirrors two different Go paths:
//! - `loadIssuesSimple` scans times as `sql.NullString` and runs
//!   `parseSQLiteTime` (sqlite.go:745-761): RFC3339Nano, RFC3339,
//!   `"2006-01-02 15:04:05"`, `"2006-01-02 15:04:05-07:00"`.
//! - the main query scans times as `sql.NullTime`, which the modernc driver
//!   only materialises for TEXT values in columns declared
//!   DATE/DATETIME/TIMESTAMP (vendor rows.go decltype switch), parsed with a
//!   wider layout list (`parseTimeFormats`, vendor sqlite.go:64-72) plus a
//!   `t.String()`-style layout first.

use crate::discovery::go_path_error;
use crate::fingerprint::{format_go_instant, parse_go_timestamp};
use crate::model::{Comment, Dependency, DependencyType, Issue, Status};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags, Row};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Go `datasource.LoadReport` fields the SQLite reader fills
/// (internal/datasource/load.go:25-46). `Warnings` is capped at 10 like
/// `maxLoadReportWarnings`; `WarningCount` counts every warning emitted.
#[derive(Debug, Default, Clone)]
pub struct SqliteReport {
    pub path: String,
    pub valid: usize,
    pub errors: usize,
    pub read_errors: usize,
    pub warnings: Vec<String>,
    pub warning_count: usize,
    pub tombstone_ids: Vec<String>,
}

const MAX_LOAD_REPORT_WARNINGS: usize = 10;

/// Result of a successful SQLite load: visible issues plus the authority
/// report Go's `LoadIssueAuthority` produces (valid/tombstones counted before
/// the tombstone filter, matching `LoadIssuesFiltered(nil)`).
#[derive(Debug)]
pub struct SqliteLoad {
    pub issues: Vec<Issue>,
    pub report: SqliteReport,
}

/// Go `NewSQLiteReader` — opens the database read-only with the same pragma
/// set the Go DSN applies (`sqliteReadOnlyDSN`, sqlite.go:59-72).
pub struct SqliteReader {
    conn: Connection,
    report: SqliteReport,
}

fn open_readonly(path: &str) -> Result<Connection, String> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("cannot connect to database: {}", sqlite_err(&e)))?;
    // `_pragma` DSN parameters in Go: busy_timeout(5000), cache_size(-64000),
    // mmap_size(268435456), temp_store(MEMORY), query_only(1).
    conn.execute_batch(
        "PRAGMA busy_timeout = 5000; \
         PRAGMA cache_size = -64000; \
         PRAGMA mmap_size = 268435456; \
         PRAGMA temp_store = MEMORY; \
         PRAGMA query_only = ON;",
    )
    .map_err(|e| format!("cannot connect to database: {}", sqlite_err(&e)))?;
    conn.query_row("SELECT 1", [], |_| Ok(()))
        .map_err(|e| format!("cannot connect to database: {}", sqlite_err(&e)))?;
    Ok(conn)
}

/// modernc.org/sqlite renders driver errors as `"<msg> (<extended code>)"`
/// (e.g. `file is not a database (26)`). rusqlite's `Display` drops the code,
/// so re-append it or rejected-source warnings drift from the oracle.
fn sqlite_err(e: &rusqlite::Error) -> String {
    if let rusqlite::Error::SqliteFailure(err, _) = e {
        let n = if err.extended_code != 0 {
            err.extended_code
        } else {
            err.code as i32
        };
        if n != 0 {
            return format!("{e} ({n})");
        }
    }
    e.to_string()
}

fn push_warning(report: &mut SqliteReport, msg: String) {
    report.warning_count += 1;
    if report.warnings.len() < MAX_LOAD_REPORT_WARNINGS {
        report.warnings.push(msg);
    }
}

struct SqliteSchema {
    dependency_type: String,
    labels_table: bool,
    comments_table: bool,
}

/// An issue plus the raw status text the scan produced. Go validates
/// `Status.IsValid()` (non-blank) on the raw string; the Rust `Status` enum
/// cannot represent a blank or custom status, so the raw text rides along
/// for the validation step.
struct ScannedIssue {
    issue: Issue,
    status_raw: String,
}

impl SqliteReader {
    /// Go `NewSQLiteReader` (the `source.Type` check is the caller's).
    pub fn open(path: &str) -> Result<SqliteReader, String> {
        std::fs::metadata(path).map_err(|e| {
            format!(
                "cannot access database: {}",
                go_path_error("stat", path, &e)
            )
        })?;
        let conn = open_readonly(path)?;
        Ok(SqliteReader {
            conn,
            report: SqliteReport {
                path: path.to_string(),
                ..Default::default()
            },
        })
    }

    /// Go `tableExists` — tables AND views (sqlite.go:619-623).
    fn table_exists(&self, name: &str) -> Result<bool, rusqlite::Error> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type IN ('table', 'view') AND name = ?1",
                [name],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n > 0)
    }

    /// Go `tableColumns` — lowercased column name -> (uppercased decltype).
    /// Only `issues` and `dependencies` have a PRAGMA; anything else is empty.
    fn table_columns(&self, table: &str) -> HashMap<String, String> {
        let query = match table {
            "dependencies" => "PRAGMA table_info(dependencies)",
            "issues" => "PRAGMA table_info(issues)",
            _ => return HashMap::new(),
        };
        let mut stmt = match self.conn.prepare(query) {
            Ok(s) => s,
            Err(_) => return HashMap::new(),
        };
        let mut rows = match stmt.query([]) {
            Ok(r) => r,
            Err(_) => return HashMap::new(),
        };
        let mut map = HashMap::new();
        while let Ok(Some(row)) = rows.next() {
            // cid, name, type, notnull, dflt_value, pk
            let name: String = match row.get(1) {
                Ok(n) => n,
                Err(_) => continue,
            };
            let decl: String = row
                .get::<_, Option<String>>(2)
                .ok()
                .flatten()
                .unwrap_or_default();
            map.insert(name.to_lowercase(), decl.to_uppercase());
        }
        map
    }

    /// Go `hasLabelsColumn` — case-insensitive `labels` on issues.
    fn has_labels_column(&self) -> bool {
        self.table_columns("issues").contains_key("labels")
    }

    /// Go `dependencyTypeExpr` (sqlite.go:625-649): `dependency_type` column
    /// wins over `type`; `''` when neither exists; error when the table lacks
    /// `issue_id`/`depends_on_id`.
    fn dependency_type_expr(&self) -> Result<String, String> {
        match self.table_exists("dependencies") {
            Ok(true) => {}
            Ok(false) => return Ok(String::new()),
            Err(e) => return Err(format!("inspecting dependency table: {}", sqlite_err(&e))),
        }
        let columns = self.table_columns("dependencies");
        if !columns.contains_key("issue_id") || !columns.contains_key("depends_on_id") {
            return Err("dependency table is missing issue_id or depends_on_id".to_string());
        }
        if columns.contains_key("dependency_type") {
            return Ok("dependency_type".to_string());
        }
        if columns.contains_key("type") {
            return Ok("type".to_string());
        }
        Ok("''".to_string())
    }

    /// Go `LoadIssueAuthority` (sqlite.go:233-262): load rows (main query
    /// with simple fallback), then validate + dedup, then fill the report.
    pub fn load_issue_authority(&mut self) -> Result<Vec<Issue>, String> {
        let mut report = SqliteReport {
            path: self.report.path.clone(),
            ..Default::default()
        };
        let loaded = match self.load_rows(&mut report) {
            Ok(l) => l,
            Err(e) => {
                // Go stores the partial report (accumulated Errors /
                // ReadErrors / Warnings) before returning the error
                // (sqlite.go:250-262).
                self.report = report;
                return Err(e);
            }
        };
        let mut seen: HashSet<String> = HashSet::with_capacity(loaded.len());
        let mut unique: Vec<Issue> = Vec::with_capacity(loaded.len());
        for scanned in loaded {
            if let Some(err) = go_issue_validate(&scanned.issue, &scanned.status_raw) {
                report.errors += 1;
                push_warning(
                    &mut report,
                    format!("invalid issue {:?}: {}", scanned.issue.id, err),
                );
                continue;
            }
            if !seen.insert(scanned.issue.id.clone()) {
                report.errors += 1;
                push_warning(
                    &mut report,
                    format!("duplicate issue ID {:?}", scanned.issue.id),
                );
                continue;
            }
            unique.push(scanned.issue);
        }
        report.valid = unique.len();
        report.tombstone_ids = unique
            .iter()
            .filter(|i| i.status.is_tombstone())
            .map(|i| i.id.clone())
            .collect();
        self.report = report;
        Ok(unique)
    }

    /// The report accumulated so far — Go stores `r.report` even when
    /// `LoadIssueAuthority` returns an error, so callers building a failed
    /// `LoadResult` still get the partial accounting.
    pub fn last_report(&self) -> &SqliteReport {
        &self.report
    }

    /// `LoadIssues` + report in one call: visible issues (tombstones removed)
    /// and the accounting Go's `LoadReport()` returns.
    pub fn load_issues(&mut self) -> Result<SqliteLoad, String> {
        let all = self.load_issue_authority()?;
        let report = self.report.clone();
        let issues = all
            .into_iter()
            .filter(|i| !i.status.is_tombstone())
            .collect();
        Ok(SqliteLoad { issues, report })
    }

    fn load_rows(&self, report: &mut SqliteReport) -> Result<Vec<ScannedIssue>, String> {
        // dependencyTypeExpr failure is non-fatal: empty expr -> no deps.
        let dependency_type = match self.dependency_type_expr() {
            Ok(expr) => expr,
            Err(e) => {
                report.read_errors += 1;
                push_warning(report, e);
                String::new()
            }
        };
        let mut labels_table = false;
        let mut comments_table = false;
        for (name, dst) in [
            ("labels", &mut labels_table),
            ("comments", &mut comments_table),
        ] {
            match self.table_exists(name) {
                Ok(exists) => *dst = exists,
                Err(e) => {
                    report.read_errors += 1;
                    push_warning(
                        report,
                        format!("inspecting {name} table: {}", sqlite_err(&e)),
                    );
                }
            }
        }
        let schema = SqliteSchema {
            dependency_type,
            labels_table,
            comments_table,
        };

        match self.try_main_query(report, &schema)? {
            Some(rows) => Ok(rows),
            // Go falls back ONLY when `db.Query` itself fails (prepare-time
            // errors like a missing column). Iteration errors are fatal.
            None => self.load_issues_simple(report, &schema),
        }
    }

    /// The 23-column query (sqlite.go:305-322). Returns `Ok(None)` when the
    /// statement cannot be prepared — the signal for `loadIssuesSimple`.
    fn try_main_query(
        &self,
        report: &mut SqliteReport,
        schema: &SqliteSchema,
    ) -> Result<Option<Vec<ScannedIssue>>, String> {
        let issues_cols = self.table_columns("issues");
        let has_col = |name: &str| issues_cols.contains_key(name);
        let decltype =
            |name: &str| -> String { issues_cols.get(name).cloned().unwrap_or_default() };

        let labels_expr = if !self.has_labels_column() && schema.labels_table {
            "(SELECT json_group_array(label) FROM labels WHERE issue_id = i.id)".to_string()
        } else {
            "i.labels".to_string()
        };
        let defer_expr = if has_col("defer_until") {
            "i.defer_until".to_string()
        } else {
            "NULL".to_string()
        };
        // Go fmt string, verbatim column order (sqlite.go:307-316).
        let query = format!(
            "SELECT i.id, i.title, i.description, i.status, i.priority, i.issue_type, \
             i.assignee, i.estimated_minutes, i.created_at, i.updated_at, \
             i.due_date, {defer_expr}, i.closed_at, i.external_ref, i.compaction_level, \
             i.compacted_at, i.compacted_at_commit, i.original_size, \
             {labels_expr}, i.design, i.acceptance_criteria, i.notes, i.source_repo, i.tombstone \
             FROM issues i ORDER BY i.updated_at DESC"
        );
        let mut stmt = match self.conn.prepare(&query) {
            Ok(s) => s,
            Err(_) => return Ok(None),
        };
        let mut rows = match stmt.query([]) {
            Ok(r) => r,
            Err(_) => return Ok(None),
        };

        // Decltypes for the six sql.NullTime columns. The `NULL` literal
        // defer_until substitute reports "" — never dateish, and it is
        // always NULL anyway.
        let dateish = |idx: usize, col: &str| -> bool {
            if idx == 11 && defer_expr == "NULL" {
                return false;
            }
            matches!(decltype(col).as_str(), "DATE" | "DATETIME" | "TIMESTAMP")
        };

        let mut out = Vec::new();
        loop {
            let row = match rows.next() {
                Ok(Some(r)) => r,
                Ok(None) => break,
                Err(e) => return Err(format!("error iterating issues: {}", sqlite_err(&e))),
            };
            match self.scan_main_row(row, &dateish, schema, report) {
                Ok(scanned) => out.push(scanned),
                Err(e) => {
                    report.errors += 1;
                    push_warning(report, format!("reading issue row: {e}"));
                }
            }
        }
        Ok(Some(out))
    }

    /// `loadIssuesSimple` (sqlite.go:435-550) — the reduced-column fallback.
    fn load_issues_simple(
        &self,
        report: &mut SqliteReport,
        schema: &SqliteSchema,
    ) -> Result<Vec<ScannedIssue>, String> {
        let columns = self.table_columns("issues");
        let has = |name: &str| columns.contains_key(name);
        let expr = |name: &str, fallback: &str| -> String {
            if has(name) {
                name.to_string()
            } else {
                fallback.to_string()
            }
        };
        let coalesce = |name: &str, fallback: &str| -> String {
            if has(name) {
                format!("COALESCE({name}, {fallback})")
            } else {
                fallback.to_string()
            }
        };
        let order_by = if has("updated_at") {
            "ORDER BY updated_at DESC"
        } else {
            "ORDER BY id"
        };
        let query = format!(
            "SELECT id, title, {}, status, {}, {}, {}, {}, {}, {}, {}, {} FROM issues {order_by}",
            expr("description", "NULL"),
            coalesce("priority", "3"),
            coalesce("issue_type", "'task'"),
            expr("assignee", "NULL"),
            expr("created_at", "NULL"),
            expr("updated_at", "NULL"),
            expr("defer_until", "NULL"),
            expr("labels", "NULL"),
            expr("tombstone", "NULL"),
        );
        let mut stmt = self
            .conn
            .prepare(&query)
            .map_err(|e| format!("query failed: {}", sqlite_err(&e)))?;
        let mut rows = stmt
            .query([])
            .map_err(|e| format!("query failed: {}", sqlite_err(&e)))?;

        let mut out = Vec::new();
        loop {
            let row = match rows.next() {
                Ok(Some(r)) => r,
                Ok(None) => break,
                Err(e) => return Err(format!("error iterating issues: {}", sqlite_err(&e))),
            };
            match self.scan_simple_row(row, schema, report) {
                Ok(scanned) => out.push(scanned),
                Err(e) => {
                    report.errors += 1;
                    push_warning(report, format!("reading issue row: {e}"));
                }
            }
        }
        Ok(out)
    }

    /// Main-query row scan (sqlite.go:326-426) including per-issue
    /// dependencies/comments — Go runs them as separate queries inside the
    /// row loop.
    fn scan_main_row(
        &self,
        row: &Row<'_>,
        dateish: &dyn Fn(usize, &str) -> bool,
        schema: &SqliteSchema,
        report: &mut SqliteReport,
    ) -> Result<ScannedIssue, ScanError> {
        let id = scan_req_string(row, 0)?;
        let title = scan_req_string(row, 1)?;
        let description = scan_opt_string(row, 2)?.unwrap_or_default();
        let mut status_raw = scan_req_string(row, 3)?;
        let priority = scan_req_int(row, 4)?;
        let issue_type = scan_req_string(row, 5)?;
        let assignee = scan_opt_string(row, 6)?.unwrap_or_default();
        let estimated_minutes = scan_opt_int(row, 7)?;
        let created_at = scan_modernc_time(row, 8, dateish(8, "created_at"))?;
        let updated_at = scan_modernc_time(row, 9, dateish(9, "updated_at"))?;
        let due_date = scan_modernc_time(row, 10, dateish(10, "due_date"))?;
        let defer_until = scan_modernc_time(row, 11, dateish(11, "defer_until"))?;
        let closed_at = scan_modernc_time(row, 12, dateish(12, "closed_at"))?;
        let external_ref = scan_opt_string(row, 13)?;
        let compaction_level = scan_opt_int(row, 14)?.unwrap_or(0);
        let compacted_at = scan_modernc_time(row, 15, dateish(15, "compacted_at"))?;
        let compacted_at_commit = scan_opt_string(row, 16)?;
        let original_size = scan_opt_int(row, 17)?.unwrap_or(0);
        let labels_json = scan_opt_string(row, 18)?;
        let design = scan_opt_string(row, 19)?.unwrap_or_default();
        let acceptance_criteria = scan_opt_string(row, 20)?.unwrap_or_default();
        let notes = scan_opt_string(row, 21)?.unwrap_or_default();
        let source_repo = scan_opt_string(row, 22)?.unwrap_or_default();
        let tombstone = scan_opt_int(row, 23)?;
        if tombstone.is_some_and(|t| t != 0) {
            status_raw = "tombstone".to_string();
        }

        // Go's main path does NOT call loadLabelsFromTable — labels arrive
        // via the column or the json_group_array subquery only. Order of
        // side effects matches Go: labels parse, then deps, then comments.
        let labels = labels_json
            .as_deref()
            .filter(|s| !s.is_empty() && *s != "null")
            .map(|raw| self.parse_sqlite_labels(raw, &id, report))
            .unwrap_or_default();
        let status = Status::parse(&status_raw).unwrap_or(Status::Open);
        let mut issue = Issue {
            id: id.clone(),
            content_hash: String::new(),
            title,
            description,
            design,
            acceptance_criteria,
            notes,
            status,
            priority,
            issue_type,
            assignee,
            estimated_minutes,
            created_at,
            updated_at,
            due_date,
            defer_until,
            closed_at,
            external_ref,
            compaction_level,
            compacted_at,
            compacted_at_commit,
            original_size,
            labels,
            dependencies: Vec::new(),
            comments: Vec::new(),
            source_repo,
        };
        issue.dependencies = self.load_dependencies(&id, &schema.dependency_type, report);
        if schema.comments_table {
            issue.comments = self.load_comments(&id, report);
        }
        Ok(ScannedIssue { issue, status_raw })
    }

    /// Simple-query row scan (sqlite.go:477-544).
    fn scan_simple_row(
        &self,
        row: &Row<'_>,
        schema: &SqliteSchema,
        report: &mut SqliteReport,
    ) -> Result<ScannedIssue, ScanError> {
        let id = scan_req_string(row, 0)?;
        let title = scan_req_string(row, 1)?;
        let description = scan_opt_string(row, 2)?.unwrap_or_default();
        let mut status_raw = scan_req_string(row, 3)?;
        let priority = scan_req_int(row, 4)?;
        let issue_type = scan_req_string(row, 5)?;
        let assignee = scan_opt_string(row, 6)?.unwrap_or_default();
        let created_raw = scan_opt_string(row, 7)?;
        let updated_raw = scan_opt_string(row, 8)?;
        let defer_raw = scan_opt_string(row, 9)?;
        let labels_json = scan_opt_string(row, 10)?;
        let tombstone = scan_opt_int(row, 11)?;
        if tombstone.is_some_and(|t| t != 0) {
            status_raw = "tombstone".to_string();
        }

        // Times resolve with per-field warnings on invalid non-blank values
        // (sqlite.go:505-528).
        let created_at = resolve_simple_time(&id, "created_at", created_raw, report);
        let updated_at = resolve_simple_time(&id, "updated_at", updated_raw, report);
        let defer_until = resolve_simple_time(&id, "defer_until", defer_raw, report);

        let mut labels = labels_json
            .as_deref()
            .filter(|s| !s.is_empty() && *s != "null")
            .map(|raw| self.parse_sqlite_labels(raw, &id, report))
            .unwrap_or_default();
        // Go's simple path DOES fall back to the labels table
        // (sqlite.go:532-535, "beads-rs compatibility").
        if schema.labels_table && labels.is_empty() {
            labels = self.load_labels_from_table(&id, report);
        }

        let status = Status::parse(&status_raw).unwrap_or(Status::Open);
        let mut issue = Issue {
            id: id.clone(),
            content_hash: String::new(),
            title,
            description,
            design: String::new(),
            acceptance_criteria: String::new(),
            notes: String::new(),
            status,
            priority,
            issue_type,
            assignee,
            estimated_minutes: None,
            created_at,
            updated_at,
            due_date: None,
            defer_until,
            closed_at: None,
            external_ref: None,
            compaction_level: 0,
            compacted_at: None,
            compacted_at_commit: None,
            original_size: 0,
            labels,
            dependencies: Vec::new(),
            comments: Vec::new(),
            source_repo: String::new(),
        };
        issue.dependencies = self.load_dependencies(&id, &schema.dependency_type, report);
        if schema.comments_table {
            issue.comments = self.load_comments(&id, report);
        }
        Ok(ScannedIssue { issue, status_raw })
    }

    /// Go `parseSQLiteLabels` (sqlite.go:686-694): strict `[]string`
    /// unmarshal, else ReadErrors + warning + `parseJSONStringArray`.
    fn parse_sqlite_labels(
        &self,
        raw: &str,
        issue_id: &str,
        report: &mut SqliteReport,
    ) -> Vec<String> {
        match serde_json::from_str::<Vec<String>>(raw) {
            Ok(labels) => labels,
            Err(e) => {
                report.read_errors += 1;
                push_warning(report, format!("parsing labels for {issue_id:?}: {e}"));
                parse_json_string_array(raw)
            }
        }
    }

    /// Go `loadLabelsFromTable` (sqlite.go:554-578).
    fn load_labels_from_table(&self, issue_id: &str, report: &mut SqliteReport) -> Vec<String> {
        let mut stmt = match self
            .conn
            .prepare("SELECT label FROM labels WHERE issue_id = ?1")
        {
            Ok(s) => s,
            Err(e) => {
                report.read_errors += 1;
                push_warning(
                    report,
                    format!("reading labels for {issue_id:?}: {}", sqlite_err(&e)),
                );
                return Vec::new();
            }
        };
        let mut rows = match stmt.query([issue_id]) {
            Ok(r) => r,
            Err(e) => {
                report.read_errors += 1;
                push_warning(
                    report,
                    format!("reading labels for {issue_id:?}: {}", sqlite_err(&e)),
                );
                return Vec::new();
            }
        };
        let mut labels = Vec::new();
        loop {
            match rows.next() {
                Ok(Some(row)) => match scan_req_string(row, 0) {
                    Ok(label) => labels.push(label),
                    Err(e) => {
                        report.read_errors += 1;
                        push_warning(report, format!("reading label for {issue_id:?}: {e}"));
                    }
                },
                Ok(None) => break,
                Err(e) => {
                    report.read_errors += 1;
                    push_warning(
                        report,
                        format!("iterating labels for {issue_id:?}: {}", sqlite_err(&e)),
                    );
                    break;
                }
            }
        }
        labels
    }

    /// Go `loadDependencies` (sqlite.go:581-617). `created_at`/`created_by`
    /// are deliberately NOT selected — they stay zero values.
    fn load_dependencies(
        &self,
        issue_id: &str,
        dependency_type_expr: &str,
        report: &mut SqliteReport,
    ) -> Vec<Dependency> {
        if dependency_type_expr.is_empty() {
            return Vec::new();
        }
        let query = format!(
            "SELECT depends_on_id, {dependency_type_expr} FROM dependencies WHERE issue_id = ?1"
        );
        let mut stmt = match self.conn.prepare(&query) {
            Ok(s) => s,
            Err(e) => {
                report.read_errors += 1;
                push_warning(
                    report,
                    format!("reading dependencies for {issue_id:?}: {}", sqlite_err(&e)),
                );
                return Vec::new();
            }
        };
        let mut rows = match stmt.query([issue_id]) {
            Ok(r) => r,
            Err(e) => {
                report.read_errors += 1;
                push_warning(
                    report,
                    format!("reading dependencies for {issue_id:?}: {}", sqlite_err(&e)),
                );
                return Vec::new();
            }
        };
        let mut deps = Vec::new();
        loop {
            let row = match rows.next() {
                Ok(Some(r)) => r,
                Ok(None) => break,
                Err(e) => {
                    report.read_errors += 1;
                    push_warning(
                        report,
                        format!(
                            "iterating dependencies for {issue_id:?}: {}",
                            sqlite_err(&e)
                        ),
                    );
                    break;
                }
            };
            let scanned = (|| -> Result<(String, String), ScanError> {
                let target = scan_req_string(row, 0)?;
                let ty = scan_req_string(row, 1)?;
                Ok((target, ty))
            })();
            let (depends_on_id, dep_type) = match scanned {
                Ok(v) => v,
                Err(e) => {
                    report.read_errors += 1;
                    push_warning(report, format!("reading dependency for {issue_id:?}: {e}"));
                    continue;
                }
            };
            // Go: `TrimSpace(DependsOnID) == "" || (Type != "" && !Type.IsValid())`
            // where IsValid is a non-blank test — a whitespace-only type is
            // invalid, an empty one is kept.
            if depends_on_id.trim().is_empty()
                || (!dep_type.is_empty() && dep_type.trim().is_empty())
            {
                report.read_errors += 1;
                push_warning(
                    report,
                    format!(
                        "invalid dependency for {issue_id:?}: target {depends_on_id:?}, type {dep_type:?}"
                    ),
                );
                continue;
            }
            deps.push(Dependency {
                issue_id: issue_id.to_string(),
                depends_on_id,
                depends_on_legacy: String::new(),
                target_id_legacy: String::new(),
                r#type: DependencyType::parse(&dep_type),
                created_at: None,
                created_by: String::new(),
            });
        }
        deps
    }

    /// Go `loadComments` (sqlite.go:652-684).
    fn load_comments(&self, issue_id: &str, report: &mut SqliteReport) -> Vec<Comment> {
        let mut stmt = match self.conn.prepare(
            "SELECT id, author, text, created_at FROM comments WHERE issue_id = ?1 ORDER BY created_at",
        ) {
            Ok(s) => s,
            Err(e) => {
                report.read_errors += 1;
                push_warning(report, format!("reading comments for {issue_id:?}: {}", sqlite_err(&e)));
                return Vec::new();
            }
        };
        let mut rows = match stmt.query([issue_id]) {
            Ok(r) => r,
            Err(e) => {
                report.read_errors += 1;
                push_warning(
                    report,
                    format!("reading comments for {issue_id:?}: {}", sqlite_err(&e)),
                );
                return Vec::new();
            }
        };
        let mut comments = Vec::new();
        loop {
            let row = match rows.next() {
                Ok(Some(r)) => r,
                Ok(None) => break,
                Err(e) => {
                    report.read_errors += 1;
                    push_warning(
                        report,
                        format!("iterating comments for {issue_id:?}: {}", sqlite_err(&e)),
                    );
                    break;
                }
            };
            let scanned = (|| -> Result<Comment, ScanError> {
                let id = scan_req_string(row, 0)?;
                let author = scan_req_string(row, 1)?;
                let text = scan_req_string(row, 2)?;
                let created_raw = scan_opt_string(row, 3)?;
                // Go: invalid created_at is silently left as the zero time —
                // no ReadErrors, no warning (sqlite.go:671-675).
                let created_at = created_raw
                    .as_deref()
                    .and_then(|s| parse_sqlite_time(s).map(|(sec, n)| format_go_instant(sec, n)));
                Ok(Comment {
                    id,
                    issue_id: issue_id.to_string(),
                    author,
                    text,
                    created_at,
                })
            })();
            match scanned {
                Ok(c) => comments.push(c),
                Err(e) => {
                    report.read_errors += 1;
                    push_warning(report, format!("reading comment for {issue_id:?}: {e}"));
                }
            }
        }
        comments
    }
}

// ---------------------------------------------------------------------------
// Column value coercion — mirrors database/sql convertAssign for the
// destination types the Go queries scan into.
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct ScanError(String);

impl std::fmt::Display for ScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn col_value<'a>(row: &'a Row<'_>, idx: usize) -> Result<ValueRef<'a>, ScanError> {
    row.get_ref(idx).map_err(|e| ScanError(sqlite_err(&e)))
}

/// Go `strconv.FormatFloat(f, 'g', -1, 64)` approximation for the integer
/// range SQLite realistically stores. Non-integral or huge values fall back
/// to Rust's shortest-round-trip rendering; the divergence surface is a
/// REAL column holding a non-integral float inside a string field, which no
/// beads writer produces.
fn go_float_string(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 1e18 && f.is_finite() {
        format!("{}", f as i64)
    } else {
        format!("{f}")
    }
}

/// `*string` destination (id, title, status, issue_type, dep target/type,
/// comment fields): NULL is an error, everything else stringifies.
fn go_text(v: &ValueRef<'_>) -> Result<String, ScanError> {
    match v {
        ValueRef::Null => Err(ScanError("converting NULL to string is unsupported".into())),
        ValueRef::Text(t) => Ok(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Integer(i) => Ok(i.to_string()),
        ValueRef::Real(f) => Ok(go_float_string(*f)),
        ValueRef::Blob(b) => Ok(String::from_utf8_lossy(b).into_owned()),
    }
}

/// `sql.NullString`: NULL -> None, otherwise the `*string` coercion.
fn go_opt_text(v: &ValueRef<'_>) -> Result<Option<String>, ScanError> {
    if matches!(v, ValueRef::Null) {
        return Ok(None);
    }
    go_text(v).map(Some)
}

/// Strict `strconv.ParseInt(s, 10, 64)` for TEXT/BLOB payloads.
fn go_parse_int(s: &str) -> Result<i64, ScanError> {
    s.parse::<i64>()
        .map_err(|_| ScanError(format!("parsing {s:?}: invalid syntax")))
}

/// Integer coercion shared by `*int` and `sql.NullInt64` destinations.
fn go_int_value(v: &ValueRef<'_>) -> Result<Option<i64>, ScanError> {
    match v {
        ValueRef::Null => Ok(None),
        ValueRef::Integer(i) => Ok(Some(*i)),
        ValueRef::Text(t) => {
            let s = String::from_utf8_lossy(t);
            go_parse_int(&s).map(Some)
        }
        ValueRef::Blob(b) => {
            let s = String::from_utf8_lossy(b);
            go_parse_int(&s).map(Some)
        }
        ValueRef::Real(f) => {
            // Go: float64 -> asString ('g' -1) -> ParseInt. Integral floats
            // within range stringify without an exponent and parse; anything
            // else fails.
            if f.fract() == 0.0 && f.is_finite() && f.abs() < 9.3e18 {
                Ok(Some(*f as i64))
            } else {
                Err(ScanError(format!("converting {f} to int is unsupported")))
            }
        }
    }
}

fn scan_req_string(row: &Row<'_>, idx: usize) -> Result<String, ScanError> {
    go_text(&col_value(row, idx)?)
}

fn scan_opt_string(row: &Row<'_>, idx: usize) -> Result<Option<String>, ScanError> {
    go_opt_text(&col_value(row, idx)?)
}

/// `*int` (issue.Priority): NULL is an error.
fn scan_req_int(row: &Row<'_>, idx: usize) -> Result<i64, ScanError> {
    match go_int_value(&col_value(row, idx)?)? {
        Some(v) => Ok(v),
        None => Err(ScanError("converting NULL to int is unsupported".into())),
    }
}

/// `sql.NullInt64` -> Option<i64>.
fn scan_opt_int(row: &Row<'_>, idx: usize) -> Result<Option<i64>, ScanError> {
    go_int_value(&col_value(row, idx)?)
}

/// `sql.NullTime` under the modernc driver (main query only): NULL -> None;
/// TEXT -> parsed only when the column decltype is DATE/DATETIME/TIMESTAMP,
/// otherwise the raw string reaches `NullTime.Scan` and fails; any other
/// storage class fails the same way.
fn scan_modernc_time(
    row: &Row<'_>,
    idx: usize,
    dateish: bool,
) -> Result<Option<String>, ScanError> {
    let v = col_value(row, idx)?;
    match v {
        ValueRef::Null => Ok(None),
        ValueRef::Text(t) if dateish => {
            let s = String::from_utf8_lossy(t);
            match parse_modernc_time(s.trim()) {
                Some((sec, n)) => Ok(Some(format_go_instant(sec, n))),
                None => Err(ScanError(
                    "unsupported Scan, storing driver.Value type string into type *time.Time"
                        .into(),
                )),
            }
        }
        _ => Err(ScanError(
            "unsupported Scan, storing driver.Value type into type *time.Time".into(),
        )),
    }
}

// ---------------------------------------------------------------------------
// Timestamp parsing
// ---------------------------------------------------------------------------

/// Go `parseSQLiteTime` (sqlite.go:745-761) -> `(unix_seconds, nanos)`.
/// Layouts: RFC3339Nano, RFC3339 (both the strict RFC3339 grammar),
/// `"2006-01-02 15:04:05"` (UTC), `"2006-01-02 15:04:05-07:00"`.
pub fn parse_sqlite_time(raw: &str) -> Option<(i64, u32)> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    // RFC3339Nano / RFC3339 — identical acceptance.
    if let Some(v) = parse_go_timestamp(raw) {
        return Some(v);
    }
    parse_space_time(raw)
}

/// `"2006-01-02 15:04:05"` (UTC) and `"2006-01-02 15:04:05-07:00"`.
/// Go `time.Parse` accepts an optional fraction after the seconds even
/// though the layouts do not show one.
fn parse_space_time(raw: &str) -> Option<(i64, u32)> {
    let b = raw.as_bytes();
    let mut i = 0usize;
    let (y, mo, d, h, mi, s, nano) = scan_date_time(b, &mut i)?;
    if i == b.len() {
        // No zone -> UTC.
        return civil_unix(y, mo, d, h, mi, s, nano).map(|secs| (secs, nano));
    }
    // Optional `-07:00` / `+07:00` offset.
    let sign = match b[i] {
        b'-' => -1i64,
        b'+' => 1i64,
        _ => return None,
    };
    i += 1;
    let oh = scan_ndigits(b, &mut i, 2)? as i64;
    if b.get(i) != Some(&b':') {
        return None;
    }
    i += 1;
    let om = scan_ndigits(b, &mut i, 2)? as i64;
    if i != b.len() || oh > 24 || om > 60 {
        return None;
    }
    let local = civil_unix(y, mo, d, h, mi, s, nano)?;
    Some((local - sign * (oh * 3600 + om * 60), nano))
}

/// modernc `parseTimeFormats` order (vendor sqlite.go:64-72) plus the
/// leading `t.String()` layout. Returns `(unix_seconds, nanos)`.
fn parse_modernc_time(raw: &str) -> Option<(i64, u32)> {
    // parseTimeString: `time.Parse("2006-01-02 15:04:05.999999999 -0700 MST")`
    // after truncating at the first "m=" and trimming. Go only truncates
    // when "m=" is found at index > 0; otherwise the full trimmed string is
    // tried.
    match raw.find("m=") {
        Some(x) if x > 0 => {
            if let Some(v) = parse_go_string_time(raw[..x].trim()) {
                return Some(v);
            }
        }
        _ => {
            if let Some(v) = parse_go_string_time(raw.trim()) {
                return Some(v);
            }
        }
    }
    // Z-suffix strip: the remainder parses as UTC.
    let ts = raw.strip_suffix('Z').unwrap_or(raw);
    // Layouts in declaration order.
    for layout in [
        ModerncLayout {
            sep: b' ',
            seconds: true,
            zone: Zone::OffsetColon,
        },
        ModerncLayout {
            sep: b'T',
            seconds: true,
            zone: Zone::OffsetColon,
        },
        ModerncLayout {
            sep: b' ',
            seconds: true,
            zone: Zone::None,
        },
        ModerncLayout {
            sep: b'T',
            seconds: true,
            zone: Zone::None,
        },
        ModerncLayout {
            sep: b' ',
            seconds: false,
            zone: Zone::None,
        },
        ModerncLayout {
            sep: b'T',
            seconds: false,
            zone: Zone::None,
        },
        ModerncLayout {
            sep: b' ',
            seconds: false,
            zone: Zone::DateOnly,
        },
    ] {
        if let Some(v) = parse_modernc_layout(ts, &layout) {
            return Some(v);
        }
    }
    None
}

#[derive(Clone, Copy)]
struct ModerncLayout {
    sep: u8,
    seconds: bool,
    zone: Zone,
}

#[derive(Clone, Copy, PartialEq)]
enum Zone {
    /// No zone in the layout; input must end after the time (or date).
    None,
    /// `-07:00` — sign + HH:MM required.
    OffsetColon,
    /// Date only (`2006-01-02`): no time part at all.
    DateOnly,
}

/// `"2006-01-02 15:04:05.999999999 -0700 MST"` — the Go `t.String()` shape.
fn parse_go_string_time(s: &str) -> Option<(i64, u32)> {
    let b = s.as_bytes();
    let mut i = 0usize;
    let (y, mo, d, h, mi, sec, nano) = scan_date_time(b, &mut i)?;
    if i >= b.len() || b[i] != b' ' {
        return None;
    }
    i += 1;
    let sign = match b.get(i) {
        Some(b'-') => -1i64,
        Some(b'+') => 1i64,
        _ => return None,
    };
    i += 1;
    let oh = scan_ndigits(b, &mut i, 2)? as i64;
    let om = scan_ndigits(b, &mut i, 2)? as i64;
    if i >= b.len() || b[i] != b' ' {
        return None;
    }
    i += 1;
    // MST: zone abbreviation — Go accepts a run of letters; the numeric
    // offset is what fixes the instant.
    let start = i;
    while i < b.len() && b[i].is_ascii_alphabetic() {
        i += 1;
    }
    if i == start || i != b.len() || oh > 24 || om > 60 {
        return None;
    }
    let local = civil_unix(y, mo, d, h, mi, sec, nano)?;
    Some((local - sign * (oh * 3600 + om * 60), nano))
}

fn parse_modernc_layout(s: &str, layout: &ModerncLayout) -> Option<(i64, u32)> {
    let b = s.as_bytes();
    let mut i = 0usize;
    if layout.zone == Zone::DateOnly {
        // "2006-01-02" — date only, midnight UTC.
        let (y, mo, d) = scan_date(b, &mut i)?;
        if i != b.len() {
            return None;
        }
        return civil_unix(y, mo, d, 0, 0, 0, 0).map(|secs| (secs, 0));
    }
    let (y, mo, d, h, mi, ss, nano) = if layout.seconds {
        scan_date_time(b, &mut i)?
    } else {
        let (y, mo, d) = scan_date(b, &mut i)?;
        if i >= b.len() || b[i] != layout.sep {
            return None;
        }
        i += 1;
        let hh = scan_ndigits(b, &mut i, 2)?;
        if b.get(i) != Some(&b':') {
            return None;
        }
        i += 1;
        let mm = scan_ndigits(b, &mut i, 2)?;
        (y, mo, d, hh, mm, 0, 0)
    };
    if layout.seconds {
        // scan_date_time already consumed ` sep HH:MM:SS[.frac]`.
    } else if i < b.len() && b[i] == layout.sep {
        // unreachable: HH:MM layouts have no seconds, so a ':' here means
        // trailing junk -> handled by the end checks below.
    }
    let local = civil_unix(y, mo, d, h, mi, ss, nano)?;
    match layout.zone {
        Zone::None => {
            if i != b.len() {
                return None;
            }
            Some((local, nano))
        }
        Zone::OffsetColon => {
            let sign = match b.get(i) {
                Some(b'-') => -1i64,
                Some(b'+') => 1i64,
                _ => return None,
            };
            i += 1;
            let oh = scan_ndigits(b, &mut i, 2)? as i64;
            if b.get(i) != Some(&b':') {
                return None;
            }
            i += 1;
            let om = scan_ndigits(b, &mut i, 2)? as i64;
            if i != b.len() || oh > 24 || om > 60 {
                return None;
            }
            Some((local - sign * (oh * 3600 + om * 60), nano))
        }
        Zone::DateOnly => unreachable!(),
    }
}

/// `YYYY-MM-DD` — strict 4-2-2 digit widths like Go's `2006-01-02`.
fn scan_date(b: &[u8], i: &mut usize) -> Option<(i64, u32, u32)> {
    let y = scan_ndigits(b, i, 4)? as i64;
    if b.get(*i) != Some(&b'-') {
        return None;
    }
    *i += 1;
    let mo = scan_ndigits(b, i, 2)?;
    if b.get(*i) != Some(&b'-') {
        return None;
    }
    *i += 1;
    let d = scan_ndigits(b, i, 2)?;
    Some((y, mo, d))
}

/// `YYYY-MM-DD<sep>HH:MM:SS[.frac|,frac]` — the date plus a ` ` or `T`
/// separated time with required seconds. Returns all civil fields.
fn scan_date_time(b: &[u8], i: &mut usize) -> Option<(i64, u32, u32, u32, u32, u32, u32)> {
    let (y, mo, d) = scan_date(b, i)?;
    if *i >= b.len() || (b[*i] != b' ' && b[*i] != b'T') {
        return None;
    }
    let sep = b[*i];
    *i += 1;
    let hh = scan_ndigits(b, i, 2)?;
    if b.get(*i) != Some(&b':') {
        return None;
    }
    *i += 1;
    let mm = scan_ndigits(b, i, 2)?;
    if b.get(*i) != Some(&b':') {
        return None;
    }
    *i += 1;
    let ss = scan_ndigits(b, i, 2)?;
    let nano = scan_fraction(b, i)?;
    let _ = sep;
    Some((y, mo, d, hh, mm, ss, nano))
}

/// Optional fraction: `.` or `,` plus ≥1 digit, truncated to nanoseconds —
/// Go accepts the fraction even when the layout does not show one, and
/// accepts either separator.
fn scan_fraction(b: &[u8], i: &mut usize) -> Option<u32> {
    let mut nano = 0u32;
    if matches!(b.get(*i), Some(b'.') | Some(b',')) {
        *i += 1;
        let start = *i;
        let mut kept = 0usize;
        while *i < b.len() && b[*i].is_ascii_digit() {
            if kept < 9 {
                nano = nano * 10 + u32::from(b[*i] - b'0');
                kept += 1;
            }
            *i += 1;
        }
        if *i == start {
            return None;
        }
        for _ in kept..9 {
            nano *= 10;
        }
    }
    Some(nano)
}

fn scan_ndigits(b: &[u8], i: &mut usize, n: usize) -> Option<u32> {
    if *i + n > b.len() {
        return None;
    }
    let mut v: u32 = 0;
    for _ in 0..n {
        let c = b[*i];
        if !c.is_ascii_digit() {
            return None;
        }
        v = v * 10 + u32::from(c - b'0');
        *i += 1;
    }
    Some(v)
}

/// days-from-civil (proleptic Gregorian, same algorithm as
/// `fingerprint::parse_go_timestamp`) -> unix seconds for the civil fields,
/// range-checked like Go's `time.Date`/`time.Parse`.
fn civil_unix(y: i64, mo: u32, d: u32, h: u32, mi: u32, s: u32, _nano: u32) -> Option<i64> {
    if !(1..=12).contains(&mo) || h > 23 || mi > 59 || s > 59 {
        return None;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let dim = match mo {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    if d == 0 || d > dim {
        return None;
    }
    let y = if mo <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (i64::from(mo) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let secs = days * 86400 + i64::from(h) * 3600 + i64::from(mi) * 60 + i64::from(s);
    Some(secs)
}

/// Simple-path time resolution with the Go warnings
/// (`issue %q has invalid <field> %q` — sqlite.go:505-528). Blank-after-trim
/// values are silently unset.
fn resolve_simple_time(
    issue_id: &str,
    field: &str,
    raw: Option<String>,
    report: &mut SqliteReport,
) -> Option<String> {
    let raw = raw?;
    match parse_sqlite_time(&raw) {
        Some((sec, n)) => Some(format_go_instant(sec, n)),
        None => {
            if !raw.trim().is_empty() {
                report.read_errors += 1;
                push_warning(
                    report,
                    format!("issue {issue_id:?} has invalid {field} {raw:?}"),
                );
            }
            None
        }
    }
}

/// Go `parseJSONStringArray` (sqlite.go:764-788).
fn parse_json_string_array(s: &str) -> Vec<String> {
    let s = s.trim();
    if s.is_empty() || s == "null" || s == "[]" {
        return Vec::new();
    }
    if let Ok(result) = serde_json::from_str::<Vec<String>>(s) {
        return result;
    }
    // Fallback: strip ONE leading '[' and ONE trailing ']', split on ',',
    // trim spaces then trim '"' chars, drop empties.
    let inner = s
        .strip_prefix('[')
        .and_then(|t| t.strip_suffix(']'))
        .or_else(|| s.strip_prefix('['))
        .unwrap_or(s);
    let inner = inner.trim();
    if inner.is_empty() {
        return Vec::new();
    }
    inner
        .split(',')
        .map(|item| item.trim().trim_matches('"').to_string())
        .filter(|item| !item.is_empty())
        .collect()
}

// ---------------------------------------------------------------------------
// Go Issue.Validate parity for scanned rows
// ---------------------------------------------------------------------------

/// Go `Issue.Validate` (pkg/model/types.go:266-283) error text, or None.
/// Status validity is tested on the RAW scanned string because the Rust
/// `Status` enum cannot hold Go's arbitrary non-blank statuses; a blank raw
/// status is the only invalid case the enum cannot express, and custom
/// non-blank statuses are VALID in Go (types.go:303-305).
fn go_issue_validate(issue: &Issue, status_raw: &str) -> Option<String> {
    if issue.id.is_empty() {
        return Some("issue ID cannot be empty".to_string());
    }
    if issue.title.is_empty() {
        return Some("issue title cannot be empty".to_string());
    }
    if status_raw.trim().is_empty() {
        return Some(format!("invalid status: {status_raw}"));
    }
    // Go `IssueType.IsValid` is `t != ""` — whitespace is valid.
    if issue.issue_type.is_empty() {
        return Some(format!("invalid issue type: {}", issue.issue_type));
    }
    if let (Some(c), Some(u)) = (&issue.created_at, &issue.updated_at) {
        if let (Some(c), Some(u)) = (parse_go_timestamp(c), parse_go_timestamp(u)) {
            // Go skips the check when either time IsZero().
            let c_zero = c.0 == -62135596800 && c.1 == 0;
            let u_zero = u.0 == -62135596800 && u.1 == 0;
            if !c_zero && !u_zero && (u.0, u.1) < (c.0, c.1) {
                return Some(format!(
                    "updated_at ({}) cannot be before created_at ({})",
                    go_time_display(u.0, u.1),
                    go_time_display(c.0, c.1)
                ));
            }
        }
    }
    None
}

/// Go `time.Time.String()`-ish rendering for the inverted-timestamp warning:
/// `"2006-01-02 15:04:05.999999999 +0000 UTC"`.
fn go_time_display(secs: i64, nanos: u32) -> String {
    let rfc = format_go_instant(secs, nanos);
    // "YYYY-MM-DDTHH:MM:SS[.f]Z" -> "YYYY-MM-DD HH:MM:SS[.f] +0000 UTC"
    let body = rfc.strip_suffix('Z').unwrap_or(&rfc);
    let body = body.replacen('T', " ", 1);
    format!("{body} +0000 UTC")
}

// ---------------------------------------------------------------------------
// validateSQLite — the smart-path gate (internal/datasource/validate.go:168)
// ---------------------------------------------------------------------------

/// Go `validateSQLite`: stat, integrity_check, issues table, required
/// columns id/title/status. Returns Go-style error strings.
pub fn validate_source(path: &str) -> Result<(), String> {
    let info = std::fs::metadata(path)
        .map_err(|e| format!("cannot access file: {}", go_path_error("stat", path, &e)))?;
    if info.is_dir() {
        return Err("path is a directory, not a file".to_string());
    }
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("cannot open database: {}", sqlite_err(&e)))?;
    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .map_err(|e| format!("integrity check failed: {}", sqlite_err(&e)))?;
    if integrity != "ok" {
        return Err(format!("database corrupt: {integrity}"));
    }
    let table = conn.query_row(
        "SELECT name FROM sqlite_master WHERE type='table' AND name='issues'",
        [],
        |r| r.get::<_, String>(0),
    );
    match table {
        Ok(_) => {}
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Err("missing issues table".to_string());
        }
        Err(e) => return Err(format!("cannot query schema: {}", sqlite_err(&e))),
    }
    let mut stmt = conn
        .prepare("PRAGMA table_info(issues)")
        .map_err(|e| format!("cannot query table info: {}", sqlite_err(&e)))?;
    let mut rows = stmt
        .query([])
        .map_err(|e| format!("cannot query table info: {}", sqlite_err(&e)))?;
    let mut columns = HashSet::new();
    loop {
        let row = match rows.next() {
            Ok(Some(r)) => r,
            Ok(None) => break,
            Err(e) => return Err(format!("error iterating column info: {}", sqlite_err(&e))),
        };
        let name: String = row
            .get(1)
            .map_err(|e| format!("cannot scan column info: {}", sqlite_err(&e)))?;
        columns.insert(name.to_lowercase());
    }
    for col in ["id", "title", "status"] {
        if !columns.contains(col) {
            return Err(format!("missing required column: {col}"));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Backwards-compatible entry point used by discovery::load_issues_from_repo
// ---------------------------------------------------------------------------

/// Load visible (non-tombstone) issues from a beads SQLite database.
///
/// Kept for `discovery::load_issues_from_repo`'s JSONL-less fallback; the
/// datasource path uses [`SqliteReader::load_issues`] for full reporting.
pub fn load_issues_sqlite(db_path: &Path) -> Result<Vec<Issue>, String> {
    let path = db_path.to_string_lossy().into_owned();
    let mut reader = SqliteReader::open(&path)?;
    let load = reader.load_issues()?;
    Ok(load.issues)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Space-separated SQLite timestamps must come back as a bare RFC3339
    /// instant — a `Zoned` Display suffix like `[UTC]` would be rejected by
    /// every downstream `jiff::Timestamp` parse and silently drop the time.
    #[test]
    fn sqlite_time_output_reparses_as_timestamp() {
        for raw in [
            "2026-01-02 03:04:05",
            "2026-01-02T03:04:05",
            "2026-01-02 03:04:05.123456789",
            "2026-01-02T03:04:05+00:00",
            "2026-01-02 03:04:05+02:00",
        ] {
            let out = parse_sqlite_time(raw).unwrap_or_else(|| panic!("{raw} parses"));
            let rendered = format_go_instant(out.0, out.1);
            assert!(
                !rendered.contains('['),
                "{raw} -> {rendered} must not carry a bracketed zone"
            );
            rendered
                .parse::<jiff::Timestamp>()
                .unwrap_or_else(|e| panic!("{raw} -> {rendered} must reparse as Timestamp: {e}"));
        }
    }

    #[test]
    fn sqlite_time_empty_and_garbage() {
        assert_eq!(parse_sqlite_time(""), None);
        assert_eq!(parse_sqlite_time("   "), None);
        assert_eq!(parse_sqlite_time("not a time"), None);
    }
}
