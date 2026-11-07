//! TopK Set algorithm: Greedy submodular selection for maximum unlock.
//!
//! Finds the k issues that, when completed, maximize the total downstream
//! unlocks. Uses greedy submodular optimization where each iteration picks
//! the node with maximum marginal gain.

use crate::graph::DiGraph;
use crate::reachability::actionable_nodes;
use crate::whatif::what_if_close;
use serde::Serialize;

/// An item in the TopK Set result.
#[derive(Debug, Clone, Serialize)]
pub struct TopKSetItem {
    /// Node index in the graph
    pub node: usize,
    /// Marginal gain from selecting this node (direct newly-actionable count)
    pub marginal_gain: usize,
    /// Indices of nodes that become actionable after this selection
    pub unblocked_ids: Vec<usize>,
}

/// Result of the TopK Set algorithm.
#[derive(Debug, Clone, Serialize)]
pub struct TopKSetResult {
    /// Selected items in order of selection
    pub items: Vec<TopKSetItem>,
    /// Total gain across all selections
    pub total_gain: usize,
    /// Number of open (non-closed) nodes considered
    pub open_nodes: usize,
}

/// Greedy submodular selection for maximum unlock.
///
/// Ports Go `generateTopKSet` (pkg/analysis/advanced_insights.go:458):
/// each iteration re-derives the actionable set after the picks simulated
/// so far and selects only from it, so a blocked high-fanout node can never
/// precede its prerequisite. The gain is the DIRECT newly-actionable count
/// (`computeMarginalUnblocksFromBefore`), not the transitive cascade, and
/// only the selected node is marked complete — unblocked nodes stay open
/// and become candidates in later iterations.
///
/// # Arguments
/// * `graph` - The directed dependency graph
/// * `closed_set` - Boolean array where true means the node is closed/completed
/// * `k` - Maximum number of items to select
///
/// # Returns
/// TopKSetResult with selected items sorted by selection order.
///
/// # Complexity
/// O(k * (n + a * (n + e))) where a is the actionable count and e the edge
/// count — one what-if evaluation per actionable candidate per iteration.
pub fn topk_set(graph: &DiGraph, closed_set: &[bool], k: usize) -> TopKSetResult {
    let n = graph.len();
    if n == 0 || k == 0 {
        return TopKSetResult {
            items: Vec::new(),
            total_gain: 0,
            open_nodes: 0,
        };
    }

    // Initialize working closed set (real closed set + simulated completions)
    let mut current_closed = closed_set.to_vec();
    current_closed.resize(n, false);

    // Count open nodes
    let open_nodes = (0..n).filter(|&i| !current_closed[i]).count();

    let mut selected = Vec::new();
    let mut total_gain = 0;

    for _ in 0..k {
        // Go getActionableIssuesAfterCompletions(completed): the candidate
        // pool is re-derived each step — only nodes whose blockers are all
        // closed or already selected. Sorted by index for determinism.
        let remaining = actionable_nodes(graph, &current_closed);
        if remaining.is_empty() {
            break;
        }

        // `before` = the currently-actionable set. An already-ready issue is
        // not a new unlock (advanced_insights.go:582), so it can never
        // appear in a candidate's unblocks.
        let mut before = vec![false; n];
        for &id in &remaining {
            before[id] = true;
        }

        let mut best_node: Option<usize> = None;
        let mut best_gain = 0usize;
        let mut best_unblocks: Vec<usize> = Vec::new();

        for &cand in &remaining {
            // Direct newly-actionable count only — equivalent to Go's
            // computeMarginalUnblocksFromBefore: successors of `cand` that
            // were blocked before and are actionable after, excluding the
            // already-actionable `before` set. what_if_close's
            // unblocked_ids is exactly that set (the transitive cascade in
            // cascade_ids must NOT be used for the gain).
            let mut unblocks = what_if_close(graph, cand, &current_closed).unblocked_ids;
            unblocks.retain(|&w| !before[w]);
            unblocks.sort_unstable();
            unblocks.dedup();
            let gain = unblocks.len();

            // Prefer higher gain, or lower node index for determinism
            // (Go tie-breaks on ID; index order matches ID order here).
            let better = match best_node {
                None => true,
                Some(b) => gain > best_gain || (gain == best_gain && cand < b),
            };
            if better {
                best_gain = gain;
                best_node = Some(cand);
                best_unblocks = unblocks;
            }
        }

        // Go selects the best candidate unconditionally once `remaining` is
        // non-empty (bestGain starts at -1), even when its gain is 0.
        let node = match best_node {
            Some(node) => node,
            None => break,
        };
        selected.push(TopKSetItem {
            node,
            marginal_gain: best_gain,
            unblocked_ids: best_unblocks,
        });
        // Mark ONLY the selected node as complete. Go inserts just
        // `completed[bestID]` — newly actionable nodes remain open and are
        // re-evaluated as candidates on the next iteration.
        current_closed[node] = true;
        total_gain += best_gain;
    }

    TopKSetResult {
        items: selected,
        total_gain,
        open_nodes,
    }
}

