//! Detail pane — renders full issue information matching Go bv's viewport.

use bv_core::model::Issue;
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

pub struct GraphScores {
    pub pagerank: f64,
    pub betweenness: f64,
    pub eigenvector: f64,
    pub hubs: f64,
    pub authorities: f64,
    pub critical_path: f64,
}

fn type_icon_md(t: &str) -> &'static str {
    match t {
        "bug" => "🐛",
        "feature" => "✨",
        "task" => "📋",
        "epic" => "🚀",
        "chore" => "🧹",
        _ => "•",
    }
}

fn priority_icon(p: i64) -> &'static str {
    match p {
        0 => "🔴 P0",
        1 => "🟠 P1",
        2 => "🔵 P2",
        3 => "⚪ P3",
        _ => "⚫ P4",
    }
}

fn status_icon(s: &str) -> &'static str {
    match s {
        "open" => "🟢",
        "in_progress" => "🔵",
        "blocked" => "🔴",
        "closed" => "⚫",
        _ => "⚪",
    }
}

fn dep_type_icon(t: &str) -> &'static str {
    match t {
        "blocks" => "⛔",
        "related" => "🔗",
        "parent-child" => "📦",
        "discovered-from" => "🔍",
        _ => "•",
    }
}

fn line_kv(key: &str, val: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{key}: "), Style::default().fg(Color::DarkGray)),
        Span::raw(val.to_string()),
    ])
}

fn line_kv_colored(key: &str, val: &str, color: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{key}: "), Style::default().fg(Color::DarkGray)),
        Span::styled(
            val.to_string(),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
    ])
}

fn section_line(title: &str) -> Line<'static> {
    Line::from(Span::styled(
        title.to_string(),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ))
}

fn indent(text: &str) -> Line<'static> {
    Line::from(Span::raw(format!("  {text}")))
}

/// Build full detail lines for an issue (matching Go updateViewportContent).
pub fn build_detail_lines(
    issue: &Issue,
    graph_scores: Option<&GraphScores>,
    all_issues: Option<&std::collections::HashMap<String, Issue>>,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(Color::DarkGray);

    // Title with type icon
    let icon = type_icon_md(&issue.issue_type);
    lines.push(Line::from(Span::styled(
        format!("{icon} {}", issue.title),
        bold,
    )));
    lines.push(Line::from(""));

    // Meta
    let status_upper = issue.status.as_str().to_uppercase();
    let prio = priority_icon(issue.priority);
    let assignee = if issue.assignee.is_empty() {
        "-"
    } else {
        &issue.assignee
    };
    let created = issue
        .created_at
        .as_deref()
        .and_then(|t| t.parse::<jiff::Timestamp>().ok())
        .map(|t| t.strftime("%Y-%m-%d").to_string())
        .unwrap_or_default();

    lines.push(line_kv("ID", &issue.id));
    lines.push(line_kv_colored("Status", &status_upper, Color::Green));
    lines.push(line_kv("Priority", prio));
    lines.push(line_kv("Assignee", assignee));
    lines.push(line_kv("Created", &created));

    if !issue.labels.is_empty() {
        lines.push(line_kv("Labels", &issue.labels.join(", ")));
    }
    lines.push(Line::from(""));

    // Graph Analysis
    if let Some(gs) = graph_scores {
        lines.push(section_line("📊 Graph Analysis"));
        lines.push(indent(&format!(
            "Impact Depth: {:.0} (downstream chain length)",
            gs.critical_path
        )));
        lines.push(indent(&format!(
            "Centrality: PR {:.4} \u{2022} BW {:.4} \u{2022} EV {:.4}",
            gs.pagerank, gs.betweenness, gs.eigenvector
        )));
        lines.push(indent(&format!(
            "Flow Role: Hub {:.4} \u{2022} Authority {:.4}",
            gs.hubs, gs.authorities
        )));
        lines.push(Line::from(""));
    }

    // Dependency Graph (tree format, matching Go BuildDependencyTree)
    if !issue.dependencies.is_empty() {
        lines.push(section_line("Dependency Graph"));
        let mut visited = std::collections::HashSet::new();
        visited.insert(issue.id.clone());

        // Root node
        let root_icon = status_icon(issue.status.as_str());
        let root_type = type_icon_md(&issue.issue_type);
        lines.push(Line::from(vec![
            Span::raw(format!("\u{1f4cd} {root_icon} {root_type} ")),
            Span::styled(issue.id.clone(), Style::default().fg(Color::Cyan)),
            Span::styled(
                format!(
                    " {} ({})",
                    truncate_title(&issue.title, 40),
                    issue.status.as_str()
                ),
                dim,
            ),
            Span::styled(" [root]", Style::default().fg(Color::DarkGray)),
        ]));

        // Render children recursively
        render_tree_children(
            &issue.id,
            &issue.dependencies,
            all_issues,
            &mut visited,
            0,  // depth
            3,  // max_depth (same as Go)
            "", // prefix
            &mut lines,
        );
        lines.push(Line::from(""));
    }

    // Description
    if !issue.description.is_empty() {
        lines.push(section_line("📝 Description"));
        for l in issue.description.lines() {
            lines.push(Line::from(Span::raw(format!("  {l}"))));
        }
        lines.push(Line::from(""));
    }

    // Design Notes
    if !issue.design.is_empty() {
        lines.push(section_line("🎨 Design Notes"));
        for l in issue.design.lines() {
            lines.push(Line::from(Span::raw(format!("  {l}"))));
        }
        lines.push(Line::from(""));
    }

    // Acceptance Criteria
    if !issue.acceptance_criteria.is_empty() {
        lines.push(section_line("✅ Acceptance Criteria"));
        for l in issue.acceptance_criteria.lines() {
            lines.push(Line::from(Span::raw(format!("  {l}"))));
        }
        lines.push(Line::from(""));
    }

    // Notes
    if !issue.notes.is_empty() {
        lines.push(section_line("📌 Notes"));
        for l in issue.notes.lines() {
            lines.push(Line::from(Span::raw(format!("  {l}"))));
        }
        lines.push(Line::from(""));
    }

    // Comments
    if !issue.comments.is_empty() {
        lines.push(section_line(&format!(
            "💬 Comments ({})",
            issue.comments.len()
        )));
        for c in &issue.comments {
            let author = if c.author.is_empty() {
                "unknown"
            } else {
                &c.author
            };
            lines.push(indent(&format!("{author}: {}", c.text)));
        }
        lines.push(Line::from(""));
    }

    lines
}

