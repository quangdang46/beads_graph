//! Tests for the `beads.sqlite3` writer and the vendored asset tree.
//!
//! Two layers:
//!
//! * hermetic cases that run everywhere (schema, row counts, chunking, vendor
//!   bytes) — these are the gate;
//! * a differential comparison against the Go oracle, skipped unless
//!   `BVR_GO_PAGES` points at a directory produced by
//!   `bv --db .beads/issues.jsonl --export-pages <dir>`. Run it with:
//!
//!   ```text
//!   BVR_GO_PAGES=/tmp/gopages cargo test -p bv-export --test pages_sqlite_parity
//!   ```
//!
//! `beads.sqlite3` itself is never byte-compared: it embeds `generated_at` and
//! `readiness_at` wall-clock stamps and is `VACUUM`ed by a different SQLite
//! build, so the contract is schema + rows + query equivalence.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use bv_core::model::{Comment, Dependency, DependencyType, Issue, Status};
use bv_export::sqlite_export::{
    copy_vendor_assets, MetricTables, SqliteExportConfig, SqliteExporter, VENDOR_ASSETS,
};
use bv_export::sqlite_schema::count_rows;
use rusqlite::Connection;

/// The tables a Go export always contains, in the order the parity check
/// reports them.
const CORE_TABLES: [&str; 8] = [
    "issues",
    "dependencies",
    "comments",
    "issue_metrics",
    "triage_recommendations",
    "export_meta",
    "issue_overview_mv",
    "issues_fts",
];

