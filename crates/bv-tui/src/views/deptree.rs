//! Dependency-tree builder for the left pane.
//!
//! [`build_dep_tree`] walks the outgoing dependency edges of every issue in
//! the filtered set and flattens the result into row descriptors carrying the
//! box-drawing prefix already computed, in the same shape
//! [`crate::detail::render_tree_children`] uses (`├── `/`└── ` with `│   `
//! continuations). The renderer only has to prepend `prefix` to the normal
//! list-row spans.
//!
//! The tree is *display-only*: it never reorders or removes anything from
//! `filtered_indices`, so `App::cursor` keeps indexing `filtered_indices`
//! exactly as it does in flat mode, and every consumer of that index
//! (`selected()`, the mouse click handler, the search-rank badge,
//! `select_issue_in_list`) is unaffected.

use std::collections::HashMap;

#[cfg(test)]
use bv_core::model::Dependency;
use bv_core::model::{DependencyType, Issue};

/// How a node hangs off its parent.
///
/// `ParentChild` and [`EdgeKind::Blocking`] are structural — they make the
/// child *appear under* its target. [`EdgeKind::Advisory`] edges carry only
/// context, so they and their subtrees are rendered dimmed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    /// `parent-child` — structural containment.
    ParentChild,
    /// `blocks` / `conditional-blocks` / `waits-for` — structural gating.
    Blocking,
    /// `related` / `discovered-from` / unknown — advisory only.
    Advisory,
}

impl EdgeKind {
    /// Classify a dependency type.
    pub fn of(dep: &DependencyType) -> Self {
        match dep {
            DependencyType::ParentChild => EdgeKind::ParentChild,
            DependencyType::Blocks
            | DependencyType::ConditionalBlocks
            | DependencyType::WaitsFor => EdgeKind::Blocking,
            _ => EdgeKind::Advisory,
        }
    }

    /// True when the edge is advisory (rendered dimmed).
    pub fn is_advisory(&self) -> bool {
        matches!(self, EdgeKind::Advisory)
    }
}

/// The wire form of a dependency type, for the `[type]` annotation.
///
/// Free function rather than a method on [`EdgeKind`] because the three
/// blocking kinds collapse into one variant but still need to print
/// distinctly (`blocks` vs `conditional-blocks` vs `waits-for`).
pub fn dep_type_label(dep: &DependencyType) -> String {
    dep.as_str().to_string()
}

/// One row of the flattened dependency tree.
#[derive(Debug, Clone)]
pub struct DepNode {
    /// Index into `filtered_indices` (and therefore into `App::rows`), so the
    /// renderer can look the issue up without a second lookup structure.
    pub filtered_pos: usize,
    /// 0 for roots; one deeper per level.
    pub depth: usize,
    /// Pre-computed indent + connector, e.g. `"│   └── "`. Empty for roots.
    pub prefix: String,
    /// How this node hangs off its parent.
    pub kind: EdgeKind,
    /// Set when a cycle or the depth cap stopped the walk here.
    pub truncated: Option<Truncation>,
    /// True when this node (or an ancestor) hangs off an advisory edge.
    pub dimmed: bool,
    /// Why this node hangs off its parent, in wire form (e.g. `"blocks"`).
    /// Empty for roots. Rendered as a dimmed `[label]` suffix so a tree row
    /// says *why* it hangs where it does.
    pub label: String,
}

/// Why a subtree was not expanded further.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Truncation {
    /// The target already appears on this root path.
    Cycle,
    /// `max_depth` reached.
    MaxDepth,
}

/// Child edges of one issue, resolved to positions in the filtered set.
type Edges = Vec<ChildEdge>;

/// A resolved child edge.
#[derive(Debug, Clone)]
struct ChildEdge {
    pos: usize,
    kind: EdgeKind,
    /// The dependency type as written, so the renderer can annotate it.
    label: String,
}

