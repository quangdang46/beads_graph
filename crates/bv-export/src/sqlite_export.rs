//! Writer for the exported static site's `beads.sqlite3` and its
//! `beads.sqlite3.config.json` sidecar.
//!
//! Port of Go `pkg/export/sqlite_export.go` (v0.25.0, commit 18afafa). Go is the
//! spec; where the two differ, Go wins.
//!
//! The database is built inside a `.beads-*` scratch directory under the output
//! directory and moved into place with a single `rename` only after every step
//! has succeeded, so a failed export never replaces a good bundle (Go
//! `Export`, lines 116-126 and 200-206). Chunks follow the same ownership rule:
//! cleanup only ever touches `NNNNN.bin` files this exporter itself names.
//!
//! ## Byte-parity is not the target
//!
//! `beads.sqlite3` embeds `generated_at` and `readiness_at` wall-clock stamps
//! and is `VACUUM`ed by a different SQLite build, so a byte-identical file is
//! impossible. The contract this module keeps is schema + row counts + query
//! equivalence; `tests/sqlite_parity.rs` checks that against the Go oracle.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};

use bv_core::model::{Dependency, Issue};
use bv_correlation::readiness::{closed_for_readiness, ReadinessIndex, ReadinessIssue};

use crate::sqlite_schema::{
    create_fts_index, create_materialized_views, create_schema, disable_foreign_keys,
    insert_meta_value, optimize_database, DEFAULT_PAGE_SIZE, SCHEMA_VERSION,
};

/// Go `export.SQLiteExportConfig` defaults.
#[derive(Debug, Clone)]
pub struct SqliteExportConfig {
    /// Go `ChunkThreshold` — 5 MiB. Below this the database ships whole.
    pub chunk_threshold: u64,
    /// Go `ChunkSize` — 1 MiB, tuned for httpvfs range requests.
    pub chunk_size: u64,
    /// Go `PageSize` — 1024, tuned for httpvfs range requests.
    pub page_size: i64,
}

impl Default for SqliteExportConfig {
    /// Go `DefaultSQLiteExportConfig`.
    fn default() -> Self {
        Self {
            chunk_threshold: 5 * 1024 * 1024,
            chunk_size: 1024 * 1024,
            page_size: DEFAULT_PAGE_SIZE,
        }
    }
}

/// Errors this module can return. Kept narrow on purpose: every variant names a
/// step a caller might reasonably want to report differently.
#[derive(Debug)]
pub enum SqliteExportError {
    /// Any `rusqlite` failure. The message carries the SQL context Go wraps.
    Sqlite(rusqlite::Error),
    Io(std::io::Error),
    /// JSON encoding of a column value (labels, reasons, resolved ids).
    Json(serde_json::Error),
    /// `rename` of the finished database into place failed.
    Publish(std::io::Error),
}

impl std::fmt::Display for SqliteExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sqlite(e) => write!(f, "sqlite: {e}"),
            Self::Io(e) => write!(f, "io: {e}"),
            Self::Json(e) => write!(f, "json: {e}"),
            Self::Publish(e) => write!(f, "publish database: {e}"),
        }
    }
}

impl std::error::Error for SqliteExportError {}

impl From<rusqlite::Error> for SqliteExportError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sqlite(e)
    }
}
impl From<std::io::Error> for SqliteExportError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<serde_json::Error> for SqliteExportError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

type Result<T> = std::result::Result<T, SqliteExportError>;

/// Go `analysis.GraphStats`'s three score maps, as `insertMetrics` consumes
/// them.
///
/// `None` for the whole bundle means "no stats available", which is Go's
/// `e.Stats == nil` and suppresses the `issue_metrics` rows entirely — not the
/// same as an empty bundle, which would insert a zero row per issue.
#[derive(Debug, Clone, Default)]
pub struct MetricTables {
    /// Go `e.Stats.PageRank()`.
    pub pagerank: BTreeMap<String, f64>,
    /// Go `e.Stats.Betweenness()`.
    pub betweenness: BTreeMap<String, f64>,
    /// Go `e.Stats.CriticalPathScore()` — float, truncated to int on insert
    /// exactly as Go's `int(criticalPathMap[id])` does.
    pub critical_path: BTreeMap<String, f64>,
}

/// Go `analysis.TriageResult.Recommendations`, narrowed to the six columns the
/// `triage_recommendations` table stores.
///
/// The `Option`s reproduce Go's nil-vs-empty slice distinction: a nil
/// `reasons` marshals to the JSON literal `null`, an empty non-nil slice
/// marshals to `[]`. Collapsing them to `Vec` would change stored bytes.
#[derive(Debug, Clone, Default)]
pub struct TriageRow {
    pub issue_id: String,
    pub score: f64,
    pub action: String,
    pub reasons: Option<Vec<String>>,
    pub unblocks_ids: Option<Vec<String>>,
    pub blocked_by_ids: Option<Vec<String>>,
}

/// Go `(*SQLiteExporter)`.
///
/// ## Scope of the readiness index
///
/// Go normally receives a `Config.Readiness` that the CLI already computed over
/// the *full* source, and only falls back to building one from `Issues` + `Deps`
/// when a standalone caller leaves it nil. This port always takes the fallback
/// path, so `issues` must be the unscoped set for `dependency_state` and
/// `is_actionable` to agree with the oracle. That is the default anyway —
/// `--pages-include-closed` is true upstream — but a caller that filters the
/// issue list before handing it over will silently export a narrower readiness
/// picture than Go produces.
pub struct SqliteExporter<'a> {
    pub issues: &'a [Issue],
    pub deps: &'a [Dependency],
    /// `None` mirrors Go's nil `Stats`: no `issue_metrics` rows and no
    /// `in_cycle` flags.
    pub metrics: Option<&'a MetricTables>,
    /// Go `triageScores` — `rec.Score` per recommendation id. Empty when there
    /// is no triage result, which is not the same as "score 0".
    pub triage_scores: BTreeMap<String, f64>,
    /// Go `e.Triage.Recommendations` for `triage_recommendations`.
    pub triage: &'a [TriageRow],
    /// Every issue id appearing in `e.Stats.Cycles()`. Go flattens the cycle
    /// list into a set and flags those rows `in_cycle = 1`.
    pub cycle_ids: BTreeSet<String>,
    pub git_hash: String,
    pub title: String,
    pub config: SqliteExportConfig,
    /// Go `Config.ReadinessAt`; `None` means "use the export clock", matching
    /// Go's zero-value fallback.
    pub readiness_at: Option<jiff::Timestamp>,
}

