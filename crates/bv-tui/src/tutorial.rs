//! Tutorial content — multi-page guided tutorial for new users.
//! Port of Go `pkg/ui/tutorial.go` + `tutorial_content.go` + `tutorial_progress.go`.
//! Each page is a structured tutorial step with title, content, and navigation.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

/// A single tutorial page.
#[derive(Debug, Clone)]
pub struct TutorialPage {
    pub title: String,
    pub content: Vec<String>,
    pub tips: Vec<String>,
}

/// Tutorial state held by App.
pub struct TutorialState {
    pub pages: Vec<TutorialPage>,
    pub current_page: usize,
    pub visible: bool,
}

impl Default for TutorialState {
    fn default() -> Self {
        Self::new()
    }
}

impl TutorialState {
    pub fn new() -> Self {
        TutorialState {
            pages: build_tutorial_pages(),
            current_page: 0,
            visible: false,
        }
    }

    pub fn toggle(&mut self) {
        self.visible = !self.visible;
        if self.visible {
            self.current_page = 0;
        }
    }

    pub fn next_page(&mut self) {
        if self.current_page + 1 < self.pages.len() {
            self.current_page += 1;
        }
    }

    pub fn prev_page(&mut self) {
        if self.current_page > 0 {
            self.current_page -= 1;
        }
    }
}

/// Build the tutorial pages content.
fn build_tutorial_pages() -> Vec<TutorialPage> {
    vec![
        TutorialPage {
            title: "Welcome to bvr".into(),
            content: vec![
                "bvr is a graph-aware triage engine for Beads issue trackers.".into(),
                "It reads .beads/issues.jsonl, builds a dependency DAG,".into(),
                "and computes graph metrics to help you prioritize work.".into(),
                "".into(),
                "This tutorial will guide you through the main features.".into(),
            ],
            tips: vec![
                "Press h or ? anytime to see keyboard shortcuts".into(),
                "Press Esc to close any overlay or return to list".into(),
            ],
        },
        TutorialPage {
            title: "The Issue List".into(),
            content: vec![
                "The main view shows all issues in a scrollable list.".into(),
                "".into(),
                "Navigation:".into(),
                "  j/k or arrow keys: move up/down".into(),
                "  Enter: toggle detail pane".into(),
                "  Tab: focus detail pane".into(),
                "".into(),
                "Filtering:".into(),
                "  a: show all issues".into(),
                "  o: show open issues only".into(),
                "  c: show closed issues".into(),
                "  r: show ready (open, non-blocked) issues".into(),
            ],
            tips: vec![
                "Use / to search by text".into(),
                "L cycles through label filters".into(),
            ],
        },
        TutorialPage {
            title: "Views".into(),
            content: vec![
                "bvr has multiple views for different perspectives:".into(),
                "".into(),
                "  b: Board view (swimlanes by status/priority/type)".into(),
                "  E: Tree view (parent-child hierarchy)".into(),
                "  G: Graph view (ego-graph with metrics)".into(),
                "  i: Insights view (graph metrics panels)".into(),
                "  f: Flow-matrix view (cross-label dependencies)".into(),
                "  A: Attention view (label attention scores)".into(),
                "  h: History view (bead-commit correlations)".into(),
                "  t/T: Time-travel diff vs a git revision".into(),
                "  P: Sprint view (sprint progress dashboard)".into(),
                "  !: Alerts view (drift warnings)".into(),
                "".into(),
                "Press the same key again to return to list view.".into(),
            ],
            tips: vec![
                "The board view shows swimlanes for status, priority, or type".into(),
                "Graph view shows the selected issue's dependency neighborhood".into(),
            ],
        },
        TutorialPage {
            title: "Robot Commands".into(),
            content: vec![
                "bvr provides JSON output commands for AI agents:".into(),
                "".into(),
                "  --robot-triage    Unified triage (top recommendations)".into(),
                "  --robot-next      Single top pick".into(),
                "  --robot-insights  Graph metrics + top-N lists".into(),
                "  --robot-plan      Dependency-respecting execution plan".into(),
                "  --robot-search    Semantic search across issues".into(),
                "  --robot-history   Bead-commit correlation".into(),
                "  --robot-graph     Dependency graph as JSON".into(),
                "".into(),
                "Run bvr --robot-help for the full list.".into(),
                "Run bvr --robot-capabilities for implementation status.".into(),
            ],
            tips: vec![
                "All robot commands output JSON to stdout".into(),
                "Exit code 0=success, 1=error, 2=usage error".into(),
            ],
        },
        TutorialPage {
            title: "Graph Metrics".into(),
            content: vec![
                "bvr computes these graph metrics for each issue:".into(),
                "".into(),
                "  PageRank:    importance based on incoming links".into(),
                "  Betweenness: centrality as bridge between clusters".into(),
                "  HITS Hub/Authority: hub links to many, authority linked from many".into(),
                "  Eigenvector: connects to other important issues".into(),
                "  Critical Path: on the longest dependency chain".into(),
                "  k-Core: in the densest connected subgraph".into(),
                "".into(),
                "These metrics drive the triage scoring algorithm.".into(),
            ],
            tips: vec![
                "High PageRank = many issues depend on this one".into(),
                "High Betweenness = often a bottleneck between groups".into(),
            ],
        },
        TutorialPage {
            title: "Correlation Engine".into(),
            content: vec![
                "bvr correlates commits with beads (issues) using:".into(),
                "".into(),
                "  Explicit ID:  bead ID mentioned in commit message".into(),
                "  Temporal:     same author active in the bead's window".into(),
                "  Co-commit:    files changed alongside the bead's status change".into(),
                "".into(),
                "Use --robot-correlation-stats to see correlation quality.".into(),
                "Use --robot-explain-correlation SHA:beadID for details.".into(),
            ],
            tips: vec![
                "Higher confidence = stronger correlation signal".into(),
                "Use --robot-confirm/reject-correlation to provide feedback".into(),
            ],
        },
    ]
}

/// Render the tutorial view (full area — reached via `` ` ``). Pages turn
/// with `j`/`k` through `TutorialState::next_page`/`prev_page`.
pub fn render_tutorial(f: &mut Frame, state: &TutorialState, area: Rect) {
    if state.pages.is_empty() {
        return;
    }

    let page = &state.pages[state.current_page.min(state.pages.len() - 1)];
    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            format!(" {} ", page.title),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];

    // Content
    for line in &page.content {
        lines.push(Line::from(Span::raw(line.clone())));
    }

    // Tips
    if !page.tips.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            " Tips:",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        for tip in &page.tips {
            lines.push(Line::from(Span::styled(
                format!("  • {tip}"),
                Style::default().fg(Color::Cyan),
            )));
        }
    }

    // Navigation
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(
            " Page {}/{} | j/k: page | `/esc: close",
            state.current_page.min(state.pages.len() - 1) + 1,
            state.pages.len()
        ),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" 📖 TUTORIAL ")
        .border_style(Style::default().fg(Color::Cyan));

    let para = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    f.render_widget(para, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tutorial_navigation() {
        let mut state = TutorialState::new();
        assert_eq!(state.current_page, 0);
        state.next_page();
        assert_eq!(state.current_page, 1);
        state.prev_page();
        assert_eq!(state.current_page, 0);
        state.prev_page(); // no-op at 0
        assert_eq!(state.current_page, 0);
    }

    #[test]
    fn tutorial_has_pages() {
        let state = TutorialState::new();
        assert!(state.pages.len() >= 5);
    }
}