/// Greedy TopK Set with default limit of 5.
pub fn topk_set_default(graph: &DiGraph, closed_set: &[bool]) -> TopKSetResult {
    topk_set(graph, closed_set, 5)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_graph(edges: &[(usize, usize)]) -> DiGraph {
        let mut g = DiGraph::new();
        let max_node = edges.iter().flat_map(|(a, b)| [*a, *b]).max().unwrap_or(0);
        for i in 0..=max_node {
            g.add_node(&format!("n{}", i));
        }
        for (from, to) in edges {
            g.add_edge(*from, *to);
        }
        g
    }

    #[test]
    fn test_empty_graph() {
        let g = DiGraph::new();
        let result = topk_set(&g, &[], 5);
        assert!(result.items.is_empty());
        assert_eq!(result.total_gain, 0);
    }

    #[test]
    fn test_single_node() {
        let mut g = DiGraph::new();
        g.add_node("a");
        let result = topk_set(&g, &[false], 5);
        // Go selects the only actionable candidate even at gain 0
        // (bestGain starts at -1; there is no positive-gain gate).
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].node, 0);
        assert_eq!(result.items[0].marginal_gain, 0);
        assert_eq!(result.total_gain, 0);
    }

    #[test]
    fn test_simple_chain() {
        // 0 -> 1 -> 2 -> 3
        // Go generateTopKSet picks only ACTIONABLE nodes and counts DIRECT
        // newly-actionable unblocks: each pick gains exactly 1.
        let g = make_graph(&[(0, 1), (1, 2), (2, 3)]);
        let result = topk_set(&g, &[false; 4], 5);

        assert_eq!(result.items.len(), 4);
        assert_eq!(result.items[0].node, 0);
        assert_eq!(result.items[0].marginal_gain, 1);
        assert_eq!(result.items[0].unblocked_ids, vec![1]);
        assert_eq!(result.total_gain, 3);
    }

    #[test]
    fn test_blocked_hub_not_picked_first() {
        // Regression: 0 -> 1, and 1 fans out to 2,3,4. Node 1 has the
        // highest fanout but is BLOCKED by 0 — Go only picks from the
        // actionable set, so 0 must come first.
        let g = make_graph(&[(0, 1), (1, 2), (1, 3), (1, 4)]);
        let result = topk_set(&g, &[false; 5], 5);

        assert_eq!(result.items[0].node, 0);
        assert_eq!(result.items[1].node, 1);
        assert_eq!(result.items[1].marginal_gain, 3);
        assert_eq!(result.items[1].unblocked_ids, vec![2, 3, 4]);
    }

    #[test]
    fn test_fork_pattern() {
        // Hub -> A, B, C (hub unblocks 3 directly)
        let g = make_graph(&[(0, 1), (0, 2), (0, 3)]);
        let result = topk_set(&g, &[false; 4], 5);

        assert_eq!(result.items[0].node, 0); // Hub
        assert_eq!(result.items[0].marginal_gain, 3);
        assert_eq!(result.items[0].unblocked_ids, vec![1, 2, 3]);
        assert_eq!(result.total_gain, 3);
        // After the hub, the three leaves are picked with gain 0 (Go selects
        // unconditionally while candidates remain).
        assert_eq!(result.items.len(), 4);
    }

    #[test]
    fn test_multiple_hubs() {
        // Hub1 -> {A, B}, Hub2 -> {X, Y, Z}
        // Hub2 should be picked first (3 > 2)
        let g = make_graph(&[(0, 1), (0, 2), (3, 4), (3, 5), (3, 6)]);
        let result = topk_set(&g, &[false; 7], 5);

        assert!(result.items.len() >= 2);
        // Node 3 (Hub2) has more impact (3) than Node 0 (Hub1, 2)
        assert_eq!(result.items[0].node, 3);
        assert_eq!(result.items[0].marginal_gain, 3);
        assert_eq!(result.items[1].node, 0);
        assert_eq!(result.items[1].marginal_gain, 2);
        assert_eq!(result.total_gain, 5);
    }

    #[test]
    fn test_submodularity() {
        // Chain: 0 -> 1 -> 2 -> 3 -> 4
        // Each pick directly unblocks exactly one node; Go still fills all
        // k slots while candidates remain (last pick has gain 0).
        let g = make_graph(&[(0, 1), (1, 2), (2, 3), (3, 4)]);
        let result = topk_set(&g, &[false; 5], 5);

        assert_eq!(result.items.len(), 5);
        assert_eq!(result.items[0].node, 0);
        assert_eq!(result.items[0].marginal_gain, 1);
        assert_eq!(result.items[4].marginal_gain, 0);
        assert_eq!(result.total_gain, 4);
    }

    #[test]
    fn test_partially_closed() {
        // 0 -> 2, 1 -> 2, 2 -> 3
        // If 0 is already closed, closing 1 unblocks 2 (then 3)
        let g = make_graph(&[(0, 2), (1, 2), (2, 3)]);
        let closed = vec![true, false, false, false];
        let result = topk_set(&g, &closed, 5);

        assert!(!result.items.is_empty());
        assert_eq!(result.items[0].node, 1);
        // Direct unblocks only: closing 1 makes 2 actionable (3 still waits
        // on 2), so gain is 1 — not the transitive cascade of 2.
        assert_eq!(result.items[0].marginal_gain, 1);
        assert_eq!(result.items[0].unblocked_ids, vec![2]);
    }

    #[test]
    fn test_limit_respected() {
        // Many independent chains
        let g = make_graph(&[(0, 1), (2, 3), (4, 5), (6, 7), (8, 9)]);
        let result = topk_set(&g, &[false; 10], 2);

        assert_eq!(result.items.len(), 2);
    }

    #[test]
    fn test_deterministic() {
        // Same gains - should pick by node index for determinism
        // 0 -> 1, 2 -> 3 (both have gain 1)
        let g = make_graph(&[(0, 1), (2, 3)]);

        // Run multiple times
        for _ in 0..5 {
            let result = topk_set(&g, &[false; 4], 5);
            // Should consistently pick 0 first (lower index)
            assert_eq!(result.items[0].node, 0);
        }
    }

    #[test]
    fn test_monotonic_marginal_gains() {
        // Submodularity: marginal gains should be non-increasing
        // Tree: 0 -> {1, 2, 3}, 1 -> {4, 5}
        let g = make_graph(&[(0, 1), (0, 2), (0, 3), (1, 4), (1, 5)]);
        let result = topk_set(&g, &[false; 6], 5);

        if result.items.len() >= 2 {
            for i in 1..result.items.len() {
                assert!(
                    result.items[i].marginal_gain <= result.items[i - 1].marginal_gain,
                    "Marginal gains should be monotonically non-increasing"
                );
            }
        }
    }

    #[test]
    fn test_open_nodes_count() {
        let g = make_graph(&[(0, 1), (1, 2)]);
        let closed = vec![true, false, false];
        let result = topk_set(&g, &closed, 5);

        // 1 node closed, 2 open
        assert_eq!(result.open_nodes, 2);
    }

    #[test]
    fn test_all_closed() {
        let g = make_graph(&[(0, 1), (1, 2)]);
        let closed = vec![true, true, true];
        let result = topk_set(&g, &closed, 5);

        assert!(result.items.is_empty());
        assert_eq!(result.open_nodes, 0);
    }

    #[test]
    fn test_deep_cascade() {
        // Deep chain: 0 -> 1 -> 2 -> 3 -> 4 -> 5 -> 6 -> 7 -> 8 -> 9
        let mut edges = Vec::new();
        for i in 0..9 {
            edges.push((i, i + 1));
        }
        let g = make_graph(&edges);
        let result = topk_set(&g, &[false; 10], 5);

        assert_eq!(result.items.len(), 5);
        assert_eq!(result.items[0].node, 0);
        // Direct unblocks only: each pick in a chain unlocks exactly one node.
        assert_eq!(result.items[0].marginal_gain, 1);
        assert_eq!(result.items[0].unblocked_ids, vec![1]);
        assert_eq!(result.total_gain, 5);
    }
}