impl<'a> SqliteExporter<'a> {
    /// Go `NewSQLiteExporter` + `DefaultSQLiteExportConfig`.
    pub fn new(issues: &'a [Issue], deps: &'a [Dependency]) -> Self {
        Self {
            issues,
            deps,
            metrics: None,
            triage_scores: BTreeMap::new(),
            triage: &[],
            cycle_ids: BTreeSet::new(),
            git_hash: String::new(),
            title: String::new(),
            config: SqliteExportConfig::default(),
            readiness_at: None,
        }
    }

    /// Go `SetGitHash`.
    pub fn with_git_hash(mut self, hash: impl Into<String>) -> Self {
        self.git_hash = hash.into();
        self
    }

    /// Go's `e.Stats` — the three score maps `insertMetrics` reads, and the
    /// switch that also enables the `in_cycle` flags.
    pub fn with_metrics(mut self, metrics: &'a MetricTables) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// Go's `e.Triage.Recommendations`, for the `triage_recommendations` table.
    pub fn with_triage(mut self, triage: &'a [TriageRow]) -> Self {
        self.triage = triage;
        self
    }

    /// Go's triage score lookup, `triage_score` in `issue_metrics`.
    pub fn with_triage_scores(mut self, scores: BTreeMap<String, f64>) -> Self {
        self.triage_scores = scores;
        self
    }

    /// Every issue id in `e.Stats.Cycles()`, which become `in_cycle = 1`.
    pub fn with_cycle_ids(mut self, cycle_ids: BTreeSet<String>) -> Self {
        self.cycle_ids = cycle_ids;
        self
    }

    /// Go `Config.Title` — also surfaces as the `title` metadata key, and the
    /// static-site group uses it for the page heading.
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// Go `Config.ReadinessAt` — the snapshot clock for deferral.
    pub fn with_readiness_at(mut self, at: jiff::Timestamp) -> Self {
        self.readiness_at = Some(at);
        self
    }

    /// Go `Config.PageSize`.
    pub fn with_page_size(mut self, page_size: i64) -> Self {
        self.config.page_size = page_size;
        self
    }

    /// Go `Config` wholesale — the chunk threshold and size in particular,
    /// which the CLI leaves at their defaults but tests need to move.
    pub fn with_config(mut self, config: SqliteExportConfig) -> Self {
        self.config = config;
        self
    }

    /// The full Go `Export` sequence: build the database, publish it, then
    /// write the chunk sidecar.
    ///
    /// The static-site group needs `data/*.json` written between those two
    /// steps (Go does the same, from inside `Export`); use
    /// [`Self::export_database`] and [`Self::write_chunk_config`] directly if
    /// you are driving the whole bundle yourself.
    pub fn export(&self, output_dir: &Path) -> Result<()> {
        self.export_database(output_dir)?;
        self.write_chunk_config(output_dir)?;
        Ok(())
    }

    /// Go `Export`, database half only: build into a scratch dir, then rename
    /// into `<output_dir>/beads.sqlite3`.
    pub fn export_database(&self, output_dir: &Path) -> Result<PathBuf> {
        fs::create_dir_all(output_dir)?;
        let data_dir = output_dir.join("data");
        fs::create_dir_all(&data_dir)?;

        let db_path = output_dir.join("beads.sqlite3");
        let scratch = make_scratch_dir(output_dir)?;
        let scratch_db = scratch.join("beads.sqlite3");

        let result = self.build_database(&scratch_db);
        if let Err(e) = result {
            // Drop the scratch tree so a failed run leaves nothing behind. Only
            // the directory this call just created is touched.
            let _ = fs::remove_dir_all(&scratch);
            return Err(e);
        }

        fs::rename(&scratch_db, &db_path).map_err(SqliteExportError::Publish)?;
        let _ = fs::remove_dir_all(&scratch);
        Ok(db_path)
    }

    /// The database build proper — Go `Export` lines 140-203.
    fn build_database(&self, path: &Path) -> Result<()> {
        let conn = Connection::open(path)?;
        disable_foreign_keys(&conn)?;
        create_schema(&conn)?;

        self.insert_issues(&conn)?;
        self.insert_dependencies(&conn)?;
        self.insert_comments(&conn)?;
        self.insert_metrics(&conn)?;
        self.insert_triage_recommendations(&conn)?;

        // Go logs "Warning: FTS5 not available" and continues; the site still
        // works, it just loses client-side full-text search.
        if let Err(e) = create_fts_index(&conn) {
            eprintln!("Warning: FTS5 not available: {e}");
        }

        create_materialized_views(&conn)?;
        self.populate_overview_readiness(&conn)?;
        self.populate_overview_metrics(&conn)?;
        self.insert_meta(&conn)?;

        let mut conn = conn;
        optimize_database(&mut conn, self.config.page_size)?;
        Ok(())
    }

    /// Go `insertIssues`.
    fn insert_issues(&self, conn: &Connection) -> Result<()> {
        let tx = conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO issues (id, title, description, design, acceptance_criteria, notes, \
                 status, priority, issue_type, assignee, labels, created_at, updated_at, closed_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )?;
            for issue in self.issues {
                // Go marshals a non-empty label list and falls back to the
                // literal "[]" otherwise; serde emits the same bytes.
                let labels = serde_json::to_string(&issue.labels)?;
                stmt.execute(params![
                    issue.id,
                    issue.title,
                    issue.description,
                    issue.design,
                    issue.acceptance_criteria,
                    issue.notes,
                    issue.status.as_str(),
                    issue.priority,
                    issue.issue_type,
                    issue.assignee,
                    labels,
                    rfc3339_seconds(issue.created_at.as_deref()),
                    rfc3339_seconds(issue.updated_at.as_deref()),
                    optional_rfc3339_seconds(issue.closed_at.as_deref()),
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Go `insertDependencies`.
    fn insert_dependencies(&self, conn: &Connection) -> Result<()> {
        let tx = conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO dependencies (issue_id, depends_on_id, type) VALUES (?, ?, ?)",
            )?;
            for dep in self.deps {
                stmt.execute(params![
                    dep.issue_id,
                    dep.depends_on_id,
                    dep.r#type.as_str()
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Go `insertComments`.
    ///
    /// The primary key is the composite `"{issue_id}:{comment_id}"`, not
    /// `comment.id` — a workspace with several repos can otherwise collide on
    /// the same numeric comment id.
    fn insert_comments(&self, conn: &Connection) -> Result<()> {
        if self.issues.iter().all(|i| i.comments.is_empty()) {
            return Ok(());
        }
        let tx = conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO comments (id, issue_id, author, text, created_at) \
                 VALUES (?, ?, ?, ?, ?)",
            )?;
            for issue in self.issues {
                for comment in &issue.comments {
                    stmt.execute(params![
                        format!("{}:{}", issue.id, comment.id),
                        issue.id,
                        comment.author,
                        comment.text,
                        rfc3339_seconds(comment.created_at.as_deref()),
                    ])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Go `insertMetrics`.
    ///
    /// `blocks_count` / `blocked_by_count` count an edge only when *both*
    /// endpoints are known issues that are neither closed nor tombstoned, and
    /// only for blocking dependency types. Dropping either guard double-counts
    /// against `issue_overview_mv`, which joins `issues` on both ends.
    fn insert_metrics(&self, conn: &Connection) -> Result<()> {
        let Some(metrics) = self.metrics else {
            return Ok(());
        };

        let mut status_by_id: BTreeMap<&str, bv_core::model::Status> = BTreeMap::new();
        for issue in self.issues {
            status_by_id.insert(issue.id.as_str(), issue.status);
        }
        let is_active_counterpart = |id: &str| match status_by_id.get(id) {
            None => false,
            Some(s) => !s.is_closed() && !s.is_tombstone(),
        };

        let mut blocks_count: BTreeMap<&str, i64> = BTreeMap::new();
        let mut blocked_by_count: BTreeMap<&str, i64> = BTreeMap::new();
        for dep in self.deps {
            if !dep.r#type.is_blocking() {
                continue;
            }
            // `dep.issue_id` depends on `dep.depends_on_id`, so
            // `depends_on_id` blocks `issue_id`.
            if !is_active_counterpart(&dep.depends_on_id) || !is_active_counterpart(&dep.issue_id) {
                continue;
            }
            *blocks_count.entry(dep.depends_on_id.as_str()).or_default() += 1;
            *blocked_by_count.entry(dep.issue_id.as_str()).or_default() += 1;
        }

        let tx = conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO issue_metrics (issue_id, pagerank, betweenness, critical_path_depth, \
                 triage_score, blocks_count, blocked_by_count) VALUES (?, ?, ?, ?, ?, ?, ?)",
            )?;
            for issue in self.issues {
                let pr = metrics.pagerank.get(&issue.id).copied().unwrap_or(0.0);
                let bw = metrics.betweenness.get(&issue.id).copied().unwrap_or(0.0);
                // Go: `int(criticalPathMap[id])` — truncation toward zero.
                let cp = metrics.critical_path.get(&issue.id).copied().unwrap_or(0.0) as i64;
                let score = self.triage_scores.get(&issue.id).copied().unwrap_or(0.0);
                stmt.execute(params![
                    issue.id,
                    pr,
                    bw,
                    cp,
                    score,
                    blocks_count.get(issue.id.as_str()).copied().unwrap_or(0),
                    blocked_by_count
                        .get(issue.id.as_str())
                        .copied()
                        .unwrap_or(0),
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Go `insertTriageRecommendations`.
    fn insert_triage_recommendations(&self, conn: &Connection) -> Result<()> {
        if self.triage.is_empty() {
            return Ok(());
        }
        let tx = conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO triage_recommendations (issue_id, score, action, reasons, \
                 unblocks_ids, blocked_by_ids) VALUES (?, ?, ?, ?, ?, ?)",
            )?;
            for rec in self.triage {
                stmt.execute(params![
                    rec.issue_id,
                    rec.score,
                    rec.action,
                    nullable_json(&rec.reasons)?,
                    nullable_json(&rec.unblocks_ids)?,
                    nullable_json(&rec.blocked_by_ids)?,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Go `populateOverviewReadiness`.
    ///
    /// Visible graph edges alone cannot establish readiness: the materialized
    /// view starts every row at `'unknown'`, and this pass is what writes the
    /// real `dependency_state` / `is_actionable` pair.
    fn populate_overview_readiness(&self, conn: &Connection) -> Result<()> {
        let index = self.readiness_index();
        let now = self.readiness_at.unwrap_or_else(jiff::Timestamp::now);

        let tx = conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "UPDATE issue_overview_mv SET dependency_state = ?, is_actionable = ? WHERE id = ?",
            )?;
            for issue in self.issues {
                let state = index.dependency_state(&issue.id);
                stmt.execute(params![
                    dependency_state_str(state),
                    self.is_ready(&index, issue, now),
                    issue.id,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Go `ReadinessIndex.Ready` — visible, not deferred past the clock, and
    /// with every prerequisite proven closed.
    fn is_ready(&self, index: &ReadinessIndex, issue: &Issue, now: jiff::Timestamp) -> bool {
        if !issue.status.is_open() {
            return false;
        }
        if let Some(defer_until) = issue.defer_until.as_deref() {
            if let Ok(t) = defer_until.parse::<jiff::Timestamp>() {
                if t > now {
                    return false;
                }
            }
        }
        index.dependency_state(&issue.id) == DependencyState::Satisfied
    }

    /// Go `ReadinessIndex` built over the FULL source, not the visible scope.
    ///
    /// Go copies each issue's own `Dependencies` and then appends every entry
    /// from `e.Deps` that points at a known issue. Duplicated edges are
    /// harmless: `compute` folds a node's own edges into that node's own state
    /// and the parent walk is commutative.
    fn readiness_index(&self) -> ReadinessIndex {
        let mut source: Vec<ReadinessIssue> = Vec::with_capacity(self.issues.len());
        let mut positions: BTreeMap<&str, usize> = BTreeMap::new();

        for issue in self.issues {
            positions.insert(issue.id.as_str(), source.len());
            let mut deps: Vec<(String, String)> = issue
                .dependencies
                .iter()
                .map(|d| (d.depends_on_id.clone(), d.r#type.as_str().to_string()))
                .collect();
            // The visible slice is a filter of the same source, so a dep owned
            // by a filtered-out issue is not lost: rebuild from `self.deps`.
            for dep in self.deps {
                if dep.issue_id == issue.id {
                    deps.push((dep.depends_on_id.clone(), dep.r#type.as_str().to_string()));
                }
            }
            source.push(ReadinessIssue {
                id: issue.id.clone(),
                status: issue.status.as_str().to_string(),
                dependencies: deps,
            });
        }

        ReadinessIndex::new(&source)
    }

    /// Go `ReadinessIndex.ResolvedIDs` — sorted ids that release dependants.
    ///
    /// The materialized view drops every edge whose endpoint is closed, so the
    /// export records which ids were resolved to keep that reason explainable
    /// after a filter hides them.
    fn resolved_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .issues
            .iter()
            .filter(|i| closed_for_readiness(i.status.as_str()))
            .map(|i| i.id.clone())
            .collect();
        ids.sort();
        ids
    }

    /// Go `populateOverviewMetrics` — cycle flags. A no-op when there are no
    /// stats or no cycles, exactly as in Go.
    fn populate_overview_metrics(&self, conn: &Connection) -> Result<()> {
        if self.metrics.is_none() || self.cycle_ids.is_empty() {
            return Ok(());
        }
        let tx = conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare("UPDATE issue_overview_mv SET in_cycle = ? WHERE id = ?")?;
            for id in &self.cycle_ids {
                stmt.execute(params![1i64, id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Go `insertMeta`.
    ///
    /// `generated_at` and `readiness_at` are wall-clock stamps, so this table is
    /// the one place `beads.sqlite3` can never be byte-identical to Go's.
    fn insert_meta(&self, conn: &Connection) -> Result<()> {
        let now = jiff::Timestamp::now();
        let readiness_at = self.readiness_at.unwrap_or(now);

        let mut meta: BTreeMap<String, String> = BTreeMap::new();
        meta.insert("version".into(), "1.0.0".into());
        meta.insert("generated_at".into(), rfc3339_seconds_ts(now));
        meta.insert("issue_count".into(), self.issues.len().to_string());
        meta.insert("dependency_count".into(), self.deps.len().to_string());
        meta.insert("schema_version".into(), SCHEMA_VERSION.to_string());
        meta.insert("readiness_at".into(), rfc3339_nano_seconds_ts(readiness_at));
        if !self.git_hash.is_empty() {
            meta.insert("git_commit".into(), self.git_hash.clone());
        }
        if !self.title.is_empty() {
            meta.insert("title".into(), self.title.clone());
        }
        let resolved = self.resolved_ids();
        if !resolved.is_empty() {
            meta.insert(
                "resolved_issue_ids".into(),
                serde_json::to_string(&resolved)?,
            );
        }

        let tx = conn.unchecked_transaction()?;
        for (key, value) in &meta {
            insert_meta_value(&tx, key, value)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Go `chunkIfNeeded` — always writes `beads.sqlite3.config.json`.
    ///
    /// The sha256 is computed unconditionally: it is the viewer's OPFS
    /// cache-invalidation key, so an export below the chunk threshold that
    /// omitted it would leave every deployment sharing one stale cache entry.
    ///
    /// Must run after [`Self::export_database`], and — because the static-site
    /// group folds this file's own hash into `coi-serviceworker.js` — before
    /// the service worker is generated.
    pub fn write_chunk_config(&self, output_dir: &Path) -> Result<PathBuf> {
        let db_path = output_dir.join("beads.sqlite3");
        let db_bytes = fs::read(&db_path)?;
        let total_size = db_bytes.len() as u64;
        let hash = hex(&Sha256::digest(&db_bytes));

        let chunks_dir = output_dir.join("chunks");
        let mut config = ChunkConfig {
            chunked: false,
            chunk_count: 0,
            chunk_size: 0,
            total_size,
            hash: hash.clone(),
            chunks: Vec::new(),
        };

        if total_size < self.config.chunk_threshold {
            // A previous export of a larger database left chunks behind. The
            // config about to be written says `chunked: false`, but the stale
            // files would still be served, so a client holding the prior config
            // would splice them into a malformed image (#175).
            remove_chunk_artifacts(&chunks_dir);
        } else {
            fs::create_dir_all(&chunks_dir)?;
            let chunk_size = self.config.chunk_size.max(1) as usize;
            let mut chunk_count = 0usize;
            for (i, chunk) in db_bytes.chunks(chunk_size).enumerate() {
                let name = format!("{i:05}.bin");
                fs::write(chunks_dir.join(&name), chunk)?;
                chunk_count += 1;
            }
            prune_stale_chunks(&chunks_dir, chunk_count)?;

            config.chunked = true;
            config.chunk_count = chunk_count as i64;
            config.chunk_size = self.config.chunk_size as i64;
            for i in 0..chunk_count {
                let data = fs::read(chunks_dir.join(format!("{i:05}.bin")))?;
                config.chunks.push(ChunkInfo {
                    path: format!("chunks/{i:05}.bin"),
                    hash: hex(&Sha256::digest(&data)),
                    size: data.len() as i64,
                });
            }
        }

        let path = output_dir.join("beads.sqlite3.config.json");
        fs::write(&path, encode_json_2sp(&config))?;
        Ok(path)
    }
}

/// Go `DependencyState`, as stored: lowercase, matching the Rust enum's serde
/// spelling in `bv_correlation::readiness`.
use bv_correlation::readiness::DependencyState;

/// The string `issue_overview_mv.dependency_state` stores. Go's three
/// `DependencyState` constants are `satisfied` / `unsatisfied` / `unknown`.
fn dependency_state_str(state: DependencyState) -> &'static str {
    match state {
        DependencyState::Satisfied => "satisfied",
        DependencyState::Unsatisfied => "unsatisfied",
        DependencyState::Unknown => "unknown",
    }
}

/// Go `export.ChunkInfo`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChunkInfo {
    pub path: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub hash: String,
    #[serde(skip_serializing_if = "is_zero_i64")]
    pub size: i64,
}

/// Go `export.ChunkConfig`. Field order is load-bearing: Go's
/// `encoding/json` emits struct fields in declaration order and the file is on
/// the acceptance list.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChunkConfig {
    pub chunked: bool,
    pub chunk_count: i64,
    pub chunk_size: i64,
    pub total_size: u64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub hash: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub chunks: Vec<ChunkInfo>,
}

fn is_zero_i64(v: &i64) -> bool {
    *v == 0
}

/// Go `encodeChunkConfig` — `json.Encoder` with `SetIndent("", "  ")` and
/// `Encode`, which appends the trailing newline serde's pretty printer omits.
fn encode_json_2sp<T: serde::Serialize>(value: &T) -> Vec<u8> {
    let mut buf = serde_json::to_vec_pretty(value).expect("chunk config is plain data");
    buf.push(b'\n');
    buf
}

/// Go `chunkFileIndex` — five digits plus `.bin`, nothing else. Deliberately
/// strict so cleanup can never touch a file a user placed under `chunks/`.
fn chunk_file_index(name: &str) -> Option<u32> {
    let stem = name.strip_suffix(".bin")?;
    if stem.len() != 5 || !stem.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    stem.parse().ok()
}

/// Go `pruneStaleChunks` — remove `NNNNN.bin` at index `>= keep`. A missing
/// directory is not an error.
fn prune_stale_chunks(chunks_dir: &Path, keep: usize) -> std::io::Result<()> {
    let Ok(entries) = fs::read_dir(chunks_dir) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        match chunk_file_index(name) {
            Some(idx) if idx as usize >= keep => {
                let _ = fs::remove_file(entry.path());
            }
            _ => {}
        }
    }
    Ok(())
}

/// Go `removeChunkArtifacts` — drop this exporter's chunks, then drop the
/// directory itself.
///
/// `remove_dir` only succeeds on an empty directory, so an unrelated user file
/// keeps both itself and the directory. This is never a recursive delete.
fn remove_chunk_artifacts(chunks_dir: &Path) {
    let _ = prune_stale_chunks(chunks_dir, 0);
    let _ = fs::remove_dir(chunks_dir);
}

/// Go `os.MkdirTemp(outputDir, ".beads-*")` — a private sibling directory so an
/// in-progress database is never served.
fn make_scratch_dir(output_dir: &Path) -> Result<PathBuf> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);

    for _ in 0..64 {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = jiff::Timestamp::now().as_nanosecond();
        let candidate = output_dir.join(format!(".beads-{}-{nanos}-{n}", std::process::id()));
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(SqliteExportError::Io(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a .beads-* scratch directory",
    )))
}

/// Go's zero `time.Time` renders as this; `created_at` / `updated_at` are NOT
/// NULL and a legacy record may carry no timestamp at all.
const GO_ZERO_TIME: &str = "0001-01-01T00:00:00Z";

/// Go `t.Format(time.RFC3339)` — second precision, original offset preserved.
///
/// The JSONL carries nanoseconds (`2026-08-22T08:10:32.348671Z`); Go drops the
/// fraction on the way into SQLite, so a raw string pass-through would store
/// bytes the oracle never writes. The fields are assembled by hand rather than
/// through `strftime`, whose `%S` renders the fractional part whenever it is
/// non-zero.
fn rfc3339_seconds(raw: Option<&str>) -> String {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return GO_ZERO_TIME.to_string();
    };
    match raw.parse::<jiff::Timestamp>() {
        // Go's `Z07:00` layout prints "Z" at zero offset and "+07:00" above it,
        // and it prints the LOCAL wall clock — so a record stamped
        // `08:10:32+07:00` is stored as `08:10:32+07:00`, not as its UTC twin.
        // `Timestamp` normalizes to UTC, so the original offset has to be read
        // back off the text to re-render the same fields Go would.
        Ok(ts) => match offset_seconds(raw) {
            Some(0) => format_wall_clock(&ts.to_zoned(jiff::tz::TimeZone::UTC)) + "Z",
            Some(seconds) => match jiff::tz::Offset::from_seconds(seconds) {
                Ok(off) => {
                    let zoned = ts.to_zoned(jiff::tz::TimeZone::fixed(off));
                    format_wall_clock(&zoned) + &format_offset(seconds)
                }
                Err(_) => raw.to_string(),
            },
            None => raw.to_string(),
        },
        // Go's loader would already have rejected the record; keeping the raw
        // text beats losing the row.
        Err(_) => raw.to_string(),
    }
}

/// `YYYY-MM-DDTHH:MM:SS` for a zoned instant, with no fraction and no zone
/// suffix.
fn format_wall_clock(zoned: &jiff::Zoned) -> String {
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        zoned.year(),
        zoned.month(),
        zoned.day(),
        zoned.hour(),
        zoned.minute(),
        zoned.second(),
    )
}

/// The UTC offset in seconds that an RFC3339 string is written in, or `None`
/// when the text carries no offset at all.
///
/// A trailing `Z` and a bare local timestamp both read as UTC; a `+HH:MM` /
/// `-HHMM` suffix after the date reads as itself. The sign is searched for
/// only after the date so a leading `-` in a negative year cannot be mistaken
/// for one.
fn offset_seconds(raw: &str) -> Option<i32> {
    let body = raw.strip_suffix('Z').or_else(|| raw.strip_suffix('z'));
    if body.is_some() {
        return Some(0);
    }
    // Position 10 is past "YYYY-MM-DD" and past the "T" separator.
    let sign_at = raw[10..].find(['+', '-'])? + 10;
    let sign = if &raw[sign_at..sign_at + 1] == "-" {
        -1
    } else {
        1
    };
    let digits: String = raw[sign_at + 1..]
        .chars()
        .filter(char::is_ascii_digit)
        .collect();
    let (hours, minutes) = match digits.len() {
        2 => (digits.parse::<i32>().ok()?, 0),
        4 => (digits[..2].parse().ok()?, digits[2..].parse().ok()?),
        6 => (digits[..2].parse().ok()?, digits[2..4].parse().ok()?),
        _ => return None,
    };
    Some(sign * (hours * 3600 + minutes * 60))
}

/// Go's `Z07:00` layout element: `Z` at zero offset, `+07:00` otherwise.
fn format_offset(seconds: i32) -> String {
    let sign = if seconds < 0 { '-' } else { '+' };
    let abs = seconds.unsigned_abs();
    format!("{sign}{:02}:{:02}", abs / 3600, (abs % 3600) / 60)
}

/// [`rfc3339_seconds`] for a nullable column — SQL NULL, not the zero time.
fn optional_rfc3339_seconds(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| rfc3339_seconds(Some(s)))
}

/// Go `time.Time.Format(time.RFC3339)` for an already-instant clock value.
fn rfc3339_seconds_ts(ts: jiff::Timestamp) -> String {
    ts.strftime("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Go `time.Time.Format(time.RFC3339Nano)` — `2006-01-02T15:04:05.999999999Z07:00`
/// trims trailing zeros, and the whole fraction when it is zero.
fn rfc3339_nano_seconds_ts(ts: jiff::Timestamp) -> String {
    let with_nanos = ts.strftime("%Y-%m-%dT%H:%M:%S.%f").to_string();
    let trimmed = with_nanos.trim_end_matches('0');
    let trimmed = trimmed.strip_suffix('.').unwrap_or(trimmed);
    format!("{trimmed}Z")
}

/// Go `json.Marshal` of a possibly-nil slice: nil marshals to `null`, an empty
/// non-nil slice to `[]`. `None`/`Some(vec![])` carry that distinction.
fn nullable_json(value: &Option<Vec<String>>) -> Result<String> {
    Ok(match value {
        None => "null".to_string(),
        Some(items) => serde_json::to_string(items)?,
    })
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

// ---------------------------------------------------------------------------
// 16 vendor assets — Go `pkg/export/viewer_embed.go:30` `CopyEmbeddedAssets`
// walks `//go:embed viewer_assets` and writes every embedded file to
// `outputDir/<relPath>` verbatim. The relPath is a literal passthrough, so the
// checkout mirrors Go's `viewer_assets/vendor/` layout exactly.
// ---------------------------------------------------------------------------

/// `(relPath, bytes)` for each file under `vendor/`, in the order Go's
/// `fs.WalkDir` yields them (lexical within a directory).
///
/// These are third-party builds plus one prebuilt repo artifact. They are never
/// regenerated: `bv_graph_bg.wasm` and `bv_graph.js` come from a toolchain
/// pinned by `scripts/build_graph_wasm.sh` (nightly-2026-08-31, wasm-bindgen
/// 0.2.128, wasm-opt 132) against a v0.25.0 `bv-graph-wasm/src` that this
/// workspace does not carry, so a rebuild cannot reproduce these bytes.
/// `MANIFEST.json` is copied as-is even though 10 of its 15 recorded hashes are
/// stale upstream — regenerating them would make this one file differ from Go's.
pub const VENDOR_ASSETS: &[(&str, &[u8])] = &[
    (
        "vendor/alpine-collapse.min.js",
        include_bytes!("../assets/viewer_assets/vendor/alpine-collapse.min.js"),
    ),
    (
        "vendor/alpine.min.js",
        include_bytes!("../assets/viewer_assets/vendor/alpine.min.js"),
    ),
    (
        "vendor/bv_graph.js",
        include_bytes!("../assets/viewer_assets/vendor/bv_graph.js"),
    ),
    (
        "vendor/bv_graph_bg.wasm",
        include_bytes!("../assets/viewer_assets/vendor/bv_graph_bg.wasm"),
    ),
    (
        "vendor/chart.umd.min.js",
        include_bytes!("../assets/viewer_assets/vendor/chart.umd.min.js"),
    ),
    (
        "vendor/d3.v7.min.js",
        include_bytes!("../assets/viewer_assets/vendor/d3.v7.min.js"),
    ),
    (
        "vendor/dompurify.min.js",
        include_bytes!("../assets/viewer_assets/vendor/dompurify.min.js"),
    ),
    (
        "vendor/force-graph.min.js",
        include_bytes!("../assets/viewer_assets/vendor/force-graph.min.js"),
    ),
    (
        "vendor/inter-variable.woff2",
        include_bytes!("../assets/viewer_assets/vendor/inter-variable.woff2"),
    ),
    (
        "vendor/jetbrains-mono-regular.woff2",
        include_bytes!("../assets/viewer_assets/vendor/jetbrains-mono-regular.woff2"),
    ),
    (
        "vendor/MANIFEST.json",
        include_bytes!("../assets/viewer_assets/vendor/MANIFEST.json"),
    ),
    (
        "vendor/marked.min.js",
        include_bytes!("../assets/viewer_assets/vendor/marked.min.js"),
    ),
    (
        "vendor/mermaid.min.js",
        include_bytes!("../assets/viewer_assets/vendor/mermaid.min.js"),
    ),
    (
        "vendor/sql-wasm.js",
        include_bytes!("../assets/viewer_assets/vendor/sql-wasm.js"),
    ),
    (
        "vendor/sql-wasm.wasm",
        include_bytes!("../assets/viewer_assets/vendor/sql-wasm.wasm"),
    ),
    (
        "vendor/tailwindcss.js",
        include_bytes!("../assets/viewer_assets/vendor/tailwindcss.js"),
    ),
];

/// Go `CopyEmbeddedAssets`, vendor subtree only.
///
/// Returns the relPaths written, in write order. The static-site group calls
/// this alongside its own assets; the two sets are disjoint, so ordering
/// between them does not matter.
pub fn copy_vendor_assets(output_dir: &Path) -> Result<Vec<String>> {
    let vendor_root = output_dir.join("vendor");
    fs::create_dir_all(&vendor_root)?;

    let mut written = Vec::with_capacity(VENDOR_ASSETS.len());
    for (rel, bytes) in VENDOR_ASSETS {
        let path = output_dir.join(rel);
        let mut file = fs::File::create(&path)?;
        file.write_all(bytes)?;
        written.push((*rel).to_string());
    }

    // Go `WriteGitHubActionsWorkflow` (pkg/export/github.go:787-801) writes
    // `.github/workflows/static.yml` from a Go string constant, not from the
    // embed — so it is not in VENDOR_ASSETS. It is copied verbatim: the
    // `${{ }}` tokens are literal GitHub Actions expressions, not Go
    // template actions, so the file needs no substitution.
    let workflow_dir = output_dir.join(".github").join("workflows");
    fs::create_dir_all(&workflow_dir)?;
    let workflow_path = workflow_dir.join("static.yml");
    fs::File::create(&workflow_path)?.write_all(GITHUB_ACTIONS_WORKFLOW)?;
    written.push(".github/workflows/static.yml".to_string());

    Ok(written)
}

/// Go `GitHubActionsWorkflowContent` (pkg/export/github.go), byte for byte.
/// Ensures Pages deployment triggers explicitly rather than relying on the
/// built-in workflow, which does not always fire on push.
pub const GITHUB_ACTIONS_WORKFLOW: &[u8] =
    include_bytes!("../assets/viewer_assets/.github/workflows/static.yml");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_assets_are_the_documented_sixteen() {
        assert_eq!(VENDOR_ASSETS.len(), 16);
    }

    #[test]
    fn vendor_asset_hashes_match_the_go_oracle() {
        // Recorded from beads_viewer/pkg/export/viewer_assets/vendor at
        // 18afafa; every one of these files is a verbatim copy and must stay
        // one. A mismatch means a reformat, a re-minify, or a bad merge.
        const EXPECTED: &[(&str, &str, usize)] = &[
            (
                "vendor/alpine-collapse.min.js",
                "e3d6b9b551aff52614a4871af8f1eb070558eb7fdd7ea070f1623ac4e36abc78",
                2362,
            ),
            (
                "vendor/alpine.min.js",
                "0516dd9b5417835f370d426ffd85fa38f13fb0725096e9c66c8103901a584f56",
                70836,
            ),
            (
                "vendor/bv_graph.js",
                "51d98027ed7e10befea35fcbb5a589c744dab9be43fb38fef9758d7deefbca12",
                31915,
            ),
            (
                "vendor/bv_graph_bg.wasm",
                "833799c32a00ba2ae16aa6caced25347c709b4b8be2ba9661074d8bd7468076e",
                216094,
            ),
            (
                "vendor/chart.umd.min.js",
                "bbee045032aeaebecde3e82d42dfece093bb10e97fe3a435e6827b91fb47eacb",
                331832,
            ),
            (
                "vendor/d3.v7.min.js",
                "7ec3e12a522415e8ae24fb64d51ce6e763cfb2e9c6b23ca6de41b1ea1f21c39e",
                472990,
            ),
            (
                "vendor/dompurify.min.js",
                "eb55fb3f4f9c213cccceffd62400681e48519cbcb11a26b6a6fd2a4c206a81fd",
                33427,
            ),
            (
                "vendor/force-graph.min.js",
                "d7f73b07c9d0452d20829f3c6bfdc8cfa636b57a0ea7362bef79072e6c0a2f94",
                274043,
            ),
            (
                "vendor/inter-variable.woff2",
                "4bfb027b313b0487d4e2f1c3d2781d4011f8155b46744ad5799077dc103e8f0b",
                23812,
            ),
            (
                "vendor/jetbrains-mono-regular.woff2",
                "f1a7a03672cdd494ce0d5543fac6e4360fe22403c6de297fdc2e55a815f7baff",
                92380,
            ),
            (
                "vendor/MANIFEST.json",
                "f38c73b2b399eb38956c2f52447e3ec9f2e8c1f669aec17ff34df98248263dc7",
                8057,
            ),
            (
                "vendor/marked.min.js",
                "1f2a6819e1ff54fdea2bdbb12be9c076bcbd321bab24d7f6f97595e6f8759543",
                57222,
            ),
            (
                "vendor/mermaid.min.js",
                "14ea436bbe9bd7a5d52a69bd7ae256c9eb1b25963c1f2f30b700a3a491a4fda9",
                3336758,
            ),
            (
                "vendor/sql-wasm.js",
                "791da0e18730479884618a1ce4f32e3008a60fb1c5f7906804d3f5a85016c9a8",
                83791,
            ),
            (
                "vendor/sql-wasm.wasm",
                "d7e61b828523001f26ce0b3f88dabcf6c12e5e6edf80eb4f08b26ac7b946ff88",
                655300,
            ),
            (
                "vendor/tailwindcss.js",
                "71ef0a78a39e154580f927827e5b1edda643eb312cf7b0bc0989a4823f073543",
                690631,
            ),
        ];

        for (rel, want_hash, want_len) in EXPECTED {
            let (found_rel, bytes) = VENDOR_ASSETS
                .iter()
                .find(|(r, _)| r == rel)
                .unwrap_or_else(|| panic!("{rel} not embedded"));
            assert_eq!(*found_rel, *rel);
            assert_eq!(bytes.len(), *want_len, "{rel} length");
            assert_eq!(hex(&Sha256::digest(bytes)), *want_hash, "{rel} sha256");
        }
    }

    #[test]
    fn rfc3339_truncates_to_seconds_and_keeps_offset() {
        // The JSONL carries nanoseconds; Go's time.RFC3339 drops the fraction.
        assert_eq!(
            rfc3339_seconds(Some("2026-08-22T08:10:32.348671Z")),
            "2026-08-22T08:10:32Z"
        );
        assert_eq!(
            rfc3339_seconds(Some("2026-08-22T08:10:32Z")),
            "2026-08-22T08:10:32Z"
        );
        // A non-UTC offset survives, as Go's `Z07:00` layout does.
        assert_eq!(
            rfc3339_seconds(Some("2026-08-22T08:10:32.5+07:00")),
            "2026-08-22T08:10:32+07:00"
        );
        // Absent timestamps become Go's zero time, not NULL — the columns are
        // NOT NULL.
        assert_eq!(rfc3339_seconds(None), GO_ZERO_TIME);
        assert_eq!(rfc3339_seconds(Some("  ")), GO_ZERO_TIME);
        // closed_at is nullable.
        assert_eq!(optional_rfc3339_seconds(None), None);
        assert_eq!(
            optional_rfc3339_seconds(Some("2026-08-22T15:02:35.642421Z")),
            Some("2026-08-22T15:02:35Z".to_string())
        );
    }

    #[test]
    fn rfc3339_nano_trims_trailing_zeros_like_go() {
        // 2026-09-28T14:48:41Z. Go's `2006-01-02T15:04:05.999999999Z07:00`
        // drops trailing zeros in the fraction, and the whole fraction when it
        // is zero.
        let with_nanos = jiff::Timestamp::new(1_790_606_921, 281_826_000).unwrap();
        assert_eq!(
            rfc3339_nano_seconds_ts(with_nanos),
            "2026-09-28T14:48:41.281826Z"
        );
        let whole = jiff::Timestamp::from_second(1_790_606_921).unwrap();
        assert_eq!(rfc3339_nano_seconds_ts(whole), "2026-09-28T14:48:41Z");
        // Trailing zeros inside the fraction are trimmed too.
        let pad = jiff::Timestamp::new(1_790_606_921, 500_000_000).unwrap();
        assert_eq!(rfc3339_nano_seconds_ts(pad), "2026-09-28T14:48:41.5Z");
    }

    #[test]
    fn nullable_json_distinguishes_nil_from_empty() {
        assert_eq!(nullable_json(&None).unwrap(), "null");
        assert_eq!(nullable_json(&Some(vec![])).unwrap(), "[]");
        assert_eq!(nullable_json(&Some(vec!["a".into()])).unwrap(), "[\"a\"]");
    }

    #[test]
    fn chunk_file_index_is_strict() {
        assert_eq!(chunk_file_index("00000.bin"), Some(0));
        assert_eq!(chunk_file_index("00007.bin"), Some(7));
        // Every one of these must be left alone: user files, not ours.
        assert_eq!(chunk_file_index("notes.txt"), None);
        assert_eq!(chunk_file_index("0007.bin"), None);
        assert_eq!(chunk_file_index("000007.bin"), None);
        assert_eq!(chunk_file_index("0000a.bin"), None);
        assert_eq!(chunk_file_index("00000.BIN"), None);
        assert_eq!(chunk_file_index("00000.bin.bak"), None);
    }

    #[test]
    fn chunk_config_omits_empty_optional_fields() {
        let config = ChunkConfig {
            chunked: false,
            chunk_count: 0,
            chunk_size: 0,
            total_size: 305152,
            hash: "2297e1c2".into(),
            chunks: vec![],
        };
        let encoded = String::from_utf8(encode_json_2sp(&config)).unwrap();
        assert_eq!(
            encoded,
            "{\n  \"chunked\": false,\n  \"chunk_count\": 0,\n  \"chunk_size\": 0,\n  \
             \"total_size\": 305152,\n  \"hash\": \"2297e1c2\"\n}\n"
        );
    }
}
