//! Markdown rendering for TUI detail pane.
//! Port of Go `pkg/ui/markdown.go` — simplified terminal markdown renderer.
//! Converts markdown text to ratatui Lines with basic styling.

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

/// Render markdown text into ratatui Lines for terminal display.
pub fn render_markdown(text: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for raw_line in text.lines() {
        let trimmed = raw_line.trim_end();

        // Headings
        if let Some(rest) = trimmed.strip_prefix("### ") {
            lines.push(Line::from(Span::styled(
                format!("  {rest}"),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("## ") {
            lines.push(Line::from(Span::styled(
                format!(" {rest}"),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("# ") {
            lines.push(Line::from(Span::styled(
                rest.to_string(),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
            continue;
        }

        // Horizontal rule
        if trimmed == "---" || trimmed == "***" || trimmed == "___" {
            lines.push(Line::from(Span::styled(
                "─".repeat(40),
                Style::default().fg(Color::DarkGray),
            )));
            continue;
        }

        // Code blocks (``` fences)
        if trimmed.starts_with("```") {
            lines.push(Line::from(Span::styled(
                "  ┌─ code ─┐",
                Style::default().fg(Color::DarkGray),
            )));
            continue;
        }

        // Blockquote
        if let Some(rest) = trimmed.strip_prefix("> ") {
            lines.push(Line::from(Span::styled(
                format!("  │ {rest}"),
                Style::default().fg(Color::Cyan),
            )));
            continue;
        }

        // Bullet list
        if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ") {
            let content = &trimmed[2..];
            lines.push(inline_line("  • ", content));
            continue;
        }

        // Numbered list — "N. item" where N is one or more ASCII digits.
        let digits_len: usize = trimmed
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .map(|c| c.len_utf8())
            .sum();
        if digits_len > 0 {
            if let Some(rest) = trimmed[digits_len..].strip_prefix(". ") {
                lines.push(inline_line(
                    &format!("  {}. ", &trimmed[..digits_len]),
                    rest,
                ));
                continue;
            }
        }

        // Regular paragraph
        if !trimmed.is_empty() {
            lines.push(Line::from(render_inline(trimmed)));
        } else {
            lines.push(Line::from(""));
        }
    }
    lines
}

/// Build a `Line` from a literal prefix followed by inline-markdown spans.
fn inline_line(prefix: &str, content: &str) -> Line<'static> {
    let mut spans = Vec::with_capacity(8);
    spans.push(Span::raw(prefix.to_string()));
    spans.extend(render_inline(content));
    Line::from(spans)
}

/// Render inline markdown (bold, italic, code, links) into styled spans.
///
/// Single left-to-right pass: markers are only consumed when a matching
/// closing marker exists, so stray/unmatched markers are preserved literally
/// and no text is dropped or duplicated.
fn render_inline(text: &str) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut rest = text;

    fn flush(buf: &mut String, spans: &mut Vec<Span<'static>>) {
        if !buf.is_empty() {
            spans.push(Span::raw(std::mem::take(buf)));
        }
    }

    while !rest.is_empty() {
        // Bold: **text**  (must be checked before single '*')
        if let Some(after) = rest.strip_prefix("**") {
            if let Some(end) = after.find("**") {
                flush(&mut buf, &mut spans);
                for mut span in render_inline(&after[..end]) {
                    span.style = span.style.add_modifier(Modifier::BOLD);
                    spans.push(span);
                }
                rest = &after[end + 2..];
                continue;
            }
            buf.push_str("**");
            rest = after;
            continue;
        }

        // Inline code: `text`
        if let Some(after) = rest.strip_prefix('`') {
            if let Some(end) = after.find('`') {
                flush(&mut buf, &mut spans);
                spans.push(Span::styled(
                    after[..end].to_string(),
                    Style::default().fg(Color::Green),
                ));
                rest = &after[end + 1..];
                continue;
            }
            buf.push('`');
            rest = after;
            continue;
        }

        // Italic: *text*
        if let Some(after) = rest.strip_prefix('*') {
            if let Some(end) = after.find('*') {
                flush(&mut buf, &mut spans);
                for mut span in render_inline(&after[..end]) {
                    span.style = span.style.add_modifier(Modifier::ITALIC);
                    spans.push(span);
                }
                rest = &after[end + 1..];
                continue;
            }
            buf.push('*');
            rest = after;
            continue;
        }

        // Link: [text](url) — display the link text.
        if rest.starts_with('[') {
            if let Some(close) = rest.find("](") {
                let link_text = &rest[1..close];
                if let Some(url_end) = rest[close + 2..].find(')') {
                    flush(&mut buf, &mut spans);
                    spans.push(Span::styled(
                        link_text.to_string(),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::UNDERLINED),
                    ));
                    rest = &rest[close + 2 + url_end + 1..];
                    continue;
                }
            }
        }

        // Plain character — copy one full UTF-8 char.
        let ch_len = rest.chars().next().map_or(1, |c| c.len_utf8());
        buf.push_str(&rest[..ch_len]);
        rest = &rest[ch_len..];
    }
    flush(&mut buf, &mut spans);
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_heading() {
        let lines = render_markdown("# Title");
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn render_bullet() {
        let lines = render_markdown("- item one\n- item two");
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn render_empty() {
        let lines = render_markdown("");
        assert!(lines.is_empty());
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn inline_code_preserves_text() {
        let lines = render_markdown("use `code` here");
        assert_eq!(line_text(&lines[0]), "use code here");
    }

    #[test]
    fn inline_bold_and_italic() {
        let lines = render_markdown("a **bold** b *it* c");
        assert_eq!(line_text(&lines[0]), "a bold b it c");
        let has_bold = lines[0]
            .spans
            .iter()
            .any(|s| s.content.as_ref() == "bold" && s.style.add_modifier.contains(Modifier::BOLD));
        assert!(has_bold);
    }

    #[test]
    fn inline_stray_markers_kept() {
        let lines = render_markdown("x * y and `z");
        assert_eq!(line_text(&lines[0]), "x * y and `z");
    }

    #[test]
    fn numbered_list_multi_digit() {
        let lines = render_markdown("10. item");
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "  10. item");
    }
}
