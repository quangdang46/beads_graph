//! Context-sensitive help overlay — shows relevant shortcuts for the current view.
//! Port of Go `pkg/ui/context_help.go`.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};

use super::keybindings::{build_default_registry, Focus};

/// Truncate `s` to at most `max` chars, replacing the last char with `…`
/// when anything was dropped.
fn truncate_chars(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let mut chars = s.chars();
    let mut out: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        out.pop();
        out.push('…');
    }
    out
}

/// Render the context-sensitive help overlay for the current focus.
///
/// The List focus alone documents ~40 bindings across three categories, so
/// on a short terminal the single-column layout silently clipped most of
/// them (audit: `audit-tui-contexthelp-truncate`). When the one-per-line
/// layout cannot fit, fall back to a two-column compact layout — every
/// binding stays on screen, with long descriptions ellipsized horizontally
/// instead of dropped vertically.
pub fn render_context_help(f: &mut Frame, focus: Focus, area: Rect) {
    let reg = build_default_registry();
    let bindings = reg.bindings_for(focus);

    // Content rows the single-column layout needs: title + blank + one line
    // per binding + one header per category change + blank + footer.
    let categories = bindings
        .iter()
        .fold((String::new(), 0usize), |(prev, n), b| {
            if b.category != prev {
                (b.category.clone(), n + 1)
            } else {
                (prev, n)
            }
        })
        .1;
    let single_rows = bindings.len() + categories + 4;
    // Popup borders eat 2 rows inside `area.height - 2`.
    let avail_rows = area.height.saturating_sub(4) as usize;
    let compact = single_rows > avail_rows && !bindings.is_empty();

    let (popup_width, content_rows) = if compact {
        // Two columns: each binding is `key` + space + truncated desc on one
        // line, so rows halve. Categories are dropped — the keys are unique
        // within a focus anyway.
        let rows = bindings.len().div_ceil(2) + 4;
        (area.width.saturating_sub(4).min(110), rows)
    } else {
        (50.min(area.width.saturating_sub(4)), single_rows)
    };
    let popup_height = (content_rows as u16 + 2).min(area.height.saturating_sub(2));
    let x = (area.width.saturating_sub(popup_width)) / 2;
    let y = (area.height.saturating_sub(popup_height)) / 2;
    let popup = Rect {
        x,
        y,
        width: popup_width,
        height: popup_height,
    };

    f.render_widget(Clear, popup);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!(" Help — {:?}", focus),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    if compact {
        // Column width inside the borders, minus the gap between columns.
        let inner_w = popup_width.saturating_sub(2) as usize;
        let col_w = inner_w.saturating_sub(1) / 2;
        let mid = bindings.len().div_ceil(2);
        for row in 0..mid {
            let mut cells = Vec::with_capacity(2);
            for idx in [row, row + mid] {
                if let Some(b) = bindings.get(idx) {
                    cells.push(format!(
                        "{:<12} {}",
                        b.key,
                        truncate_chars(&b.desc, col_w.saturating_sub(13))
                    ));
                } else {
                    cells.push(String::new());
                }
            }
            lines.push(Line::from(format!(
                "{:<width$}{}",
                cells[0],
                cells[1],
                width = col_w
            )));
        }
    } else {
        let mut current_cat = String::new();
        for binding in bindings {
            if binding.category != current_cat {
                current_cat = binding.category.clone();
                lines.push(Line::from(Span::styled(
                    format!(" {current_cat}:"),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )));
            }
            lines.push(Line::from(format!(
                "  {:<14} {}",
                binding.key, binding.desc
            )));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " Press any key to close",
        Style::default().fg(Color::DarkGray),
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let para = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    f.render_widget(para, popup);
}
