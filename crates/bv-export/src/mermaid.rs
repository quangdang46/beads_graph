//! Mermaid + markdown export — port of Go `pkg/export/mermaid_generator.go`
//! and `markdown.go` sanitization/class rules.
//!
//! This module is the single source of truth for the Mermaid diagram: Go has
//! one `GenerateMermaidGraph`, so the sanitizers and the generator live here
//! once and `graph_export::generate_mermaid_graph` re-exports the generator
//! rather than keeping a second copy that can drift.

use bv_core::model::{Dependency, Issue, Status};
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// Go: `sanitizeMermaidID` — keep letters/digits/-/_ else drop; empty → "node".
pub fn sanitize_mermaid_id(id: &str) -> String {
    let result: String = id
        .chars()
        .filter(|r| r.is_alphanumeric() || *r == '-' || *r == '_')
        .collect();
    if result.is_empty() {
        "node".to_string()
    } else {
        result
    }
}

/// Go: `sanitizeMermaidText` (pkg/export/markdown.go:322) — one left-to-right
/// pass over the ORIGINAL string, then drop control chars, trim, and cap at 40
/// runes (37 + "...").
///
/// Go uses `strings.NewReplacer`, which matches against the original string and
/// never rescans what it just wrote. Every pattern it matches is a single rune,
/// so this per-char walk is exactly equivalent.
pub fn sanitize_mermaid_text(text: &str) -> String {
    let mut replaced = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '"' => replaced.push('\''),
            '[' => replaced.push('('),
            ']' => replaced.push(')'),
            '{' => replaced.push('('),
            '}' => replaced.push(')'),
            '<' => replaced.push_str("&lt;"),
            '>' => replaced.push_str("&gt;"),
            '|' => replaced.push('/'),
            '`' => replaced.push('\''),
            '\n' => replaced.push(' '),
            '\r' => {}
            _ => replaced.push(c),
        }
    }

    let cleaned: String = replaced.chars().filter(|c| !c.is_control()).collect();
    let trimmed = cleaned.trim();
    let runes: Vec<char> = trimmed.chars().collect();
    if runes.len() > 40 {
        let head: String = runes[..37].iter().collect();
        format!("{head}...")
    } else {
        trimmed.to_string()
    }
}

/// FNV-1a 32-bit hash (Go fnv.New32a) for collision suffixes.
fn fnv1a32(data: &[u8]) -> u32 {
    const OFFSET: u32 = 0x811C_9DC5;
    const PRIME: u32 = 0x0100_0193;
    let mut h = OFFSET;
    for &b in data {
        h ^= b as u32;
        h = h.wrapping_mul(PRIME);
    }
    h
}

/// Go: the `class` switch in `GenerateMermaidGraph`. Go emits a `class` line
/// only for the four styled statuses plus the closed-like pair — every other
/// status (deferred/draft/pinned/hooked/review) yields a bare node.
fn mermaid_class(status: Status) -> Option<&'static str> {
    match status {
        Status::Closed | Status::Tombstone => Some("closed"),
        Status::Open => Some("open"),
        Status::InProgress => Some("inprogress"),
        Status::Blocked => Some("blocked"),
        Status::Deferred | Status::Draft | Status::Pinned | Status::Hooked | Status::Review => None,
    }
}

/// Go: `MermaidConfig` — the single knob on the diagram.
#[derive(Clone, Copy)]
pub struct MermaidConfig {
    /// Append a `NoLinks` placeholder when the graph has nodes but no edges.
    pub show_no_dependencies_node: bool,
}

/// Go: the `getSafeID` closure, pre-run over the ID-sorted issue list. A node's
/// ID is therefore fixed by its sort position; a later ID that sanitizes to an
/// already-used form gets a stable FNV-1a suffix.
fn safe_id_map<'a>(sorted: &[&'a Issue]) -> BTreeMap<&'a str, String> {
    let mut safe_map: BTreeMap<&str, String> = BTreeMap::new();
    let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
    for issue in sorted {
        let orig = issue.id.as_str();
        if safe_map.contains_key(orig) {
            continue;
        }
        // `sanitize_mermaid_id` already returns "node" for an all-punctuation
        // ID, so Go's `if base == ""` fallback can never fire.
        let base = sanitize_mermaid_id(orig);
        let mut safe = base.clone();
        if used.contains(&safe) {
            safe = format!("{base}_{:x}", fnv1a32(orig.as_bytes()));
        }
        used.insert(safe.clone());
        safe_map.insert(orig, safe);
    }
    safe_map
}

