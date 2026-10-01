//! The `README.md` that ships at the root of an exported bundle.
//!
//! Unlike the rest of the site, this file is **not** an asset — it is rendered
//! per run from the live analysis, and the oracle builds it with
//! `generateREADME` (`cmd/bv/main.go:5527`). It is the only file in the bundle
//! whose content depends on the issues rather than on the build, so a byte
//! comparison against a frozen golden is impossible by construction: the footer
//! embeds `time.Now()`. Compare everything above the `---`, and treat the
//! footer as a shape check.
//!
//! Every section is conditional, and on a fully-closed workspace five of the
//! seven are absent. That is not a Rust shortcut — it is what the oracle emits,
//! because Go gates each one on a length check. The `Alerts` section is the one
//! that can never appear from `--export-pages` at all: `TriageResult.Alerts`
//! is only ever populated on the drift route (`cmd/bv/robot_registry.go:1222`),
//! and `generateREADME` is handed the `TriageResult` from
//! `ComputeTriageWithOptions`, whose struct literal (`pkg/analysis/triage.go:715`)
//! sets no `Alerts` field. The section is ported anyway — it costs four lines,
//! and the deploy path (`main.go:5919`) does reach a triage that carries them.

use bv_analysis::triage::ProjectCounts;

/// The default heading when `--pages-title` is empty.
const DEFAULT_TITLE: &str = "Project Dashboard";

/// Go's `maxBlockers` / `maxWins` / `maxCycles` caps.
const MAX_TABLE_ROWS: usize = 5;
const MAX_CYCLES: usize = 3;

/// Go's `truncateTitle(blocker.Title, 40)`.
const TITLE_MAX_RUNES: usize = 40;

/// One row of the Critical Bottlenecks table.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockerRow {
    pub id: String,
    pub title: String,
    pub unblocks_count: usize,
    /// Go's `blocker.Actionable`. When false the status cell names the blocker
    /// count instead of saying "Ready".
    pub actionable: bool,
    /// `len(blocker.BlockedBy)` — printed as `Blocked by N`, and deliberately
    /// independent of `actionable` (a blocker with no open blockers and no
    /// unblocks cannot reach this list, so the two never disagree in practice).
    pub blocked_by_count: usize,
}

/// One bullet of the Quick Wins list.
#[derive(Debug, Clone, PartialEq)]
pub struct QuickWinRow {
    pub id: String,
    pub title: String,
    /// `len(qw.UnblocksIDs)`; rendered as ` (unblocks N)` only when non-zero.
    pub unblocks_count: usize,
    pub reason: String,
}

/// One numbered entry of the Top Priorities list.
#[derive(Debug, Clone, PartialEq)]
pub struct TopPickRow {
    pub id: String,
    pub title: String,
    pub score: f64,
    /// `pick.Unblocks`; rendered as ` | **Unblocks:** N issues` when non-zero.
    pub unblocks: usize,
    pub reasons: Vec<String>,
}

/// One line of the Alerts list.
#[derive(Debug, Clone, PartialEq)]
pub struct AlertRow {
    pub alert_type: String,
    /// `critical` or `warning`; anything else is filtered out, as in Go.
    pub severity: String,
    pub message: String,
}

/// Everything `generateREADME` reads. The arguments mirror Go's, which is why
/// they are separate fields rather than a struct borrowed from the analysis
/// crate: Go's `triage *TriageResult` can be `nil`, and each `nil` suppresses
/// a different set of sections.
pub struct ReadmeInput<'a> {
    /// `--pages-title`; empty falls back to [`DEFAULT_TITLE`].
    pub title: &'a str,
    /// Always `""` from `--export-pages` (`main.go:3173` passes a literal), so
    /// the two "View Live Dashboard" lines are unreachable there. They are
    /// ported because the deploy path at `main.go:5919` passes a real URL.
    pub pages_url: &'a str,
    /// Go's `triage.ProjectHealth.Counts`. `None` suppresses the Executive
    /// Summary and the Status Summary.
    pub counts: Option<&'a ProjectCounts>,
    pub top_picks: &'a [TopPickRow],
    pub blockers_to_clear: &'a [BlockerRow],
    pub quick_wins: &'a [QuickWinRow],
    pub alerts: &'a [AlertRow],
    /// Go's `stats.NodeCount` / `stats.EdgeCount` / `stats.Density`. The
    /// Graph Analysis block is gated on `NodeCount > 0`.
    pub node_count: usize,
    pub edge_count: usize,
    pub density: f64,
    /// Go's `stats.Cycles()` — each entry is a node list rendered as
    /// `` `a` → `b` → `c` ``.
    pub cycles: &'a [Vec<String>],
    /// Rendered into the footer.
    pub now: jiff::Timestamp,
}

/// The three row lists, read out of the JSON `--robot-triage` already emits.
///
/// Go has no such split: `generateREADME` takes the whole `*TriageResult` and
/// picks the fields it needs. Rust keeps the ranking inside one `triage_views`
/// helper, so the export path re-reads that helper's JSON rather than scoring
/// the issues a second time. Only the fields the README actually prints are
/// carried over; everything else in the robot payload is ignored on purpose.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct TriageViews {
    pub top_picks: Vec<TopPickRow>,
    pub blockers_to_clear: Vec<BlockerRow>,
    pub quick_wins: Vec<QuickWinRow>,
}

