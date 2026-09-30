//! Sprint dashboard view (Go `pkg/ui/sprint_view.go`).
//!
//! Shows sprint progress, burndown chart, at-risk items, and bead list.
//! Data comes from `bv_core::sprint` (`.beads/sprints.jsonl`).

use bv_core::model::Status;
use bv_core::Sprint;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

/// Sprint dashboard state held by App.
pub struct SprintState {
    pub sprints: Vec<Sprint>,
    pub selected_idx: usize,
}

impl SprintState {
    pub fn is_empty(&self) -> bool {
        self.sprints.is_empty()
    }

    pub fn selected(&self) -> Option<&Sprint> {
        self.sprints.get(self.selected_idx)
    }

    pub fn move_up(&mut self) {
        self.selected_idx = self.selected_idx.saturating_sub(1);
    }

    pub fn move_down(&mut self) {
        if self.selected_idx + 1 < self.sprints.len() {
            self.selected_idx += 1;
        }
    }
}

/// Render the full sprint dashboard.
///
/// Takes `&[&Issue]` so the caller can borrow straight out of `App::issue_map`.
/// It needs nothing but `id`, `status` and `sprint_id`, but the call site used
/// to hand it a freshly deep-cloned `Vec<Issue>` on every frame — ~3 MB of
/// `String` copy for a dashboard that reads three small fields (measured
/// 1.42-1.66 ms/frame on the ultraworkers dataset).
pub fn render_sprint(
    f: &mut Frame,
    state: &SprintState,
    issues: &[&bv_core::model::Issue],
    area: Rect,
) {
    if state.is_empty() {
        let msg = Paragraph::new("No sprints defined (.beads/sprints.jsonl)")
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(msg, area);
        return;
    }

    let sprint = &state.sprints[state.selected_idx];

    // Split into header (sprint selector) and dashboard
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(10)])
        .split(area);

    // Sprint selector header
    render_sprint_header(f, state, chunks[0]);

    // Dashboard content
    let mut lines: Vec<Line> = Vec::new();

    // Title
    lines.push(Line::from(Span::styled(
        format!("Sprint: {}", sprint.name),
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    // Date range and days remaining
    let now = jiff::Timestamp::now();
    render_dates_section(&mut lines, sprint, now);

    // Compute bead stats
    let bead_ids: std::collections::HashSet<&str> =
        sprint.bead_ids.iter().map(|s| s.as_str()).collect();
    let sprint_issues: Vec<&bv_core::model::Issue> = issues
        .iter()
        .copied()
        .filter(|i| bead_ids.contains(i.id.as_str()))
        .collect();
    let total = sprint_issues.len();
    let closed = sprint_issues
        .iter()
        .filter(|i| i.status.is_closed())
        .count();
    let in_progress = sprint_issues
        .iter()
        .filter(|i| i.status == Status::InProgress)
        .count();
    let blocked = sprint_issues
        .iter()
        .filter(|i| i.status == Status::Blocked)
        .count();
    let open = total - closed - in_progress - blocked;

    // Progress bar
    render_progress_bar(&mut lines, total, closed, area.width);

    // Status breakdown
    lines.push(Line::from(vec![
        Span::styled("  Status:  ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("closed={} ", closed),
            Style::default().fg(Color::Green),
        ),
        Span::styled(
            format!("in-progress={} ", in_progress),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(
            format!("blocked={} ", blocked),
            Style::default().fg(Color::Red),
        ),
        Span::styled(format!("open={}", open), Style::default().fg(Color::White)),
    ]));
    lines.push(Line::from(""));

    // Burndown chart
    render_burndown(&mut lines, sprint, total, closed, now, area.width);

    // Sprint beads list (top 10)
    render_bead_list(&mut lines, &sprint_issues, 10);

    // Footer
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "P/esc: close sprint view | j/k: switch sprint",
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" SPRINT DASHBOARD ");

    let para = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    f.render_widget(para, chunks[1]);
}

fn render_sprint_header(f: &mut Frame, state: &SprintState, area: Rect) {
    let items: Vec<Line> = state
        .sprints
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let marker = if i == state.selected_idx {
                "▶ "
            } else {
                "  "
            };
            let style = if i == state.selected_idx {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            Line::from(Span::styled(format!("{}{}", marker, s.name), style))
        })
        .collect();

    let block = Block::default().borders(Borders::ALL).title(" Sprints ");

    let para = Paragraph::new(items).block(block);
    f.render_widget(para, area);
}

fn render_dates_section(lines: &mut Vec<Line>, sprint: &Sprint, now: jiff::Timestamp) {
    let label = Style::default().fg(Color::DarkGray);

    // Date range
    let dates = match (&sprint.start_date, &sprint.end_date) {
        (Some(s), Some(e)) => format!("{} to {}", short_date(s), short_date(e)),
        (Some(s), None) => format!("{} to ...", short_date(s)),
        (None, Some(e)) => format!("... to {}", short_date(e)),
        (None, None) => "no dates set".to_string(),
    };
    lines.push(Line::from(vec![
        Span::styled("  Dates:     ", label),
        Span::raw(dates),
    ]));

    // Days remaining
    if let Some(end) = &sprint.end_date {
        if let Ok(end_ts) = end.parse::<jiff::Timestamp>() {
            let remaining = end_ts.since(now).map(|d| d.get_days()).unwrap_or(0);
            let color = if remaining <= 0 {
                Color::Red
            } else if remaining <= 2 {
                Color::Yellow
            } else {
                Color::Green
            };
            lines.push(Line::from(vec![
                Span::styled("  Remaining: ", label),
                Span::styled(
                    format!("{} days", remaining.max(0)),
                    Style::default().fg(color),
                ),
            ]));
        }
    }
    lines.push(Line::from(""));
}

fn render_progress_bar(lines: &mut Vec<Line>, total: usize, closed: usize, width: u16) {
    let bar_width = (width as usize).saturating_sub(22).clamp(10, 50);
    let pct = if total > 0 {
        closed as f64 / total as f64
    } else {
        0.0
    };
    let filled = (pct * bar_width as f64).round() as usize;
    let empty = bar_width.saturating_sub(filled);

    lines.push(Line::from(vec![
        Span::styled("  Progress:  ", Style::default().fg(Color::DarkGray)),
        Span::styled("█".repeat(filled), Style::default().fg(Color::Green)),
        Span::styled("░".repeat(empty), Style::default().fg(Color::DarkGray)),
        Span::raw(format!(" {}/{} ({:.0}%)", closed, total, pct * 100.0)),
    ]));
}

fn render_burndown(
    lines: &mut Vec<Line>,
    sprint: &Sprint,
    total: usize,
    closed: usize,
    now: jiff::Timestamp,
    width: u16,
) {
    lines.push(Line::from(Span::styled(
        "  Burndown:",
        Style::default().fg(Color::DarkGray),
    )));

    let start = sprint
        .start_date
        .as_deref()
        .and_then(|s| s.parse::<jiff::Timestamp>().ok());
    let end = sprint
        .end_date
        .as_deref()
        .and_then(|s| s.parse::<jiff::Timestamp>().ok());

    if let (Some(start), Some(end)) = (start, end) {
        let total_days = end.since(start).map(|d| d.get_days()).unwrap_or(1).max(1);
        let elapsed = now.since(start).map(|d| d.get_days()).unwrap_or(0).max(0);
        let remaining = total - closed;

        // Simple ASCII burndown: show ideal line and actual point
        let chart_width = (width as usize).saturating_sub(6).clamp(10, 40);
        let chart_height = 5;

        for row in 0..chart_height {
            let threshold = total as f64 * (chart_height - row) as f64 / chart_height as f64;
            let mut line_str = String::from("    ");
            for col in 0..chart_width {
                let day_frac = col as f64 / chart_width as f64;
                let ideal_val = total as f64 * (1.0 - day_frac);
                let elapsed_frac = elapsed as f64 / total_days as f64;

                if (ideal_val - threshold).abs() < total as f64 / chart_height as f64 * 0.5 {
                    line_str.push('·');
                } else if col as f64 / chart_width as f64 <= elapsed_frac
                    && (remaining as f64 - threshold).abs()
                        < total as f64 / chart_height as f64 * 0.5
                {
                    line_str.push('●');
                } else {
                    line_str.push(' ');
                }
            }
            lines.push(Line::raw(line_str));
        }
        lines.push(Line::raw(format!("    {}", "─".repeat(chart_width + 1))));
        lines.push(Line::from(Span::styled(
            "      · ideal  ● actual",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "    (insufficient data)",
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines.push(Line::from(""));
}

fn render_bead_list(lines: &mut Vec<Line>, issues: &[&bv_core::model::Issue], limit: usize) {
    let label = Style::default().fg(Color::DarkGray);
    lines.push(Line::from(Span::styled("  Beads in Sprint:", label)));

    let display_limit = limit.min(issues.len());
    for iss in issues.iter().take(display_limit) {
        let (icon, color) = match iss.status {
            Status::Closed | Status::Tombstone => ("✓", Color::Green),
            Status::InProgress => ("⏳", Color::Cyan),
            Status::Blocked => ("⛔", Color::Red),
            _ => ("○", Color::White),
        };
        let title = truncate_sprint_str(&iss.title, 40);
        lines.push(Line::from(vec![
            Span::styled(format!("    {icon} "), Style::default().fg(color)),
            Span::styled(
                format!("{} - {}", iss.id, title),
                Style::default().fg(color),
            ),
        ]));
    }
    if issues.len() > display_limit {
        lines.push(Line::from(Span::styled(
            format!("    ... +{} more", issues.len() - display_limit),
            Style::default().fg(Color::DarkGray),
        )));
    }
}

fn short_date(s: &str) -> String {
    // "2026-01-15T00:00:00Z" -> "Jan 15"
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    // `str::get` returns `None` for a range that is not on a character
    // boundary, so a non-ASCII date degrades to the raw string instead of
    // panicking. The length test alone was not enough: `s.len() >= 10`
    // measures BYTES, and a date like "2026年01月15日" is 16 bytes, which put
    // `s[5..7]` in the middle of the `年` and took the process down with it.
    // The sprint file is user data, so any encoding can reach here.
    if let (Some(y), Some(m), Some(d)) = (s.get(0..4), s.get(5..7), s.get(8..10)) {
        if let (Ok(y), Ok(m), Ok(d)) = (y.parse::<i32>(), m.parse::<usize>(), d.parse::<usize>()) {
            if y > 0 && (1..=12).contains(&m) {
                return format!("{} {}", months[m - 1], d);
            }
        }
    }
    s.to_string()
}

fn truncate_sprint_str(s: &str, max_len: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_len {
        s.to_string()
    } else if max_len <= 1 {
        "…".to_string()
    } else {
        let truncated: String = chars[..max_len - 1].iter().collect();
        format!("{truncated}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bv_core::model::Issue;

    /// `short_date` used to slice by byte index behind a `s.len() >= 10` guard,
    /// which is a length check, not a boundary check. A sprint file is user
    /// data, so a date written as "2026年01月15日" is 16 bytes with `s[5..7]`
    /// landing inside the `年` — a slice panic at any terminal size.
    ///
    /// The pre-existing "not a date" case could never have caught this: it is
    /// 10 ASCII bytes, all of them boundaries.
    #[test]
    fn short_date_survives_non_ascii_input() {
        for input in [
            "2026年01月15日",
            "2026-01-15T00:00:00Z",
            "2026-01-15",
            "",
            "x",
            "2026年",
            "日本語のadine",
        ] {
            // Must not panic; a date-shaped ASCII prefix still formats.
            let out = short_date(input);
            if input == "2026-01-15T00:00:00Z" {
                assert_eq!(out, "Jan 15");
            }
        }
        assert_eq!(short_date("2026-01-15T00:00:00Z"), "Jan 15");
        assert_eq!(short_date("2026-12-31"), "Dec 31");
    }

    #[allow(dead_code)]
    fn make_issue(id: &str, status: Status) -> Issue {
        Issue {
            id: id.to_string(),
            content_hash: String::new(),
            title: format!("Title for {id}"),
            description: String::new(),
            design: String::new(),
            acceptance_criteria: String::new(),
            notes: String::new(),
            status,
            priority: 2,
            issue_type: "task".into(),
            assignee: String::new(),
            estimated_minutes: None,
            created_at: Some("2026-01-01T00:00:00Z".into()),
            updated_at: Some("2026-01-01T00:00:00Z".into()),
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

    #[test]
    fn sprint_state_navigation() {
        let sprints = vec![
            bv_core::Sprint {
                id: "s1".into(),
                name: "Sprint 1".into(),
                start_date: None,
                end_date: None,
                bead_ids: vec![],
                velocity_target: None,
                created_at: None,
                updated_at: None,
            },
            bv_core::Sprint {
                id: "s2".into(),
                name: "Sprint 2".into(),
                start_date: None,
                end_date: None,
                bead_ids: vec![],
                velocity_target: None,
                created_at: None,
                updated_at: None,
            },
        ];
        let mut state = SprintState {
            sprints,
            selected_idx: 0,
        };
        assert_eq!(state.selected().unwrap().id, "s1");
        state.move_down();
        assert_eq!(state.selected().unwrap().id, "s2");
        state.move_down(); // no-op at end
        assert_eq!(state.selected().unwrap().id, "s2");
        state.move_up();
        assert_eq!(state.selected().unwrap().id, "s1");
    }

    #[test]
    fn short_date_formats_correctly() {
        assert_eq!(short_date("2026-01-15T00:00:00Z"), "Jan 15");
        assert_eq!(short_date("2026-12-01T00:00:00Z"), "Dec 1");
        assert_eq!(short_date("not a date"), "not a date");
    }

    #[test]
    fn truncate_sprint_str_works() {
        assert_eq!(truncate_sprint_str("hello", 10), "hello");
        assert_eq!(truncate_sprint_str("hello world", 5), "hell…");
        assert_eq!(truncate_sprint_str("hi", 1), "…");
    }
}
