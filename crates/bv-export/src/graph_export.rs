//! Graph export — port of Go `pkg/export/graph_export.go`.
//! Formats: json (adjacency), dot, mermaid. Deterministic output (sorted IDs).
//!
//! The Mermaid diagram and its sanitizers live in [`crate::mermaid`], which is
//! the only copy — Go has a single `GenerateMermaidGraph`, and a second Rust
//! copy is how the two implementations drifted apart in the first place.

/// Render a float the way Go's encoding/json does: shortest round-trip, with a
/// whole number written without a trailing ".0". The binary crate has its own
/// `go_format_f64`; this crate needs the same rule and cannot import it.
fn json_f64(f: f64) -> serde_json::Value {
    if f == f.trunc() && f.abs() < 1e15 {
        serde_json::Value::from(f as i64)
    } else {
        serde_json::Number::from_f64(f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null)
    }
}

use bv_core::model::{Issue, Status};

fn truncate_runes(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        s.to_string()
    } else if max <= 3 {
        chars[..max].iter().collect()
    } else {
        let mut out: String = chars[..max - 3].iter().collect();
        out.push_str("...");
        out
    }
}

fn escape_dot_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace(['\n', '\r'], " ")
}

fn dot_status_color(status: Status) -> &'static str {
    match status {
        Status::Closed | Status::Tombstone => "#CFD8DC",
        Status::Open => "#C8E6C9",
        Status::InProgress => "#BBDEFB",
        Status::Blocked => "#FFCDD2",
        _ => "#FFFFFF",
    }
}

/// Sorted issues by ID for deterministic output.
fn sorted(issues: &[Issue]) -> Vec<&Issue> {
    let mut v: Vec<&Issue> = issues.iter().collect();
    v.sort_by(|a, b| a.id.cmp(&b.id));
    v
}

/// JSON adjacency graph (Go generateAdjacency).
pub fn generate_adjacency(
    issues: &[Issue],
    pagerank: Option<&std::collections::BTreeMap<String, f64>>,
) -> serde_json::Value {
    let ids: std::collections::HashSet<&String> = issues.iter().map(|i| &i.id).collect();
    let nodes: Vec<serde_json::Value> = sorted(issues)
        .into_iter()
        .map(|i| {
            // Go's AdjacencyNode (graph_export.go) is id, title, status,
            // priority, labels (omitempty) and pagerank (omitempty). The
            // adjacency in the JSON export was missing the last two, so the
            // exported graph did not match even where it was structurally
            // right.
            let mut node = serde_json::Map::new();
            node.insert("id".into(), serde_json::json!(i.id));
            node.insert("title".into(), serde_json::json!(i.title));
            node.insert("status".into(), serde_json::json!(i.status.as_str()));
            node.insert("priority".into(), serde_json::json!(i.priority));
            if !i.labels.is_empty() {
                node.insert("labels".into(), serde_json::json!(i.labels));
            }
            if let Some(pr) = pagerank.and_then(|m| m.get(&i.id)).copied() {
                if pr != 0.0 {
                    node.insert("pagerank".into(), json_f64(pr));
                }
            }
            serde_json::Value::Object(node)
        })
        .collect();
    let mut edges = Vec::new();
    for i in sorted(issues) {
        let mut deps: Vec<_> = i.dependencies.iter().collect();
        deps.sort_by(|a, b| a.depends_on_id.cmp(&b.depends_on_id));
        for dep in deps {
            let target = dep.effective_depends_on();
            if !ids.contains(&target.to_string()) {
                continue;
            }
            edges.push(serde_json::json!({
                "from": i.id,
                "to": target,
                "type": dep.r#type.as_str(),
            }));
        }
    }
    serde_json::json!({ "nodes": nodes, "edges": edges })
}

/// DOT format (Go generateDOT): rankdir=LR, box nodes, status colors,
/// penwidth scaled by pagerank when provided.
pub fn generate_dot(
    issues: &[Issue],
    pagerank: Option<&std::collections::BTreeMap<String, f64>>,
) -> String {
    let ids: std::collections::HashSet<&String> = issues.iter().map(|i| &i.id).collect();
    let mut sb = String::new();
    sb.push_str("digraph G {\n");
    sb.push_str("    rankdir=LR;\n");
    sb.push_str("    node [shape=box, fontname=\"Helvetica\", fontsize=10];\n");
    sb.push_str("    edge [fontname=\"Helvetica\", fontsize=8];\n\n");

    for i in sorted(issues) {
        let title = escape_dot_string(&truncate_runes(&i.title, 30));
        let escaped_id = escape_dot_string(&i.id);
        let color = dot_status_color(i.status);
        let label = format!(
            "{escaped_id}\\n{title}\\nP{} {}",
            i.priority,
            i.status.as_str()
        );
        let penwidth = pagerank
            .and_then(|pr| pr.get(&i.id))
            .map(|&v| 1.0 + v * 3.0)
            .unwrap_or(1.0);
        sb.push_str(&format!(
            "    \"{}\" [label=\"{}\", fillcolor=\"{}\", style=filled, penwidth={:.1}];\n",
            escape_dot_string(&i.id),
            label,
            color,
            penwidth
        ));
    }

    sb.push('\n');
    for i in sorted(issues) {
        let mut deps: Vec<_> = i.dependencies.iter().collect();
        deps.sort_by(|a, b| a.depends_on_id.cmp(&b.depends_on_id));
        for dep in deps {
            let target = dep.effective_depends_on();
            if !ids.contains(&target.to_string()) {
                continue;
            }
            let (style, color) = if dep.r#type.is_blocking() {
                ("bold", "#E53935")
            } else {
                ("dashed", "#999999")
            };
            sb.push_str(&format!(
                "    \"{}\" -> \"{}\" [style={}, color=\"{}\"];\n",
                escape_dot_string(&i.id),
                escape_dot_string(target),
                style,
                color
            ));
        }
    }

    sb.push_str("}\n");
    sb
}

