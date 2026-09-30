//! Per-keystroke cost of semantic search (Ctrl+S).
//!
//! Semantic ranking is the one key path that is not O(viewport): it scores
//! every row, so anything it does per row is multiplied by the size of the
//! issue set. Before the document cache, every `Char` keypress rebuilt the
//! Go `IssueDocument` text for *every* issue, lowercased it, and tokenized it
//! into a fresh `Vec<String>` — on a 171-issue / 3.4 MB dataset that is
//! ~60 ms per character, which is four dropped frames before the first
//! character of a query has even been matched.
//!
//! These are wall-clock guards with a deliberately loose ceiling; they exist
//! to catch a regression back to "re-derive every document per keystroke",
//! not to police microseconds. Run with `--release`:
//!
//! ```sh
//! cargo test --release -p bv-tui --test semantic_search_cost_test -- --ignored --nocapture
//! BV_BENCH_REPO=../ultraworkers cargo test --release -p bv-tui \
//!     --test semantic_search_cost_test -- --ignored --nocapture
//! ```

use bv_tui::App;
use crossterm::event::KeyCode;
use std::time::Instant;

/// Mean wall-clock cost of one character typed into semantic search.
///
/// Times the `Char` arm only: `Backspace` onto an empty query short-circuits
/// `apply_semantic` (an empty query restores the unfiltered order without
/// touching a single document), so including it would flatter the average.
fn keystroke_ms(app: &mut App, query: &str, iters: u32) -> f64 {
    app.semantic_searching = true;
    app.semantic_query.clear();
    // Warm up: the first Ctrl+S builds the persistent index and computes the
    // `data_hash` used to decide whether it is stale. Both are memoized, so
    // only the very first keystroke pays for them and none should.
    for c in query.chars() {
        app.handle_key(KeyCode::Char(c));
    }

    let mut total = std::time::Duration::ZERO;
    for _ in 0..iters {
        app.semantic_query.clear();
        for c in query.chars() {
            let t = Instant::now();
            app.handle_key(KeyCode::Char(c));
            total += t.elapsed();
        }
    }
    total.as_secs_f64() * 1000.0 / f64::from(iters * query.chars().count() as u32)
}

/// A fat synthetic issue: the shape that made the original dataset
/// pathological — a record carrying kilobytes of prose.
fn fat_issue(i: usize) -> bv_core::model::Issue {
    bv_core::model::Issue {
        id: format!("T-{i:05}"),
        content_hash: String::new(),
        title: format!("Issue number {i} with a reasonably long title to exercise truncation"),
        description: "prose ".repeat(2_000),
        design: String::new(),
        acceptance_criteria: "criterion ".repeat(500),
        notes: "note ".repeat(500),
        status: bv_core::model::Status::Open,
        priority: 2,
        issue_type: "task".into(),
        assignee: String::new(),
        estimated_minutes: None,
        created_at: None,
        updated_at: None,
        due_date: None,
        defer_until: None,
        closed_at: None,
        external_ref: None,
        compaction_level: 0,
        compacted_at: None,
        compacted_at_commit: None,
        original_size: 0,
        labels: vec!["backend".into()],
        dependencies: vec![],
        comments: vec![],
        source_repo: String::new(),
    }
}

#[test]
#[ignore = "wall-clock perf guard; run explicitly with `cargo test --release -- --ignored`"]
fn keystroke_cost_stays_flat_as_the_dataset_grows() {
    // 40 rows and 400 rows differ by 10x in data. Nothing a keystroke does
    // should be proportional to issue *count* — the cost that matters is
    // proportional to the text actually scanned, and the doc strings no
    // longer get rebuilt from scratch on every character.
    let mut small = App::new((0..40).map(fat_issue).collect());
    let mut large = App::new((0..400).map(fat_issue).collect());
    let small_ms = keystroke_ms(&mut small, "graph", 10);
    let large_ms = keystroke_ms(&mut large, "graph", 10);

    println!("semantic keystroke vs dataset size\n  40 rows: {small_ms:6.2} ms   400 rows: {large_ms:6.2} ms   ratio: {:.2}x",
        large_ms / small_ms.max(0.001));

    assert!(
        large_ms < 15.0,
        "one keystroke took {large_ms:.2} ms on 400 fat issues — documents are being rebuilt per keypress again\n\
         (40 rows: {small_ms:.2} ms)"
    );
}

/// Stage-by-stage breakdown of one keystroke, so a future regression can be
/// attributed to a stage rather than guessed at. Print-only.
#[test]
#[ignore = "needs a real repo; set BV_BENCH_REPO to a project root"]
fn keystroke_breakdown() {
    let repo = std::env::var("BV_BENCH_REPO").expect("set BV_BENCH_REPO");
    let cwd = std::path::PathBuf::from(&repo);
    let (issues, _) = bv_core::discovery::load_issues_from_repo(&cwd).expect("load");
    let app = App::new(issues);
    let dim = bv_search::embedder::DEFAULT_DIM;

    let stage = |name: &str, iters: u32, f: &mut dyn FnMut()| {
        f();
        let t = Instant::now();
        for _ in 0..iters {
            f();
        }
        println!(
            "  {name:<28} {:8.2} ms",
            t.elapsed().as_secs_f64() * 1000.0 / f64::from(iters)
        );
    };

    let refs: Vec<&bv_core::model::Issue> = app.issue_map.values().collect();
    stage("data_hash", 10, &mut || {
        let _ = bv_core::data_hash::compute_data_hash_refs(&refs);
    });
    stage("build docs", 5, &mut || {
        for i in app.issue_map.values() {
            let _ = bv_search::query::issue_document(&i.id, &i.title, &i.labels, &i.description);
        }
    });
    stage("lowercase every doc", 10, &mut || {
        for d in app.semantic_docs().values() {
            let _ = std::hint::black_box(d.text.to_lowercase());
        }
    });
    stage("lexical boost over docs", 10, &mut || {
        for d in app.semantic_docs().values() {
            let _ = std::hint::black_box(d.lexical.boost("graph"));
        }
    });
    stage("lexical boost, uncached", 10, &mut || {
        for d in app.semantic_docs().values() {
            let _ = std::hint::black_box(bv_search::query::short_query_lexical_boost(
                "graph", &d.text,
            ));
        }
    });
    stage("substring search over docs", 10, &mut || {
        for d in app.semantic_docs().values() {
            let _ = std::hint::black_box(d.text.contains("graph"));
        }
    });
    stage("hash_embed every doc", 5, &mut || {
        for d in app.semantic_docs().values() {
            let _ = std::hint::black_box(bv_search::embedder::hash_embed(&d.text, dim));
        }
    });
    println!(
        "  whole keystroke              {:8.2} ms",
        keystroke_ms(
            &mut App::new(
                bv_core::discovery::load_issues_from_repo(&cwd)
                    .expect("load")
                    .0
            ),
            "graph",
            10
        )
    );
}

