//! Schema DDL for the exported `beads.sqlite3`.
//!
//! Port of Go `pkg/export/sqlite_schema.go` (v0.25.0, commit 18afafa). Every
//! DDL string is copied verbatim from the Go source so `sqlite_master.sql`
//! matches; the only structural difference is the driver — Go uses
//! `modernc.org/sqlite`, this uses `rusqlite` with the bundled C SQLite, which
//! `libsqlite3-sys` 0.30.1 compiles with `-DSQLITE_ENABLE_FTS5` and
//! `-DSQLITE_ENABLE_STAT4`. Both `issues_fts` and `sqlite_stat4` therefore
//! reproduce.

use rusqlite::Connection;

/// Go `export.SchemaVersion`.
pub const SCHEMA_VERSION: i64 = 1;

/// Go `(*SQLiteExporter).Config.PageSize` default. 1024 is tuned for httpvfs
/// range requests, not for throughput — do not "improve" it to 4096.
pub const DEFAULT_PAGE_SIZE: i64 = 1024;

/// Turn off foreign-key enforcement, which Go's driver also leaves off.
///
/// The schema declares `FOREIGN KEY (issue_id) REFERENCES issues(id)` on
/// `dependencies`, but a beads source may legitimately contain an edge whose
/// far endpoint is not in the export — a filtered scope, or a tombstone the
/// loader dropped. Go stores those rows: `modernc.org/sqlite` only issues
/// `pragma foreign_keys` when the DSN asks for it, and `bv` opens the database
/// by bare path. Verified — `bv --export-pages` over a source with an edge to
/// `does-not-exist` exits 0 and writes the row.
///
/// This matters because `libsqlite3-sys` compiles the bundled SQLite with
/// `-DSQLITE_DEFAULT_FOREIGN_KEYS=1`, the opposite of the oracle. Without this
/// pragma the Rust export would abort with `FOREIGN KEY constraint failed` on
/// data Go accepts — a hard failure where the contract says success.
///
/// SQLite ignores the pragma inside a transaction, so this must run before
/// [`create_schema`] opens one.
pub fn disable_foreign_keys(conn: &Connection) -> rusqlite::Result<()> {
    conn.pragma_update(None, "foreign_keys", false)
}

/// Go `CreateSchema` — issues/dependencies/comments, then metrics, then
/// indexes, then `export_meta`, in one transaction.
pub fn create_schema(conn: &Connection) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    create_core_tables(&tx)?;
    create_metrics_tables(&tx)?;
    create_indexes(&tx)?;
    create_meta_table(&tx)?;
    tx.commit()
}

/// Go `createCoreTables`.
fn create_core_tables(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS issues (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            description TEXT,
            design TEXT,
            acceptance_criteria TEXT,
            notes TEXT,
            status TEXT NOT NULL,
            priority INTEGER NOT NULL,
            issue_type TEXT NOT NULL,
            assignee TEXT,
            labels TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            closed_at TEXT
        );
        CREATE TABLE IF NOT EXISTS dependencies (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            issue_id TEXT NOT NULL,
            depends_on_id TEXT NOT NULL,
            type TEXT NOT NULL DEFAULT 'blocks',
            FOREIGN KEY (issue_id) REFERENCES issues(id),
            FOREIGN KEY (depends_on_id) REFERENCES issues(id)
        );
        CREATE TABLE IF NOT EXISTS comments (
            id TEXT PRIMARY KEY,
            issue_id TEXT NOT NULL,
            author TEXT,
            text TEXT NOT NULL,
            created_at TEXT NOT NULL,
            FOREIGN KEY (issue_id) REFERENCES issues(id)
        );
        "#,
    )
}