impl TriageViews {
    /// Reads `triage_views`' `{"top_picks": …, "quick_wins": …,
    /// "blockers_to_clear": …}` object.
    ///
    /// Missing or non-array members yield an empty list, which is what the
    /// renderer already does for an absent section — so a payload that grows a
    /// new key cannot break this.
    pub fn from_robot_payload(views: &serde_json::Value) -> Self {
        let strings = |v: Option<&serde_json::Value>| -> Vec<String> {
            v.and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        };
        let rows = |v: Option<&serde_json::Value>| -> Vec<serde_json::Value> {
            v.and_then(|x| x.as_array()).cloned().unwrap_or_default()
        };
        let text = |v: &serde_json::Value, key: &str| {
            v.get(key)
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string()
        };
        let num = |v: &serde_json::Value, key: &str| {
            v.get(key).and_then(|x| x.as_u64()).unwrap_or(0) as usize
        };
        let score = |v: &serde_json::Value| v.get("score").and_then(|x| x.as_f64()).unwrap_or(0.0);
        Self {
            top_picks: rows(views.get("top_picks"))
                .iter()
                .map(|v| TopPickRow {
                    id: text(v, "id"),
                    title: text(v, "title"),
                    score: score(v),
                    unblocks: num(v, "unblocks"),
                    reasons: strings(v.get("reasons")),
                })
                .collect(),
            blockers_to_clear: rows(views.get("blockers_to_clear"))
                .iter()
                .map(|v| BlockerRow {
                    id: text(v, "id"),
                    title: text(v, "title"),
                    unblocks_count: num(v, "unblocks_count"),
                    actionable: v
                        .get("actionable")
                        .and_then(|x| x.as_bool())
                        .unwrap_or(false),
                    // `blocked_by` is present only when the row is not
                    // actionable (`main.rs`), so an absent key means zero and
                    // means it only on the branch that does not read it.
                    blocked_by_count: strings(v.get("blocked_by")).len(),
                })
                .collect(),
            quick_wins: rows(views.get("quick_wins"))
                .iter()
                .map(|v| QuickWinRow {
                    id: text(v, "id"),
                    title: text(v, "title"),
                    unblocks_count: strings(v.get("unblocks_ids")).len(),
                    reason: text(v, "reason"),
                })
                .collect(),
        }
    }
}

/// Renders the bundle's `README.md` and writes it to `bundle_path/README.md`.
///
/// Go treats a README failure as non-fatal (`main.go:3174` logs a warning and
/// continues), so callers get the error and decide.
pub fn generate_readme(bundle_path: &std::path::Path, input: &ReadmeInput) -> std::io::Result<()> {
    std::fs::write(bundle_path.join("README.md"), render_readme(input))
}

/// Renders the document. Split out from the write so the bytes can be asserted
/// without a filesystem.
pub fn render_readme(input: &ReadmeInput) -> String {
    let mut b = String::with_capacity(1024);
    let title = if input.title.is_empty() {
        DEFAULT_TITLE
    } else {
        input.title
    };
    b.push_str(&format!("# {title}\n\n"));

    if !input.pages_url.is_empty() {
        b.push_str(&format!(
            "## 🔗 [View Live Dashboard]({})\n\n",
            input.pages_url
        ));
    }

    write_executive_summary(&mut b, input);
    write_top_priorities(&mut b, input);
    write_bottlenecks(&mut b, input);
    write_cycles_section(&mut b, input);
    write_alerts(&mut b, input);
    write_graph_analysis(&mut b, input);
    write_quick_wins(&mut b, input);
    write_status_summary(&mut b, input);
    write_footer(&mut b, input);
    b
}

fn write_executive_summary(b: &mut String, input: &ReadmeInput) {
    let Some(counts) = input.counts else { return };
    let completion_pct = if counts.total > 0 {
        counts.closed as f64 / counts.total as f64 * 100.0
    } else {
        0.0
    };
    b.push_str("## 📊 Executive Summary\n\n");
    b.push_str(&format!(
        "**{}** total issues | **{:.0}%** complete | **{}** ready to work | **{}** blocked\n\n",
        counts.total, completion_pct, counts.actionable, counts.blocked
    ));
    // Go guards on both counts being non-zero before dividing, so a project with
    // nothing actionable is not reported as 100% blocked.
    if counts.blocked > 0
        && counts.actionable > 0
        && counts.blocked as f64 / counts.actionable as f64 > 1.0
    {
        b.push_str(
            "⚠️ **Health Warning:** More issues are blocked than actionable. \
             Focus on clearing blockers.\n\n",
        );
    }
}

fn write_top_priorities(b: &mut String, input: &ReadmeInput) {
    if input.top_picks.is_empty() {
        return;
    }
    b.push_str("## 🎯 Top Priorities\n\n");
    b.push_str("The graph analysis identified these as the highest-impact items to work on:\n\n");
    for (i, pick) in input.top_picks.iter().enumerate() {
        b.push_str(&format!("### {}. {}\n", i + 1, pick.title));
        b.push_str(&format!(
            "**ID:** `{}` | **Impact Score:** {:.2}",
            pick.id, pick.score
        ));
        if pick.unblocks > 0 {
            b.push_str(&format!(" | **Unblocks:** {} issues", pick.unblocks));
        }
        b.push_str("\n\n");
        if !pick.reasons.is_empty() {
            b.push_str("**Why this matters:**\n");
            for reason in &pick.reasons {
                b.push_str(&format!("- {reason}\n"));
            }
            b.push('\n');
        }
    }
}