fn truncate_title(title: &str, max: usize) -> String {
    let chars: Vec<char> = title.chars().collect();
    if chars.len() > max {
        format!(
            "{}...",
            chars[..max.saturating_sub(3)].iter().collect::<String>()
        )
    } else {
        title.to_string()
    }
}

#[allow(clippy::too_many_arguments)]
fn render_tree_children(
    _parent_id: &str,
    deps: &[bv_core::model::Dependency],
    all_issues: Option<&std::collections::HashMap<String, bv_core::model::Issue>>,
    visited: &mut std::collections::HashSet<String>,
    depth: usize,
    max_depth: usize,
    prefix: &str,
    lines: &mut Vec<Line<'static>>,
) {
    for (i, dep) in deps.iter().enumerate() {
        let target_id = dep.effective_depends_on().to_string();
        let is_last = i == deps.len() - 1;

        let connector = if is_last {
            "\u{2514}\u{2500}\u{2500} "
        } else {
            "\u{251c}\u{2500}\u{2500} "
        };

        // Cycle detection
        if visited.contains(&target_id) {
            lines.push(Line::from(Span::styled(
                format!("{prefix}{connector}\u{26aa} {target_id} (cycle)"),
                Style::default().fg(Color::DarkGray),
            )));
            continue;
        }

        // Depth limit
        if max_depth > 0 && depth >= max_depth {
            lines.push(Line::from(Span::styled(
                format!("{prefix}{connector}\u{26aa} {target_id} (max depth)"),
                Style::default().fg(Color::DarkGray),
            )));
            continue;
        }

        // Look up target issue
        if let Some(target) = all_issues.and_then(|m| m.get(&target_id)) {
            visited.insert(target_id.clone());

            let s_icon = status_icon(target.status.as_str());
            let dt_icon = dep_type_icon(dep.r#type.as_str());
            let title = truncate_title(&target.title, 40);
            let status_str = target.status.as_str();

            lines.push(Line::from(vec![
                Span::raw(format!("{prefix}{connector}")),
                Span::raw(s_icon),
                Span::raw(" "),
                Span::raw(dt_icon),
                Span::raw(" "),
                Span::styled(target_id.clone(), Style::default().fg(Color::Cyan)),
                Span::raw(" "),
                Span::styled(title, Style::default()),
                Span::styled(
                    format!(" ({}) [{}]", status_str, dep.r#type.as_str()),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));

            // Recurse into target's dependencies
            let child_prefix = if is_last {
                format!("{prefix}    ")
            } else {
                format!("{prefix}\u{2502}   ")
            };
            render_tree_children(
                &target_id,
                &target.dependencies,
                all_issues,
                visited,
                depth + 1,
                max_depth,
                &child_prefix,
                lines,
            );

            visited.remove(&target_id);
        } else {
            lines.push(Line::from(Span::styled(
                format!("{prefix}{connector}? {target_id} (not found)"),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }
}