/// Go `createMetricsTables`.
fn create_metrics_tables(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS issue_metrics (
            issue_id TEXT PRIMARY KEY,
            pagerank REAL DEFAULT 0,
            betweenness REAL DEFAULT 0,
            critical_path_depth INTEGER DEFAULT 0,
            triage_score REAL DEFAULT 0,
            blocks_count INTEGER DEFAULT 0,
            blocked_by_count INTEGER DEFAULT 0,
            FOREIGN KEY (issue_id) REFERENCES issues(id)
        );
        CREATE TABLE IF NOT EXISTS triage_recommendations (
            issue_id TEXT PRIMARY KEY,
            score REAL NOT NULL,
            action TEXT NOT NULL,
            reasons TEXT,
            unblocks_ids TEXT,
            blocked_by_ids TEXT,
            FOREIGN KEY (issue_id) REFERENCES issues(id)
        );
        "#,
    )
}

/// Go `createIndexes`.
fn create_indexes(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        r#"
        CREATE INDEX IF NOT EXISTS idx_issues_status ON issues(status);
        CREATE INDEX IF NOT EXISTS idx_issues_priority ON issues(priority, status);
        CREATE INDEX IF NOT EXISTS idx_issues_updated ON issues(updated_at DESC);
        CREATE INDEX IF NOT EXISTS idx_issues_type_status ON issues(issue_type, status);
        CREATE INDEX IF NOT EXISTS idx_deps_issue ON dependencies(issue_id);
        CREATE INDEX IF NOT EXISTS idx_deps_depends ON dependencies(depends_on_id);
        CREATE INDEX IF NOT EXISTS idx_deps_type ON dependencies(type);
        CREATE INDEX IF NOT EXISTS idx_comments_issue ON comments(issue_id);
        CREATE INDEX IF NOT EXISTS idx_comments_created ON comments(created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_metrics_score ON issue_metrics(triage_score DESC);
        CREATE INDEX IF NOT EXISTS idx_metrics_pagerank ON issue_metrics(pagerank DESC);
        "#,
    )
}

/// Go `createMetaTable`.
fn create_meta_table(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS export_meta (
            key TEXT PRIMARY KEY,
            value TEXT
        );
        "#,
    )
}

/// Go `CreateFTSIndex` — the `issues_fts` external-content table plus a
/// `rebuild` pass.
///
/// Go treats failure here as non-fatal (`Export` logs "Warning: FTS5 not
/// available" and continues), so this returns the error and lets the caller
/// decide rather than aborting the export.
pub fn create_fts_index(conn: &Connection) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        r#"
        CREATE VIRTUAL TABLE IF NOT EXISTS issues_fts USING fts5(
            id,
            title,
            description,
            labels,
            assignee,
            content='issues',
            content_rowid='rowid',
            tokenize='porter unicode61'
        );
        INSERT INTO issues_fts(issues_fts) VALUES('rebuild');
        "#,
    )?;
    tx.commit()
}

