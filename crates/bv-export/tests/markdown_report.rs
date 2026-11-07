//! Byte-exact document assertions for the Markdown report writer.
//!
//! The expected strings below are transcribed from Go
//! `pkg/export/markdown.go:359` (`GenerateMarkdown`) at parity commit
//! `18afafa` — section order, table rows, anchors, emoji tables and spacing
//! included. They exist so a paraphrase of the Go writer cannot pass review
//! again: any change to the document has to be justified against the Go source
//! in the same commit that changes this file.

use bv_core::model::{Comment, Dependency, DependencyType, Issue, Status};
use bv_export::markdown::{generate_markdown, ReportOptions};

fn issue(id: &str, title: &str, status: Status, issue_type: &str, priority: i64) -> Issue {
    Issue {
        id: id.to_string(),
        content_hash: String::new(),
        title: title.to_string(),
        description: String::new(),
        design: String::new(),
        acceptance_criteria: String::new(),
        notes: String::new(),
        status,
        priority,
        issue_type: issue_type.to_string(),
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

fn dependency(issue_id: &str, depends_on: &str, kind: DependencyType) -> Dependency {
    Dependency {
        issue_id: issue_id.to_string(),
        depends_on_id: depends_on.to_string(),
        depends_on_legacy: String::new(),
        target_id_legacy: String::new(),
        r#type: kind,
        created_at: None,
        created_by: String::new(),
    }
}

/// Go's `robotNow()` output for the report header.
fn generated_at() -> jiff::Timestamp {
    "2025-01-02T03:04:05Z".parse().unwrap()
}

/// The graph block is owned by `bv_export::mermaid`, so this golden pins the
/// writer itself: everything before and after the Mermaid body.
fn options(include_graph: bool) -> ReportOptions {
    ReportOptions {
        generated_at: Some(generated_at()),
        include_graph,
        ..ReportOptions::default()
    }
}

fn fixture_issues() -> Vec<Issue> {
    let mut full = issue("FULL-1", "Complete Issue", Status::Open, "feature", 0);
    full.description = "Full description".to_string();
    full.acceptance_criteria = "- [ ] one".to_string();
    full.design = "Design doc".to_string();
    full.notes = "Notes here".to_string();
    // A pipe in a table cell must be escaped, and the assignee cell is
    // prefixed with `@`.
    full.assignee = "dev|one".to_string();
    full.labels = vec!["urgent".to_string(), "backend".to_string()];
    full.created_at = Some("2024-01-15T10:00:00Z".to_string());
    full.updated_at = Some("2024-01-16T14:30:00Z".to_string());
    full.dependencies = vec![dependency("FULL-1", "FULL-2", DependencyType::Blocks)];
    full.comments = vec![Comment {
        id: "1".to_string(),
        issue_id: "FULL-1".to_string(),
        author: "alice".to_string(),
        text: "First\nSecond".to_string(),
        created_at: Some("2024-01-15T10:00:00Z".to_string()),
    }];

    let mut closed = issue("FULL-2", "Second Issue", Status::Closed, "task", 3);
    closed.created_at = Some("2024-02-01T08:00:00Z".to_string());
    closed.updated_at = Some("2024-02-01T08:00:00Z".to_string());
    closed.closed_at = Some("2024-02-03T09:05:00Z".to_string());

    vec![full, closed]
}

#[test]
fn document_matches_go_generate_markdown_byte_for_byte() {
    let md = generate_markdown(&fixture_issues(), &options(false));
    let expected = r#"# Beads Export

*Generated: Thu, 02 Jan 2025 03:04:05 UTC*

## Summary

| Metric | Count |
|--------|-------|
| **Total** | 2 |
| Open | 1 |
| In Progress | 0 |
| Blocked | 0 |
| Closed | 1 |

## Table of Contents

- [🟢 FULL-1 Complete Issue](#full-1-complete-issue)
- [⚫ FULL-2 Second Issue](#full-2-second-issue)

---

<a id="full-1-complete-issue"></a>

## ✨ FULL-1 Complete Issue

| Property | Value |
|----------|-------|
| **Type** | ✨ feature |
| **Priority** | 🔥 Critical (P0) |
| **Status** | 🟢 open |
| **Assignee** | @dev\|one |
| **Created** | 2024-01-15 10:00 |
| **Updated** | 2024-01-16 14:30 |
| **Labels** | urgent, backend |

### Description

Full description

### Acceptance Criteria

- [ ] one

### Design

Design doc

### Notes

Notes here

### Dependencies

- ⛔ **blocks**: `FULL-2`

### Comments

> **alice** (2024-01-15)
>
> First
> Second

---

<a id="full-2-second-issue"></a>

## 📋 FULL-2 Second Issue

| Property | Value |
|----------|-------|
| **Type** | 📋 task |
| **Priority** | ☕ Low (P3) |
| **Status** | ⚫ closed |
| **Created** | 2024-02-01 08:00 |
| **Updated** | 2024-02-01 08:00 |
| **Closed** | 2024-02-03 09:05 |

---

"#;
    assert_eq!(md, expected);
}

#[test]
fn empty_issue_set_still_renders_header_summary_and_toc() {
    let md = generate_markdown(&[], &options(false));
    let expected = r#"# Beads Export

*Generated: Thu, 02 Jan 2025 03:04:05 UTC*

## Summary

| Metric | Count |
|--------|-------|
| **Total** | 0 |
| Open | 0 |
| In Progress | 0 |
| Blocked | 0 |
| Closed | 0 |

## Table of Contents


---

"#;
    assert_eq!(md, expected);
}

#[test]
fn graph_block_is_wrapped_in_its_own_fenced_section() {
    let md = generate_markdown(&fixture_issues(), &options(true));
    let summary_end = md.find("\n---\n\n").expect("TOC terminator");
    let after_toc = &md[summary_end + 6..];
    assert!(
        after_toc.starts_with("## Dependency Graph\n\n```mermaid\n"),
        "{after_toc}"
    );
    // The graph block is followed by its own rule, then the first issue.
    let graph_end = after_toc.find("```\n\n---\n\n").expect("graph terminator");
    let after_graph = &after_toc[graph_end + "```\n\n---\n\n".len()..];
    assert!(
        after_graph.starts_with("<a id=\"full-1-complete-issue\"></a>\n\n"),
        "{after_graph}"
    );
}

/// Go's TOC anchor must resolve to the heading it names, including when two
/// issues slugify identically.
#[test]
fn toc_links_resolve_to_unique_anchors() {
    let mut a = issue("BV_1", "Same Title", Status::Open, "bug", 2);
    a.created_at = Some("2024-01-15T10:00:00Z".to_string());
    let mut b = issue("BV-1", "Same Title", Status::Open, "bug", 2);
    b.created_at = Some("2024-01-15T10:00:00Z".to_string());

    let md = generate_markdown(&[a, b], &options(false));
    // createSlug keeps only `[a-z0-9]`, so the type emoji, the spaces and the
    // underscore all collapse and both headings collide; the second gets Go's
    // `-1` suffix.
    let base = "bv-1-same-title";
    assert!(md.contains(&format!("<a id=\"{base}\"></a>")), "{md}");
    assert!(md.contains(&format!("<a id=\"{base}-1\"></a>")), "{md}");
    assert!(
        md.contains(&format!("- [🟢 BV_1 Same Title](#{base})\n")),
        "{md}"
    );
    assert!(
        md.contains(&format!("- [🟢 BV-1 Same Title](#{base}-1)\n")),
        "{md}"
    );
}
