//! Native DiGraph — data-layout port of the verbatim wasm crate's
//! `graph.rs` (struct + core methods), with the wasm-bindgen export layer
//! removed. Field names and semantics identical so algorithm modules can
//! be included via #[path] without modification (Phase 2a parity rule).

use std::collections::HashMap;

/// Directed graph optimized for graph algorithms.
/// adj[u] = nodes u points to; rev_adj[v] = nodes pointing at v.
#[derive(Default)]
pub struct DiGraph {
    pub(crate) nodes: Vec<String>,
    pub(crate) node_index: HashMap<String, usize>,
    pub(crate) adj: Vec<Vec<usize>>,
    pub(crate) rev_adj: Vec<Vec<usize>>,
    pub(crate) edge_count: usize,
}

impl DiGraph {
    /// Create an empty graph. (Go/wasm parity constructor.)
    #[allow(clippy::new_without_default)]
    pub fn new() -> DiGraph {
        DiGraph {
            nodes: Vec::new(),
            node_index: HashMap::new(),
            adj: Vec::new(),
            rev_adj: Vec::new(),
            edge_count: 0,
        }
    }

    /// Pre-allocated variant. Edge capacity documented-but-unused upstream.
    pub fn with_capacity(node_capacity: usize, edge_capacity: usize) -> DiGraph {
        let _ = edge_capacity;
        DiGraph {
            nodes: Vec::with_capacity(node_capacity),
            node_index: HashMap::with_capacity(node_capacity),
            adj: Vec::with_capacity(node_capacity),
            rev_adj: Vec::with_capacity(node_capacity),
            edge_count: 0,
        }
    }

    /// Add a node, returns its index. Idempotent.
    pub fn add_node(&mut self, id: &str) -> usize {
        if let Some(&idx) = self.node_index.get(id) {
            return idx;
        }
        let idx = self.nodes.len();
        self.nodes.push(id.to_string());
        self.node_index.insert(id.to_string(), idx);
        self.adj.push(Vec::new());
        self.rev_adj.push(Vec::new());
        idx
    }

    /// Add a directed edge from -> to. Idempotent; silently bounds-checked.
    pub fn add_edge(&mut self, from: usize, to: usize) {
        if from >= self.nodes.len() || to >= self.nodes.len() {
            return;
        }
        if self.adj[from].contains(&to) {
            return;
        }
        self.adj[from].push(to);
        self.rev_adj[to].push(from);
        self.edge_count += 1;
    }

    /// Number of nodes.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Number of edges.
    pub fn edge_count(&self) -> usize {
        self.edge_count
    }

    /// Graph density: edges / (nodes * (nodes - 1)).
    pub fn density(&self) -> f64 {
        let n = self.node_count() as f64;
        let e = self.edge_count as f64;
        if n <= 1.0 {
            return 0.0;
        }
        e / (n * (n - 1.0))
    }

    /// Node count alias used by some algorithm modules.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Node ID by index (owned, matching wasm-crate signature).
    pub fn node_id(&self, idx: usize) -> Option<String> {
        self.nodes.get(idx).cloned()
    }

    pub fn out_degree(&self, node: usize) -> usize {
        self.adj.get(node).map(|v| v.len()).unwrap_or(0)
    }

    pub fn in_degree(&self, node: usize) -> usize {
        self.rev_adj.get(node).map(|v| v.len()).unwrap_or(0)
    }

    /// Index by ID string.
    pub fn node_idx(&self, id: &str) -> Option<usize> {
        self.node_index.get(id).copied()
    }

    /// Out-neighbours of `node`; empty for an out-of-range index, matching
    /// the wasm crate's `.get().map_or(&[])` accessors and the graceful
    /// handling `reachability` callers rely on.
    pub fn successors_slice(&self, node: usize) -> &[usize] {
        self.adj.get(node).map_or(&[], |v| v.as_slice())
    }

    /// In-neighbours of `node`; empty for an out-of-range index.
    pub fn predecessors_slice(&self, node: usize) -> &[usize] {
        self.rev_adj.get(node).map_or(&[], |v| v.as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Out-of-range indices yield empty slices, matching the wasm crate's
    /// `.get().map_or(&[])` accessors — reachability callers rely on this
    /// graceful handling instead of a panic.
    #[test]
    fn slice_accessors_tolerate_out_of_range() {
        let mut g = DiGraph::new();
        let a = g.add_node("a");
        let b = g.add_node("b");
        g.add_edge(a, b);

        assert_eq!(g.successors_slice(a), &[b]);
        assert_eq!(g.predecessors_slice(b), &[a]);
        assert!(g.successors_slice(99).is_empty());
        assert!(g.predecessors_slice(99).is_empty());
    }
}
