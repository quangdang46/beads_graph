//! The main list builds `ListItem`s only for the rows that fit on screen.
//!
//! That is a correctness-relevant optimisation, not just a speed one: the
//! window has to land on exactly the rows ratatui's `List` would have
//! scrolled to, or the TUI silently shows the wrong slice of the issue list.
//! These tests read the rendered buffer rather than the helper that produced
//! it, so an error in the window maths shows up as wrong pixels.

use bv_tui::views::tree::visible_range;
use bv_tui::App;
use ratatui::backend::TestBackend;
use ratatui::Terminal;

fn issue(i: usize) -> bv_core::model::Issue {
    bv_core::model::Issue {
        id: format!("T-{i:03}"),
        content_hash: String::new(),
        title: format!("Issue {i}"),
        description: String::new(),
        design: String::new(),
        acceptance_criteria: String::new(),
        notes: String::new(),
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
        labels: vec![],
        dependencies: vec![],
        comments: vec![],
        source_repo: String::new(),
    }
}

fn make_app(n: usize) -> App {
    App::new((0..n).map(issue).collect())
}

const WIDTH: u16 = 120;
const HEIGHT: u16 = 20;

/// Render and return only the issue-list pane as text, one string per row.
///
/// The list sits in the left 40% under split view, and the detail pane on
/// the right also prints the selected row's id (`ID: T-…`), so scanning the
/// whole width would count every id twice. Read the left column only.
fn render_list_rows(app: &mut App) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("backend");
    terminal.draw(|f| bv_tui::render(f, app)).expect("draw");
    let buf = terminal.backend().buffer().clone();
    let pane_width = (f64::from(WIDTH) * 0.4) as u16 - 1;
    (0..HEIGHT)
        .map(|y| {
            (0..pane_width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

/// The row ids the list pane painted, in screen order.
///
/// Each rendered row is `<border><cursor><type><prio><status><id>…`, and
/// the ids are the only `T-NNN` tokens on the line — the title column holds
/// "Issue N". Take the first match so a stray later token cannot displace
/// the real id.
fn painted_ids(rows: &[String]) -> Vec<String> {
    rows.iter()
        .filter_map(|line| {
            line.split_whitespace()
                .find(|tok| tok.len() == 5 && tok.starts_with("T-"))
                .map(str::to_string)
        })
        .collect()
}

/// What the list should show, read from the app's own ordering rather than
/// re-deriving it — `apply_filter` sorts by `(priority, Reverse(id))`, so the
/// display order is *descending* by id and assuming otherwise yields a test
/// that passes for the wrong reason.
fn expected_ids(app: &App, cursor: usize, capacity: usize) -> Vec<String> {
    let r = visible_range(app.filtered_indices.len(), cursor, capacity);
    r.map(|i| app.rows[app.filtered_indices[i]].id.clone())
        .collect()
}

/// Item rows the list pane can show: `HEIGHT` minus the status bar, the
/// header row, and the block's two borders.
const CAPACITY: usize = 17;

#[test]
fn window_math_matches_the_scroll_rule() {
    // Every item is one line tall, so the window is always `height` rows,
    // clamped to the list, and never lets the cursor leave. Until the cursor
    // reaches the bottom the window stays pinned at the top and simply
    // contains it; past that it follows the cursor one row at a time.
    for (len, height) in [(1usize, 10usize), (3, 10), (10, 10), (40, 10), (40, 3)] {
        for cursor in 0..len {
            let r = visible_range(len, cursor, height);
            assert!(
                r.start <= cursor && cursor < r.end,
                "cursor off-screen: {r:?}"
            );
            assert_eq!(r.len(), len.min(height), "len={len} height={height}");
            let want_start = (cursor + 1).saturating_sub(height);
            assert_eq!(
                r.start,
                want_start.min(len - 1),
                "len={len} height={height} cursor={cursor} -> {r:?}"
            );
            assert_eq!(
                r.end,
                (r.start + height).min(len),
                "len={len} height={height}"
            );
        }
    }
    // Degenerate geometry must not panic.
    assert_eq!(visible_range(10, 5, 0), 0..0);
    assert_eq!(visible_range(0, 0, 10), 0..0);
}

#[test]
fn list_paints_every_row_when_they_all_fit() {
    let mut app = make_app(5);
    let rows = render_list_rows(&mut app);
    let painted = painted_ids(&rows);
    assert_eq!(painted, expected_ids(&app, 0, CAPACITY));
    assert_eq!(painted.len(), 5);
}

#[test]
fn list_window_shows_the_rows_the_scroll_rule_picks() {
    let mut app = make_app(40);
    for cursor in [0usize, 1, 16, 17, 20, 39] {
        app.cursor = cursor;
        let rows = render_list_rows(&mut app);
        let expected = expected_ids(&app, cursor, CAPACITY);
        assert_eq!(
            painted_ids(&rows),
            expected,
            "wrong window at cursor {cursor}"
        );
    }
}

#[test]
fn the_selected_row_is_always_on_screen() {
    let mut app = make_app(40);
    for cursor in 0..40 {
        app.cursor = cursor;
        let rows = render_list_rows(&mut app);
        let selected_id = app.rows[app.filtered_indices[cursor]].id.clone();
        assert!(
            painted_ids(&rows).contains(&selected_id),
            "selected row {selected_id} scrolled out of the window at cursor {cursor}"
        );
    }
}

#[test]
fn highlight_lands_on_the_cursor_row() {
    // Guards the `ListState` index rebasing: the state now carries an index
    // relative to the window, so the highlight must still land on the right
    // physical row after the window has scrolled.
    let mut app = make_app(40);
    for cursor in [0usize, 5, 17, 30, 39] {
        app.cursor = cursor;
        let rows = render_list_rows(&mut app);
        let selected_id = app.rows[app.filtered_indices[cursor]].id.clone();
        let highlighted = rows
            .iter()
            .find(|line| line.contains('▸'))
            .unwrap_or_else(|| {
                panic!(
                    "no highlighted row at cursor {cursor}:\n{}",
                    rows.join("\n")
                )
            });
        assert!(
            highlighted.contains(&selected_id),
            "at cursor {cursor} the highlight is on {highlighted:?}, expected {selected_id}"
        );
    }
}

#[test]
fn window_scrolls_by_exactly_one_row_per_cursor_step() {
    // Past the bottom of the first full page, each cursor step must advance
    // the window by exactly one row — no more (dropped rows) and no less
    // (rows rebuilt that were never painted).
    let mut app = make_app(40);
    let mut previous: Option<Vec<String>> = None;
    for cursor in [CAPACITY - 1, CAPACITY, CAPACITY + 1, CAPACITY + 2] {
        app.cursor = cursor;
        let ids = painted_ids(&render_list_rows(&mut app));
        if let Some(prev) = &previous {
            // A one-row scroll drops exactly the first row and appends
            // exactly one at the bottom.
            assert_eq!(
                &ids[..ids.len() - 1],
                &prev[1..],
                "window did not scroll by one row at cursor {cursor}"
            );
        }
        previous = Some(ids);
    }
}

#[test]
fn window_stays_pinned_until_the_cursor_reaches_the_bottom() {
    // The first page must not creep: cursors inside it all show the same
    // rows, which is what a user scrolling a short list expects.
    let mut app = make_app(40);
    let first = painted_ids(&render_list_rows(&mut app));
    for cursor in 1..CAPACITY {
        app.cursor = cursor;
        assert_eq!(
            painted_ids(&render_list_rows(&mut app)),
            first,
            "window moved while the cursor was still on the first page (cursor {cursor})"
        );
    }
}

#[test]
fn empty_list_renders_without_panicking() {
    let mut app = make_app(0);
    let rows = render_list_rows(&mut app);
    assert!(painted_ids(&rows).is_empty());
}

#[test]
fn list_larger_than_the_screen_never_paints_off_screen_rows() {
    // The reason the window exists: 400 rows must not cost 400 ListItems, and
    // the painted output stays capped at the pane height.
    let mut app = make_app(400);
    let rows = render_list_rows(&mut app);
    assert_eq!(painted_ids(&rows).len(), CAPACITY);
}
