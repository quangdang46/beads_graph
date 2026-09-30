//! A left click must select the row under the pointer.
//!
//! Two ways this used to break, both invisible to the keyboard tests:
//!
//!  1. The handler applied a "left 40% is the list, right side is the detail
//!     pane" test unconditionally. `render` only draws a detail pane when
//!     `split_view && width > 100`; below that it stacks the panes full
//!     width, so every click past 40% of the row fell into the detail branch
//!     and did nothing but steal focus.
//!  2. It mapped the screen row straight onto `filtered_indices`, ignoring
//!     that `render_list` only builds the `visible_range` window. Once the
//!     list was scrolled, clicking a visible row selected a different issue.
//!
//! It was also off by one row: the first item is painted on screen row 2
//! (row 0 is the outer frame, row 1 the list block's top border), but the
//! handler subtracted 3, so the first row was unclickable and every other
//! click selected the row above the pointer.

use bv_tui::App;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

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

fn make_app(n: usize, width: u16, height: u16) -> App {
    let mut app = App::new((0..n).map(issue).collect());
    app.width = width;
    app.height = height;
    app
}

fn click(app: &mut App, column: u16, row: u16) {
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: crossterm::event::KeyModifiers::NONE,
    });
}

/// The id the cursor now points at.
fn selected_id(app: &App) -> String {
    app.rows[app.filtered_indices[app.cursor]].id.clone()
}

/// The id painted on the given screen row, read the way a user sees it.
fn id_on_row(app: &mut App, row: u16) -> Option<String> {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let width = app.width;
    let mut terminal = Terminal::new(TestBackend::new(width, app.height)).unwrap();
    terminal.draw(|f| bv_tui::render(f, app)).unwrap();
    let buf = terminal.backend().buffer().clone();
    let line: String = (0..width).map(|x| buf[(x, row)].symbol()).collect();
    line.split_whitespace()
        .find(|t| t.len() == 5 && t.starts_with("T-"))
        .map(str::to_string)
}

#[test]
fn clicking_a_visible_row_selects_it_in_split_view() {
    let mut app = make_app(40, 120, 20);
    for row in 2..19u16 {
        let painted = id_on_row(&mut app, row).expect("row is painted");
        click(&mut app, 10, row);
        assert_eq!(
            selected_id(&app),
            painted,
            "click on screen row {row} selected the wrong issue"
        );
    }
}

#[test]
fn clicking_past_40_percent_still_selects_when_there_is_no_detail_pane() {
    // width <= 100: `render` stacks the panes, the list is full width, and
    // there is no right-hand pane to focus. A click at column 90 must select
    // the row, not set `focus_detail`.
    let mut app = make_app(40, 90, 20);
    assert!(app.cursor == 0 && !app.focus_detail);
    let painted = id_on_row(&mut app, 5).expect("row is painted");
    click(&mut app, 85, 5);
    assert!(
        !app.focus_detail,
        "a narrow terminal has no detail pane to focus; the click stole focus instead of selecting"
    );
    assert_eq!(selected_id(&app), painted);
}

#[test]
fn clicking_the_right_pane_focuses_the_detail_in_split_view() {
    let mut app = make_app(40, 120, 20);
    click(&mut app, 100, 6); // well right of the 40% split at column 48
    assert!(
        app.focus_detail,
        "the detail pane exists here; focus should move to it"
    );
    assert_eq!(app.cursor, 0, "and the list selection must not move");
}

#[test]
fn clicking_respects_the_scroll_window() {
    // Scrolled list: the visible window starts well past index 0, so a click
    // on the top visible row must select `filtered_indices[window_start]`,
    // not `filtered_indices[0]`.
    let mut app = make_app(40, 120, 20);
    app.cursor = 30; // scrolls the window
    let window = bv_tui::views::tree::visible_range(app.filtered_indices.len(), 30, 17);
    assert!(window.start > 0, "test needs a scrolled window");

    let top_row_id = id_on_row(&mut app, 2).expect("top visible row is painted");
    let expected = app.rows[app.filtered_indices[window.start]].id.clone();
    assert_eq!(
        top_row_id, expected,
        "fixture drift: row 3 is not the window start"
    );

    click(&mut app, 10, 2);
    assert_eq!(
        selected_id(&app),
        top_row_id,
        "clicking the top visible row of a scrolled list selected the wrong issue"
    );
}

#[test]
fn clicking_the_border_or_header_selects_nothing() {
    let mut app = make_app(40, 120, 20);
    for row in 0..2u16 {
        let before = selected_id(&app);
        click(&mut app, 10, row);
        assert_eq!(
            selected_id(&app),
            before,
            "row {row} is chrome, not an item"
        );
    }
}

/// The redraw policy asks `handle_mouse` whether anything changed, and
/// repaints on its answer rather than on "an event arrived".
///
/// `EnableMouseCapture` turns on SGR 1003, so the terminal reports every
/// pointer movement. If motion reported `true` the TUI would rebuild and
/// repaint the whole view on every sweep of the mouse across the window —
/// which is what the caller used to do unconditionally. Conversely, if an
/// event that really does move the cursor reported `false`, the screen would
/// freeze on it, so both directions are pinned here.
#[test]
fn only_mouse_events_that_change_something_report_a_change() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

    let ev = |kind| MouseEvent {
        kind,
        column: 20,
        row: 10,
        modifiers: crossterm::event::KeyModifiers::NONE,
    };

    // Events that change nothing: motion, drag, release, the horizontal
    // wheels, and the buttons the TUI does not bind.
    for kind in [
        MouseEventKind::Moved,
        MouseEventKind::Drag(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
        MouseEventKind::Down(MouseButton::Middle),
        MouseEventKind::Down(MouseButton::Right),
        MouseEventKind::ScrollLeft,
        MouseEventKind::ScrollRight,
    ] {
        let mut app = make_app(40, 120, 20);
        assert!(
            !app.handle_mouse(ev(kind)),
            "{kind:?} changes nothing, so it must not ask for a repaint"
        );
    }

    // Events that do change something.
    for kind in [
        MouseEventKind::ScrollDown,
        MouseEventKind::ScrollUp,
        MouseEventKind::Down(MouseButton::Left),
    ] {
        let mut app = make_app(40, 120, 20);
        assert!(
            app.handle_mouse(ev(kind)),
            "{kind:?} moves the selection, so it must ask for a repaint"
        );
    }
}