fn write_bottlenecks(b: &mut String, input: &ReadmeInput) {
    if input.blockers_to_clear.is_empty() {
        return;
    }
    b.push_str("## 🚧 Critical Bottlenecks\n\n");
    b.push_str(
        "These issues are blocking the most downstream work. \
         Clearing them has outsized impact:\n\n",
    );
    let shown = input.blockers_to_clear.len().min(MAX_TABLE_ROWS);
    b.push_str("| Issue | Title | Unblocks | Status |\n");
    b.push_str("|-------|-------|----------|--------|\n");
    for blocker in &input.blockers_to_clear[..shown] {
        let status = if blocker.actionable {
            "Ready".to_string()
        } else {
            format!("Blocked by {}", blocker.blocked_by_count)
        };
        // Escaped *after* truncation, matching Go: a `|` past the cut is gone
        // and must not be escaped, and the escape cannot lengthen the cell back
        // over the limit.
        let safe_title =
            escape_markdown_table_cell(&truncate_title(&blocker.title, TITLE_MAX_RUNES));
        b.push_str(&format!(
            "| `{}` | {} | **{}** issues | {} |\n",
            blocker.id, safe_title, blocker.unblocks_count, status
        ));
    }
    if input.blockers_to_clear.len() > MAX_TABLE_ROWS {
        b.push_str(&format!(
            "\n*+{} more bottlenecks in the dashboard*\n",
            input.blockers_to_clear.len() - MAX_TABLE_ROWS
        ));
    }
    b.push('\n');
}

fn write_cycles_section(b: &mut String, input: &ReadmeInput) {
    if input.cycles.is_empty() {
        return;
    }
    b.push_str("## 🔴 Dependency Cycles Detected!\n\n");
    b.push_str(
        "**These are structural bugs** that make completion impossible. Fix immediately:\n\n",
    );
    for cycle in input.cycles.iter().take(MAX_CYCLES) {
        b.push_str(&format!("- `{}`\n", cycle.join("` → `")));
    }
    if input.cycles.len() > MAX_CYCLES {
        b.push_str(&format!(
            "\n*+{} more cycles - see dashboard for details*\n",
            input.cycles.len() - MAX_CYCLES
        ));
    }
    b.push('\n');
}

fn write_alerts(b: &mut String, input: &ReadmeInput) {
    if input.alerts.is_empty() {
        return;
    }
    // Go computes the two flags in one pass and then re-filters in the loop, so
    // an alerts list with only `info` entries prints nothing at all.
    let has_critical = input
        .alerts
        .iter()
        .any(|a| a.severity == "critical" || a.severity == "warning");
    if !has_critical {
        return;
    }
    b.push_str("## ⚠️ Alerts\n\n");
    for alert in input.alerts {
        if alert.severity != "critical" && alert.severity != "warning" {
            continue;
        }
        let icon = if alert.severity == "critical" {
            "🔴"
        } else {
            "🟡"
        };
        b.push_str(&format!(
            "- {} **{}**: {}\n",
            icon, alert.alert_type, alert.message
        ));
    }
    b.push('\n');
}

fn write_graph_analysis(b: &mut String, input: &ReadmeInput) {
    if input.node_count == 0 {
        return;
    }
    b.push_str("## 📈 Graph Analysis\n\n");
    // Go's band order is the reverse of the output order: the healthy band is
    // the default and each `else if` narrows it.
    let (band, desc) = if input.density > 0.15 {
        (
            "🔴 High Coupling",
            "Many inter-dependencies; changes cascade widely",
        )
    } else if input.density > 0.05 {
        ("🟡 Moderate", "Normal coupling for a complex project")
    } else {
        (
            "🟢 Healthy",
            "Issues are well-isolated and can be parallelized",
        )
    };
    b.push_str(&format!(
        "- **Dependency Density:** {:.3} ({}) — {}\n",
        input.density, band, desc
    ));
    b.push_str(&format!(
        "- **Graph Size:** {} issues with {} dependencies\n",
        input.node_count, input.edge_count
    ));
    // An empty graph prints neither line: Go gates the "None detected" form on
    // `EdgeCount > 0` so a single-node project is not told it is cycle-free.
    if !input.cycles.is_empty() {
        b.push_str(&format!(
            "- **Cycles:** {} circular dependencies detected (must fix!)\n",
            input.cycles.len()
        ));
    } else if input.edge_count > 0 {
        b.push_str("- **Cycles:** None detected ✓\n");
    }
    b.push('\n');
}

fn write_quick_wins(b: &mut String, input: &ReadmeInput) {
    if input.quick_wins.is_empty() {
        return;
    }
    b.push_str("## 🏃 Quick Wins\n\n");
    b.push_str("Low-effort items that clear the path forward:\n\n");
    for win in input.quick_wins.iter().take(MAX_TABLE_ROWS) {
        let unblock_text = if win.unblocks_count > 0 {
            format!(" (unblocks {})", win.unblocks_count)
        } else {
            String::new()
        };
        b.push_str(&format!(
            "- **{}**: {}{}\n",
            win.id, win.title, unblock_text
        ));
        if !win.reason.is_empty() {
            b.push_str(&format!("  - *{}*\n", win.reason));
        }
    }
    if input.quick_wins.len() > MAX_TABLE_ROWS {
        b.push_str(&format!(
            "\n*+{} more quick wins in the dashboard*\n",
            input.quick_wins.len() - MAX_TABLE_ROWS
        ));
    }
    b.push('\n');
}

