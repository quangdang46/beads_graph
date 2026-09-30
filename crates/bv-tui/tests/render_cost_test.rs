//! Frame-cost regression guard for the TUI's hot views.
//!
//! These are wall-clock assertions with a deliberately loose ceiling. They
//! exist to catch a *regression* back to "rebuild everything on every frame",
//! not to police microseconds — a busy CI box should not fail them. The
//! property worth protecting is the shape of the work: cost must stay
//! roughly flat as the issue count grows, because only the rows that fit on
//! screen may be built.
//!
//! Run with `--release`; a debug build is ~5x slower and the numbers below
//! would be meaningless.

use bv_tui::App;
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use std::time::{Duration, Instant};

fn issue(i: usize) -> bv_core::model::Issue {
    bv_core::model::Issue {
        id: format!("T-{i:05}"),
        content_hash: String::new(),
        title: format!("Issue number {i} with a reasonably long title to exercise truncation"),
        // The shape that made the original dataset pathological: a handful of
        // records carrying megabytes of prose, which is what a real Beads
        // project accumulates through `notes` and `acceptance_criteria`.
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

fn frame_ms(app: &mut App, iters: u32) -> f64 {
    let mut terminal = Terminal::new(TestBackend::new(200, 50)).expect("backend");
    // Warm up so the first frame's page faults and buffer growth are not
    // attributed to the measurement.
    for _ in 0..3 {
        terminal.draw(|f| bv_tui::render(f, app)).expect("draw");
    }
    let start = Instant::now();
    for _ in 0..iters {
        terminal.draw(|f| bv_tui::render(f, app)).expect("draw");
    }
    start.elapsed().as_secs_f64() * 1000.0 / f64::from(iters)
}

fn per_view_ms(app: &mut App, iters: u32) -> Vec<(&'static str, f64)> {
    [
        ("List", bv_tui::ViewMode::List),
        ("Tree", bv_tui::ViewMode::Tree),
        ("Sprint", bv_tui::ViewMode::Sprint),
        ("Graph", bv_tui::ViewMode::Graph),
        ("Board", bv_tui::ViewMode::Board),
    ]
    .iter()
    .map(|(name, view)| {
        app.current_view = *view;
        (*name, frame_ms(app, iters))
    })
    .collect()
}

#[test]
#[ignore = "wall-clock perf guard; run explicitly with `cargo test --release -- --ignored`"]
fn frame_cost_stays_flat_as_the_dataset_grows() {
    // 40 rows and 400 rows differ by 10x in data. Before the view-windowing
    // and borrow-not-clone fixes, every frame walked all of them, so the
    // larger set cost ~10x more. It must now cost roughly the same, because
    // a 200x50 terminal only ever paints ~48 rows.
    let small = per_view_ms(&mut App::new((0..40).map(issue).collect()), 20);
    let large = per_view_ms(&mut App::new((0..400).map(issue).collect()), 20);

    let mut report = String::new();
    for ((name, small_ms), (_, large_ms)) in small.iter().zip(&large) {
        report.push_str(&format!(
            "  {name:<7} 40 rows: {small_ms:6.2} ms   400 rows: {large_ms:6.2} ms   ratio: {:.2}x\n",
            large_ms / small_ms.max(0.001)
        ));
    }
    println!("frame cost vs dataset size\n{report}");

    for (name, large_ms) in &large {
        assert!(
            *large_ms < Duration::from_millis(16).as_secs_f64() * 1000.0,
            "{name} frame took {large_ms:.2} ms on 400 fat issues, over the 60fps budget\n{report}"
        );
    }
}

#[test]
#[ignore = "wall-clock perf guard; run explicitly with `cargo test --release -- --ignored`"]
fn tree_frame_does_not_clone_the_dataset() {
    // The Tree view used to deep-clone every `Issue` per frame to read three
    // fields, which cost 1.42-1.66 ms on a 169-issue / 3 MB dataset. On this
    // 400-issue fat dataset the same clone would be far worse. Anything in
    // the tens of milliseconds means the clone is back.
    let mut app = App::new((0..400).map(issue).collect());
    app.current_view = bv_tui::ViewMode::Tree;
    let ms = frame_ms(&mut app, 20);
    println!("Tree frame on 400 fat issues: {ms:.2} ms");
    assert!(
        ms < 10.0,
        "Tree frame took {ms:.2} ms — the per-frame deep clone is likely back"
    );
}

/// Same measurement, but against a real repository's `.beads/issues.jsonl`.
///
/// Point `BV_BENCH_REPO` at a project root to run it:
///
/// ```sh
/// BV_BENCH_REPO=../ultraworkers cargo test --release -p bv-tui \
///     --test render_cost_test -- --ignored --nocapture
/// ```
///
/// The pre-fix numbers on that dataset were Tree 3.91-4.97 ms and List
/// 1.45-1.76 ms per frame.
#[test]
#[ignore = "needs a real repo; set BV_BENCH_REPO to a project root"]
fn frame_cost_on_a_real_repository() {
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
    for (name, ms) in per_view_ms(&mut app, 30) {
        println!("  {name:<7} {ms:6.2} ms/frame");
    }
}
