//! Test-only issue builders. `bv_core::model::Issue` spells out all 25 fields
//! with no `Default`, so every module in this crate builds its fixtures here
//! rather than repeating the boilerplate.

use bv_core::model::{Dependency, DependencyType, Issue, Status};

/// A minimal open issue with the given id; set fields afterwards.
pub fn issue(id: &str) -> Issue {
    Issue {
        id: id.into(),
        content_hash: String::new(),
        title: String::new(),
        description: String::new(),
        design: String::new(),
        acceptance_criteria: String::new(),
        notes: String::new(),
        status: Status::Open,
        priority: 0,
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
        labels: Vec::new(),
        dependencies: Vec::new(),
        comments: Vec::new(),
        source_repo: String::new(),
    }
}

/// `issue(id)` with a status.
pub fn with_status(id: &str, status: Status) -> Issue {
    Issue {
        status,
        ..issue(id)
    }
}

/// `issue(id)` plus one blocking dependency on `depends_on` (Go `DepBlocks`).
pub fn blocking(id: &str, depends_on: &str) -> Issue {
    issue(id).depends_on(depends_on, "blocks")
}

/// Parse an RFC 3339 instant; every timestamp in the test corpus is written in
/// this shape.
pub fn ts(text: &str) -> jiff::Timestamp {
    text.parse().expect("valid RFC3339 timestamp")
}

/// Render an instant the way [`ts`] accepts it, for fixture construction.
pub fn stamp(t: jiff::Timestamp) -> String {
    t.to_string()
}

/// Fluent setters, so a fixture reads as one expression instead of a nested
/// struct-update chain.
pub trait IssueExt {
    fn title(self, title: &str) -> Issue;
    fn priority(self, priority: i64) -> Issue;
    fn status(self, status: Status) -> Issue;
    fn labels(self, labels: &[&str]) -> Issue;
    fn created_at(self, at: jiff::Timestamp) -> Issue;
    fn updated_at(self, at: jiff::Timestamp) -> Issue;
    fn defer_until(self, at: &str) -> Issue;
    fn depends_on(self, depends_on: &str, dep_type: &str) -> Issue;
}

impl IssueExt for Issue {
    fn title(mut self, title: &str) -> Issue {
        self.title = title.into();
        self
    }
    fn priority(mut self, priority: i64) -> Issue {
        self.priority = priority;
        self
    }
    fn status(mut self, status: Status) -> Issue {
        self.status = status;
        self
    }
    fn labels(mut self, labels: &[&str]) -> Issue {
        self.labels = labels.iter().map(|l| (*l).to_string()).collect();
        self
    }
    fn created_at(mut self, at: jiff::Timestamp) -> Issue {
        self.created_at = Some(stamp(at));
        self
    }
    fn updated_at(mut self, at: jiff::Timestamp) -> Issue {
        self.updated_at = Some(stamp(at));
        self
    }
    fn defer_until(mut self, at: &str) -> Issue {
        self.defer_until = Some(at.into());
        self
    }
    fn depends_on(mut self, depends_on: &str, dep_type: &str) -> Issue {
        self.dependencies.push(Dependency {
            issue_id: String::new(),
            depends_on_id: depends_on.into(),
            depends_on_legacy: String::new(),
            target_id_legacy: String::new(),
            r#type: DependencyType::parse(dep_type),
            created_at: None,
            created_by: String::new(),
        });
        self
    }
}
