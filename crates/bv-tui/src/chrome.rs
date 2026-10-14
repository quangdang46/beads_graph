//! Help overlay, shortcuts sidebar, and modals — port of Go
//! `pkg/ui` chrome components (help overlay + sidebar + tutorial TOC).

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

/// Help overlay: context-aware keyboard shortcuts for current view.
pub fn render_help(f: &mut Frame, entries: &[(&str, &str)]) {
    let area = centered_rect(60, 70, f.area());
    f.render_widget(Clear, area);

    let lines: Vec<Line> = entries
        .iter()
        .map(|(key, desc)| {
            Line::from(vec![
                Span::styled(
                    format!("  {key:<12}"),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(*desc),
            ])
        })
        .collect();

    let block = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" ? HELP — Press Esc to close "),
    );
    f.render_widget(block, area);
}

/// Shortcuts sidebar: persistent panel on the right edge (width 34).
pub fn render_sidebar(f: &mut Frame, area: Rect, sections: &[(&str, Vec<(&str, &str)>)]) {
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" \u{2328}\u{fe0f} SHORTCUTS ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut y = inner.y;
    for (section_name, bindings) in sections {
        if y >= inner.y + inner.height {
            break;
        }
        // Section header
        let header = Paragraph::new(Line::from(Span::styled(
            *section_name,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        let header_area = Rect {
            x: inner.x + 1,
            y,
            width: inner.width - 2,
            height: 1,
        };
        f.render_widget(header, header_area);
        y += 1;

        for (key, desc) in bindings {
            if y >= inner.y + inner.height {
                break;
            }
            let line = Paragraph::new(Line::from(vec![
                Span::styled(format!("  {key:<8}"), Style::default().fg(Color::Yellow)),
                Span::raw(*desc),
            ]));
            let line_area = Rect {
                x: inner.x + 1,
                y,
                width: inner.width - 2,
                height: 1,
            };
            f.render_widget(line, line_area);
            y += 1;
        }
        y += 1; // blank line between sections
    }
}

/// Modal dialog with title and message body.
pub fn render_modal(f: &mut Frame, title: &str, message: &str) {
    let area = centered_rect(50, 30, f.area());
    f.render_widget(Clear, area);
    let para = Paragraph::new(message)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {title} ")),
        )
        .wrap(ratatui::widgets::Wrap { trim: true });
    f.render_widget(para, area);
}

/// Standard help entries matching the Go keybinding registry.
pub fn default_help_entries() -> Vec<(&'static str, &'static str)> {
    vec![
        ("j/k", "Move down/up"),
        ("g/G", "Jump to top/bottom"),
        ("Ctrl+d/u", "Page down/up"),
        ("/", "Search"),
        ("o/c/r/a", "Filter mode"),
        ("s", "Cycle sort mode"),
        ("b", "Board view"),
        ("E", "Tree view"),
        ("D", "Tree left pane"),
        ("g", "Graph view"),
        ("i", "Insights dashboard"),
        ("h", "History view"),
        ("f", "Flow matrix"),
        ("[ / ]", "Labels / Attention"),
        ("!", "Alerts panel"),
        ("t/T", "Time-travel mode"),
        ("Enter", "Toggle detail pane"),
        ("q/Esc", "Quit / back"),
        ("?", "Toggle help overlay"),
        ("`", "Interactive tutorial"),
        (";", "Shortcuts sidebar"),
    ]
}

/// Centered rectangle helper.
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_help_has_21_entries() {
        assert_eq!(default_help_entries().len(), 21);
    }

    #[test]
    fn help_lists_the_dependency_tree_toggle() {
        // The toggle is a bvr addition, not a Go parity binding, so it must be
        // documented or the `?` overlay will not mention it.
        assert!(default_help_entries().iter().any(|(k, _)| *k == "D"));
    }

    #[test]
    fn help_entries_have_keys_and_descriptions() {
        for (key, desc) in default_help_entries() {
            assert!(!key.is_empty());
            assert!(!desc.is_empty());
        }
    }
}