#[test]
#[ignore = "needs a real repo; set BV_BENCH_REPO to a project root"]
fn keystroke_cost_on_a_real_repository() {
    let Ok(repo) = std::env::var("BV_BENCH_REPO") else {
        panic!("set BV_BENCH_REPO to a project root containing .beads/issues.jsonl");
    };
    let cwd = std::path::PathBuf::from(&repo);
    let (issues, _stats) = bv_core::discovery::load_issues_from_repo(&cwd)
        .unwrap_or_else(|e| panic!("load {repo}: {e}"));
    let text_bytes: usize = issues
        .iter()
        .map(|i| i.description.len() + i.notes.len() + i.acceptance_criteria.len() + i.title.len())
        .sum();
    println!(
        "\n{repo}: {} issues, {:.2} MB of text fields",
        issues.len(),
        text_bytes as f64 / (1024.0 * 1024.0)
    );

    let mut app = App::new(issues);
    // "graph" is short by both Go rules (1 token, 5 runes), so the literal
    // lexical-boost path runs for every row — the worst case.
    let ms = keystroke_ms(&mut app, "graph", 10);
    println!("  semantic keystroke: {ms:6.2} ms");
    assert!(
        ms < 20.0,
        "one keystroke took {ms:.2} ms — documents are being rebuilt per keypress again"
    );
}

/// The cache is only safe because it holds exactly what the old per-keystroke
/// call produced. This is the byte-parity assertion behind that claim: every
/// cached document must equal `issue_document` recomputed from `issue_map`,
/// and the two maps must have the same key set — `apply_semantic` looks a
/// row's document up by ID and scores 0 when it misses, so a stray or absent
/// key would silently change a score.
#[test]
fn cached_documents_match_a_freshly_built_one() {
    let app = App::new((0..24).map(fat_issue).collect());
    assert_eq!(app.semantic_docs().len(), app.issue_map.len());
    for (id, issue) in &app.issue_map {
        let fresh = bv_search::query::issue_document(
            &issue.id,
            &issue.title,
            &issue.labels,
            &issue.description,
        );
        assert_eq!(
            app.semantic_docs().get(id).map(|d| d.text.as_str()),
            Some(fresh.as_str()),
            "cached document drifted from `issue_document` for {id}"
        );
        assert_eq!(
            app.semantic_docs()[id].lexical.boost("graph"),
            bv_search::query::short_query_lexical_boost("graph", &fresh),
            "pre-lowercased document scored differently from the fresh one for {id}"
        );
    }
}

/// Ranking must be a pure function of (query, index, docs) — typing the same
/// characters again has to land on the same rows, and the index-backed and
/// fresh-embedding paths have to agree, or the cache would be ranking on
/// different text than the fallback would.
#[test]
fn ranking_is_stable_and_index_agnostic() {
    let mut app = App::new((0..40).map(fat_issue).collect());
    app.semantic_searching = true;
    for c in "graph rendering".chars() {
        app.handle_key(KeyCode::Char(c));
    }
    let first = app.filtered_indices.clone();

    app.semantic_query.clear();
    for c in "graph rendering".chars() {
        app.handle_key(KeyCode::Char(c));
    }
    assert_eq!(first, app.filtered_indices, "retyping changed the ranking");

    // Same documents, no persistent index → the fallback must agree.
    app.semantic_index = None;
    app.semantic_query.clear();
    for c in "graph rendering".chars() {
        app.handle_key(KeyCode::Char(c));
    }
    assert_eq!(
        first, app.filtered_indices,
        "cached documents and the fresh-embedding fallback ranked differently"
    );
}

#[test]
#[ignore = "wall-clock startup guard"]
fn startup_pays_for_the_document_cache() {
    let repo = std::path::PathBuf::from(
        std::env::var("BV_BENCH_REPO")
            .unwrap_or_else(|_| r"C:\Users\ADMIN\Documents\Projects\ultraworkers".to_string()),
    );
    let (issues, _) = bv_core::discovery::load_issues_from_repo(&repo).unwrap();
    let n = issues.len();
    let mut best = f64::MAX;
    for _ in 0..5 {
        let owned = issues.clone();
        let t = Instant::now();
        let _app = bv_tui::App::new(owned);
        best = best.min(t.elapsed().as_secs_f64() * 1000.0);
    }
    println!("\nApp::new on {n} issues: {best:.2} ms (best of 5)");
}