/// Build the flattened dependency tree for one filtered set.
///
/// `filtered` must be the rows selected by `filtered_indices`, in the order
/// the renderer already displays them, so roots keep today's sort/filter
/// ordering. `issue_map` supplies the full corpus so children can be resolved.
///
/// Only children that are themselves in `filtered` are emitted: a filtered-out
/// issue stays invisible, exactly as in flat mode.
///
/// Roots are the issues nothing else points at. Every issue in a dependency
/// cycle is pointed at, though, so when that leaves no roots at all (a pure
/// cycle, or a self-contained strongly-connected group) the issues are walked
/// from the first filtered one instead — rendering nothing would make the pane
/// blank.
///
/// Each root is walked independently with a fresh cycle guard, so a diamond
/// (`A→B, A→C, B→D, C→D`) renders `D` twice. That is deliberate and matches
/// [`crate::detail::render_tree_children`], whose `visited` set is
/// inserted-then-removed per branch.
pub fn build_dep_tree(
    filtered: &[&crate::ListRow],
    issue_map: &HashMap<String, Issue>,
    max_depth: usize,
) -> Vec<DepNode> {
    if filtered.is_empty() {
        return Vec::new();
    }

    // id -> position in `filtered`. Children are resolved through this, which
    // is what keeps a filtered-out issue from appearing in the tree.
    let pos_of: HashMap<&str, usize> = filtered
        .iter()
        .enumerate()
        .map(|(i, row)| (row.id.as_str(), i))
        .collect();

    // Children per issue, in filtered order, so the tree reads in the same
    // order the flat list does. A target reached via several edge types keeps
    // the first edge only — two rows for one relationship is noise.
    let mut children_of: HashMap<&str, Edges> = HashMap::new();
    for (pos, row) in filtered.iter().enumerate() {
        let Some(issue) = issue_map.get(&row.id) else {
            continue;
        };
        for dep in &issue.dependencies {
            let target = dep.effective_depends_on();
            let Some(&child_pos) = pos_of.get(target) else {
                continue;
            };
            // Skip self-edges: they render as a 1-node cycle and add nothing.
            if child_pos == pos {
                continue;
            }
            let entry = children_of.entry(row.id.as_str()).or_default();
            if entry.iter().any(|e| e.pos == child_pos) {
                continue;
            }
            entry.push(ChildEdge {
                pos: child_pos,
                kind: EdgeKind::of(&dep.r#type),
                label: dep_type_label(&dep.r#type),
            });
        }
    }
    for edges in children_of.values_mut() {
        edges.sort_by_key(|e| e.pos);
    }

    // Roots: issues nothing else points at.
    let mut is_child = vec![false; filtered.len()];
    for edges in children_of.values() {
        for edge in edges {
            is_child[edge.pos] = true;
        }
    }
    let roots: Vec<usize> = (0..filtered.len()).filter(|&p| !is_child[p]).collect();
    // A closed cycle leaves every issue pointed at. Fall back to walking from
    // the first filtered issue so the pane still shows the group.
    let roots = if roots.is_empty() { vec![0] } else { roots };

    let mut out = Vec::new();
    let mut path = Vec::new();
    for root in roots {
        walk(
            root,
            0,
            "",
            true,
            EdgeKind::ParentChild,
            false,
            "",
            filtered,
            &children_of,
            &mut path,
            max_depth,
            &mut out,
        );
    }
    out
}

/// Recursive descent emitting rows depth-first, mirroring
/// [`crate::detail::render_tree_children`]'s prefix scheme.
#[allow(clippy::too_many_arguments)]
fn walk(
    pos: usize,
    depth: usize,
    parent_prefix: &str,
    is_last: bool,
    kind: EdgeKind,
    dimmed: bool,
    label: &str,
    filtered: &[&crate::ListRow],
    children_of: &HashMap<&str, Edges>,
    path: &mut Vec<usize>,
    max_depth: usize,
    out: &mut Vec<DepNode>,
) {
    // Roots sit flush left; every deeper level carries its parent's connector.
    let prefix = if depth == 0 {
        String::new()
    } else if is_last {
        format!("{parent_prefix}\u{2514}\u{2500}\u{2500} ")
    } else {
        format!("{parent_prefix}\u{251c}\u{2500}\u{2500} ")
    };

    // The cycle guard is path-local: `path` holds the ancestors of the node
    // being emitted, and only those. A sibling's branch never counts.
    let truncated = if path.contains(&pos) {
        Some(Truncation::Cycle)
    } else if depth > 0 && depth >= max_depth {
        Some(Truncation::MaxDepth)
    } else {
        None
    };

    out.push(DepNode {
        filtered_pos: pos,
        depth,
        prefix: prefix.clone(),
        kind,
        truncated,
        dimmed,
        label: label.to_string(),
    });

    if truncated.is_some() {
        return;
    }

    let Some(edges) = children_of.get(filtered[pos].id.as_str()) else {
        return;
    };
    let edges = edges.clone();
    let count = edges.len();
    // The continuation string handed to the children is the parent's
    // continuation plus this node's own run — it must NOT include this node's
    // connector. `detail.rs` draws the same line: the connector goes into the
    // printed row, while `child_prefix` is built from the continuation above.
    let child_continuation = if depth == 0 {
        parent_prefix.to_string()
    } else if is_last {
        format!("{parent_prefix}    ")
    } else {
        format!("{parent_prefix}\u{2502}   ")
    };
    path.push(pos);
    for (i, edge) in edges.iter().enumerate() {
        let child_is_last = i + 1 == count;
        walk(
            edge.pos,
            depth + 1,
            &child_continuation,
            child_is_last,
            edge.kind,
            dimmed || edge.kind.is_advisory(),
            &edge.label,
            filtered,
            children_of,
            path,
            max_depth,
            out,
        );
    }
    path.pop();
}

/// Position of the tree row a flat cursor points at.
///
/// `cursor` indexes `filtered_indices`; the tree can contain the same issue
/// more than once (it appears under several parents). The first occurrence is
/// the cursor's home, matching how the flat list highlights one row.
///
/// A cursor whose issue is not in the tree at all (unreachable today — every
/// filtered issue is at least a root, or part of the cycle fallback) falls back
/// to the nearest tree row at or after it, so the window stays well-defined.
pub fn tree_row_for_cursor(nodes: &[DepNode], cursor: usize) -> usize {
    nodes
        .iter()
        .position(|n| n.filtered_pos == cursor)
        .or_else(|| nodes.iter().position(|n| n.filtered_pos >= cursor))
        .unwrap_or(0)
        .min(nodes.len().saturating_sub(1))
}

/// Build the digraph tools the renderer needs in one pass: the flattened tree
/// plus the tree-row position for a flat cursor. Kept together so the render
/// path cannot build the tree twice.
pub fn tree_preview(
    filtered: &[&crate::ListRow],
    issue_map: &HashMap<String, Issue>,
    max_depth: usize,
    cursor: usize,
) -> (Vec<DepNode>, usize) {
    let nodes = build_dep_tree(filtered, issue_map, max_depth);
    let row = tree_row_for_cursor(&nodes, cursor);
    (nodes, row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bv_core::model::Status;

    /// Depth cap the renderer uses. 3 levels below the root, matching
    /// `detail.rs`'s `max_depth: 3` and Go's `BuildDependencyTree`.
    const MAX_DEPTH: usize = 3;

    fn row(id: &str) -> crate::ListRow {
        crate::ListRow {
            id: id.to_string(),
            title: format!("Title for {id}"),
            status: Status::Open,
            priority: 2,
            issue_type: "task".into(),
            labels: vec![],
            created_at: None,
            description: String::new(),
            notes: String::new(),
            assignee: String::new(),
            repo_prefix: None,
        }
    }

    fn dep(id: &str, target: &str, ty: DependencyType) -> Dependency {
        Dependency {
            issue_id: id.into(),
            depends_on_id: target.into(),
            depends_on_legacy: String::new(),
            target_id_legacy: String::new(),
            r#type: ty,
            created_at: None,
            created_by: String::new(),
        }
    }

    fn issue_with_deps(id: &str, deps: Vec<Dependency>) -> Issue {
        Issue {
            id: id.into(),
            content_hash: String::new(),
            title: format!("Title for {id}"),
            description: String::new(),
            design: String::new(),
            acceptance_criteria: String::new(),
            notes: String::new(),
            status: Status::Open,
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
            dependencies: deps,
            comments: vec![],
            source_repo: String::new(),
        }
    }

    fn map_of(pairs: Vec<(&str, Vec<Dependency>)>) -> HashMap<String, Issue> {
        pairs
            .into_iter()
            .map(|(id, deps)| (id.to_string(), issue_with_deps(id, deps)))
            .collect()
    }

    /// Collect the `filtered_pos` of each emitted row.
    fn positions(nodes: &[DepNode]) -> Vec<usize> {
        nodes.iter().map(|n| n.filtered_pos).collect()
    }

    #[test]
    fn empty_set_builds_no_nodes() {
        assert!(build_dep_tree(&[], &HashMap::new(), MAX_DEPTH).is_empty());
    }

    #[test]
    fn independent_issues_are_all_roots() {
        let a = row("A");
        let b = row("B");
        let filtered = vec![&a, &b];
        let nodes = build_dep_tree(&filtered, &HashMap::new(), MAX_DEPTH);
        assert_eq!(positions(&nodes), vec![0, 1]);
        assert!(nodes.iter().all(|n| n.depth == 0 && n.prefix.is_empty()));
    }

    #[test]
    fn parent_child_edges_nest_the_child() {
        let a = row("A");
        let b = row("B");
        let filtered = vec![&a, &b];
        let map = map_of(vec![(
            "A",
            vec![dep("A", "B", DependencyType::ParentChild)],
        )]);
        let nodes = build_dep_tree(&filtered, &map, MAX_DEPTH);
        assert_eq!(positions(&nodes), vec![0, 1]);
        assert_eq!(nodes[1].depth, 1);
        assert_eq!(nodes[1].prefix, "\u{2514}\u{2500}\u{2500} ");
        assert_eq!(nodes[1].kind, EdgeKind::ParentChild);
    }

    #[test]
    fn blocking_family_all_nests() {
        let a = row("A");
        let b = row("B");
        let c = row("C");
        let d = row("D");
        let filtered = vec![&a, &b, &c, &d];
        let map = map_of(vec![(
            "A",
            vec![
                dep("A", "B", DependencyType::Blocks),
                dep("A", "C", DependencyType::ConditionalBlocks),
                dep("A", "D", DependencyType::WaitsFor),
            ],
        )]);
        let nodes = build_dep_tree(&filtered, &map, MAX_DEPTH);
        assert_eq!(positions(&nodes), vec![0, 1, 2, 3]);
        assert!(nodes[1..].iter().all(|n| n.depth == 1));
        assert!(nodes[1..].iter().all(|n| n.kind == EdgeKind::Blocking));
    }

    #[test]
    fn legacy_depends_on_field_is_honoured() {
        // `graph.rs:46` and `views/tree.rs:44` read `depends_on_id` directly and
        // silently drop legacy-spelled edges. The builder must not.
        let a = row("A");
        let b = row("B");
        let filtered = vec![&a, &b];
        let mut legacy = dep("A", "", DependencyType::ParentChild);
        legacy.depends_on_legacy = "B".into();
        let map = map_of(vec![("A", vec![legacy])]);
        let nodes = build_dep_tree(&filtered, &map, MAX_DEPTH);
        assert_eq!(positions(&nodes), vec![0, 1]);
        assert_eq!(nodes[1].depth, 1);
    }

    #[test]
    fn target_id_legacy_field_is_honoured() {
        let a = row("A");
        let b = row("B");
        let filtered = vec![&a, &b];
        let mut legacy = dep("A", "", DependencyType::ParentChild);
        legacy.target_id_legacy = "B".into();
        let map = map_of(vec![("A", vec![legacy])]);
        let nodes = build_dep_tree(&filtered, &map, MAX_DEPTH);
        assert_eq!(positions(&nodes), vec![0, 1]);
        assert_eq!(nodes[1].depth, 1);
    }

    #[test]
    fn filtered_out_target_is_not_emitted() {
        let a = row("A");
        let c = row("C");
        let filtered = vec![&a, &c];
        let map = map_of(vec![
            ("A", vec![dep("A", "B", DependencyType::ParentChild)]),
            ("B", vec![dep("B", "C", DependencyType::ParentChild)]),
        ]);
        // B is filtered out, so neither A nor C can reach it.
        let nodes = build_dep_tree(&filtered, &map, MAX_DEPTH);
        assert_eq!(positions(&nodes), vec![0, 1]);
        assert!(nodes.iter().all(|n| n.depth == 0));
    }

    #[test]
    fn cycle_terminates_and_marks_the_revisit() {
        let a = row("A");
        let b = row("B");
        let filtered = vec![&a, &b];
        let map = map_of(vec![
            ("A", vec![dep("A", "B", DependencyType::Blocks)]),
            ("B", vec![dep("B", "A", DependencyType::Blocks)]),
        ]);
        // Both are pointed at, so the cycle fallback walks from A:
        // A -> B -> A(revisit, marked, not expanded).
        let nodes = build_dep_tree(&filtered, &map, MAX_DEPTH);
        assert_eq!(positions(&nodes), vec![0, 1, 0]);
        assert_eq!(nodes[1].depth, 1);
        assert_eq!(nodes[2].depth, 2);
        assert_eq!(nodes[2].truncated, Some(Truncation::Cycle));
    }

    #[test]
    fn self_edge_is_skipped() {
        let a = row("A");
        let filtered = vec![&a];
        let map = map_of(vec![("A", vec![dep("A", "A", DependencyType::Blocks)])]);
        let nodes = build_dep_tree(&filtered, &map, MAX_DEPTH);
        assert_eq!(positions(&nodes), vec![0]);
        assert!(nodes[0].truncated.is_none());
    }

    #[test]
    fn depth_cap_stops_expansion() {
        // A -> B -> C -> D. The cap emits D at depth 3 with the marker and
        // does not expand past it.
        let a = row("A");
        let b = row("B");
        let c = row("C");
        let d = row("D");
        let filtered = vec![&a, &b, &c, &d];
        let map = map_of(vec![
            ("A", vec![dep("A", "B", DependencyType::ParentChild)]),
            ("B", vec![dep("B", "C", DependencyType::ParentChild)]),
            ("C", vec![dep("C", "D", DependencyType::ParentChild)]),
        ]);
        let nodes = build_dep_tree(&filtered, &map, MAX_DEPTH);
        assert_eq!(nodes.len(), 4);
        assert_eq!(nodes[3].depth, 3);
        assert_eq!(nodes[3].truncated, Some(Truncation::MaxDepth));
    }

    #[test]
    fn a_depth_cap_of_one_keeps_only_roots() {
        let a = row("A");
        let b = row("B");
        let filtered = vec![&a, &b];
        let map = map_of(vec![(
            "A",
            vec![dep("A", "B", DependencyType::ParentChild)],
        )]);
        let nodes = build_dep_tree(&filtered, &map, 1);
        assert_eq!(positions(&nodes), vec![0, 1]);
        assert_eq!(nodes[1].depth, 1);
        assert_eq!(nodes[1].truncated, Some(Truncation::MaxDepth));
    }

    #[test]
    fn missing_target_is_not_emitted() {
        let a = row("A");
        let filtered = vec![&a];
        let map = map_of(vec![("A", vec![dep("A", "GHOST", DependencyType::Blocks)])]);
        let nodes = build_dep_tree(&filtered, &map, MAX_DEPTH);
        assert_eq!(positions(&nodes), vec![0]);
    }

    #[test]
    fn related_edges_are_advisory() {
        let a = row("A");
        let b = row("B");
        let filtered = vec![&a, &b];
        let map = map_of(vec![("A", vec![dep("A", "B", DependencyType::Related)])]);
        let nodes = build_dep_tree(&filtered, &map, MAX_DEPTH);
        assert_eq!(nodes[1].kind, EdgeKind::Advisory);
        assert!(nodes[1].dimmed);
    }

    #[test]
    fn advisory_subtree_inherits_dimmed() {
        let a = row("A");
        let b = row("B");
        let c = row("C");
        let filtered = vec![&a, &b, &c];
        let map = map_of(vec![
            ("A", vec![dep("A", "B", DependencyType::Related)]),
            ("B", vec![dep("B", "C", DependencyType::Blocks)]),
        ]);
        let nodes = build_dep_tree(&filtered, &map, MAX_DEPTH);
        assert_eq!(positions(&nodes), vec![0, 1, 2]);
        // B is a `related` child; C hangs off B and inherits the dimming.
        assert!(nodes[1].dimmed);
        assert_eq!(nodes[2].depth, 2);
        assert!(nodes[2].dimmed);
    }

    #[test]
    fn diamond_renders_the_shared_child_twice() {
        let a = row("A");
        let b = row("B");
        let c = row("C");
        let d = row("D");
        let filtered = vec![&a, &b, &c, &d];
        let map = map_of(vec![
            (
                "A",
                vec![
                    dep("A", "B", DependencyType::ParentChild),
                    dep("A", "C", DependencyType::ParentChild),
                ],
            ),
            ("B", vec![dep("B", "D", DependencyType::ParentChild)]),
            ("C", vec![dep("C", "D", DependencyType::ParentChild)]),
        ]);
        let nodes = build_dep_tree(&filtered, &map, MAX_DEPTH);
        assert_eq!(nodes.iter().filter(|n| n.filtered_pos == 3).count(), 2);
    }

    #[test]
    fn two_edge_types_to_one_target_keep_one_row() {
        let a = row("A");
        let b = row("B");
        let filtered = vec![&a, &b];
        let map = map_of(vec![(
            "A",
            vec![
                dep("A", "B", DependencyType::ParentChild),
                dep("A", "B", DependencyType::Blocks),
            ],
        )]);
        let nodes = build_dep_tree(&filtered, &map, MAX_DEPTH);
        assert_eq!(positions(&nodes), vec![0, 1]);
        // The first edge in dependency order wins.
        assert_eq!(nodes[1].kind, EdgeKind::ParentChild);
    }

    #[test]
    fn prefixes_use_live_verticals_for_non_last_siblings() {
        // A -> {B, C} and B -> D. B is not the last sibling, so the vertical
        // under A stays live for D.
        let a = row("A");
        let b = row("B");
        let c = row("C");
        let d = row("D");
        let filtered = vec![&a, &b, &c, &d];
        let map = map_of(vec![
            (
                "A",
                vec![
                    dep("A", "B", DependencyType::ParentChild),
                    dep("A", "C", DependencyType::ParentChild),
                ],
            ),
            ("B", vec![dep("B", "D", DependencyType::ParentChild)]),
        ]);
        let nodes = build_dep_tree(&filtered, &map, MAX_DEPTH);
        // A, B, D (under B), C
        assert_eq!(positions(&nodes), vec![0, 1, 3, 2]);
        let d_node = nodes.iter().find(|n| n.filtered_pos == 3).unwrap();
        assert_eq!(d_node.depth, 2);
        assert_eq!(d_node.prefix, "\u{2502}   \u{2514}\u{2500}\u{2500} ");
        // C is the last sibling, so its vertical is blank.
        let c_node = nodes.iter().find(|n| n.filtered_pos == 2).unwrap();
        assert_eq!(c_node.prefix, "\u{2514}\u{2500}\u{2500} ");
    }

    #[test]
    fn cursor_maps_to_the_tree_row_for_that_issue() {
        let a = row("A");
        let b = row("B");
        let filtered = vec![&a, &b];
        let map = map_of(vec![("A", vec![dep("A", "B", DependencyType::Blocks)])]);
        let nodes = build_dep_tree(&filtered, &map, MAX_DEPTH);
        assert_eq!(tree_row_for_cursor(&nodes, 0), 0);
        assert_eq!(tree_row_for_cursor(&nodes, 1), 1);
    }

    #[test]
    fn cursor_maps_to_the_first_of_duplicate_rows() {
        let a = row("A");
        let b = row("B");
        let c = row("C");
        let d = row("D");
        let filtered = vec![&a, &b, &c, &d];
        let map = map_of(vec![
            (
                "A",
                vec![
                    dep("A", "B", DependencyType::ParentChild),
                    dep("A", "C", DependencyType::ParentChild),
                ],
            ),
            ("B", vec![dep("B", "D", DependencyType::ParentChild)]),
            ("C", vec![dep("C", "D", DependencyType::ParentChild)]),
        ]);
        let nodes = build_dep_tree(&filtered, &map, MAX_DEPTH);
        // D appears at tree rows 2 and 4; the cursor's home is the first.
        assert_eq!(tree_row_for_cursor(&nodes, 3), 2);
    }

    #[test]
    fn cursor_past_every_row_clamps_instead_of_panicking() {
        let nodes = build_dep_tree(&[], &HashMap::new(), MAX_DEPTH);
        assert_eq!(tree_row_for_cursor(&nodes, 99), 0);
    }
}
