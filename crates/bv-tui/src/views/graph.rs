//! Graph view — ASCII ego-graph visualization (Go `pkg/ui/graph.go`).
//!
//! Shows the selected issue as a central "ego node" with its blockers above
//! and dependents below, connected by box-drawing lines. A metrics panel
//! shows PageRank, Betweenness, HITS scores. Left panel is a scrollable
//! node list for selection.

use bv_core::model::{Issue, Status};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};
use std::collections::{BTreeMap, HashMap};

/// Precomputed dependency maps for a single issue ("ego graph" data).
pub struct GraphData {
    /// All issues indexed by ID.
    pub issue_map: HashMap<String, Issue>,
    /// Sorted list of all issue IDs.
    pub sorted_ids: Vec<String>,
    /// Blockers per issue (issues it depends on via `Blocks` dep type).
    pub blockers: HashMap<String, Vec<String>>,
    /// Dependents per issue (issues that depend on it).
    pub dependents: HashMap<String, Vec<String>>,
    /// Graph metrics if available.
    pub metrics: Option<super::super::GraphMetrics>,
}

impl GraphData {
    pub fn build(issues: Vec<Issue>, metrics: Option<super::super::GraphMetrics>) -> Self {
        let issue_map: HashMap<String, Issue> =
            issues.iter().map(|i| (i.id.clone(), i.clone())).collect();
        let mut sorted_ids: Vec<String> = issues.iter().map(|i| i.id.clone()).collect();
        sorted_ids.sort();

        let mut blockers: HashMap<String, Vec<String>> = HashMap::new();
        let mut dependents: HashMap<String, Vec<String>> = HashMap::new();

        for issue in &issues {
            let mut my_blockers = Vec::new();
            for dep in &issue.dependencies {
                // `effective_depends_on` folds legacy `depends_on`/`target_id`
                // spellings; `is_blocking` covers `blocks`,
                // `conditional-blocks` and `waits-for` — consistent with
                // `deptree.rs`/`detail.rs` (audit: `audit-tui-graph-legacy-deps`).
                let target = dep.effective_depends_on();
                if dep.r#type.is_blocking() && issue_map.contains_key(target) {
                    my_blockers.push(target.to_string());
                    dependents
                        .entry(target.to_string())
                        .or_default()
                        .push(issue.id.clone());
                }
            }
            my_blockers.sort();
            blockers.insert(issue.id.clone(), my_blockers);
        }
        for deps in dependents.values_mut() {
            deps.sort();
        }

        GraphData {
            issue_map,
            sorted_ids,
            blockers,
            dependents,
            metrics,
        }
    }
}

/// Render the full graph view.
pub fn render_graph(
    f: &mut Frame,
    graph: &GraphData,
    selected_idx: usize,
    scroll_offset: usize,
    area: Rect,
) {
    if graph.sorted_ids.is_empty() {
        let msg =
            Paragraph::new("No issues to display").style(Style::default().fg(Color::DarkGray));
        f.render_widget(msg, area);
        return;
    }

    let idx = selected_idx.min(graph.sorted_ids.len().saturating_sub(1));

    // Wide terminal: left list | right graph; narrow: just graph
    if area.width >= 80 {
        let list_width = if area.width >= 120 { 28 } else { 24 };
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(list_width),
                Constraint::Length(1), // separator
                Constraint::Min(20),
            ])
            .split(area);

        render_node_list(f, graph, selected_idx, scroll_offset, chunks[0]);
        render_separator(f, chunks[1]);
        render_visual_graph(f, graph, idx, chunks[2]);
    } else {
        render_visual_graph(f, graph, idx, area);
    }
}

fn render_separator(f: &mut Frame, area: Rect) {
    let lines: Vec<Line> = (0..area.height)
        .map(|_| Line::from(Span::styled("│", Style::default().fg(Color::DarkGray))))
        .collect();
    let sep = Paragraph::new(lines);
    f.render_widget(sep, area);
}