/// Build the mermaid diagram. Deterministic: issues sorted by ID, FNV-1a
/// suffix on sanitized-ID collisions.
///
/// This is the single implementation of Go's `GenerateMermaidGraph`;
/// `graph_export::generate_mermaid_graph` is a re-export of it.
pub fn generate_mermaid(issues: &[Issue]) -> String {
    generate_mermaid_with_config(
        issues,
        MermaidConfig {
            show_no_dependencies_node: true,
        },
    )
}

/// Go: `GenerateMermaidGraph` (pkg/export/mermaid_generator.go:18).
pub fn generate_mermaid_with_config(issues: &[Issue], config: MermaidConfig) -> String {
    let mut sb = String::new();
    sb.push_str("graph TD\n");
    sb.push_str("    classDef open fill:#50FA7B,stroke:#333,color:#000\n");
    sb.push_str("    classDef inprogress fill:#8BE9FD,stroke:#333,color:#000\n");
    sb.push_str("    classDef blocked fill:#FF5555,stroke:#333,color:#000\n");
    sb.push_str("    classDef closed fill:#6272A4,stroke:#333,color:#fff\n");
    sb.push('\n');

    let mut sorted: Vec<&Issue> = issues.iter().collect();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));

    let safe_map = safe_id_map(&sorted);

    let mut has_links = false;

    // Nodes, each optionally followed by its `class` statement.
    for issue in &sorted {
        let safe_id = &safe_map[issue.id.as_str()];
        let safe_label = sanitize_mermaid_text(&issue.id);
        let safe_title = sanitize_mermaid_text(&issue.title);
        let _ = writeln!(sb, "    {safe_id}[\"{safe_label}<br/>{safe_title}\"]");
        if let Some(class) = mermaid_class(issue.status) {
            let _ = writeln!(sb, "    class {safe_id} {class}");
        }
    }

    sb.push('\n');

    // Edges: blocking ==> thick; related -.-> dashed.
    for issue in &sorted {
        let from = &safe_map[issue.id.as_str()];
        let mut deps: Vec<&Dependency> = issue.dependencies.iter().collect();
        deps.sort_by(|a, b| a.effective_depends_on().cmp(b.effective_depends_on()));
        for dep in deps {
            // Go skips edges whose target is not in the emitted node set.
            let Some(to) = safe_map.get(dep.effective_depends_on()) else {
                continue;
            };
            let link = if dep.r#type.is_blocking() {
                "==>"
            } else {
                "-.->"
            };
            let _ = writeln!(sb, "    {from} {link} {to}");
            has_links = true;
        }
    }

    if config.show_no_dependencies_node && !has_links && !issues.is_empty() {
        sb.push_str("    NoLinks[\"No Dependencies\"]\n");
    }
    sb
}

