//! Markdown report writer — port of Go `pkg/export/markdown.go`
//! (`GenerateMarkdown` and everything it calls: `generateQuickActions`,
//! `generateIssueCommands`, `issueHeadingText`, `uniqueSlug`, `createSlug`,
//! the status/type/priority label tables, and `reportGraphContext`) at the
//! frozen parity commit `18afafa`.
//!
//! Go's source is the specification: section order, headings, table columns,
//! anchor conventions, escaping and the timestamp layouts are copied, not
//! paraphrased. `crates/bv-export/tests/markdown_report.rs` pins the resulting
//! document byte-for-byte so a paraphrase cannot creep back in.
//!
//! The one delegated piece is the Mermaid body of the `## Dependency Graph`
//! block, which comes from [`crate::mermaid`]; another owner maintains that
//! module. Everything around it — the fence, the surrounding `---` rules — is
//! produced here.

use bv_core::model::{Issue, Status};
use bv_core::tracker::{build_actions, IssueActions, IssueOrigin};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fmt::Write as _;

use crate::mermaid::generate_mermaid;

/// Go `ReportOptions` (`pkg/export/markdown.go:31`), reduced to the fields the
/// report writer actually reads. `format` and `template` exist because Go
/// validates their cross-field rules on the same struct before any format is
/// rendered.
#[derive(Debug, Clone)]
pub struct ReportOptions {
    /// Go: `Format` — one of `markdown`, `json`, `csv`, `mermaid`.
    pub format: String,
    /// Go: `Template` — path to a custom Markdown template.
    pub template: String,
    /// Go: `Title`, defaulted to `"Beads Export"` when blank.
    pub title: String,
    /// Go: `GeneratedAt`. `None` means "now", which is Go's zero-value branch.
    /// The CLI always supplies the instant `robotNow()` produced, so the
    /// rendered zone abbreviation is UTC.
    pub generated_at: Option<jiff::Timestamp>,
    /// Go: `IncludeGraph`.
    pub include_graph: bool,
    /// Go: `GraphIssues`. `None` means "the reported issues themselves",
    /// which is what Go's `GenerateMarkdown` falls back to.
    pub graph_issues: Option<Vec<Issue>>,
    /// Go carries `IssueOrigin` on each issue; this repo's frozen `Issue` has
    /// no such field, so the live tracker routes arrive out of band. A missing
    /// entry is Go's `Origin == nil`, where `Actions().show` is nil and both
    /// the Quick Actions block and the per-issue Commands block are omitted.
    pub origins: BTreeMap<String, IssueOrigin>,
    /// Go evaluates `options.AuthorityComplete && options.Readiness != nil &&
    /// options.Readiness.Claimable(id, options.GeneratedAt)`; this set holds
    /// the already-evaluated result, so the writer needs no readiness index.
    pub claimable_ids: BTreeSet<String>,
}

impl Default for ReportOptions {
    fn default() -> Self {
        Self {
            format: "markdown".to_string(),
            template: String::new(),
            title: "Beads Export".to_string(),
            generated_at: None,
            include_graph: true,
            graph_issues: None,
            origins: BTreeMap::new(),
            claimable_ids: BTreeSet::new(),
        }
    }
}

impl ReportOptions {
    /// Go `(ReportOptions).validate` (`pkg/export/markdown.go:74`), called by
    /// `ResolveReportOptions` before any format is rendered. Error strings are
    /// Go's own, since they reach the user verbatim.
    pub fn validate(&self) -> Result<(), String> {
        match self.format.as_str() {
            "markdown" | "json" | "csv" | "mermaid" => {}
            other => {
                return Err(format!(
                    "export format {other:?} must be markdown, json, csv or mermaid"
                ))
            }
        }
        if self.format == "csv" && self.include_graph {
            return Err("CSV cannot include a graph; set --export-include-graph=false".to_string());
        }
        if self.format == "mermaid" && !self.include_graph {
            return Err("Mermaid export requires include_graph=true".to_string());
        }
        if !self.template.is_empty() && self.format != "markdown" {
            return Err("custom templates require markdown export".to_string());
        }
        Ok(())
    }