/// Left panel: scrollable list of all issues.
fn render_node_list(
    f: &mut Frame,
    graph: &GraphData,
    selected_idx: usize,
    scroll_offset: usize,
    area: Rect,
) {
    let visible = area.height.saturating_sub(2) as usize; // account for borders
    if visible == 0 {
        return;
    }

    let start = scroll_offset.min(graph.sorted_ids.len().saturating_sub(visible));
    let end = (start + visible).min(graph.sorted_ids.len());

    let items: Vec<ListItem> = (start..end)
        .map(|i| {
            let id = &graph.sorted_ids[i];
            let issue = graph.issue_map.get(id);
            let (icon, color) = match issue {
                Some(iss) => status_icon_color(&iss.status),
                None => ("❓", Color::DarkGray),
            };
            let display_id = truncate_str(id, area.width.saturating_sub(4) as usize);
            let line = Line::from(vec![
                Span::raw(format!("{icon} ")),
                Span::styled(display_id, Style::default().fg(color)),
            ]);
            ListItem::new(line)
        })
        .collect();

    let header = format!("📊 Nodes ({})", graph.sorted_ids.len());
    let block = Block::default()
        .borders(Borders::ALL)
        .title(header.as_str());

    let list = List::new(items).block(block);
    let mut state = ListState::default();
    state.select(Some(selected_idx.saturating_sub(start)));
    f.render_stateful_widget(list, area, &mut state);
}

