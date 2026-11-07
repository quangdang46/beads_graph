//! Agent blurb prompt modal — port of Go `pkg/ui/agent_prompt_modal.go`.
//!
//! Asks whether to inject the bv blurb into the detected AGENTS.md/CLAUDE.md.
//! State lives here; key handling lives in `App::handle_agent_prompt_key`
//! (lib.rs), which already covers every case Go's `Update` does
//! (`agent_prompt_modal.go:46-80`).
//!
//! The preview text comes from `bv_core::agents::AGENT_BLURB`. Go reads
//! `agents.AgentBlurb` from `pkg/agents`; Rust's blurb is still marker v4
//! where Go is v7 (`pkg/agents/blurb.go:18`) — that is a `bv-core` gap, not
//! one this file can close. The preview logic below is version-agnostic: it
//! skips any `<!--` line, so it renders whatever the constant holds.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Padding, Paragraph},
    Frame,
};

/// The three options, matching Go's option order
/// (`agent_prompt_modal.go:161`, `:169`, `:177`).
pub const AGENT_PROMPT_OPTIONS: [&str; 3] = ["Yes, add it", "No thanks", "Don't ask again"];

/// Number of blurb lines the preview shows before appending `\n...`.
const PREVIEW_LINES: usize = 6;

/// Pending blurb prompt state (Go `AgentPromptModal`).
#[derive(Debug, Clone)]
pub struct AgentPromptModal {
    pub file_path: String,
    pub file_type: String,
    pub needs_upgrade: bool,
    /// Go's `selection` (0=yes, 1=no, 2=never).
    pub selected: usize,
    /// Project dir the detection ran in — prefs are keyed per project, so the
    /// modal carries it instead of re-reading the ambient cwd on each key.
    pub project_dir: String,
}

impl AgentPromptModal {
    /// Go `left`/`h`/`shift+tab` (`agent_prompt_modal.go:50-54`): a 3-way
    /// cycle that wraps, not clamps — going left off option 0 reaches
    /// "Don't ask again".
    pub fn move_left(&mut self) {
        self.selected = if self.selected == 0 {
            2
        } else {
            self.selected - 1
        };
    }

    /// Go `right`/`l`/`tab` (`agent_prompt_modal.go:55-59`): wraps from 2 to 0.
    pub fn move_right(&mut self) {
        self.selected = if self.selected == 2 {
            0
        } else {
            self.selected + 1
        };
    }
}

/// A truncated preview of the blurb content.
/// Port of Go `getBlurbPreview` (`agent_prompt_modal.go:214-241`): the first
/// 6 lines that are not an HTML marker, not a leading blank, and not a
/// horizontal rule, followed by `\n...`.
pub fn get_blurb_preview() -> String {
    let mut preview: Vec<&str> = Vec::new();
    for line in bv_core::agents::AGENT_BLURB.split('\n') {
        // Skip marker lines.
        if line.starts_with("<!--") {
            continue;
        }
        // Skip empty lines at start.
        if preview.is_empty() && line.trim().is_empty() {
            continue;
        }
        // Skip horizontal rules.
        if line.trim() == "---" {
            continue;
        }
        preview.push(line);
        if preview.len() >= PREVIEW_LINES {
            break;
        }
    }
    format!("{}\n...", preview.join("\n"))
}

/// The buttons line. Go gives the selected option a filled `Primary`
/// background and the other two a `NormalBorder` box (`agent_prompt_modal.go:
/// 158-184`); lipgloss renders a bordered button three rows tall, which a
/// single-line ratatui render cannot. The unselected options keep their box
/// edges as `(` / `)` and the selected one is filled, as in Go.
fn button_line(selected: usize) -> Line<'static> {
    let on = Style::default()
        .fg(Color::Black)
        .bg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let off = Style::default().fg(Color::White);
    // Go's third option ("Don't ask again") uses a muted style rather than
    // the bordered unselected style.
    let mute = Style::default().fg(Color::DarkGray);

    let mut spans: Vec<Span<'static>> = vec![Span::raw("  ")];
    for (i, label) in AGENT_PROMPT_OPTIONS.iter().enumerate() {
        if i == selected {
            spans.push(Span::styled(format!(" {label} "), on));
        } else if i == 2 {
            spans.push(Span::styled(format!(" {label} "), mute));
        } else {
            spans.push(Span::styled(format!("( {label} )"), off));
        }
        spans.push(Span::raw("  "));
    }
    Line::from(spans)
}