    /// Go stamps a blank title with its own default before rendering.
    pub fn resolved_title(&self) -> &str {
        if self.title.is_empty() {
            "Beads Export"
        } else {
            &self.title
        }
    }

    /// Go formats `opts.GeneratedAt` with `time.RFC1123` (markdown.go:374), and
    /// the caller supplies a pinned `robotNow()`. Falling back to the wall
    /// clock here made two runs of the ORACLE agree while bvr's footer moved,
    /// so an export was not reproducible under `SOURCE_DATE_EPOCH` — the
    /// whole point of pinning it.
    fn generated_at(&self) -> jiff::Timestamp {
        self.generated_at.unwrap_or_else(|| {
            if bv_analysis::analyzer::source_date_epoch_active() {
                jiff::Timestamp::from_second(
                    std::env::var("SOURCE_DATE_EPOCH")
                        .ok()
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(0),
                )
                .unwrap_or_else(|_| jiff::Timestamp::now())
            } else {
                jiff::Timestamp::now()
            }
        })
    }
}

/// Go `GenerateReport` (`pkg/export/markdown.go:95`) for the Markdown branch:
/// the graph block is fed the dependency closure of `selected` over the whole
/// `context`, exactly as Go builds `opts.GraphIssues` before switching on
/// format.
pub fn generate_report(selected: &[Issue], context: &[Issue], opts: &ReportOptions) -> String {
    let mut opts = opts.clone();
    if opts.include_graph {
        opts.graph_issues = Some(report_graph_context(selected, context));
    }
    generate_markdown(selected, &opts)
}