/// Full markdown report (Go GenerateMarkdown subset): title header,
/// summary table, per-issue sections with metadata, mermaid diagram.
pub fn generate_markdown(issues: &[Issue], title: &str) -> String {
    let mut sb = String::new();
    let _ = writeln!(sb, "# {title}\n");
    // Honour SOURCE_DATE_EPOCH the same way the full report writer does
    // (`ReportOptions::generated_at`), so a pinned clock reproduces byte for
    // byte instead of stamping the wall clock.
    let now = report_now();
    let _ = writeln!(sb, "_Generated {}_\n", now);

    // Summary table.
    let open = issues
        .iter()
        .filter(|i| matches!(i.status, Status::Open))
        .count();
    let in_progress = issues
        .iter()
        .filter(|i| matches!(i.status, Status::InProgress))
        .count();
    let blocked = issues
        .iter()
        .filter(|i| matches!(i.status, Status::Blocked))
        .count();
    let closed = issues.iter().filter(|i| i.status.is_closed()).count();

    let _ = writeln!(
        sb,
        "| Metric | Count |\n|---|---|\n| Total | {} |\n| Open | {open} |\n| In Progress | {in_progress} |\n| Blocked | {blocked} |\n| Closed | {closed} |\n",
        issues.len()
    );

    // TOC anchors.
    sb.push_str("## Table of Contents\n\n");
    let mut sorted: Vec<&Issue> = issues.iter().collect();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));
    for i in &sorted {
        let slug = slugify(&i.id);
        let _ = writeln!(sb, "- [{}](#{})", i.id, slug);
    }

    sb.push_str("\n## Dependency Graph\n\n```mermaid\n");
    sb.push_str(&generate_mermaid(issues));
    sb.push_str("```\n\n## Issues\n");

    for i in &sorted {
        let slug = slugify(&i.id);
        // The TOC links to `#slug`; without the anchor element the jump has
        // nowhere to land.
        let _ = write!(
            sb,
            "\n<a id=\"{slug}\"></a>\n### {} {}\n\n",
            i.id,
            sanitize_mermaid_text(&i.title)
        );
        let _ =
            writeln!(
            sb,
            "| Field | Value |\n|---|---|\n| Status | {} |\n| Priority | P{} |\n| Type | {} |{}",
            i.status.as_str(),
            i.priority,
            i.issue_type,
            if i.assignee.is_empty() {
                String::new()
            } else {
                format!("\n| Assignee | {} |", crate::markdown::table_cell(&i.assignee))
            }
        );
        if !i.description.is_empty() {
            sb.push_str("\n#### Description\n\n");
            sb.push_str(&i.description);
            sb.push('\n');
        }
        let _ = writeln!(sb, "\n[Back to top](#table-of-contents)");
    }
    sb
}

/// The report clock: `SOURCE_DATE_EPOCH` when the reproducible-build variable
/// is set, the wall clock otherwise — the same rule
/// [`crate::markdown::ReportOptions::generated_at`] applies.
fn report_now() -> jiff::Timestamp {
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
}