/// Mermaid format (Go `GenerateMermaidGraph`): `graph TD`, the four classDefs,
/// collision-free safe IDs, a `class` statement per styled node, `==>` for
/// blocking and `-.->` for related edges.
///
/// Go has exactly one Mermaid generator, so this is a re-export rather than a
/// second implementation.
pub fn generate_mermaid_graph(issues: &[Issue]) -> String {
    crate::mermaid::generate_mermaid(issues)
}

/// Subgraph rooted at `root` up to `depth` hops (Go --graph-root/--graph-depth).
pub fn subgraph<'a>(issues: &'a [&'a Issue], root: &str, depth: usize) -> Vec<&'a Issue> {
    let map: std::collections::HashMap<&String, &Issue> =
        issues.iter().copied().map(|i| (&i.id, i)).collect();
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut frontier = [root.to_string()].into_iter().collect::<Vec<_>>();
    visited.insert(root.to_string());
    for _ in 0..depth {
        let mut next = Vec::new();
        for id in &frontier {
            if let Some(issue) = map.get(id) {
                for dep in &issue.dependencies {
                    let t = dep.effective_depends_on().to_string();
                    if visited.insert(t.clone()) {
                        next.push(t);
                    }
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    issues
        .iter()
        .copied()
        .filter(|i| visited.contains(&i.id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bv_core::model::{Dependency, DependencyType};

    fn issue(id: &str, status: Status, deps: Vec<Dependency>) -> Issue {
        Issue {
            id: id.into(),
            content_hash: String::new(),
            title: format!("Issue {id}"),
            description: String::new(),
            design: String::new(),
            acceptance_criteria: String::new(),
            notes: String::new(),
            status,
            priority: 1,
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
            dependencies: deps,
            comments: vec![],
            source_repo: String::new(),
        }
    }

    fn dep(target: &str, blocking: bool) -> Dependency {
        Dependency {
            issue_id: String::new(),
            depends_on_id: target.into(),
            depends_on_legacy: String::new(),
            target_id_legacy: String::new(),
            created_at: None,
            created_by: String::new(),
            r#type: if blocking {
                DependencyType::Blocks
            } else {
                DependencyType::Related
            },
        }
    }

    /// The exact bytes Go v0.25.0 emits for this fixture — captured by running
    /// `export.GenerateMermaidGraph` against the frozen Go tree. The node ID
    /// sanitizer keeps "-", the class syntax is `class ID name` (not the `:::`
    /// shorthand), and there is no trailing `NoLinks` node once edges exist.
    #[test]
    fn mermaid_matches_go_exactly() {
        let issues = vec![
            issue("a", Status::Open, vec![dep("b", true)]),
            issue("b", Status::Blocked, vec![]),
        ];
        let want = r##"graph TD
    classDef open fill:#50FA7B,stroke:#333,color:#000
    classDef inprogress fill:#8BE9FD,stroke:#333,color:#000
    classDef blocked fill:#FF5555,stroke:#333,color:#000
    classDef closed fill:#6272A4,stroke:#333,color:#fff

    a["a<br/>Issue a"]
    class a open
    b["b<br/>Issue b"]
    class b blocked

    a ==> b
"##;
        assert_eq!(generate_mermaid_graph(&issues), want);
    }

    /// The diagram is generated in exactly one place. This guards the two
    /// entry points against drifting apart again.
    #[test]
    fn mermaid_entry_points_agree() {
        let issues = vec![
            issue("a", Status::Open, vec![dep("b", true)]),
            issue("b", Status::Blocked, vec![]),
        ];
        assert_eq!(
            generate_mermaid_graph(&issues),
            crate::mermaid::generate_mermaid(&issues)
        );
    }

    #[test]
    fn dot_has_rankdir_and_status_colors() {
        let issues = vec![
            issue("a", Status::Open, vec![]),
            issue("b", Status::Closed, vec![]),
        ];
        let d = generate_dot(&issues, None);
        assert!(d.starts_with("digraph G {\n"));
        assert!(d.contains("rankdir=LR;"));
        assert!(d.contains("fillcolor=\"#C8E6C9\""));
        assert!(d.contains("fillcolor=\"#CFD8DC\""));
        assert!(d.ends_with("}\n"));
    }

    #[test]
    fn subgraph_limits_depth() {
        let issues = [
            issue("a", Status::Open, vec![dep("b", true)]),
            issue("b", Status::Open, vec![dep("c", true)]),
            issue("c", Status::Open, vec![]),
        ];
        let refs: Vec<&Issue> = issues.iter().collect();
        assert_eq!(subgraph(&refs, "a", 1).len(), 2);
        assert_eq!(subgraph(&refs, "a", 5).len(), 3);
        assert_eq!(subgraph(&refs, "c", 3).len(), 1);
    }

    #[test]
    fn adjacency_json_deterministic() {
        let issues = [
            issue("b", Status::Open, vec![]),
            issue("a", Status::Open, vec![dep("b", true)]),
        ];
        let j = generate_adjacency(&issues, None);
        assert_eq!(j["nodes"][0]["id"], "a");
        assert_eq!(j["edges"][0]["from"], "a");
        assert_eq!(j["edges"][0]["to"], "b");
    }
}