/// Go `GenerateMarkdown` (`pkg/export/markdown.go:359`).
pub fn generate_markdown(issues: &[Issue], opts: &ReportOptions) -> String {
    let mut sb = String::new();

    // Header.
    let _ = writeln!(sb, "# {}\n", opts.resolved_title());
    let _ = writeln!(sb, "*Generated: {}*\n", rfc1123(opts.generated_at()));

    // Summary Statistics.
    sb.push_str("## Summary\n\n");
    let counts = summary_counts(issues);
    sb.push_str("| Metric | Count |\n|--------|-------|\n");
    let total = issues.len();
    let _ = writeln!(sb, "| **Total** | {total} |");
    let _ = writeln!(sb, "| Open | {} |", counts.open);
    let _ = writeln!(sb, "| In Progress | {} |", counts.in_progress);
    let _ = writeln!(sb, "| Blocked | {} |", counts.blocked);
    let _ = writeln!(sb, "| Closed | {} |\n", counts.closed);

    // Quick Actions Section.
    sb.push_str(&generate_quick_actions(issues, opts));

    // Precompute stable, unique slugs for TOC anchors and headings.
    let mut slug_counts: HashMap<String, usize> = HashMap::new();
    let issue_slugs: Vec<String> = issues
        .iter()
        .map(|issue| unique_slug(&create_slug(&issue_heading_text(issue)), &mut slug_counts))
        .collect();

    // Table of Contents.
    sb.push_str("## Table of Contents\n\n");
    for (idx, issue) in issues.iter().enumerate() {
        let status_icon = status_emoji(issue.status);
        let id = &issue.id;
        let title = &issue.title;
        let _ = writeln!(
            sb,
            "- [{status_icon} {id} {title}](#{slug})",
            slug = issue_slugs[idx]
        );
    }
    sb.push_str("\n---\n\n");

    // Dependency Graph (Mermaid).
    if opts.include_graph {
        let graph_issues: &[Issue] = opts.graph_issues.as_deref().unwrap_or(issues);
        sb.push_str("## Dependency Graph\n\n```mermaid\n");
        sb.push_str(&generate_mermaid(graph_issues));
        sb.push_str("```\n\n---\n\n");
    }

    // Individual Issues.
    for (idx, issue) in issues.iter().enumerate() {
        let slug = &issue_slugs[idx];
        let _ = writeln!(sb, "<a id=\"{slug}\"></a>\n");
        let _ = writeln!(sb, "## {}\n", issue_heading_text(issue));

        // Metadata Table.
        let type_icon = type_emoji(&issue.issue_type);
        let issue_type = &issue.issue_type;
        sb.push_str("| Property | Value |\n|----------|-------|\n");
        let _ = writeln!(sb, "| **Type** | {type_icon} {issue_type} |");
        let _ = writeln!(sb, "| **Priority** | {} |", priority_label(issue.priority));
        let _ = writeln!(
            sb,
            "| **Status** | {} {} |",
            status_emoji(issue.status),
            issue.status.as_str()
        );
        if !issue.assignee.is_empty() {
            // Sanitize assignee: replace newlines with spaces, escape pipes.
            let _ = writeln!(sb, "| **Assignee** | @{} |", table_cell(&issue.assignee));
        }
        let _ = writeln!(
            sb,
            "| **Created** | {} |",
            go_datetime(issue.created_at.as_deref())
        );
        let _ = writeln!(
            sb,
            "| **Updated** | {} |",
            go_datetime(issue.updated_at.as_deref())
        );
        if let Some(closed_at) = &issue.closed_at {
            let _ = writeln!(
                sb,
                "| **Closed** | {} |",
                go_datetime(Some(closed_at.as_str()))
            );
        }
        if !issue.labels.is_empty() {
            // Escape pipe characters and sanitize newlines in labels.
            let labels: Vec<String> = issue.labels.iter().map(|l| table_cell(l)).collect();
            let _ = writeln!(sb, "| **Labels** | {} |", labels.join(", "));
        }
        sb.push('\n');

        if !issue.description.is_empty() {
            sb.push_str("### Description\n\n");
            sb.push_str(&issue.description);
            sb.push_str("\n\n");
        }
        if !issue.acceptance_criteria.is_empty() {
            sb.push_str("### Acceptance Criteria\n\n");
            sb.push_str(&issue.acceptance_criteria);
            sb.push_str("\n\n");
        }
        if !issue.design.is_empty() {
            sb.push_str("### Design\n\n");
            sb.push_str(&issue.design);
            sb.push_str("\n\n");
        }
        if !issue.notes.is_empty() {
            sb.push_str("### Notes\n\n");
            sb.push_str(&issue.notes);
            sb.push_str("\n\n");
        }
        if !issue.dependencies.is_empty() {
            sb.push_str("### Dependencies\n\n");
            for dep in &issue.dependencies {
                let icon = if dep.r#type.is_blocking() {
                    "⛔"
                } else {
                    "🔗"
                };
                let _ = writeln!(
                    sb,
                    "- {icon} **{kind}**: `{target}`",
                    kind = dep.r#type.as_str(),
                    target = dep.effective_depends_on()
                );
            }
            sb.push('\n');
        }
        if !issue.comments.is_empty() {
            sb.push_str("### Comments\n\n");
            for comment in &issue.comments {
                let author = &comment.author;
                let created = go_date(comment.created_at.as_deref());
                let body = comment.text.replace('\n', "\n> ");
                let _ = writeln!(sb, "> **{author}** ({created})\n>\n> {body}\n");
            }
        }

        // Per-issue command snippets.
        sb.push_str(&generate_issue_commands(issue, opts));

        sb.push_str("---\n\n");
    }

    sb
}

/// Go `isClosedLikeStatus` — `closed` and `tombstone` both count as closed.
fn is_closed_like(status: Status) -> bool {
    status == Status::Closed || status == Status::Tombstone
}

struct SummaryCounts {
    open: usize,
    in_progress: usize,
    blocked: usize,
    closed: usize,
}

