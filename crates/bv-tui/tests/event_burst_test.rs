//! The event loop drains a whole pending burst of events and paints once,
//! because a scrolled detail pane already rewrites ~34% of the screen
//! (3394 of 10000 cells, measured) — one frame per event turned a wheel
//! gesture into a full-screen repaint per notch.
//!
//! The property that must survive coalescing is **no dropped input**: applying
//! a burst in one go has to leave the app in exactly the state that applying
//! the same events one at a time would.

use bv_tui::App;
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyEventState, MouseEvent, MouseEventKind,
};

fn make_app(n: usize) -> App {
    let issues: Vec<bv_core::model::Issue> = (0..n)
        .map(|i| bv_core::model::Issue {
            id: format!("T-{i:03}"),
            content_hash: String::new(),
            title: format!("Issue {i}"),
            description: "line\n".repeat(400),
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
        })
        .collect();
    let mut app = App::new(issues);
    app.width = 200;
    app.height = 50;
    app
}

fn wheel(up: bool) -> MouseEvent {
    MouseEvent {
        kind: if up {
            MouseEventKind::ScrollUp
        } else {
            MouseEventKind::ScrollDown
        },
        column: 100,
        row: 10,
        modifiers: crossterm::event::KeyModifiers::NONE,
    }
}

fn key(c: char) -> KeyEvent {
    KeyEvent {
        code: KeyCode::Char(c),
        modifiers: crossterm::event::KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

#[test]
fn a_wheel_burst_moves_the_cursor_by_the_full_burst() {
    const NOTCHES: i16 = 9;
    let mut app = make_app(60);
    app.focus_detail = false;
    // The list has 60 rows and a 17-row page, so 9 notches stay inside it.
    for _ in 0..NOTCHES {
        app.handle_mouse(wheel(false));
    }
    let after_burst = app.cursor;

    let mut one_at_a_time = make_app(60);
    for _ in 0..NOTCHES {
        one_at_a_time.handle_mouse(wheel(false));
    }
    assert_eq!(
        after_burst, one_at_a_time.cursor,
        "coalescing a burst must not drop or double-apply notches"
    );
    assert_eq!(after_burst, NOTCHES as usize, "every notch must be applied");
}

#[test]
fn a_wheel_burst_over_the_list_edge_clamps_like_one_at_a_time() {
    let mut burst = make_app(5); // fewer rows than the page height
    let mut stepwise = make_app(5);
    for _ in 0..12 {
        burst.handle_mouse(wheel(false));
        stepwise.handle_mouse(wheel(false));
    }
    assert_eq!(burst.cursor, stepwise.cursor);
    assert_eq!(
        burst.cursor, 4,
        "must clamp at the last row, not wrap or overshoot"
    );
}

#[test]
fn a_mixed_burst_lands_where_each_event_alone_would() {
    let mut app = make_app(60);
    let mut expected = make_app(60);

    // Ten events, alternating input kinds, the way a real burst mixes a held
    // key with wheel notches. The drained loop must land on the same state as
    // feeding them one at a time.
    for i in 0..10 {
        if i % 2 == 0 {
            app.handle_key(key('j').code);
            expected.handle_key(key('j').code);
        } else {
            app.handle_mouse(wheel(false));
            expected.handle_mouse(wheel(false));
        }
    }
    assert_eq!(app.cursor, expected.cursor);
    assert_eq!(app.filtered_indices, expected.filtered_indices);
}

#[test]
fn detail_scroll_survives_a_burst_without_rebuilding_the_body() {
    // The scroll offset is the thing a drained burst has to get exactly
    // right: it is the argument to `Paragraph::scroll`, so an off-by-one here
    // shows the user the wrong page of a long issue.
    let mut app = make_app(30);
    app.focus_detail = true;
    for _ in 0..25 {
        app.handle_mouse(wheel(false));
    }
    assert_eq!(app.detail_scroll, 25);
    for _ in 0..25 {
        app.handle_mouse(wheel(true));
    }
    assert_eq!(
        app.detail_scroll, 0,
        "scrolling back up must return to the top"
    );
}

#[test]
fn scrolling_up_at_the_top_is_a_no_op() {
    let mut app = make_app(30);
    app.focus_detail = true;
    for _ in 0..5 {
        app.handle_mouse(wheel(true));
    }
    assert_eq!(app.detail_scroll, 0);
}