fn slugify(id: &str) -> String {
    let lower = id.to_lowercase();
    let mut out = String::new();
    let mut last_dash = true;
    for c in lower.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use bv_core::model::DependencyType;

    fn issue(id: &str, title: &str, status: Status) -> Issue {
        Issue {
            id: id.into(),
            content_hash: String::new(),
            title: title.into(),
            description: String::new(),
            design: String::new(),
            acceptance_criteria: String::new(),
            notes: String::new(),
            status,
            priority: 2,
            issue_type: "task".into(),
            assignee: String::new(),
            estimated_minutes: None,
            created_at: None,
            updated_at: None,
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

    fn dep(from: &str, to: &str, r#type: DependencyType) -> Dependency {
        Dependency {
            issue_id: from.into(),
            depends_on_id: to.into(),
            depends_on_legacy: String::new(),
            target_id_legacy: String::new(),
            r#type,
            created_at: None,
            created_by: String::new(),
        }
    }

    /// The header Go writes ahead of the nodes, verbatim: `graph TD`, the four
    /// classDefs, then a blank line.
    const CLASS_DEFS: &str = r##"graph TD
    classDef open fill:#50FA7B,stroke:#333,color:#000
    classDef inprogress fill:#8BE9FD,stroke:#333,color:#000
    classDef blocked fill:#FF5555,stroke:#333,color:#000
    classDef closed fill:#6272A4,stroke:#333,color:#fff

"##;

    // --- sanitizeMermaidID: Go TestSanitizeMermaidID_BasicInput/_RealWorldIDs ---

    #[test]
    fn sanitize_id_matches_go() {
        for (input, want) in [
            ("ISSUE123", "ISSUE123"),
            ("ISSUE-123", "ISSUE-123"),
            ("issue_123", "issue_123"),
            ("Issue-ABC_123", "Issue-ABC_123"),
            ("", "node"),
            ("!@#$%", "node"),
            ("ISSUE!@#123", "ISSUE123"),
            ("Äbc", "Äbc"), // Ä is a unicode letter
            ("ISSUE 123", "ISSUE123"),
            ("bd-101.task", "bd-101task"),
            (
                "coding_agent_session_search-0ly.3",
                "coding_agent_session_search-0ly3",
            ),
            (
                "system_resource_protection_script-e5e.1",
                "system_resource_protection_script-e5e1",
            ),
        ] {
            assert_eq!(sanitize_mermaid_id(input), want, "input {input:?}");
        }
    }

    // --- sanitizeMermaidText: Go TestSanitizeMermaidText_BasicInput ---

    #[test]
    fn sanitize_text_matches_go() {
        for (input, want) in [
            ("Hello World", "Hello World"),
            ("Say \"Hello\"", "Say 'Hello'"),
            ("[TODO] fix", "(TODO) fix"),
            ("{config}", "(config)"),
            ("A < B > C", "A &lt; B &gt; C"),
            ("Option|Other", "Option/Other"),
            ("Issue #123", "Issue #123"),
            ("`code`", "'code'"),
            ("Line1\nLine2", "Line1 Line2"),
            ("Line1\r\nLine2", "Line1 Line2"),
        ] {
            assert_eq!(sanitize_mermaid_text(input), want, "input {input:?}");
        }
    }

    #[test]
    fn sanitize_text_truncates_at_forty_runes() {
        // Exactly 40 runes survives untouched; 41+ becomes 37 + "...".
        assert_eq!(
            sanitize_mermaid_text("0123456789012345678901234567890123456789"),
            "0123456789012345678901234567890123456789"
        );
        assert_eq!(
            sanitize_mermaid_text("This is a very long title that exceeds limit"),
            "This is a very long title that exceed..."
        );
    }

    #[test]
    fn sanitize_text_drops_control_chars_and_trims() {
        // \t is a control char and is dropped outright; \n becomes a space and
        // \r is deleted, so the survivors get trimmed at the edges.
        assert_eq!(sanitize_mermaid_text("  a\tb  "), "ab");
        assert_eq!(sanitize_mermaid_text("\n  x  \n"), "x");
    }

    // --- GenerateMermaidGraph: exact bytes, captured from Go v0.25.0 ---

    #[test]
    fn mermaid_matches_go_exactly() {
        let issues = vec![
            issue("A-1", "First", Status::Open),
            issue("B-2", "Second", Status::Closed),
        ];
        let want = format!(
            "{CLASS_DEFS}{}",
            r##"    A-1["A-1<br/>First"]
    class A-1 open
    B-2["B-2<br/>Second"]
    class B-2 closed

    NoLinks["No Dependencies"]
"##
        );
        assert_eq!(generate_mermaid(&issues), want);
    }

    #[test]
    fn mermaid_edges_and_class_statements_match_go() {
        let mut a = issue("A-1", "Depends", Status::Open);
        a.dependencies = vec![
            dep("A-1", "B-2", DependencyType::Blocks),
            dep("A-1", "C-3", DependencyType::parse("related")),
        ];
        let issues = vec![
            a,
            issue("B-2", "Blocker", Status::InProgress),
            issue("C-3", "Related", Status::Blocked),
        ];
        let want = format!(
            "{CLASS_DEFS}{}",
            r##"    A-1["A-1<br/>Depends"]
    class A-1 open
    B-2["B-2<br/>Blocker"]
    class B-2 inprogress
    C-3["C-3<br/>Related"]
    class C-3 blocked

    A-1 ==> B-2
    A-1 -.-> C-3
"##
        );
        assert_eq!(generate_mermaid(&issues), want);
    }

    /// Go emits a `class` line for open/in_progress/blocked/closed/tombstone
    /// only; the other five statuses get a bare node with no class statement.
    #[test]
    fn only_styled_statuses_get_a_class_statement() {
        let statuses = [
            Status::Open,
            Status::InProgress,
            Status::Blocked,
            Status::Deferred,
            Status::Draft,
            Status::Pinned,
            Status::Hooked,
            Status::Review,
            Status::Closed,
            Status::Tombstone,
        ];
        let issues: Vec<Issue> = statuses
            .iter()
            .map(|s| {
                let name = s.as_str();
                issue(&format!("s{name}"), &format!("T {name}"), *s)
            })
            .collect();
        let want = format!(
            "{CLASS_DEFS}{}",
            r##"    sblocked["sblocked<br/>T blocked"]
    class sblocked blocked
    sclosed["sclosed<br/>T closed"]
    class sclosed closed
    sdeferred["sdeferred<br/>T deferred"]
    sdraft["sdraft<br/>T draft"]
    shooked["shooked<br/>T hooked"]
    sin_progress["sin_progress<br/>T in_progress"]
    class sin_progress inprogress
    sopen["sopen<br/>T open"]
    class sopen open
    spinned["spinned<br/>T pinned"]
    sreview["sreview<br/>T review"]
    stombstone["stombstone<br/>T tombstone"]
    class stombstone closed

    NoLinks["No Dependencies"]
"##
        );
        assert_eq!(generate_mermaid(&issues), want);
    }

    #[test]
    fn sanitized_text_reaches_the_node_label() {
        // The label is sanitizeMermaidText(ID) — `!` survives — while the node
        // ID is sanitizeMermaidID(ID), which drops it.
        let title = "A < B > C [x] {y} \"q\" | p `t` L1\nL2\rL3\tL4 #hash";
        let issues = vec![issue("ID!1", title, Status::Open)];
        let want = format!(
            "{CLASS_DEFS}{}",
            r##"    ID1["ID!1<br/>A &lt; B &gt; C (x) (y) 'q' / p 't' L..."]
    class ID1 open

    NoLinks["No Dependencies"]
"##
        );
        assert_eq!(generate_mermaid(&issues), want);
    }

    #[test]
    fn safe_id_collision_uses_fnv1a_suffix() {
        // "a b" and "ab!" both sanitize to "ab"; the second gets Go's FNV-1a
        // hash of its original ID, formatted with %x.
        let issues = vec![
            issue("a b", "One", Status::Open),
            issue("ab!", "Two", Status::Open),
        ];
        let want = format!(
            "{CLASS_DEFS}{}",
            r##"    ab["a b<br/>One"]
    class ab open
    ab_5c4850f1["ab!<br/>Two"]
    class ab_5c4850f1 open

    NoLinks["No Dependencies"]
"##
        );
        assert_eq!(generate_mermaid(&issues), want);
        assert_eq!(fnv1a32(b"ab!"), 0x5c48_50f1);
    }

    #[test]
    fn safe_ids_keep_hyphens_and_sort_by_raw_id() {
        // "-" is a legal ID character; "." is dropped. Sorting is on the raw
        // ID, so "bd-101-task" ('-' = 0x2D) precedes "bd-101.task" ('.' = 0x2E).
        let issues = vec![
            issue("bd-101.task", "Dotted", Status::Open),
            issue("bd-101-task", "Hyphen", Status::Open),
        ];
        let want = format!(
            "{CLASS_DEFS}{}",
            r##"    bd-101-task["bd-101-task<br/>Hyphen"]
    class bd-101-task open
    bd-101task["bd-101.task<br/>Dotted"]
    class bd-101task open

    NoLinks["No Dependencies"]
"##
        );
        assert_eq!(generate_mermaid(&issues), want);
    }

    #[test]
    fn empty_graph_has_no_nolinks_placeholder() {
        // Go guards on len(issues) > 0, so an empty graph ends after the blank
        // line that separates the classDefs from the (absent) nodes.
        assert_eq!(generate_mermaid(&[]), format!("{CLASS_DEFS}\n"));
    }

    #[test]
    fn nolinks_placeholder_is_configurable() {
        let issues = vec![issue("solo", "Only", Status::Open)];
        let without = generate_mermaid_with_config(
            &issues,
            MermaidConfig {
                show_no_dependencies_node: false,
            },
        );
        assert!(!without.contains("NoLinks"));
        assert!(generate_mermaid(&issues).contains("NoLinks[\"No Dependencies\"]"));
    }

    #[test]
    fn edges_to_unknown_targets_are_skipped() {
        let mut a = issue("A-1", "Dangling", Status::Open);
        a.dependencies
            .push(dep("A-1", "missing", DependencyType::Blocks));
        let m = generate_mermaid(&[a]);
        assert!(!m.contains("missing"), "got:\n{m}");
        assert!(m.contains("NoLinks[\"No Dependencies\"]"));
    }

    #[test]
    fn markdown_has_summary_and_toc() {
        let issues = vec![issue("X-1", "Test", Status::Open)];
        let md = generate_markdown(&issues, "My Report");
        assert!(md.starts_with("# My Report"));
        assert!(md.contains("| Total | 1 |"));
        assert!(md.contains("## Table of Contents"));
        assert!(md.contains("```mermaid"));
    }
}