/// Go's summary loop: closed-like issues short-circuit into `closed`, and every
/// other status that is neither `in_progress` nor `blocked` lands in `open` —
/// which is deliberately not the same as `Status::Open`.
fn summary_counts(issues: &[Issue]) -> SummaryCounts {
    let mut counts = SummaryCounts {
        open: 0,
        in_progress: 0,
        blocked: 0,
        closed: 0,
    };
    for issue in issues {
        if is_closed_like(issue.status) {
            counts.closed += 1;
            continue;
        }
        match issue.status {
            Status::InProgress => counts.in_progress += 1,
            Status::Blocked => counts.blocked += 1,
            _ => counts.open += 1,
        }
    }
    counts
}

/// Go `issueHeadingText`.
fn issue_heading_text(issue: &Issue) -> String {
    format!(
        "{} {} {}",
        type_emoji(&issue.issue_type),
        issue.id,
        issue.title
    )
}

/// Go `uniqueSlug` — the first issue keeps the bare slug; later collisions get
/// `-1`, `-2`, … in report order.
fn unique_slug(base: &str, counts: &mut HashMap<String, usize>) -> String {
    let base = if base.is_empty() { "section" } else { base };
    match counts.get_mut(base) {
        Some(count) => {
            *count += 1;
            format!("{base}-{count}")
        }
        None => {
            counts.insert(base.to_string(), 0);
            base.to_string()
        }
    }
}

/// Go `createSlug` — lowercase, every run of `[^a-z0-9]+` collapses to a
/// single `-`, then leading/trailing dashes are trimmed. The regex is ASCII,
/// so non-ASCII letters (and the type emoji) are separators, not content.
fn create_slug(text: &str) -> String {
    let mut out = String::new();
    let mut in_run = false;
    for ch in text.to_lowercase().chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            out.push(ch);
            in_run = false;
        } else if !in_run {
            out.push('-');
            in_run = true;
        }
    }
    out.trim_matches('-').to_string()
}

/// Go `getStatusEmoji`.
fn status_emoji(status: Status) -> &'static str {
    match status {
        Status::Open => "🟢",
        Status::InProgress => "🔵",
        Status::Blocked => "🔴",
        Status::Closed | Status::Tombstone => "⚫",
        _ => "⚪",
    }
}

/// Go `getTypeEmoji`.
fn type_emoji(issue_type: &str) -> &'static str {
    match issue_type {
        // Rocket instead of mountain: VS-16 variation selector causes width
        // issues (Go's own comment).
        "bug" => "🐛",
        "feature" => "✨",
        "task" => "📋",
        "epic" => "🚀",
        "chore" => "🧹",
        _ => "•",
    }
}

/// Go `getPriorityLabel`.
fn priority_label(priority: i32) -> String {
    match priority {
        0 => "🔥 Critical (P0)".to_string(),
        1 => "⚡ High (P1)".to_string(),
        2 => "🔹 Medium (P2)".to_string(),
        3 => "☕ Low (P3)".to_string(),
        4 => "💤 Backlog (P4)".to_string(),
        other => format!("P{other}"),
    }
}

/// Newlines become spaces, carriage returns are dropped, and pipes are escaped
/// so a value cannot break out of the surrounding table cell.
fn table_cell(value: &str) -> String {
    value
        .replace('\n', " ")
        .replace('\r', "")
        .replace('|', "\\|")
}

/// Go `generateQuickActions` — inspection commands bound to each live tracker.
/// A report does not establish that every open issue is ready to be closed.
fn generate_quick_actions(issues: &[Issue], opts: &ReportOptions) -> String {
    let mut sb = String::new();
    for issue in issues {
        if is_closed_like(issue.status) {
            continue;
        }
        let Some(actions) = actions_for(issue, opts) else {
            continue;
        };
        let Some(show) = actions.show.as_ref() else {
            continue;
        };
        if sb.is_empty() {
            sb.push_str(
                "## Quick Actions\n\nInspect the current tracker before acting on this snapshot:\n\n```bash\n",
            );
        }
        let _ = writeln!(sb, "{}", show.shell);
    }
    if !sb.is_empty() {
        sb.push_str("```\n\n");
    }
    sb
}

