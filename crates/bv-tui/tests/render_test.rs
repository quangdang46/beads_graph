use bv_tui::App;
use crossterm::event::KeyCode;

fn make_app(n: usize) -> App {
    let issues: Vec<bv_core::model::Issue> = (0..n)
        .map(|i| bv_core::model::Issue {
            id: format!("T-{i}"),
            content_hash: String::new(),
            title: format!("Issue {i}"),
            description: String::new(),
            design: String::new(),
            acceptance_criteria: String::new(),
            notes: String::new(),
            status: if i % 3 == 0 {
                bv_core::model::Status::Closed
            } else {
                bv_core::model::Status::Open
            },
            priority: (i % 4) as i64,
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
    App::new(issues)
}

#[test]
fn app_renders_list_with_issues() {
    let app = make_app(9);
    assert_eq!(app.filtered_indices.len(), 9, "All mode shows all");
}

#[test]
fn filter_open_shows_correct_count() {
    let mut app = make_app(9);
    app.handle_key(KeyCode::Char('o'));
    // i=0,3,6 are Closed → 6 Open issues
    assert_eq!(app.filtered_indices.len(), 6);
}

#[test]
fn sort_mode_changes_order() {
    let mut app = make_app(6);
    let before = app.sort_mode;
    app.handle_key(KeyCode::Char('s'));
    assert_ne!(before, app.sort_mode);
}

#[test]
fn detail_toggle_works() {
    let mut app = make_app(3);
    assert!(!app.show_detail);
    app.handle_key(KeyCode::Enter);
    assert!(app.show_detail);
}

#[test]
fn quit_works() {
    let mut app = make_app(3);
    app.handle_key(KeyCode::Char('q'));
    assert!(app.quit_requested);
}

#[test]
fn search_filters_by_title() {
    let mut app = make_app(9);
    // Enter search mode
    app.searching = true;
    // Simulate typing "1" via handle_key
    app.handle_key(KeyCode::Char('1'));
    // Issues with "1" in title or id: T-1, T-10 (if n>=10), etc.
    assert!(!app.filtered_indices.is_empty());
    assert!(app.filtered_indices.len() < 9);
}

#[test]
fn esc_exits_search_mode() {
    let mut app = make_app(3);
    app.searching = true;
    app.handle_key(KeyCode::Esc);
    assert!(!app.searching);
}
