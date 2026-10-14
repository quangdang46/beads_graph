//! Shared per-row span construction for the List pane.
//!
//! [`row_spans`] is the single place the flat list and the dependency-tree
//! mode build a row's spans. Flat mode passes `prefix = ""`; tree mode passes
//! the node's computed `├── `/`└── ` run and a reduced `title_width`. Both
//! render identical columns, which is the point — a tree view that showed
//! different data than the flat view would be a second thing to learn.
//!
//! Extracted from `render_list` so the two modes cannot drift.

use ratatui::{
    style::{Color, Modifier, Style},
    text::Span,
};

/// Everything a row's spans depend on besides the prefix.
pub struct RowCtx<'a> {
    /// The issue row being rendered.
    pub row: &'a crate::ListRow,
    /// Rank within the whole filtered set, not the window — the `#n` badge
    /// means "row N of the list", not "row N of what is on screen".
    pub index: usize,
    /// `App::cursor`; the row at this index carries the `▸` marker.
    pub cursor: usize,
    /// Workspace-mode repo filter active (shows the `[API]` badge).
    pub workspace_repos: bool,
    /// Priority hints toggled on, if any.
    pub priority_hints: Option<&'a std::collections::BTreeMap<String, i32>>,
    /// Per-issue triage badge, if any (quick win / unblocks counts).
    pub triage: Option<&'a bv_analysis::triage::RowTriage>,
    /// Fuzzy (`/`) or semantic (Ctrl+S) search active (shows the `#n` badge).
    pub searching: bool,
    /// Time-Travel diff badge for this row, if any (`🆕`/`~`/`✅`).
    pub diff_badge: Option<&'static str>,
    /// Width left for the title after tree indentation.
    pub title_width: usize,
    /// Dependency edge annotation for tree rows, e.g. `[blocks]`. Empty (or
    /// `None`) in flat mode and on tree roots.
    pub edge_label: Option<&'a str>,
    /// Mute the whole row (advisory dependency subtree in tree mode).
    pub dimmed: bool,
    /// Cycle/max-depth marker suffix in tree mode (` (cycle)`,
    /// ` (max depth)`).
    pub truncation_suffix: Option<&'a str>,
}

/// Columns the row budget must reserve: selection marker (2) + type icon (1) +
/// space + priority (3) + space + status (4) + space + id (20) + space.
///
/// The flat list hardcodes this as `inner_width - 45`. It lives here so the
/// tree mode can subtract its indentation from the same number rather than
/// inventing one.
pub const COLUMN_RESERVE: usize = 45;