/// Go `generateIssueCommands` — a command snippet for a single issue.
fn generate_issue_commands(issue: &Issue, opts: &ReportOptions) -> String {
    // Skip command snippets for closed issues.
    if is_closed_like(issue.status) {
        return String::new();
    }
    let Some(actions) = actions_for(issue, opts) else {
        return String::new();
    };
    let Some(show) = actions.show.as_ref() else {
        return String::new();
    };

    let mut sb = String::new();
    sb.push_str("<details>\n<summary>📋 Commands</summary>\n\n```bash\n");
    if let Some(claim) = actions.claim.as_ref() {
        sb.push_str("# Atomically claim; the live tracker may reject a stale recommendation\n");
        let _ = writeln!(sb, "{}", claim.shell);
        sb.push('\n');
    }
    sb.push_str("# View full details\n");
    let _ = writeln!(sb, "{}", show.shell);
    sb.push_str("```\n\n</details>\n\n");
    sb
}

/// Go `Issue.Actions(claimable)`, reached only for issues that carry an
/// established origin. Without one, Go leaves `show` nil and both command
/// blocks collapse to nothing.
fn actions_for(issue: &Issue, opts: &ReportOptions) -> Option<IssueActions> {
    let origin = opts.origins.get(&issue.id)?;
    let claimable = opts.claimable_ids.contains(&issue.id);
    Some(build_actions(origin, claimable))
}

/// Go `reportGraphContext` — the selected issues plus the transitive closure
/// of their dependencies, tombstones dropped, sorted by ID. The context
/// supplies bodies for issues the selection never listed; it never contributes
/// extra output rows.
pub fn report_graph_context(selected: &[Issue], context: &[Issue]) -> Vec<Issue> {
    let mut by_id: HashMap<&str, &Issue> = HashMap::with_capacity(context.len() + selected.len());
    for issue in context {
        by_id.insert(issue.id.as_str(), issue);
    }
    let mut queue: VecDeque<&str> = VecDeque::with_capacity(selected.len());
    for issue in selected {
        by_id.insert(issue.id.as_str(), issue);
        queue.push_back(issue.id.as_str());
    }
    let mut seen: HashSet<&str> = HashSet::new();
    let mut result: Vec<Issue> = Vec::with_capacity(selected.len());
    while let Some(id) = queue.pop_front() {
        if !seen.insert(id) {
            continue;
        }
        let Some(issue) = by_id.get(id) else {
            continue;
        };
        if issue.status == Status::Tombstone {
            continue;
        }
        result.push((*issue).clone());
        for dep in &issue.dependencies {
            queue.push_back(dep.effective_depends_on());
        }
    }
    result.sort_by(|a, b| a.id.cmp(&b.id));
    result
}

/// Go renders an absent `time.Time` field as its zero value, so a record with
/// no `created_at` still shows a date instead of an empty table cell.
pub(crate) const GO_ZERO_DATETIME: &str = "0001-01-01 00:00";
const GO_ZERO_DATE: &str = "0001-01-01";

/// Go `t.Format("2006-01-02 15:04")`, rendered in the offset the source
/// timestamp carried.
fn go_datetime(raw: Option<&str>) -> String {
    match raw.and_then(wall_clock) {
        Some(local) => local.strftime("%Y-%m-%d %H:%M").to_string(),
        None => GO_ZERO_DATETIME.to_string(),
    }
}

/// Go `t.Format("2006-01-02")`.
fn go_date(raw: Option<&str>) -> String {
    match raw.and_then(wall_clock) {
        Some(local) => local.strftime("%Y-%m-%d").to_string(),
        None => GO_ZERO_DATE.to_string(),
    }
}