fn write_status_summary(b: &mut String, input: &ReadmeInput) {
    let Some(counts) = input.counts else { return };
    b.push_str("## 📋 Status Summary\n\n");

    // Go iterates the literal list `[]int{0,1,2,3,4}`, so a P5 bead is dropped
    // even when it is the only one at that priority.
    if !counts.by_priority.is_empty() {
        let items: Vec<String> = (0..=4)
            .filter_map(|p| {
                let count = *counts.by_priority.get(&p.to_string())?;
                (count > 0).then(|| format!("P{p}: {count}"))
            })
            .collect();
        if !items.is_empty() {
            b.push_str(&format!("**By Priority:** {}\n\n", items.join(" | ")));
        }
    }

    if !counts.by_type.is_empty() {
        // Go sorts the *rendered* `"type: count"` strings, not the type names.
        // The two orders agree for today's type set but diverge in general:
        // "a0: 1" sorts before "a: 2" because `0` < `:`, while the names alone
        // would put "a" first.
        let mut items: Vec<String> = counts
            .by_type
            .iter()
            .filter(|(_, count)| **count > 0)
            .map(|(t, count)| format!("{t}: {count}"))
            .collect();
        if !items.is_empty() {
            items.sort();
            b.push_str(&format!("**By Type:** {}\n\n", items.join(" | ")));
        }
    }
}

fn write_footer(b: &mut String, input: &ReadmeInput) {
    b.push_str("---\n\n");
    b.push_str(&format!(
        "*Generated {} by [bvr](https://github.com/quangdang46/beads_viewer_rust)*\n\n",
        go_readable_timestamp(input.now)
    ));
    if !input.pages_url.is_empty() {
        b.push_str(&format!(
            "**[Open Interactive Dashboard]({})** for full details, dependency graph, search, and time-travel.\n",
            input.pages_url
        ));
    }
}

/// Go's `time.Format("Jan 2, 2006 at 3:04 PM MST")`.
///
/// The layout's `MST` renders the zone's *abbreviation*, falling back to the
/// whole-hour offset when the zone has no name — the same fallback the
/// `--export-md` footer at `main.rs:5806` already had to implement. `time.Now()`
/// carries the process-local zone, so that is what this renders in.
pub fn go_readable_timestamp(ts: jiff::Timestamp) -> String {
    go_readable_timestamp_in(
        ts,
        &jiff::tz::TimeZone::try_system().unwrap_or(jiff::tz::TimeZone::UTC),
    )
}