/// Go `CreateMaterializedViews` — `issue_overview_mv` (29 columns) and its four
/// indexes.
///
/// The two `GROUP_CONCAT` subqueries are filtered to ACTIVE endpoints so that
/// closing a blocker actually unblocks its dependents in the view. They are
/// reproduced verbatim, including the `ORDER BY` inside each subquery — SQLite
/// ignores ordering on an aggregate unless it is inside the subquery, which is
/// exactly where Go has it.
pub fn create_materialized_views(conn: &Connection) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS issue_overview_mv AS
        SELECT
            i.id,
            i.title,
            i.description,
            i.design,
            i.acceptance_criteria,
            i.notes,
            i.status,
            i.priority,
            i.issue_type,
            i.assignee,
            i.labels,
            i.created_at,
            i.updated_at,
            i.closed_at,
            'unknown' as dependency_state,
            0 as is_actionable,
            COALESCE(m.pagerank, 0) as pagerank,
            COALESCE(m.betweenness, 0) as betweenness,
            COALESCE(m.critical_path_depth, 0) as critical_path_depth,
            COALESCE(m.triage_score, 0) as triage_score,
            COALESCE(m.blocks_count, 0) as blocks_count,
            COALESCE(m.blocked_by_count, 0) as blocked_by_count,
            COALESCE(m.blocked_by_count, 0) as blocker_count,
            COALESCE(m.blocks_count, 0) as dependent_count,
            COALESCE(m.critical_path_depth, 0) as critical_depth,
            0 as in_cycle,
            (SELECT COUNT(*) FROM comments c WHERE c.issue_id = i.id) as comment_count,
                (SELECT GROUP_CONCAT(issue_id) FROM (
                    SELECT d.issue_id AS issue_id
                    FROM dependencies d
                    JOIN issues i2 ON d.issue_id = i2.id
                    WHERE d.depends_on_id = i.id
                      AND d.type IN ('', 'blocks', 'conditional-blocks', 'waits-for')
                      AND i.status NOT IN ('closed', 'tombstone')
                      AND i2.status NOT IN ('closed', 'tombstone')
                    ORDER BY d.issue_id
                )) as blocks_ids,
                (SELECT GROUP_CONCAT(depends_on_id) FROM (
                    SELECT d.depends_on_id AS depends_on_id
                    FROM dependencies d
                    JOIN issues i2 ON d.depends_on_id = i2.id
                    WHERE d.issue_id = i.id
                      AND d.type IN ('', 'blocks', 'conditional-blocks', 'waits-for')
                      AND i.status NOT IN ('closed', 'tombstone')
                      AND i2.status NOT IN ('closed', 'tombstone')
                    ORDER BY d.depends_on_id
                )) as blocked_by_ids
        FROM issues i
        LEFT JOIN issue_metrics m ON i.id = m.issue_id;
        CREATE INDEX IF NOT EXISTS idx_mv_status ON issue_overview_mv(status);
        CREATE INDEX IF NOT EXISTS idx_mv_priority ON issue_overview_mv(priority);
        CREATE INDEX IF NOT EXISTS idx_mv_score ON issue_overview_mv(triage_score DESC);
        CREATE INDEX IF NOT EXISTS idx_mv_actionable ON issue_overview_mv(is_actionable);
        "#,
    )?;
    tx.commit()
}

/// Go `OptimizeDatabase`.
///
/// Go ignores errors on every pragma ("Some pragmas may fail depending on
/// state, continue") and only surfaces a `VACUUM` failure, because the pragmas
/// are advisory tuning. Only `VACUUM` is load-bearing: it must run last, outside
/// any transaction, to defragment the file the viewer downloads whole.
pub fn optimize_database(conn: &mut Connection, page_size: i64) -> rusqlite::Result<()> {
    let page_size = if page_size <= 0 {
        DEFAULT_PAGE_SIZE
    } else {
        page_size
    };

    for stmt in [
        "PRAGMA journal_mode=DELETE",
        &format!("PRAGMA page_size={page_size}"),
        "ANALYZE",
        "PRAGMA optimize",
    ] {
        let _ = conn.execute_batch(stmt);
    }

    // Best-effort: absent when FTS5 creation failed upstream.
    let _ = conn.execute_batch("INSERT INTO issues_fts(issues_fts) VALUES('optimize')");

    conn.execute_batch("VACUUM")
}

/// Go `InsertMetaValue` — one `export_meta` row.
pub fn insert_meta_value(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO export_meta (key, value) VALUES (?, ?)",
        rusqlite::params![key, value],
    )?;
    Ok(())
}