/// Build one row's spans, `prefix` first.
pub fn row_spans(prefix: &str, ctx: &RowCtx<'_>) -> Vec<Span<'static>> {
    let row = ctx.row;
    let selected = ctx.index == ctx.cursor;

    let mut spans = Vec::with_capacity(16);

    // Tree indentation goes in first so every column below keeps the same
    // absolute position it has in flat mode... where `prefix` is empty.
    if !prefix.is_empty() {
        spans.push(Span::styled(
            prefix.to_string(),
            Style::default().fg(Color::DarkGray),
        ));
    }

    // Selection indicator
    if selected {
        spans.push(Span::styled(
            "▸ ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    } else {
        spans.push(Span::raw("  "));
    }

    // Type icon
    let (icon, icon_color) = crate::type_icon(&row.issue_type);
    spans.push(Span::styled(icon, Style::default().fg(icon_color)));
    spans.push(Span::raw(" "));

    // Repo badge in workspace mode (Go `RenderRepoBadge`: "[API]", uppercase,
    // capped at 4 chars — `normalizeRepoKey` computed at load into
    // `row.repo_prefix`).
    if ctx.workspace_repos {
        if let Some(prefix) = row.repo_prefix.as_deref() {
            let display: String = prefix.to_uppercase().chars().take(4).collect();
            spans.push(Span::styled(
                format!("[{display}]"),
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::raw(" "));
        }
    }

    // Priority badge
    let (pfg, pbg) = crate::prio_badge_style(row.priority);
    let prio_label = format!("P{}", row.priority);
    spans.push(Span::styled(
        format!("{:<3}", prio_label),
        Style::default()
            .fg(pfg)
            .bg(pbg)
            .add_modifier(Modifier::BOLD),
    ));

    // Priority hint arrow (Go `p` — delegate.go): shown only while hints are
    // toggled on. suggested < current numeric priority means the issue is
    // under-prioritized (should be more urgent) -> up-arrow "increase";
    // suggested > current -> down-arrow.
    if let Some(hints) = ctx.priority_hints {
        match hints.get(&row.id) {
            Some(&suggested) if suggested < row.priority => {
                spans.push(Span::styled(
                    "↑",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            Some(&suggested) if suggested > row.priority => {
                spans.push(Span::styled(
                    "↓",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            _ => spans.push(Span::raw(" ")),
        }
    }
    spans.push(Span::raw(" "));

    // Triage indicator (Go `delegate.go` bv-151): quick-win ⭐, else
    // blocker-with-unblocks 🔓N, else any-unblocks ↪N. Go order: IsQuickWin
    // first, then IsBlocker && UnblocksCount>0, then UnblocksCount>0.
    if let Some(t) = ctx.triage {
        let indicator = if t.is_quick_win {
            Some("⭐".to_string())
        } else if t.is_blocker && t.unblocks_count > 0 {
            Some(format!("🔓{}", t.unblocks_count))
        } else if t.unblocks_count > 0 {
            Some(format!("↪{}", t.unblocks_count))
        } else {
            None
        };
        if let Some(ind) = indicator {
            spans.push(Span::styled(ind, Style::default().fg(Color::Yellow)));
            spans.push(Span::raw(" "));
        }
    }

    // Status badge
    let (slabel, scolor) = crate::status_badge(row.status.as_str());
    spans.push(Span::styled(
        format!("{:<4}", slabel),
        Style::default().fg(scolor),
    ));
    spans.push(Span::raw(" "));

    // Search score badge while a search is active (Go `delegate.go`:
    // `d.ShowSearchScores && i.SearchScoreSet` renders `[0.42]`).
    // Rust has no per-row score store; the score IS the ranking that
    // produced `filtered_indices`, so the badge shows the row's rank
    // position when a fuzzy (/) or semantic (Ctrl+S) filter is active.
    if ctx.searching {
        spans.push(Span::styled(
            format!("#{}", ctx.index + 1),
            Style::default().fg(Color::Cyan),
        ));
        spans.push(Span::raw(" "));
    }

    // ID. In tree mode, a dimmed node (advisory edge or its subtree) renders
    // its id muted, mirroring beans' `RenderBeanRow` `Dimmed` branch.
    spans.push(Span::styled(
        format!("{:<20}", row.id),
        Style::default().fg(if ctx.dimmed {
            Color::DarkGray
        } else {
            Color::Cyan
        }),
    ));
    spans.push(Span::raw(" "));

    // Diff badge in Time-Travel mode (Go `IssueItem.DiffStatus.Badge()` via
    // `getDiffStatus` over newIssueIDs/closedIssueIDs/modifiedIssueIDs).
    if let Some(badge) = ctx.diff_badge {
        spans.push(Span::raw(badge));
        spans.push(Span::raw(" "));
    }

    // Title (truncated to fit)
    let title = if row.title.chars().count() > ctx.title_width {
        let truncated: String = row
            .title
            .chars()
            .take(ctx.title_width.saturating_sub(1))
            .collect();
        format!("{}…", truncated)
    } else {
        format!("{:<width$}", row.title, width = ctx.title_width)
    };
    // Advisory edges (and their subtrees) read as secondary: the title keeps
    // its selected-bold state but loses its ink, matching the muted id above.
    let title_style = if ctx.dimmed {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().add_modifier(if selected {
            Modifier::BOLD
        } else {
            Modifier::empty()
        })
    };
    spans.push(Span::styled(title, title_style));

    // Dependency edge annotation in tree mode (`[blocks]`, `[parent-child]`,
    // ...), dimmed like `detail.rs`'s ` ({status}) [{type}]` suffix. Space
    // reserved inside the title budget by the caller.
    if let Some(label) = ctx.edge_label {
        if !label.is_empty() {
            spans.push(Span::styled(
                format!(" [{label}]"),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }

    // Truncation marker in tree mode, e.g. ` (cycle)` or ` (max depth)`.
    if let Some(marker) = ctx.truncation_suffix {
        if !marker.is_empty() {
            spans.push(Span::styled(
                marker.to_string(),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }

    // Advisory edges (and their subtrees) read as secondary: see the ID and
    // title spans above, which carry the dimming.

    // Age (right-aligned)
    let age = crate::age_str(&row.created_at);
    if !age.is_empty() {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(age, Style::default().fg(Color::DarkGray)));
    }

    spans
}