/// As [`go_readable_timestamp`], in an explicit zone.
///
/// Separated so the layout can be asserted hermetically: the rendered zone label
/// is a property of the machine, not of the format string.
pub fn go_readable_timestamp_in(ts: jiff::Timestamp, tz: &jiff::tz::TimeZone) -> String {
    let zoned = ts.to_zoned(tz.clone());
    // `%-d` and `%-I` are the unpadded forms, matching Go's `2` and `3`, which
    // are the layout's own no-pad day and 12-hour directives rather than the
    // `02`/`03` next to them.
    jiff::fmt::strtime::format("%b %-d, %Y at %-I:%M %p %Z", &zoned)
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// Go `truncateTitle` (`main.go:5770`): cut to `max_len` runes, the last three
/// being the ellipsis. A title that already fits is returned untouched — Go
/// does not trim to exactly `max_len` with a trailing `...`.
///
/// The minimum of 4 is Go's own guard: below it, `max_len - 3` would be
/// negative and the slice would panic.
pub fn truncate_title(title: &str, max_len: usize) -> String {
    let max_len = if max_len < 4 { 4 } else { max_len };
    // Go slices `[]rune`, so the cut is by character. `char_indices` walks the
    // same units, and its byte offsets are what the truncation needs.
    let mut cut = max_len;
    let mut end = title.len();
    for (seen, (at, _)) in title.char_indices().enumerate() {
        if seen == max_len {
            end = at;
            cut = max_len - 3;
            break;
        }
    }
    if end == title.len() && title.chars().count() <= max_len {
        return title.to_string();
    }
    let head: String = title.chars().take(cut).collect();
    format!("{head}...")
}

/// Go `escapeMarkdownTableCell` (`main.go:5781`).
///
/// `|` is what breaks a table row; a newline would end the row and a CR would
/// survive it as a literal. No other character needs escaping in GitHub
/// tables, and Go escapes nothing else.
pub fn escape_markdown_table_cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ").replace('\r', "")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(rfc3339: &str) -> jiff::Timestamp {
        rfc3339.parse().unwrap()
    }

    /// The counts the Go oracle reports for this repository's
    /// `.beads/issues.jsonl`, read off a real `bv --export-pages` run. Only
    /// these four fields reach a fully-closed workspace's README.
    fn golden_counts() -> ProjectCounts {
        ProjectCounts {
            total: 46,
            open: 0,
            closed: 46,
            blocked: 0,
            actionable: 0,
            not_closed: 0,
            dependency_blocked: 0,
            not_actionable_count: 0,
            by_status: [("closed".to_string(), 46)].into_iter().collect(),
            by_type: [
                ("chore".to_string(), 1),
                ("epic".to_string(), 1),
                ("feature".to_string(), 2),
                ("task".to_string(), 42),
            ]
            .into_iter()
            .collect(),
            by_priority: [
                ("0".to_string(), 2),
                ("1".to_string(), 22),
                ("2".to_string(), 13),
                ("3".to_string(), 9),
            ]
            .into_iter()
            .collect(),
        }
    }

    /// Byte-for-byte against the oracle's own output for this repository:
    /// `bv --export-pages` on `.beads/issues.jsonl` emits a 569-byte README.
    /// The footer is the only run-dependent line, so it is pinned to both a
    /// fixed instant and a fixed zone and the whole file becomes comparable.
    ///
    /// With the machine-local zone instead of UTC this renders
    /// `at 9:55 PM +07`, which is what the oracle printed on the same host at
    /// the same instant — the differential harness compares two processes on
    /// one machine, so the zone agrees even though it is not portable.
    #[test]
    fn matches_the_oracle_byte_for_byte() {
        let counts = golden_counts();
        let input = ReadmeInput {
            title: "",
            pages_url: "",
            counts: Some(&counts),
            top_picks: &[],
            blockers_to_clear: &[],
            quick_wins: &[],
            alerts: &[],
            node_count: 46,
            edge_count: 41,
            density: 41.0 / (46.0 * 45.0),
            cycles: &[],
            now: at("2026-09-28T14:55:00Z"),
        };
        let mut rendered = render_readme(&input);
        let footer_at = rendered.find("*Generated ").unwrap();
        let stamp = go_readable_timestamp_in(input.now, &jiff::tz::TimeZone::UTC);
        rendered.replace_range(footer_at.., &format!("*Generated {stamp} by [bvr](https://github.com/quangdang46/beads_viewer_rust)*\n\n"));
        assert_eq!(
            rendered,
            concat!(
                "# Project Dashboard\n\n",
                "## 📊 Executive Summary\n\n",
                "**46** total issues | **100%** complete | **0** ready to work | **0** blocked\n\n",
                "## 📈 Graph Analysis\n\n",
                "- **Dependency Density:** 0.020 (🟢 Healthy) — Issues are well-isolated and can be parallelized\n",
                "- **Graph Size:** 46 issues with 41 dependencies\n",
                "- **Cycles:** None detected ✓\n\n",
                "## 📋 Status Summary\n\n",
                "**By Priority:** P0: 2 | P1: 22 | P2: 13 | P3: 9\n\n",
                "**By Type:** chore: 1 | epic: 1 | feature: 2 | task: 42\n\n",
                "---\n\n",
                "*Generated Sep 28, 2026 at 2:55 PM UTC by [bvr](https://github.com/quangdang46/beads_viewer_rust)*\n\n",
            ),
            "569-byte oracle output"
        );
    }

    /// The oracle's 569-byte README has no `View Live Dashboard` line on the
    /// `--export-pages` path, because `main.go:3173` passes a literal `""`.
    #[test]
    fn the_live_dashboard_link_is_reachable_only_with_a_url() {
        let counts = golden_counts();
        let without = ReadmeInput {
            title: "",
            pages_url: "",
            counts: Some(&counts),
            top_picks: &[],
            blockers_to_clear: &[],
            quick_wins: &[],
            alerts: &[],
            node_count: 0,
            edge_count: 0,
            density: 0.0,
            cycles: &[],
            now: at("2026-01-01T00:00:00Z"),
        };
        assert!(!render_readme(&without).contains("View Live Dashboard"));

        let with = ReadmeInput {
            pages_url: "https://example.test/",
            ..without
        };
        let rendered = render_readme(&with);
        assert!(rendered.contains("## 🔗 [View Live Dashboard](https://example.test/)"));
        assert!(rendered
            .contains("**[Open Interactive Dashboard](https://example.test/)** for full details"));
    }

    /// Go gates Graph Analysis on `NodeCount > 0`, and prints the cycles line
    /// only when `EdgeCount > 0` — an edgeless graph says nothing about cycles.
    #[test]
    fn an_empty_graph_prints_no_graph_section() {
        let input = ReadmeInput {
            title: "T",
            pages_url: "",
            counts: None,
            top_picks: &[],
            blockers_to_clear: &[],
            quick_wins: &[],
            alerts: &[],
            node_count: 0,
            edge_count: 0,
            density: 0.0,
            cycles: &[],
            now: at("2026-01-01T00:00:00Z"),
        };
        let rendered = render_readme(&input);
        assert!(!rendered.contains("Graph Analysis"), "{rendered}");
        assert!(!rendered.contains("Cycles"), "{rendered}");
        // With every conditional section suppressed, the title and the footer
        // are the whole document.
        assert!(rendered.starts_with("# T\n\n---\n\n"), "{rendered}");
    }

    /// A single-node graph has edges but no cycles, and Go's `EdgeCount > 0`
    /// guard is what turns that into the reassuring "None detected ✓" line.
    #[test]
    fn cycles_line_requires_edges() {
        let base = ReadmeInput {
            title: "T",
            pages_url: "",
            counts: None,
            top_picks: &[],
            blockers_to_clear: &[],
            quick_wins: &[],
            alerts: &[],
            node_count: 1,
            edge_count: 0,
            density: 0.0,
            cycles: &[],
            now: at("2026-01-01T00:00:00Z"),
        };
        assert!(!render_readme(&base).contains("Cycles:"));
    }

    /// The three density bands, checked on their exact boundaries. Go tests
    /// `> 0.15` and `> 0.05`, so 0.15 itself is "Moderate" and 0.05 is "Healthy".
    #[test]
    fn density_bands_match_go_boundaries() {
        let line_for = |density: f64| {
            let input = ReadmeInput {
                title: "T",
                pages_url: "",
                counts: None,
                top_picks: &[],
                blockers_to_clear: &[],
                quick_wins: &[],
                alerts: &[],
                node_count: 10,
                edge_count: 5,
                density,
                cycles: &[],
                now: at("2026-01-01T00:00:00Z"),
            };
            render_readme(&input)
                .lines()
                .find(|l| l.contains("Dependency Density"))
                .unwrap()
                .to_string()
        };
        assert!(line_for(0.5).contains("(🔴 High Coupling) — Many inter-dependencies"));
        assert!(line_for(0.150001).contains("(🔴 High Coupling)"));
        assert!(line_for(0.15).contains("(🟡 Moderate) — Normal coupling"));
        assert!(line_for(0.050001).contains("(🟡 Moderate)"));
        assert!(line_for(0.05).contains("(🟢 Healthy) — Issues are well-isolated"));
        assert!(line_for(0.0).contains("(🟢 Healthy)"));
        // The density itself is rendered at three decimals.
        assert!(line_for(0.150001).contains("**Dependency Density:** 0.150"));
    }

    /// More blocked than actionable raises the health warning; the guard is on
    /// both being non-zero, so a zero-actionable project is not reported as
    /// infinitely blocked.
    #[test]
    fn health_warning_only_when_both_nonzero_and_blocked_exceeds() {
        let render = |blocked: usize, actionable: usize| {
            let mut counts = golden_counts();
            counts.blocked = blocked;
            counts.actionable = actionable;
            render_readme(&ReadmeInput {
                title: "T",
                pages_url: "",
                counts: Some(&counts),
                top_picks: &[],
                blockers_to_clear: &[],
                quick_wins: &[],
                alerts: &[],
                node_count: 0,
                edge_count: 0,
                density: 0.0,
                cycles: &[],
                now: at("2026-01-01T00:00:00Z"),
            })
        };
        assert!(!render(3, 3).contains("Health Warning"));
        assert!(render(4, 3).contains("Health Warning"));
        assert!(!render(0, 3).contains("Health Warning"));
        assert!(!render(3, 0).contains("Health Warning"));
    }

    #[test]
    fn priority_breaks_stop_at_p4() {
        let mut counts = golden_counts();
        counts.by_priority.insert("5".to_string(), 7);
        let rendered = render_readme(&ReadmeInput {
            title: "T",
            pages_url: "",
            counts: Some(&counts),
            top_picks: &[],
            blockers_to_clear: &[],
            quick_wins: &[],
            alerts: &[],
            node_count: 0,
            edge_count: 0,
            density: 0.0,
            cycles: &[],
            now: at("2026-01-01T00:00:00Z"),
        });
        assert!(rendered.contains("P0: 2 | P1: 22 | P2: 13 | P3: 9"));
        assert!(!rendered.contains("P5"), "Go iterates [0,1,2,3,4] only");
    }

    /// Go sorts the rendered `"type: count"` strings, not the names. `:` is
    /// 0x3a and `0` is 0x30, so a numeric suffix sorts *before* its prefix.
    #[test]
    fn types_sort_on_the_rendered_string() {
        let mut counts = golden_counts();
        counts.by_type = [
            ("a".to_string(), 2),
            ("a0".to_string(), 1),
            ("a-".to_string(), 1),
        ]
        .into_iter()
        .collect();
        let rendered = render_readme(&ReadmeInput {
            title: "T",
            pages_url: "",
            counts: Some(&counts),
            top_picks: &[],
            blockers_to_clear: &[],
            quick_wins: &[],
            alerts: &[],
            node_count: 0,
            edge_count: 0,
            density: 0.0,
            cycles: &[],
            now: at("2026-01-01T00:00:00Z"),
        });
        // Byte order over the rendered strings: `-` (0x2d) < `0` (0x30) < `:` (0x3a).
        // Sorting the names instead would give "a, a0, a-".
        assert!(
            rendered.contains("**By Type:** a-: 1 | a0: 1 | a: 2"),
            "{rendered}"
        );
    }

    #[test]
    fn alerts_ignore_info_severity() {
        let alerts = vec![
            AlertRow {
                alert_type: "stale".into(),
                severity: "info".into(),
                message: "fyi".into(),
            },
            AlertRow {
                alert_type: "cycle".into(),
                severity: "critical".into(),
                message: "boom".into(),
            },
            AlertRow {
                alert_type: "velocity_drop".into(),
                severity: "warning".into(),
                message: "slow".into(),
            },
        ];
        let rendered = render_readme(&ReadmeInput {
            title: "T",
            pages_url: "",
            counts: None,
            top_picks: &[],
            blockers_to_clear: &[],
            quick_wins: &[],
            alerts: &alerts,
            node_count: 0,
            edge_count: 0,
            density: 0.0,
            cycles: &[],
            now: at("2026-01-01T00:00:00Z"),
        });
        assert!(rendered.contains("## ⚠️ Alerts"));
        assert!(rendered.contains("- 🔴 **cycle**: boom"));
        assert!(rendered.contains("- 🟡 **velocity_drop**: slow"));
        assert!(!rendered.contains("fyi"), "info severity is filtered");
    }

    /// An alerts list with nothing at `critical`/`warning` prints no heading at
    /// all — Go's two-pass guard, not a bug.
    #[test]
    fn an_info_only_alert_list_prints_nothing() {
        let alerts = vec![AlertRow {
            alert_type: "stale".into(),
            severity: "info".into(),
            message: "fyi".into(),
        }];
        let rendered = render_readme(&ReadmeInput {
            title: "T",
            pages_url: "",
            counts: None,
            top_picks: &[],
            blockers_to_clear: &[],
            quick_wins: &[],
            alerts: &alerts,
            node_count: 0,
            edge_count: 0,
            density: 0.0,
            cycles: &[],
            now: at("2026-01-01T00:00:00Z"),
        });
        assert!(!rendered.contains("Alerts"));
    }

    #[test]
    fn the_bottleneck_table_caps_at_five_and_counts_the_rest() {
        let rows: Vec<BlockerRow> = (0..8)
            .map(|i| BlockerRow {
                id: format!("bd-{i}"),
                title: format!("Issue {i}"),
                unblocks_count: i,
                actionable: i % 2 == 0,
                blocked_by_count: 2,
            })
            .collect();
        let rendered = render_readme(&ReadmeInput {
            title: "T",
            pages_url: "",
            counts: None,
            top_picks: &[],
            blockers_to_clear: &rows,
            quick_wins: &[],
            alerts: &[],
            node_count: 0,
            edge_count: 0,
            density: 0.0,
            cycles: &[],
            now: at("2026-01-01T00:00:00Z"),
        });
        assert_eq!(rendered.matches("| `bd-").count(), 5);
        assert!(rendered.contains("*+3 more bottlenecks in the dashboard*"));
        assert!(rendered.contains("| `bd-0` | Issue 0 | **0** issues | Ready |"));
        assert!(rendered.contains("| `bd-1` | Issue 1 | **1** issues | Blocked by 2 |"));
    }

    #[test]
    fn quick_wins_cap_at_five_and_spell_the_unblock_count() {
        let rows: Vec<QuickWinRow> = (0..7)
            .map(|i| QuickWinRow {
                id: format!("qw-{i}"),
                title: format!("Win {i}"),
                unblocks_count: i,
                reason: if i == 0 {
                    String::new()
                } else {
                    format!("Unblocks {i} items")
                },
            })
            .collect();
        let rendered = render_readme(&ReadmeInput {
            title: "T",
            pages_url: "",
            counts: None,
            top_picks: &[],
            blockers_to_clear: &[],
            quick_wins: &rows,
            alerts: &[],
            node_count: 0,
            edge_count: 0,
            density: 0.0,
            cycles: &[],
            now: at("2026-01-01T00:00:00Z"),
        });
        assert!(
            rendered.contains("- **qw-0**: Win 0\n"),
            "no parenthetical at 0"
        );
        assert!(rendered.contains("- **qw-1**: Win 1 (unblocks 1)\n"));
        assert!(rendered.contains("  - *Unblocks 1 items*\n"));
        assert_eq!(rendered.matches("- **qw-").count(), 5);
        assert!(rendered.contains("*+2 more quick wins in the dashboard*"));
    }

    #[test]
    fn top_picks_have_no_cap_and_hide_zero_unblocks() {
        let picks: Vec<TopPickRow> = (0..8)
            .map(|i| TopPickRow {
                id: format!("tp-{i}"),
                title: format!("Pick {i}"),
                score: 1.5,
                unblocks: i,
                reasons: vec![format!("reason {i}")],
            })
            .collect();
        let rendered = render_readme(&ReadmeInput {
            title: "T",
            pages_url: "",
            counts: None,
            top_picks: &picks,
            blockers_to_clear: &[],
            quick_wins: &[],
            alerts: &[],
            node_count: 0,
            edge_count: 0,
            density: 0.0,
            cycles: &[],
            now: at("2026-01-01T00:00:00Z"),
        });
        assert_eq!(rendered.matches("### ").count(), 8);
        assert!(rendered.contains("**ID:** `tp-0` | **Impact Score:** 1.50\n\n"));
        assert!(rendered
            .contains("**ID:** `tp-1` | **Impact Score:** 1.50 | **Unblocks:** 1 issues\n\n"));
    }

    #[test]
    fn cycles_cap_at_three() {
        let cycles: Vec<Vec<String>> = (0..5)
            .map(|i| vec![format!("a{i}"), format!("b{i}"), format!("a{i}")])
            .collect();
        let rendered = render_readme(&ReadmeInput {
            title: "T",
            pages_url: "",
            counts: None,
            top_picks: &[],
            blockers_to_clear: &[],
            quick_wins: &[],
            alerts: &[],
            node_count: 4,
            edge_count: 4,
            density: 0.0,
            cycles: &cycles,
            now: at("2026-01-01T00:00:00Z"),
        });
        assert_eq!(rendered.matches("- `a0` → `b0` → `a0`").count(), 1);
        assert!(rendered.contains("- `a0` → `b0` → `a0`"));
        assert!(rendered.contains("*+2 more cycles - see dashboard for details*"));
        assert!(rendered.contains("- **Cycles:** 5 circular dependencies detected (must fix!)"));
    }

    /// The bridge from the `--robot-triage` JSON to the README's rows. The
    /// payload shape below is what `main.rs::triage_views` actually emits: the
    /// `unblocks_ids` and `blocked_by` keys are conditional, so both the
    /// present and absent cases are covered.
    #[test]
    fn reads_rows_out_of_the_robot_payload() {
        let views = TriageViews::from_robot_payload(&serde_json::json!({
            "top_picks": [
                {"id": "tp-1", "title": "First", "score": 0.75, "reasons": ["a", "b"], "unblocks": 3}
            ],
            "quick_wins": [
                {"id": "qw-1", "title": "Quick", "status": "open", "score": 0.5,
                 "reason": "Unblocks 2 items", "unblocks_ids": ["x", "y"]},
                {"id": "qw-2", "title": "Solo", "status": "open", "score": 0.4,
                 "reason": "Low complexity"}
            ],
            "blockers_to_clear": [
                {"id": "bd-1", "title": "Blocked one", "unblocks_count": 4,
                 "actionable": false, "blocked_by": ["a", "b", "c"]},
                {"id": "bd-2", "title": "Ready one", "unblocks_count": 1,
                 "actionable": true}
            ]
        }));
        assert_eq!(
            views.top_picks,
            vec![TopPickRow {
                id: "tp-1".into(),
                title: "First".into(),
                score: 0.75,
                unblocks: 3,
                reasons: vec!["a".into(), "b".into()],
            }]
        );
        assert_eq!(views.quick_wins[0].unblocks_count, 2);
        assert_eq!(views.quick_wins[0].reason, "Unblocks 2 items");
        assert_eq!(views.quick_wins[1].unblocks_count, 0, "absent unblocks_ids");
        assert!(!views.blockers_to_clear[0].actionable);
        assert_eq!(views.blockers_to_clear[0].blocked_by_count, 3);
        assert_eq!(
            views.blockers_to_clear[1].blocked_by_count, 0,
            "absent blocked_by means zero"
        );
    }

    /// A payload missing every key is an empty view, not a panic — the
    /// renderer already treats an empty list as "section absent".
    #[test]
    fn an_empty_payload_yields_no_rows() {
        assert_eq!(
            TriageViews::from_robot_payload(&serde_json::json!({})),
            TriageViews::default()
        );
        assert_eq!(
            TriageViews::from_robot_payload(&serde_json::Value::Null),
            TriageViews::default()
        );
    }

    #[test]
    fn truncate_title_cuts_by_runes_with_an_ellipsis() {
        assert_eq!(truncate_title("short", 40), "short");
        let forty = "x".repeat(40);
        assert_eq!(
            truncate_title(&forty, 40),
            forty,
            "exactly max_len is untouched"
        );
        let forty_one = "x".repeat(41);
        assert_eq!(truncate_title(&forty_one, 40).len(), 40);
        assert!(truncate_title(&forty_one, 40).ends_with("..."));
        // Multibyte: Go slices `[]rune`, so 41 accented characters cut at 37
        // characters plus the ellipsis, and the byte count is not 40.
        let accented = "é".repeat(41);
        let cut = truncate_title(&accented, 40);
        assert_eq!(cut.chars().count(), 40);
        assert_eq!(cut.chars().take(37).collect::<String>(), "é".repeat(37));
    }

    #[test]
    fn table_cells_escape_pipes_and_newlines_only() {
        assert_eq!(escape_markdown_table_cell("a|b"), "a\\|b");
        // Go replaces `\n` with a space first, so `a\nb\rc` becomes "a bc" —
        // the CR is removed outright rather than becoming a second space.
        assert_eq!(escape_markdown_table_cell("a\nb\rc"), "a bc");
        assert_eq!(
            escape_markdown_table_cell("100% <b> & 'q'"),
            "100% <b> & 'q'"
        );
    }

    /// Go's `MST` renders a whole-hour offset when the zone has no name. On a
    /// machine whose zone does have one it renders the name instead, so the
    /// layout is asserted in UTC and only the shape is pinned.
    #[test]
    fn readable_timestamp_has_go_layout() {
        let utc = jiff::tz::TimeZone::UTC;
        let stamp = go_readable_timestamp_in(at("2026-01-02T15:04:00Z"), &utc);
        let fields: Vec<&str> = stamp.split(' ').collect();
        assert_eq!(fields[0], "Jan", "abbreviated month");
        assert_eq!(fields[1], "2,", "unpadded day");
        assert_eq!(fields[2], "2026", "four-digit year");
        assert_eq!(fields[3], "at");
        assert_eq!(fields[4], "3:04", "unpadded 12-hour clock");
        assert_eq!(fields[5], "PM");
        assert_eq!(fields[6], "UTC", "zone abbreviation");
        // Midnight is 12 AM, not 0 AM, and noon is 12 PM.
        assert!(go_readable_timestamp_in(at("2026-01-02T00:00:00Z"), &utc).contains("12:00 AM"));
        assert!(go_readable_timestamp_in(at("2026-01-02T12:00:00Z"), &utc).contains("12:00 PM"));
    }
}