/// Go's `time.Parse(time.RFC3339, raw)` keeps the offset it read, and `Format`
/// then renders that wall clock — so `2024-01-15T10:00:00+02:00` prints
/// `10:00`, not the `08:00` its UTC instant would suggest. jiff collapses the
/// instant to UTC, so re-apply the parsed offset and format the shifted
/// instant, whose UTC fields are exactly the source's wall clock.
pub(crate) fn wall_clock(raw: &str) -> Option<jiff::Timestamp> {
    let instant = raw.parse::<jiff::Timestamp>().ok()?;
    instant
        .checked_add(jiff::SignedDuration::from_secs(source_offset_seconds(raw)?))
        .ok()
}

/// The seconds east of UTC in an RFC 3339 timestamp's trailing offset. `jiff`
/// exposes the offset only on a `Zoned`, and its parser demands a bracketed
/// zone annotation that beads JSONL never carries, so read it off the text.
fn source_offset_seconds(raw: &str) -> Option<i64> {
    let time_part = raw.split_once('T')?.1;
    match time_part.as_bytes().last()? {
        b'Z' | b'z' => return Some(0),
        _ => {}
    }
    let sign_at = time_part.rfind(['+', '-'])?;
    let sign = match time_part.as_bytes()[sign_at] {
        b'+' => 1,
        _ => -1,
    };
    // Accept `+HH:MM`, `+HHMM` and `+HH`.
    let digits: String = time_part[sign_at + 1..]
        .chars()
        .filter(char::is_ascii_digit)
        .collect();
    let hours: i64 = digits.get(..2)?.parse().ok()?;
    let minutes: i64 = digits.get(2..4).map_or(Ok(0), str::parse).ok()?;
    Some(sign * (hours * 3600 + minutes * 60))
}

