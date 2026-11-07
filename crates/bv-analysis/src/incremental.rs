//! Incremental analysis engine — Phase 10a.
//!
//! Instead of full graph recompute on every change, identify the affected
//! subgraph from dirty nodes and update only affected metrics.

use bv_graph_core::DiGraph;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// Tracks which issues changed since last analysis.
#[derive(Debug, Default)]
pub struct DirtySet {
    /// Issue IDs that were added, modified, or had dependency changes.
    pub dirty: BTreeSet<String>,
    /// Issue IDs that were removed entirely.
    pub removed: BTreeSet<String>,
}

impl DirtySet {
    pub fn mark_dirty(&mut self, id: &str) {
        self.dirty.insert(id.to_string());
    }

    pub fn mark_removed(&mut self, id: &str) {
        self.dirty.remove(id);
        self.removed.insert(id.to_string());
    }

    /// Compute the affected subgraph closure (forward dependents).
    pub fn affected_closure(
        &self,
        graph: &DiGraph,
        id_to_idx: &BTreeMap<String, usize>,
    ) -> BTreeSet<usize> {
        let mut affected = std::collections::BTreeSet::new();
        for id in &self.dirty {
            if let Some(&idx) = id_to_idx.get(id) {
                // Forward closure via BFS on dependents
                let mut queue = vec![idx];
                while let Some(cur) = queue.pop() {
                    if affected.insert(cur) {
                        // Find all nodes that depend on cur (reverse edges)
                        for pred in graph.predecessors_slice(cur) {
                            queue.push(*pred);
                        }
                    }
                }
            }
        }
        for id in &self.removed {
            if let Some(&idx) = id_to_idx.get(id) {
                affected.insert(idx);
            }
        }
        affected
    }
}

/// Result of an incremental refresh.
#[derive(Debug, Clone, Serialize)]
pub struct IncrementalResult {
    /// Number of nodes recomputed vs total.
    pub recomputed_nodes: usize,
    pub total_nodes: usize,
    /// Whether a full recompute was needed instead.
    pub fell_back_to_full: bool,
}

/// Decide whether incremental is viable or full recompute is needed.
pub fn should_incremental(dirty_count: usize, total_nodes: usize) -> bool {
    // If > 30% of nodes are dirty, full recompute is faster.
    if total_nodes == 0 {
        return false;
    }
    (dirty_count as f64 / total_nodes as f64) < 0.3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_dirty_set_is_incremental() {
        assert!(should_incremental(5, 100));
        assert!(should_incremental(29, 100));
    }

    #[test]
    fn large_dirty_set_falls_back() {
        assert!(!should_incremental(40, 100));
        assert!(!should_incremental(100, 100));
    }

    #[test]
    fn empty_graph_never_incremental() {
        assert!(!should_incremental(0, 0));
    }
}
