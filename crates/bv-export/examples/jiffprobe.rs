//! Print the Markdown report for a fixed issue set.
//!
//! The document below is the one `tests/markdown_report.rs` pins
//! byte-for-byte against Go's `GenerateMarkdown`, so running this example and
//! diffing the output against the golden is the fastest way to see what a
//! writer change did to the document.
//!
//!     cargo run -p bv-export --example jiffprobe
//!
//! (This example was born as a jiff formatting probe; the file name is a
//! leftover and is worth renaming to `markdown_report` when convenient.)

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

fn main() {
    let mut full = issue("FULL-1", "Complete Issue", Status::Open, "feature", 0);
    full.description = "Full description".to_string();
    full.acceptance_criteria = "- [ ] one".to_string();
    full.design = "Design doc".to_string();
    full.notes = "Notes here".to_string();
    full.assignee = "dev|one".to_string();
    full.labels = vec!["urgent".to_string(), "backend".to_string()];
    full.created_at = Some("2024-01-15T10:00:00Z".to_string());
    full.updated_at = Some("2024-01-16T14:30:00Z".to_string());
    full.dependencies = vec![Dependency {
        issue_id: "FULL-1".to_string(),
        depends_on_id: "FULL-2".to_string(),
        depends_on_legacy: String::new(),
        target_id_legacy: String::new(),
        r#type: DependencyType::Blocks,
        created_at: None,
        created_by: String::new(),
    }];
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

    let options = ReportOptions {
        generated_at: Some("2025-01-02T03:04:05Z".parse().unwrap()),
        // The graph body belongs to `bv_export::mermaid`; leave it out so this
        // example's output is exactly the pinned golden document.
        include_graph: false,
        ..Default::default()
    };
    print!("{}", generate_markdown(&[full, closed], &options));
}