/// Go `t.Format(time.RFC1123)` = `Mon, 02 Jan 2006 15:04:05 MST`. The CLI
/// hands the writer `robotNow()`, which is UTC, so the zone is always `UTC`.
fn rfc1123(timestamp: jiff::Timestamp) -> String {
    timestamp.strftime("%a, %d %b %Y %H:%M:%S UTC").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bv_core::model::{Comment, Dependency, DependencyType};

    fn base_issue(id: &str, title: &str, status: Status, issue_type: &str) -> Issue {
        Issue {
            id: id.to_string(),
            content_hash: String::new(),
            title: title.to_string(),
            description: String::new(),
            design: String::new(),
            acceptance_criteria: String::new(),
            notes: String::new(),
            status,
            priority: 2,
            issue_type: issue_type.to_string(),
            assignee: String::new(),
            estimated_minutes: None,
            created_at: Some("2024-01-15T10:00:00Z".to_string()),
            updated_at: Some("2024-01-16T14:30:00Z".to_string()),
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

    fn fixed_generated_at() -> jiff::Timestamp {
        "2025-01-02T03:04:05Z".parse().unwrap()
    }

    #[test]
    fn create_slug_collapses_runs_and_trims() {
        assert_eq!(create_slug("✨ BV-1 Fix Parser"), "bv-1-fix-parser");
        assert_eq!(create_slug("---"), "");
        assert_eq!(create_slug("Already-Slugged"), "already-slugged");
    }

    #[test]
    fn unique_slug_disambiguates_in_report_order() {
        let mut counts = HashMap::new();
        assert_eq!(unique_slug("dup", &mut counts), "dup");
        assert_eq!(unique_slug("dup", &mut counts), "dup-1");
        assert_eq!(unique_slug("dup", &mut counts), "dup-2");
        assert_eq!(unique_slug("", &mut counts), "section");
    }

    #[test]
    fn summary_counts_fold_every_non_blocking_status_into_open() {
        let issues = vec![
            base_issue("A", "a", Status::Open, "task"),
            base_issue("B", "b", Status::Draft, "task"),
            base_issue("C", "c", Status::InProgress, "task"),
            base_issue("D", "d", Status::Blocked, "task"),
            base_issue("E", "e", Status::Closed, "task"),
            base_issue("F", "f", Status::Tombstone, "task"),
        ];
        let counts = summary_counts(&issues);
        assert_eq!(counts.open, 2, "open bucket is `open`, not `is_open`");
        assert_eq!(counts.in_progress, 1);
        assert_eq!(counts.blocked, 1);
        assert_eq!(counts.closed, 2, "tombstone is closed-like");
    }

    #[test]
    fn priority_labels_match_go_table() {
        assert_eq!(priority_label(0), "🔥 Critical (P0)");
        assert_eq!(priority_label(2), "🔹 Medium (P2)");
        assert_eq!(priority_label(4), "💤 Backlog (P4)");
        assert_eq!(priority_label(7), "P7");
    }

    #[test]
    fn timestamps_render_in_the_source_offset_and_never_blank() {
        // Go keeps the offset `time.Parse` read, so a +02:00 stamp keeps its
        // local wall-clock reading rather than shifting to UTC.
        assert_eq!(
            go_datetime(Some("2024-01-15T10:00:00+02:00")),
            "2024-01-15 10:00"
        );
        assert_eq!(
            go_datetime(Some("2024-01-15T10:00:00Z")),
            "2024-01-15 10:00"
        );
        assert_eq!(go_datetime(None), GO_ZERO_DATETIME);
        assert_eq!(go_date(Some("2024-03-04T05:06:07Z")), "2024-03-04");
        assert_eq!(go_date(None), GO_ZERO_DATE);
    }

    #[test]
    fn rfc1123_uses_utc_zone_abbreviation() {
        assert_eq!(
            rfc1123(fixed_generated_at()),
            "Thu, 02 Jan 2025 03:04:05 UTC"
        );
    }

    #[test]
    fn graph_context_closes_over_dependencies_and_drops_tombstones() {
        let mut a = base_issue("A", "a", Status::Open, "task");
        a.dependencies = vec![Dependency {
            issue_id: "A".into(),
            depends_on_id: "B".into(),
            depends_on_legacy: String::new(),
            target_id_legacy: String::new(),
            r#type: DependencyType::Blocks,
            created_at: None,
            created_by: String::new(),
        }];
        let b = base_issue("B", "b", Status::Tombstone, "task");
        let c = base_issue("C", "c", Status::Open, "task");
        let unrelated = base_issue("Z", "z", Status::Open, "task");

        // Selected is out of ID order to prove the sort, and Z is in the
        // context only — it must not become a graph row.
        let selected = vec![c.clone(), a.clone()];
        let context = vec![unrelated, b];
        let ids: Vec<String> = report_graph_context(&selected, &context)
            .into_iter()
            .map(|i| i.id)
            .collect();
        assert_eq!(ids, vec!["A".to_string(), "C".to_string()]);
    }

    #[test]
    fn commands_appear_only_for_routed_open_issues() {
        let issues = vec![base_issue("A", "a", Status::Open, "task")];
        let mut opts = ReportOptions {
            generated_at: Some(fixed_generated_at()),
            ..ReportOptions::default()
        };
        // No origin: Go's `Origin == nil` leaves `show` nil, so both blocks
        // stay empty rather than inventing a command.
        assert!(generate_quick_actions(&issues, &opts).is_empty());
        assert!(generate_issue_commands(&issues[0], &opts).is_empty());

        opts.origins.insert(
            "A".to_string(),
            IssueOrigin {
                local_id: "a-1".into(),
                working_directory: "/repo".into(),
                tracker_directory: "/repo/.beads".into(),
                database: "/repo/.beads/br.db".into(),
                tracker: "br".into(),
                executable: "br".into(),
                supports_claim: true,
                read_only_reason: String::new(),
            },
        );
        let quick = generate_quick_actions(&issues, &opts);
        assert!(quick.starts_with("## Quick Actions\n\n"), "{quick}");
        assert!(quick.contains("'show' '--json' '--' 'a-1'"), "{quick}");

        // Claimable: the claim snippet precedes the show snippet.
        opts.claimable_ids.insert("A".to_string());
        let commands = generate_issue_commands(&issues[0], &opts);
        let claim_at = commands.find("'--claim'").expect("claim snippet");
        let show_at = commands.find("'show'").expect("show snippet");
        assert!(claim_at < show_at, "{commands}");
        assert!(
            commands.starts_with("<details>\n<summary>📋 Commands</summary>"),
            "{commands}"
        );

        // Closed issues never carry commands, even with a working route.
        let mut closed = base_issue("B", "b", Status::Closed, "task");
        closed.id = "A".into();
        assert!(generate_issue_commands(&closed, &opts).is_empty());
    }

    #[test]
    fn table_cells_neutralize_pipes_and_newlines() {
        assert_eq!(table_cell("a|b"), "a\\|b");
        assert_eq!(table_cell("a\nb\rc"), "a bc");
    }

    #[test]
    fn validate_reports_go_error_text() {
        let bad_format = ReportOptions {
            format: "xml".to_string(),
            ..ReportOptions::default()
        };
        assert_eq!(
            bad_format.validate().unwrap_err(),
            "export format \"xml\" must be markdown, json, csv or mermaid"
        );
        let csv_with_graph = ReportOptions {
            format: "csv".to_string(),
            include_graph: true,
            ..ReportOptions::default()
        };
        assert_eq!(
            csv_with_graph.validate().unwrap_err(),
            "CSV cannot include a graph; set --export-include-graph=false"
        );
        let mermaid_without_graph = ReportOptions {
            format: "mermaid".to_string(),
            include_graph: false,
            ..ReportOptions::default()
        };
        assert_eq!(
            mermaid_without_graph.validate().unwrap_err(),
            "Mermaid export requires include_graph=true"
        );
        let template_with_csv = ReportOptions {
            format: "csv".to_string(),
            include_graph: false,
            template: "t.tmpl".to_string(),
            ..ReportOptions::default()
        };
        assert_eq!(
            template_with_csv.validate().unwrap_err(),
            "custom templates require markdown export"
        );
        assert!(ReportOptions::default().validate().is_ok());
    }

    #[test]
    fn dependencies_carry_a_blocking_or_loose_icon() {
        let mut issue = base_issue("A", "a", Status::Open, "task");
        issue.dependencies = vec![
            Dependency {
                issue_id: "A".into(),
                depends_on_id: "B".into(),
                depends_on_legacy: String::new(),
                target_id_legacy: String::new(),
                r#type: DependencyType::Blocks,
                created_at: None,
                created_by: String::new(),
            },
            Dependency {
                issue_id: "A".into(),
                depends_on_id: "C".into(),
                depends_on_legacy: String::new(),
                target_id_legacy: String::new(),
                r#type: DependencyType::Related,
                created_at: None,
                created_by: String::new(),
            },
        ];
        issue.comments = vec![Comment {
            id: "1".into(),
            issue_id: "A".into(),
            author: "alice".into(),
            text: "First\nSecond".into(),
            created_at: Some("2024-01-15T10:00:00Z".into()),
        }];
        let opts = ReportOptions {
            generated_at: Some(fixed_generated_at()),
            include_graph: false,
            ..ReportOptions::default()
        };
        let md = generate_markdown(&[issue], &opts);
        assert!(md.contains("- ⛔ **blocks**: `B`\n"), "{md}");
        assert!(md.contains("- 🔗 **related**: `C`\n"), "{md}");
        assert!(
            md.contains("> **alice** (2024-01-15)\n>\n> First\n> Second\n"),
            "{md}"
        );
    }
}