fn scratch(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "bvr-pages-sqlite-{}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed),
        name
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn issue(id: &str, status: Status, deps: &[(&str, &str)], comments: Vec<Comment>) -> Issue {
    Issue {
        id: id.into(),
        content_hash: String::new(),
        title: format!("title {id}"),
        description: "desc".into(),
        design: String::new(),
        acceptance_criteria: String::new(),
        notes: String::new(),
        status,
        priority: 2,
        issue_type: "task".into(),
        assignee: String::new(),
        estimated_minutes: None,
        // Nanosecond precision, as the JSONL carries it — the writer must
        // truncate to seconds the way Go's `time.RFC3339` does.
        created_at: Some("2026-08-22T08:10:32.348671Z".into()),
        updated_at: Some("2026-08-22T15:02:35.642499Z".into()),
        due_date: None,
        defer_until: None,
        closed_at: (status.is_closed()).then(|| "2026-08-22T15:02:35.642421Z".into()),
        external_ref: None,
        compaction_level: 0,
        compacted_at: None,
        compacted_at_commit: None,
        original_size: 0,
        labels: if id == "a" {
            vec!["perf".into(), "parity".into()]
        } else {
            vec![]
        },
        dependencies: deps
            .iter()
            .map(|(target, ty)| Dependency {
                issue_id: id.into(),
                depends_on_id: (*target).into(),
                depends_on_legacy: String::new(),
                target_id_legacy: String::new(),
                r#type: DependencyType::parse(ty),
                created_at: None,
                created_by: String::new(),
            })
            .collect(),
        comments,
        source_repo: String::new(),
    }
}

fn comment(id: &str) -> Comment {
    Comment {
        id: id.into(),
        issue_id: String::new(),
        author: "someone".into(),
        text: "note".into(),
        created_at: Some("2026-09-25T19:02:15.123456Z".into()),
    }
}

/// A graph with the shapes that are easy to get wrong: a closed blocker that
/// must drop out of the counts, a live edge that must not, a `parent-child`
/// link that is not blocking, and a dangling reference to an unknown issue.
fn fixture() -> (Vec<Issue>, Vec<Dependency>) {
    let issues = vec![
        issue("a", Status::Closed, &[("b", "blocks")], vec![comment("1")]),
        issue("b", Status::Open, &[("c", "blocks")], vec![comment("2")]),
        issue("c", Status::Open, &[], vec![]),
        // parent-child is not a blocking edge, so it must not reach the table
        // and must not move any blocks/blocked_by count.
        issue("d", Status::Open, &[("c", "parent-child")], vec![]),
        // Depends on an issue that is not in the export at all.
        issue("e", Status::Open, &[("missing", "blocks")], vec![]),
    ];
    let deps: Vec<Dependency> = issues
        .iter()
        .flat_map(|i| i.dependencies.iter())
        .filter(|d| d.r#type.is_blocking())
        .cloned()
        .collect();
    (issues, deps)
}

#[test]
fn hermetic_row_counts_and_schema() {
    let (issues, deps) = fixture();
    let out = scratch("counts");
    let metrics = MetricTables::default();
    SqliteExporter::new(&issues, &deps)
        .with_metrics(&metrics)
        .export(&out)
        .unwrap();

    let conn = Connection::open(out.join("beads.sqlite3")).unwrap();

    assert_eq!(count_rows(&conn, "issues").unwrap(), 5);
    assert_eq!(count_rows(&conn, "dependencies").unwrap(), 3);
    assert_eq!(count_rows(&conn, "comments").unwrap(), 2);
    assert_eq!(count_rows(&conn, "issue_metrics").unwrap(), 5);
    assert_eq!(count_rows(&conn, "issue_overview_mv").unwrap(), 5);
    assert_eq!(count_rows(&conn, "issues_fts").unwrap(), 5);
    assert_eq!(count_rows(&conn, "triage_recommendations").unwrap(), 0);

    // `data/` is created so the static-site group can write its JSON beside it.
    assert!(out.join("data").is_dir());
    // The scratch directory Go uses must not survive the export.
    let leftovers: Vec<String> = std::fs::read_dir(&out)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".beads-"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "scratch dirs left behind: {leftovers:?}"
    );

    // page_size / journal_mode are tuned for httpvfs range requests.
    let page_size: i64 = conn
        .query_row("PRAGMA page_size", [], |r| r.get(0))
        .unwrap();
    let journal: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(page_size, 1024);
    assert_eq!(journal, "delete");
}

#[test]
fn timestamps_are_truncated_to_seconds_like_go() {
    let (issues, deps) = fixture();
    let out = scratch("timestamps");
    SqliteExporter::new(&issues, &deps).export(&out).unwrap();
    let conn = Connection::open(out.join("beads.sqlite3")).unwrap();

    let created: String = conn
        .query_row("SELECT created_at FROM issues WHERE id='a'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(created, "2026-08-22T08:10:32Z");

    // closed_at is nullable and stays NULL when the issue is open.
    let closed: Option<String> = conn
        .query_row("SELECT closed_at FROM issues WHERE id='b'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(closed, None);
    let closed_a: Option<String> = conn
        .query_row("SELECT closed_at FROM issues WHERE id='a'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(closed_a.as_deref(), Some("2026-08-22T15:02:35Z"));

    // Comments use the composite primary key, not comment.id.
    let ids: Vec<String> = conn
        .prepare("SELECT id FROM comments ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(ids, vec!["a:1".to_string(), "b:2".to_string()]);
}

#[test]
fn blocking_counts_ignore_closed_dangling_and_non_blocking_edges() {
    let (issues, deps) = fixture();
    let out = scratch("counts2");
    let metrics = MetricTables::default();
    SqliteExporter::new(&issues, &deps)
        .with_metrics(&metrics)
        .export(&out)
        .unwrap();
    let conn = Connection::open(out.join("beads.sqlite3")).unwrap();

    let read = |id: &str| -> (i64, i64) {
        conn.query_row(
            "SELECT blocks_count, blocked_by_count FROM issue_metrics WHERE issue_id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    };

    // a -> b -> c, all live except `a` which is closed.
    // `b` blocks `a` but `a` is closed, so `b` counts 0 blocks.
    assert_eq!(
        read("b"),
        (0, 1),
        "b is blocked by nothing live; a is closed"
    );
    assert_eq!(read("c"), (1, 0), "c blocks b");
    // `d`'s only edge is parent-child, which is not blocking.
    assert_eq!(read("d"), (0, 0), "parent-child must not count");
    // `e` depends on an issue outside the export; the edge is dangling.
    assert_eq!(read("e"), (0, 0), "a dangling edge must not count");
}

#[test]
fn readiness_and_resolved_ids_land_in_the_view_and_meta() {
    let (issues, deps) = fixture();
    let out = scratch("readiness");
    SqliteExporter::new(&issues, &deps)
        .with_title("Sprint 42")
        .with_git_hash("deadbeef")
        .export(&out)
        .unwrap();
    let conn = Connection::open(out.join("beads.sqlite3")).unwrap();

    let state = |id: &str| -> (String, i64) {
        conn.query_row(
            "SELECT dependency_state, is_actionable FROM issue_overview_mv WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    };

    // `b` depends on the live `c`, so it is unsatisfied and not actionable.
    assert_eq!(state("b"), ("unsatisfied".to_string(), 0));
    // `c` has no edges at all: satisfied, and open, so actionable.
    assert_eq!(state("c"), ("satisfied".to_string(), 1));
    // `e` points at an unknown issue — incomplete data reads as unknown.
    assert_eq!(state("e"), ("unknown".to_string(), 0));
    // A closed issue is never actionable however clean its graph is.
    assert_eq!(state("a"), ("satisfied".to_string(), 0));

    let meta = |k: &str| -> Option<String> {
        conn.query_row("SELECT value FROM export_meta WHERE key = ?1", [k], |r| {
            r.get::<_, Option<String>>(0)
        })
        .unwrap()
    };
    assert_eq!(meta("version").as_deref(), Some("1.0.0"));
    assert_eq!(meta("schema_version").as_deref(), Some("1"));
    assert_eq!(meta("issue_count").as_deref(), Some("5"));
    assert_eq!(meta("dependency_count").as_deref(), Some("3"));
    assert_eq!(meta("title").as_deref(), Some("Sprint 42"));
    assert_eq!(meta("git_commit").as_deref(), Some("deadbeef"));
    // Only `a` is closed, so only it releases dependants.
    assert_eq!(meta("resolved_issue_ids").as_deref(), Some("[\"a\"]"));

    // readiness_at and generated_at are RFC3339; the nano form trims to no
    // fraction when the sub-second part is zero, which is clock-dependent, so
    // only the shape is asserted.
    let at = meta("readiness_at").unwrap();
    assert!(
        at.ends_with('Z') && at.contains('T'),
        "bad readiness_at: {at}"
    );
}

#[test]
fn below_threshold_export_writes_an_unchunked_config_and_cleans_stale_chunks() {
    let (issues, deps) = fixture();
    let out = scratch("chunk-low");
    let exporter = SqliteExporter::new(&issues, &deps);

    // Simulate a previous, larger export: a chunks/ directory with two chunks
    // and an unrelated user file.
    let chunks = out.join("chunks");
    std::fs::create_dir_all(&chunks).unwrap();
    std::fs::write(chunks.join("00000.bin"), b"stale").unwrap();
    std::fs::write(chunks.join("00001.bin"), b"stale").unwrap();

    exporter.export(&out).unwrap();

    let cfg = std::fs::read_to_string(out.join("beads.sqlite3.config.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&cfg).unwrap();
    assert_eq!(v["chunked"], serde_json::json!(false));
    assert_eq!(v["chunk_count"], serde_json::json!(0));
    assert_eq!(v["chunk_size"], serde_json::json!(0));
    assert!(v["total_size"].as_u64().unwrap() > 0);
    assert_eq!(v["hash"].as_str().unwrap().len(), 64);
    // `chunks` is omitted entirely when unchunked, matching Go's `omitempty`.
    assert!(
        v.get("chunks").is_none(),
        "unchunked config must omit chunks"
    );
    // 2-space indent and a trailing newline, as Go's json.Encoder writes.
    assert!(
        cfg.starts_with("{\n  \"chunked\""),
        "unexpected indent: {cfg}"
    );
    assert!(cfg.ends_with("}\n"), "missing trailing newline");

    // The stale chunks are gone and the directory went with them.
    assert!(!chunks.exists(), "stale chunk directory survived");
}

#[test]
fn above_threshold_export_chunks_splits_and_prunes() {
    let (issues, deps) = fixture();
    let out = scratch("chunk-high");

    // A tiny threshold forces the chunked path on a small database.
    let cfg = SqliteExportConfig {
        chunk_threshold: 1,
        chunk_size: 1024,
        ..Default::default()
    };
    let exporter = SqliteExporter::new(&issues, &deps).with_config(cfg);

    // Leave an orphan from a previous run; it must be pruned, and a file that
    // is not `NNNNN.bin` must be left alone.
    let chunks = out.join("chunks");
    std::fs::create_dir_all(&chunks).unwrap();
    std::fs::write(chunks.join("99999.bin"), b"orphan").unwrap();
    std::fs::write(chunks.join("notes.txt"), b"keep me").unwrap();

    exporter.export(&out).unwrap();

    let v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(out.join("beads.sqlite3.config.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(v["chunked"], serde_json::json!(true));
    assert_eq!(v["chunk_size"], serde_json::json!(1024));
    let chunk_count = v["chunk_count"].as_i64().unwrap();
    assert!(
        chunk_count >= 2,
        "expected several 1 KiB chunks, got {chunk_count}"
    );

    // Every declared chunk exists, hashes to its recorded sha256, and the
    // concatenation reproduces the database.
    let listed = v["chunks"].as_array().unwrap();
    assert_eq!(listed.len() as i64, chunk_count);
    let mut rebuilt = Vec::new();
    for (i, c) in listed.iter().enumerate() {
        assert_eq!(c["path"], serde_json::json!(format!("chunks/{i:05}.bin")));
        let bytes = std::fs::read(out.join(c["path"].as_str().unwrap())).unwrap();
        assert_eq!(c["size"].as_u64().unwrap(), bytes.len() as u64);
        assert_eq!(c["hash"].as_str().unwrap().len(), 64);
        rebuilt.extend_from_slice(&bytes);
    }
    assert_eq!(rebuilt, std::fs::read(out.join("beads.sqlite3")).unwrap());

    // The orphan from the higher-numbered previous run is gone; the user's
    // file is not.
    assert!(!chunks.join("99999.bin").exists(), "stale chunk not pruned");
    assert!(chunks.join("notes.txt").exists(), "user file was deleted");
}

#[test]
fn vendor_assets_are_sixteen_files_written_verbatim() {
    let out = scratch("vendor");
    let written = copy_vendor_assets(&out).unwrap();

    assert_eq!(written.len(), 16);
    assert_eq!(written.len(), VENDOR_ASSETS.len());
    for (rel, bytes) in VENDOR_ASSETS {
        let path = out.join(rel);
        assert_eq!(std::fs::read(&path).unwrap(), *bytes, "{rel} was rewritten");
    }

    // Sizes are the acceptance list's; a reflowed or re-minified asset would
    // move here first. The per-file sha256 test above is the tighter gate.
    let total: usize = VENDOR_ASSETS.iter().map(|(_, b)| b.len()).sum();
    assert_eq!(total, 6_381_450, "vendor tree size changed");
}

#[test]
fn export_creates_no_database_when_the_write_fails() {
    // A read-only parent must fail before anything is published, so a good
    // bundle in place is never replaced by a broken one.
    let (issues, deps) = fixture();
    let out = scratch("readonly");
    let good = SqliteExporter::new(&issues, &deps);
    good.export(&out).unwrap();
    let before = std::fs::read(out.join("beads.sqlite3")).unwrap();

    let blocked = out.join("blocked");
    std::fs::create_dir_all(&blocked).unwrap();
    let mut perms = std::fs::metadata(&blocked).unwrap().permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o500);
    }
    std::fs::set_permissions(&blocked, perms).unwrap();

    let result = SqliteExporter::new(&issues, &deps).export(&blocked);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&blocked).unwrap().permissions();
        perms.set_mode(0o700);
        std::fs::set_permissions(&blocked, perms).unwrap();
    }
    assert!(
        result.is_err(),
        "export into a read-only directory should fail"
    );
    assert!(!blocked.join("beads.sqlite3").exists());

    // The good bundle is untouched.
    assert_eq!(std::fs::read(out.join("beads.sqlite3")).unwrap(), before);
}

// ---------------------------------------------------------------------------
// Differential layer — runs only against a real Go export.
// ---------------------------------------------------------------------------

fn go_pages_dir() -> Option<PathBuf> {
    std::env::var_os("BVR_GO_PAGES")
        .map(PathBuf::from)
        .filter(|p| p.join("beads.sqlite3").exists())
}

#[test]
fn matches_go_oracle() {
    let Some(go_dir) = go_pages_dir() else {
        eprintln!("BVR_GO_PAGES unset; skipping the differential comparison");
        return;
    };

    // `cargo test` runs with the crate directory as cwd, so resolve the
    // workspace's own JSONL from the manifest rather than from `.`.
    let jsonl = std::env::var_os("BVR_JSONL")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../.beads/issues.jsonl")
                .canonicalize()
                .unwrap_or_else(|_| PathBuf::from(".beads/issues.jsonl"))
        });
    let file = std::fs::File::open(&jsonl).unwrap_or_else(|e| panic!("BVR_JSONL {jsonl:?}: {e}"));
    let mut reader = std::io::BufReader::new(file);
    let (issues, _stats) =
        bv_core::loader::parse_issues_with_options(&mut reader, &Default::default(), |_| {})
            .unwrap();

    // Go's driver (cmd/bv/main.go:3128-3139) hands the exporter blocking edges
    // only; `parent-child` links never reach the `dependencies` table.
    let deps: Vec<Dependency> = issues
        .iter()
        .flat_map(|i| i.dependencies.iter())
        .filter(|d| d.r#type.is_blocking())
        .cloned()
        .collect();

    let out = scratch("differential");
    let metrics = MetricTables::default();
    SqliteExporter::new(&issues, &deps)
        .with_metrics(&metrics)
        .export(&out)
        .unwrap();

    let ours = Connection::open(out.join("beads.sqlite3")).unwrap();
    let theirs = Connection::open(go_dir.join("beads.sqlite3")).unwrap();

    let table_names = |c: &Connection| -> Vec<String> {
        let mut stmt = c
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap();
        stmt.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    assert_eq!(
        table_names(&ours),
        table_names(&theirs),
        "table set differs"
    );

    for t in CORE_TABLES {
        assert_eq!(
            count_rows(&ours, t).unwrap(),
            count_rows(&theirs, t).unwrap(),
            "row count differs for {t}"
        );
    }

    for p in ["page_size", "user_version", "application_id", "encoding"] {
        let a: rusqlite::types::Value = ours
            .query_row(&format!("PRAGMA {p}"), [], |r| r.get(0))
            .unwrap();
        let b: rusqlite::types::Value = theirs
            .query_row(&format!("PRAGMA {p}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(a, b, "pragma {p} differs");
    }

    // Every column of every issue row, byte for byte. This is the check that
    // catches timestamp precision, label encoding and NULL-vs-empty drift.
    let issue_rows = |c: &Connection| -> BTreeMap<String, Vec<String>> {
        let mut stmt = c
            .prepare(
                "SELECT id,title,description,design,acceptance_criteria,notes,status,\
                 CAST(priority AS TEXT),issue_type,assignee,labels,created_at,updated_at,closed_at \
                 FROM issues ORDER BY id",
            )
            .unwrap();
        stmt.query_map([], |r| {
            let mut v: Vec<String> = Vec::with_capacity(14);
            for i in 0..14 {
                v.push(r.get::<_, Option<String>>(i)?.unwrap_or_default());
            }
            Ok((v[0].clone(), v))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
    };
    let our_issues = issue_rows(&ours);
    let go_issues = issue_rows(&theirs);
    assert_eq!(our_issues.len(), go_issues.len(), "issue count differs");
    for (id, a) in &our_issues {
        let b = go_issues
            .get(id)
            .unwrap_or_else(|| panic!("{id} missing from the Go export"));
        for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
            assert_eq!(x, y, "issue {id} column {i} differs");
        }
    }

    let string_rows = |c: &Connection, sql: &str| -> Vec<Vec<String>> {
        let mut stmt = c.prepare(sql).unwrap();
        let n = stmt.column_count();
        stmt.query_map([], |r| {
            let mut v: Vec<String> = Vec::with_capacity(n);
            for i in 0..n {
                v.push(r.get::<_, Option<String>>(i)?.unwrap_or_default());
            }
            Ok(v)
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
    };
    assert_eq!(
        string_rows(
            &ours,
            "SELECT issue_id,depends_on_id,type FROM dependencies \
             ORDER BY issue_id,depends_on_id,type"
        ),
        string_rows(
            &theirs,
            "SELECT issue_id,depends_on_id,type FROM dependencies \
             ORDER BY issue_id,depends_on_id,type"
        ),
        "dependencies differ"
    );
    assert_eq!(
        string_rows(
            &ours,
            "SELECT id,issue_id,author,text,created_at FROM comments ORDER BY id"
        ),
        string_rows(
            &theirs,
            "SELECT id,issue_id,author,text,created_at FROM comments ORDER BY id"
        ),
        "comments differ (composite id / timestamp precision)"
    );

    let meta_keys = |c: &Connection| -> Vec<String> {
        let mut stmt = c
            .prepare("SELECT key FROM export_meta ORDER BY key")
            .unwrap();
        stmt.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    assert_eq!(
        meta_keys(&ours),
        meta_keys(&theirs),
        "export_meta keys differ"
    );

    // The queries the exported viewer actually runs.
    for sql in [
        "SELECT COUNT(*) FROM issues_fts WHERE issues_fts MATCH 'pagerank'",
        "SELECT COUNT(*) FROM issues WHERE status NOT IN ('closed','tombstone')",
        "SELECT COUNT(*) FROM issue_overview_mv WHERE blocks_ids IS NOT NULL",
        "SELECT COUNT(*) FROM issue_overview_mv WHERE is_actionable=1",
        "SELECT COUNT(*) FROM issue_overview_mv WHERE dependency_state='satisfied'",
    ] {
        let a: i64 = ours.query_row(sql, [], |r| r.get(0)).unwrap();
        let b: i64 = theirs.query_row(sql, [], |r| r.get(0)).unwrap();
        assert_eq!(a, b, "query differs: {sql}\n ours={a} go={b}");
    }

    // Vendor tree, compared against what Go actually shipped.
    copy_vendor_assets(&out).unwrap();
    for (rel, _) in VENDOR_ASSETS {
        assert_eq!(
            std::fs::read(out.join(rel)).unwrap(),
            std::fs::read(go_dir.join(rel)).unwrap(),
            "{rel} differs from the Go export"
        );
    }
}

#[test]
fn config_sidecar_matches_the_go_shape() {
    let Some(go_dir) = go_pages_dir() else {
        return;
    };
    let go_cfg = std::fs::read_to_string(go_dir.join("beads.sqlite3.config.json")).unwrap();

    let (issues, deps) = fixture();
    let out = scratch("config-shape");
    SqliteExporter::new(&issues, &deps).export(&out).unwrap();
    let mine = std::fs::read_to_string(out.join("beads.sqlite3.config.json")).unwrap();

    // The hash and total_size track this database, so only the shape is shared.
    let shape = |s: &str| -> String {
        s.lines()
            .map(|l| {
                if l.contains("\"hash\"") {
                    "  \"hash\": \"<64 hex>\"".to_string()
                } else if l.contains("\"total_size\"") {
                    "  \"total_size\": <n>".to_string()
                } else {
                    l.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(shape(&go_cfg), shape(&mine), "config shape differs");
    assert!(!Path::new(&out.join("chunks")).exists());
}
