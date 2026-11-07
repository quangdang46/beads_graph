//! Kanban board view — port of Go `pkg/ui/board.go` swimlane modes.

use crate::App;
use bv_core::model::Status;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

/// Swimlane grouping mode (cycled with `s`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwimlaneMode {
    Status,
    Priority,
    Type,
}

impl SwimlaneMode {
    pub fn next(self) -> Self {
        match self {
            SwimlaneMode::Status => SwimlaneMode::Priority,
            SwimlaneMode::Priority => SwimlaneMode::Type,
            SwimlaneMode::Type => SwimlaneMode::Status,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SwimlaneMode::Status => "Status",
            SwimlaneMode::Priority => "Priority",
            SwimlaneMode::Type => "Type",
        }
    }
}

/// Every `Status` variant, in enum order. The board must not hide issues
/// whose status is deferred/draft/pinned/hooked/review/tombstone — the old
/// 4-column Kanban silently dropped them (audit: `audit-tui-board-statuses`).
const ALL_STATUSES: [Status; 10] = [
    Status::Open,
    Status::InProgress,
    Status::Blocked,
    Status::Deferred,
    Status::Draft,
    Status::Pinned,
    Status::Hooked,
    Status::Review,
    Status::Closed,
    Status::Tombstone,
];

/// Column label for a status: `IN PROGRESS` keeps its space, every other
/// variant is the uppercased wire form (`OPEN`, `TOMBSTONE`, …).
fn status_label(status: Status) -> String {
    match status {
        Status::InProgress => "IN PROGRESS".to_string(),
        _ => status.as_str().to_uppercase(),
    }
}

/// Border/accent color per status — mirrors `graph::status_icon_color` so
/// the same status reads the same color in every view.
fn status_color(status: Status) -> Color {
    match status {
        Status::Open => Color::Green,
        Status::InProgress => Color::Cyan,
        Status::Blocked => Color::Red,
        Status::Deferred | Status::Closed | Status::Tombstone => Color::DarkGray,
        Status::Draft | Status::Review => Color::Yellow,
        Status::Pinned | Status::Hooked => Color::Magenta,
    }
}

/// Column labels for a swimlane mode, data-driven where the mode needs it.
/// `Status` renders every [`Status`] variant so no issue is invisible;
/// `Priority` is P0–P4 plus every other `priority` value present in the
/// rows, sorted (out-of-range priorities get their own column);
/// `Type` is the sorted distinct `issue_type` values present in the rows
/// (single `ALL` fallback when no row declares a type).
pub fn column_labels(app: &App, mode: SwimlaneMode) -> Vec<String> {
    match mode {
        SwimlaneMode::Status => ALL_STATUSES.iter().map(|s| status_label(*s)).collect(),
        SwimlaneMode::Priority => {
            // P0–P4 always exist as columns (stable navigation); any
            // priority outside that range present in the data gets its own
            // column appended in sorted order instead of being dropped.
            let mut priorities: std::collections::BTreeSet<i64> = (0..=4i64).collect();
            priorities.extend(app.rows.iter().map(|r| r.priority));
            priorities.iter().map(|p| format!("P{p}")).collect()
        }
        SwimlaneMode::Type => {
            let mut types: Vec<String> = app
                .rows
                .iter()
                .map(|r| r.issue_type.clone())
                .filter(|t| !t.is_empty())
                .collect();
            types.sort();
            types.dedup();
            if types.is_empty() {
                vec!["ALL".to_string()]
            } else {
                types
            }
        }
    }
}

/// Render the board view with swimlanes. `selected` highlights the active
/// column (moved with `h`/`l`, jumped with `1`-`9`).
pub fn render_board(f: &mut Frame, app: &App, area: Rect, mode: SwimlaneMode, selected: usize) {
    let columns = column_labels(app, mode);
    // Resolve each column label back to its `Status` once per frame
    // (Status mode only) — doing it per row would allocate a `String`
    // for every row × column.
    let column_status: Vec<Option<Status>> = columns
        .iter()
        .map(|label| {
            ALL_STATUSES
                .iter()
                .copied()
                .find(|s| status_label(*s) == *label)
        })
        .collect();

    let n = columns.len().max(1) as u16;
    let constraints: Vec<Constraint> = (0..n).map(|_| Constraint::Percentage(100 / n)).collect();
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .split(area);

    for (i, label) in columns.iter().enumerate() {
        let items: Vec<&crate::ListRow> = app
            .rows
            .iter()
            .filter(|r| match mode {
                SwimlaneMode::Status => column_status[i] == Some(r.status),
                SwimlaneMode::Priority => {
                    label.strip_prefix('P').and_then(|d| d.parse::<i64>().ok()) == Some(r.priority)
                }
                SwimlaneMode::Type => label == "ALL" || r.issue_type == *label,
            })
            .collect();

        let color = match mode {
            SwimlaneMode::Status => column_status[i].map(status_color).unwrap_or(Color::Gray),
            SwimlaneMode::Priority => {
                match label.strip_prefix('P').and_then(|d| d.parse::<i32>().ok()) {
                    Some(0 | 1) => Color::Red,
                    Some(2) => Color::Yellow,
                    Some(3) => Color::Cyan,
                    _ => Color::Gray,
                }
            }
            SwimlaneMode::Type => Color::Cyan,
        };

        let lines: Vec<Line> = items
            .iter()
            .map(|r| {
                Line::from(vec![
                    Span::styled(format!("P{}", r.priority), Style::default().fg(Color::Red)),
                    Span::raw(" "),
                    Span::styled(&r.title, Style::default().add_modifier(Modifier::BOLD)),
                ])
            })
            .collect();

        let title = if i == selected {
            format!("▶ {label} ({}) ", items.len())
        } else {
            format!(" {label} ({}) ", items.len())
        };
        let border = if i == selected {
            Style::default().fg(color).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(color)
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(border);
        f.render_widget(Paragraph::new(lines).block(block), chunks[i]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swimlane_mode_cycles_three() {
        assert_eq!(SwimlaneMode::Status.next(), SwimlaneMode::Priority);
        assert_eq!(SwimlaneMode::Priority.next(), SwimlaneMode::Type);
        assert_eq!(SwimlaneMode::Type.next(), SwimlaneMode::Status);
    }

    #[test]
    fn labels_are_readable() {
        assert_eq!(SwimlaneMode::Status.label(), "Status");
        assert_eq!(SwimlaneMode::Priority.label(), "Priority");
        assert_eq!(SwimlaneMode::Type.label(), "Type");
    }
}