/// Right panel: visual ego-graph with blockers → ego → dependents + metrics.
fn render_visual_graph(f: &mut Frame, graph: &GraphData, selected_idx: usize, area: Rect) {
    let id = &graph.sorted_ids[selected_idx];
    let issue = graph.issue_map.get(id);

    let mut lines: Vec<Line> = Vec::new();

    let blocker_ids = graph.blockers.get(id).cloned().unwrap_or_default();
    let dependent_ids = graph.dependents.get(id).cloned().unwrap_or_default();

    // ── Blockers section ──
    if !blocker_ids.is_empty() {
        lines.push(Line::from(Span::styled(
            "▲ BLOCKED BY (must complete first) ▲",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        render_node_row(&mut lines, &blocker_ids, graph, area.width, false);
        render_connectors(&mut lines, blocker_ids.len());
    }

    // ── Ego node ──
    if let Some(iss) = issue {
        let (status_icon, _) = status_icon_color(&iss.status);
        let prio_icon = prio_badge(iss.priority);
        let type_icon = type_icon_str(&iss.issue_type);
        let truncated_id = truncate_str(id, area.width.saturating_sub(4) as usize);
        let truncated_title = truncate_str(&iss.title, area.width.saturating_sub(4) as usize);
        let b_count = blocker_ids.len();
        let d_count = dependent_ids.len();

        lines.push(Line::from(""));
        // Double-border ego node using ╔═══╗ style
        let border_width = (area.width as usize).clamp(20, 60);
        let top_border = format!("╔{}╗", "═".repeat(border_width.saturating_sub(2)));
        lines.push(Line::from(Span::styled(
            center_str(&top_border, area.width as usize),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            center_str(
                &format!("║ {status_icon} {prio_icon} {type_icon} {truncated_id} ║"),
                area.width as usize,
            ),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
        if !truncated_title.is_empty() {
            lines.push(Line::from(Span::styled(
                center_str(&format!("║ {truncated_title} ║"), area.width as usize),
                Style::default().fg(Color::Yellow),
            )));
        }
        lines.push(Line::from(Span::styled(
            center_str(&format!("║ ⬆{b_count}  ⬇{d_count} ║"), area.width as usize),
            Style::default().fg(Color::Yellow),
        )));
        let bot_border = format!("╚{}╝", "═".repeat(border_width.saturating_sub(2)));
        lines.push(Line::from(Span::styled(
            center_str(&bot_border, area.width as usize),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            format!("  ❓ {id}"),
            Style::default().fg(Color::DarkGray),
        )));
    }

    // ── Dependents section ──
    if !dependent_ids.is_empty() {
        render_connectors(&mut lines, dependent_ids.len());
        lines.push(Line::from(Span::styled(
            "▼ BLOCKS (waiting on this) ▼",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        render_node_row(&mut lines, &dependent_ids, graph, area.width, false);
    }

    // ── Metrics panel ──
    lines.push(Line::from(""));
    if let Some(metrics) = &graph.metrics {
        render_metrics_panel(&mut lines, id, metrics, area.width);
    }

    // ── Navigation hint ──
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "j/k: navigate • enter: view details",
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
    )));

    let block = Block::default().borders(Borders::ALL).title(" GRAPH VIEW ");

    let para = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    f.render_widget(para, area);
}

/// Render a horizontal row of node boxes as lines of text.
fn render_node_row(
    lines: &mut Vec<Line>,
    ids: &[String],
    graph: &GraphData,
    total_width: u16,
    _is_ego: bool,
) {
    let max_boxes = 5.min(ids.len()).max(1);
    let box_width = ((total_width as usize).saturating_sub(4) / max_boxes).clamp(12, 20);

    for (i, bid) in ids.iter().enumerate() {
        if i >= 5 {
            let remaining = ids.len() - 5;
            lines.push(Line::from(Span::styled(
                format!("  +{remaining} more"),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )));
            break;
        }
        let issue = graph.issue_map.get(bid);
        let (icon, color) = match issue {
            Some(iss) => status_icon_color(&iss.status),
            None => ("❓", Color::DarkGray),
        };
        let display_id = truncate_str(bid, box_width.saturating_sub(3));
        let title = issue
            .map(|i| truncate_str(&i.title, box_width.saturating_sub(3)))
            .unwrap_or_default();

        // Box top
        let border = format!("┌{}┐", "─".repeat(box_width.saturating_sub(2)));
        lines.push(Line::from(Span::styled(
            format!("  {border}"),
            Style::default().fg(color),
        )));
        // Content line
        let content = format!(
            "  │{icon} {display_id:<w$}│",
            w = box_width.saturating_sub(4)
        );
        lines.push(Line::from(Span::styled(
            content,
            Style::default().fg(color),
        )));
        // Title
        if !title.is_empty() && box_width > 14 {
            let title_line = format!("  │{title:<w$}│", w = box_width.saturating_sub(4));
            lines.push(Line::from(Span::styled(
                title_line,
                Style::default().fg(color),
            )));
        }
        // Box bottom
        let border_bot = format!("└{}┘", "─".repeat(box_width.saturating_sub(2)));
        lines.push(Line::from(Span::styled(
            format!("  {border_bot}"),
            Style::default().fg(color),
        )));
    }
}

/// Render connector lines between blocker/ego/dependent sections.
fn render_connectors(lines: &mut Vec<Line>, count: usize) {
    if count == 0 {
        return;
    }
    lines.push(Line::from(Span::styled(
        "  │",
        Style::default().fg(Color::DarkGray),
    )));
    if count == 1 {
        lines.push(Line::from(Span::styled(
            "  │",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(Span::styled(
            "  ▼",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        // Fan pattern: ├─┼─┤
        let mut pattern = String::from("├");
        for i in 0..count.min(4) {
            if i > 0 {
                pattern.push('┼');
            }
            pattern.push('─');
        }
        pattern.push('┤');
        lines.push(Line::from(Span::styled(
            format!("  {pattern}"),
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(Span::styled(
            "  ▼",
            Style::default().fg(Color::DarkGray),
        )));
    }
}

/// Render the metrics panel below the graph.
fn render_metrics_panel(
    lines: &mut Vec<Line>,
    id: &str,
    metrics: &super::super::GraphMetrics,
    _width: u16,
) {
    lines.push(Line::from(Span::styled(
        "--- Metrics ---",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));

    render_single_metric(lines, id, "PageRank", &metrics.pagerank);
    render_single_metric(lines, id, "Betweenness", &metrics.betweenness);
    render_single_metric(lines, id, "HITS Hub", &metrics.hubs);
    render_single_metric(lines, id, "HITS Auth", &metrics.authorities);
    render_single_metric(lines, id, "Eigenvector", &metrics.eigenvector);
}

fn render_single_metric(lines: &mut Vec<Line>, id: &str, name: &str, map: &BTreeMap<String, f64>) {
    if let Some(&val) = map.get(id) {
        let rank = rank_in_map(map, id);
        let total = map.len();
        let bar = metric_bar(val, 20);
        let line = format!("  {:<14} {} {:.4}  rank {}/{}", name, bar, val, rank, total);
        lines.push(Line::from(Span::styled(
            line,
            Style::default().fg(Color::White),
        )));
    }
}

/// Generate a visual bar for a metric value (0.0-1.0 mapped to filled blocks).
/// Clamped to `[0, max_width]` — a metric above 1.0 (or below 0.0) must not
/// overflow the bar and push the `]` off the line.
fn metric_bar(val: f64, max_width: usize) -> String {
    // `f64::max` drops NaN to the non-NaN operand; the usize cast saturates.
    let filled = ((val.max(0.0) * max_width as f64).round() as usize).min(max_width);
    let empty = max_width.saturating_sub(filled);
    format!("[{}{}]", "█".repeat(filled), "░".repeat(empty))
}

/// Rank of `id` in the map (1-indexed, by descending value).
fn rank_in_map(map: &BTreeMap<String, f64>, id: &str) -> usize {
    let mut vals: Vec<(&String, &f64)> = map.iter().collect();
    vals.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap_or(std::cmp::Ordering::Equal));
    vals.iter().position(|(k, _)| k.as_str() == id).unwrap_or(0) + 1
}

/// Center a string within `width` characters.
fn center_str(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        return s.to_string();
    }
    let pad = (width - len) / 2;
    format!("{}{}", " ".repeat(pad), s)
}

/// Truncate a string to `max_len` characters, appending "…" if truncated.
fn truncate_str(s: &str, max_len: usize) -> String {
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

/// Status icon and color for a Status.
pub fn status_icon_color(status: &Status) -> (&'static str, Color) {
    match status {
        Status::Open => ("🟢", Color::Green),
        Status::InProgress => ("🔵", Color::Cyan),
        Status::Blocked => ("🔴", Color::Red),
        Status::Deferred => ("⏸️", Color::DarkGray),
        Status::Draft => ("📝", Color::Yellow),
        Status::Pinned => ("📌", Color::Magenta),
        Status::Hooked => ("🪝", Color::Magenta),
        Status::Review => ("👀", Color::Yellow),
        Status::Closed => ("✅", Color::DarkGray),
        Status::Tombstone => ("🪦", Color::DarkGray),
    }
}

/// Priority badge string.
pub fn prio_badge(priority: i64) -> &'static str {
    match priority {
        0 => "P0",
        1 => "P1",
        2 => "P2",
        3 => "P3",
        _ => "P4",
    }
}

/// Type icon string.
pub fn type_icon_str(itype: &str) -> &'static str {
    match itype.to_lowercase().as_str() {
        "epic" => "🏛️",
        "feature" => "✨",
        "bug" => "🐛",
        "task" => "📋",
        "chore" => "🧹",
        "docs" => "📄",
        _ => "📋",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bv_core::model::{Dependency, DependencyType, Issue};

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
    fn graph_data_builds_blocker_and_dependent_maps() {
        let a = make_issue("A", Status::Open);
        let mut b = make_issue("B", Status::Open);
        b.dependencies.push(Dependency {
            issue_id: "B".into(),
            depends_on_id: "A".into(),
            depends_on_legacy: String::new(),
            target_id_legacy: String::new(),
            r#type: DependencyType::Blocks,
            created_at: None,
            created_by: String::new(),
        });
        let graph = GraphData::build(vec![a, b], None);
        assert_eq!(graph.blockers.get("B").unwrap().len(), 1);
        assert_eq!(graph.blockers.get("B").unwrap()[0], "A");
        assert!(graph.dependents.contains_key("A"));
        assert_eq!(graph.dependents.get("A").unwrap()[0], "B");
    }

    #[test]
    fn truncate_str_works() {
        assert_eq!(truncate_str("hello", 10), "hello");
        assert_eq!(truncate_str("hello world", 5), "hell…");
        assert_eq!(truncate_str("hi", 1), "…");
    }

    #[test]
    fn metric_bar_produces_correct_length() {
        let bar = metric_bar(0.5, 10);
        assert!(bar.starts_with('['));
        assert!(bar.ends_with(']'));
        assert_eq!(bar.chars().count(), 12); // [ + 10 chars + ]
    }
}