/// Render the blurb prompt modal.
///
/// Go `View` (`agent_prompt_modal.go:83-195`): title, four body lines, the
/// "Preview of content to add:" header, the bordered preview box
/// (`Width(m.width-8)`, `MaxHeight(8)`), the buttons, and the footer hint.
pub fn render_agent_prompt(f: &mut Frame, modal: &AgentPromptModal, area: Rect) {
    // Go's `modalStyle` width is 60 (`NewAgentPromptModal`), clamped to the
    // terminal.
    let width = 60u16.min(area.width);
    // Go's `MaxHeight(8)` caps the bordered box at 8 rows; the two border
    // rows leave at most 6 content rows. A 7-line preview (6 content lines +
    // the trailing `...`) would lose the ellipsis to clipping, so fold the
    // overflow back into the `...` marker instead of dropping it.
    const MAX_PREVIEW_ROWS: usize = 6; // 8 - 2 border rows
    let preview_text = get_blurb_preview();
    let mut preview_rows: Vec<&str> = preview_text.lines().collect();
    if preview_rows.len() > MAX_PREVIEW_ROWS {
        preview_rows.truncate(MAX_PREVIEW_ROWS - 1);
        preview_rows.push("...");
    }
    let preview_lines = preview_rows.len();
    // 2 (border) + preview rows, capped by Go's MaxHeight(8).
    let preview_height = (preview_lines as u16 + 2).clamp(3, 8);
    // inner height = height - borders(2) - padding(2), and must fit
    // body(9) + preview + bottom(4) -> height = preview + 17.
    let height = (preview_height + 17).min(area.height);
    let popup = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };

    f.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .padding(Padding::new(2, 2, 1, 1));
    let inner = block.inner(popup);

    // Body text above the preview box (`agent_prompt_modal.go:136-148`).
    let body: Vec<Line<'static>> = vec![
        Line::from(Span::styled(
            "📝 Enhance AI Agent Integration?",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!("We found {} in this project but it", modal.file_type),
            Style::default().fg(Color::White),
        )),
        Line::from(Span::styled(
            "doesn't include beads_viewer instructions.",
            Style::default().fg(Color::White),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Adding these helps AI coding agents understand",
            Style::default().fg(Color::White),
        )),
        Line::from(Span::styled(
            "how to use your issue tracking workflow.",
            Style::default().fg(Color::White),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Preview of content to add:",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )),
    ];
    let body_height = body.len() as u16;
    let top = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: body_height.min(inner.height),
    };
    f.render_widget(Paragraph::new(body).block(block), popup);

    // Preview box: `Width(m.width-8)`, `MaxHeight(8)`
    // (`agent_prompt_modal.go:104-109`).
    let preview_top = top.y.saturating_add(body_height);
    let preview_w = inner.width.saturating_sub(2).max(1);
    let after_preview = preview_top + preview_height;
    let bottom_lines = vec![
        Line::from(""),
        button_line(modal.selected),
        Line::from(""),
        Line::from(Span::styled(
            "← → to select • Enter to confirm • Esc to cancel",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )),
    ];
    let bottom = Rect {
        x: inner.x,
        y: after_preview,
        width: inner.width,
        height: inner
            .y
            .saturating_add(inner.height)
            .saturating_sub(after_preview),
    };
    if bottom.height > 0 {
        f.render_widget(Paragraph::new(bottom_lines), bottom);
    }
    if preview_top < inner.y + inner.height && preview_height > 0 {
        let preview_area = Rect {
            x: top.x,
            y: preview_top,
            width: preview_w,
            height: preview_height.min((inner.y + inner.height).saturating_sub(preview_top)),
        };
        let preview_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .padding(Padding::horizontal(1));
        f.render_widget(
            Paragraph::new(preview_rows.join("\n")).block(preview_block),
            preview_area,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modal() -> AgentPromptModal {
        AgentPromptModal {
            file_path: "/test/AGENTS.md".into(),
            file_type: "AGENTS.md".into(),
            needs_upgrade: false,
            selected: 0,
            project_dir: "/test".into(),
        }
    }

    // -- Go pkg/ui/agent_prompt_modal_test.go -------------------------------

    #[test]
    fn selection_wraps_in_both_directions() {
        let mut m = modal();
        // Left off option 0 wraps to option 2 — otherwise "Don't ask again"
        // is unreachable by arrowing.
        m.move_left();
        assert_eq!(m.selected, 2);
        m.move_right();
        assert_eq!(m.selected, 0);
        m.move_right();
        assert_eq!(m.selected, 1);
        m.move_right();
        assert_eq!(m.selected, 2);
        m.move_right();
        assert_eq!(m.selected, 0);
        m.move_left();
        assert_eq!(m.selected, 2);
        m.move_left();
        assert_eq!(m.selected, 1);
    }

    #[test]
    fn test_get_blurb_preview() {
        let preview = get_blurb_preview();
        assert!(!preview.is_empty());
        assert!(preview.ends_with("..."), "preview must end with ellipsis");
        assert!(preview.contains("Beads"), "preview must contain 'Beads'");
        assert!(
            !preview.contains("<!--"),
            "preview must not contain HTML comment markers"
        );
    }

    #[test]
    fn test_blurb_preview_skips_markers_blanks_and_rules() {
        // The real blurb's first non-marker line should be the first shown.
        let preview = get_blurb_preview();
        let first = preview.lines().next().unwrap();
        assert!(!first.trim().is_empty());
        assert_ne!(first.trim(), "---");
    }
}