/// Read one `export_meta` value. Used by the parity test, not by the exporter.
pub fn read_meta_value(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    // `query_row` signals "no such row" as an error, and a stored NULL comes
    // back as `Ok(None)`; both mean the same thing here.
    match conn.query_row("SELECT value FROM export_meta WHERE key = ?", [key], |r| {
        r.get::<_, Option<String>>(0)
    }) {
        Ok(value) => Ok(value),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Read a scalar `COUNT(*)` — used by the parity test.
pub fn count_rows(conn: &Connection, table: &str) -> rusqlite::Result<i64> {
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_creates_every_go_table() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        create_fts_index(&conn).unwrap();
        create_materialized_views(&conn).unwrap();

        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap();
        let names: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();

        for expected in [
            "comments",
            "dependencies",
            "export_meta",
            "issue_metrics",
            "issue_overview_mv",
            "issues",
            "issues_fts",
            "issues_fts_config",
            "issues_fts_data",
            "issues_fts_docsize",
            "issues_fts_idx",
            "sqlite_sequence",
            "triage_recommendations",
        ] {
            assert!(
                names.iter().any(|n| n == expected),
                "missing table {expected}; got {names:?}"
            );
        }
    }

    #[test]
    fn schema_creates_every_go_index() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO issues (id,title,status,priority,issue_type,created_at,updated_at)
             VALUES ('a','t','open',0,'task','x','x');",
        )
        .unwrap();
        create_materialized_views(&conn).unwrap();

        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='index' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .unwrap();
        let names: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(
            names,
            vec![
                "idx_comments_created",
                "idx_comments_issue",
                "idx_deps_depends",
                "idx_deps_issue",
                "idx_deps_type",
                "idx_issues_priority",
                "idx_issues_status",
                "idx_issues_type_status",
                "idx_issues_updated",
                "idx_metrics_pagerank",
                "idx_metrics_score",
                "idx_mv_actionable",
                "idx_mv_priority",
                "idx_mv_score",
                "idx_mv_status",
            ]
        );
    }

    #[test]
    fn overview_mv_has_the_29_go_columns_in_order() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO issues (id,title,status,priority,issue_type,created_at,updated_at)
             VALUES ('a','t','open',0,'task','x','x');",
        )
        .unwrap();
        create_materialized_views(&conn).unwrap();

        let stmt = conn
            .prepare("SELECT * FROM issue_overview_mv LIMIT 0")
            .unwrap();
        let cols: Vec<String> = stmt
            .column_names()
            .iter()
            .map(|s| (*s).to_string())
            .collect();

        assert_eq!(cols.len(), 29, "column count: {cols:?}");
        assert_eq!(cols[0], "id");
        assert_eq!(cols[13], "closed_at");
        assert_eq!(cols[14], "dependency_state");
        assert_eq!(cols[15], "is_actionable");
        assert_eq!(cols[22], "blocker_count");
        assert_eq!(cols[23], "dependent_count");
        assert_eq!(cols[24], "critical_depth");
        assert_eq!(cols[25], "in_cycle");
        assert_eq!(cols[26], "comment_count");
        assert_eq!(cols[27], "blocks_ids");
        assert_eq!(cols[28], "blocked_by_ids");
    }

    #[test]
    fn fts_index_is_queryable() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO issues (id,title,description,labels,assignee,status,priority,issue_type,created_at,updated_at)
             VALUES ('a','pagerank tuning','how to tune','[\"perf\"]','','open',0,'task','x','x');",
        )
        .unwrap();
        create_fts_index(&conn).unwrap();

        let hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM issues_fts WHERE issues_fts MATCH 'pagerank'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1);
    }

    #[test]
    fn read_meta_value_distinguishes_absent_from_null() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        assert_eq!(read_meta_value(&conn, "nope").unwrap(), None);
        insert_meta_value(&conn, "version", "1.0.0").unwrap();
        assert_eq!(
            read_meta_value(&conn, "version").unwrap(),
            Some("1.0.0".to_string())
        );
    }

    #[test]
    fn optimize_sets_page_size_and_leaves_no_wal() {
        let dir = tempdir();
        let path = dir.join("beads.sqlite3");
        let mut conn = Connection::open(&path).unwrap();
        create_schema(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO issues (id,title,status,priority,issue_type,created_at,updated_at)
             VALUES ('a','t','open',0,'task','x','x');",
        )
        .unwrap();
        optimize_database(&mut conn, DEFAULT_PAGE_SIZE).unwrap();

        let page_size: i64 = conn
            .query_row("PRAGMA page_size", [], |r| r.get(0))
            .unwrap();
        let journal: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(page_size, 1024);
        assert_eq!(journal, "delete");
    }

    /// Minimal scratch directory helper; `tempfile` is not a bv-export
    /// dependency and these tests only need one unique path each.
    fn tempdir() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("bvr-schema-test-{}-{}", std::process::id(), n));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
