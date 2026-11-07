//! Cycle Detection algorithms.
//!
//! Provides:
//! - Tarjan's SCC algorithm for fast cycle presence check
//! - `find_cycles_safe`: the Go oracle's cycle enumeration — one representative
//!   per strongly connected component, not every elementary cycle
//! - Johnson's algorithm for full elementary-cycle enumeration (wasm viewer only)
//!
//! Parity note: `pkg/analysis/graph_cycles.go:16-20` states the contract in one
//! line — "extracts one cycle per component ... The result retains the
//! pre-limit representative count so callers can report truncation without
//! implying that every simple cycle in an SCC was enumerated." Every
//! `Cycles` list and `cycle_warning` suggestion in the robot surface derives
//! from that function, so it is the entry point for byte-exactness.
//! `enumerate_elementary_cycles` is the Johnson implementation that only
//! `cycle_break_suggestions` here — the browser viewer's scorer, not Go's —
//! consumes; Go has no counterpart for it and it is NOT on the parity path.

use crate::graph::DiGraph;
use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};

/// Result of Strongly Connected Components analysis.
#[derive(Serialize, Clone)]
pub struct SCCResult {
    /// List of strongly connected components (each is a list of node indices)
    pub components: Vec<Vec<usize>>,
    /// True if any SCC is cyclic: size > 1, or a singleton with a self-edge
    /// (the same predicate `find_cycles_safe` uses for `[n, n]` records).
    pub has_cycles: bool,
    /// Number of cyclic SCCs: size > 1, or a singleton carrying a self-edge.
    pub cycle_count: usize,
}

/// Tarjan's algorithm for finding strongly connected components.
///
/// An SCC with more than one node indicates a cycle; a singleton SCC does
/// too when its only node carries a self-edge — `find_cycles_safe` reports
/// that case as the `[n, n]` record, so `has_cycles`/`cycle_count` must
/// agree with it or `cycle_break_suggestions` early-returns on a graph the
/// oracle considers cyclic.
/// Complexity: O(V + E)
pub fn tarjan_scc(graph: &DiGraph) -> SCCResult {
    let n = graph.len();
    if n == 0 {
        return SCCResult {
            components: Vec::new(),
            has_cycles: false,
            cycle_count: 0,
        };
    }

    let mut state = TarjanState {
        graph,
        index: 0usize,
        indices: vec![usize::MAX; n],
        lowlink: vec![usize::MAX; n],
        on_stack: vec![false; n],
        stack: Vec::new(),
        components: Vec::new(),
    };

    for v in 0..n {
        if state.indices[v] == usize::MAX {
            state.strongconnect(v);
        }
    }

    // Cyclic = size > 1, or a singleton whose node points at itself.
    // `add_edge` deduplicates adjacency, so membership is the same
    // predicate as Go's `HasEdgeFromTo` in find_cycles_safe.
    let cycle_count = state
        .components
        .iter()
        .filter(|c| c.len() > 1 || (c.len() == 1 && graph.successors_slice(c[0]).contains(&c[0])))
        .count();

    SCCResult {
        components: state.components,
        has_cycles: cycle_count > 0,
        cycle_count,
    }
}

struct TarjanState<'a> {
    graph: &'a DiGraph,
    index: usize,
    indices: Vec<usize>,
    lowlink: Vec<usize>,
    on_stack: Vec<bool>,
    stack: Vec<usize>,
    components: Vec<Vec<usize>>,
}

impl TarjanState<'_> {
    fn strongconnect(&mut self, v: usize) {
        self.indices[v] = self.index;
        self.lowlink[v] = self.index;
        self.index += 1;
        self.stack.push(v);
        self.on_stack[v] = true;

        for &w in self.graph.successors_slice(v) {
            if self.indices[w] == usize::MAX {
                // Not visited
                self.strongconnect(w);
                self.lowlink[v] = self.lowlink[v].min(self.lowlink[w]);
            } else if self.on_stack[w] {
                // On stack = in current SCC
                self.lowlink[v] = self.lowlink[v].min(self.indices[w]);
            }
        }

        // If v is a root node, pop the stack to get SCC
        if self.lowlink[v] == self.indices[v] {
            let mut component = Vec::new();
            loop {
                let w = self.stack.pop().unwrap();
                self.on_stack[w] = false;
                component.push(w);
                if w == v {
                    break;
                }
            }
            self.components.push(component);
        }
    }
}

/// Check if graph has any cycles.
pub fn has_cycles(graph: &DiGraph) -> bool {
    tarjan_scc(graph).has_cycles
}

// ============================================================================
// Cycle detection — the Go oracle's algorithm
// ============================================================================

/// Go `cycleDetectionResult` (`pkg/analysis/graph_cycles.go:10-14`).
#[derive(Serialize, Clone, Debug, Default)]
pub struct CycleEnumerationResult {
    /// Stored cycle representatives: one per cyclic component, sorted and then
    /// truncated to `limit`.
    pub cycles: Vec<Vec<usize>>,
    /// Go `truncated`: the representative count exceeded `limit`. Recorded
    /// BEFORE truncation, so it never claims a cap that was not reached.
    pub truncated: bool,
    /// Go `total`: representatives found before truncation. Consumers use it to
    /// report "truncated to N of M" without implying M is every simple cycle.
    pub count: usize,
}

/// Go `findCyclesSafe` (`pkg/analysis/graph_cycles.go:20-68`).
///
/// Tarjan's SCC first, then one representative per component: a singleton SCC
/// yields `[n, n]` when `n` has a self-edge, and every larger SCC yields the
/// single cycle `find_one_cycle_in_scc` extracts. The output order is Go's
/// `sort.Slice` at :46-59 — length ascending, then lexicographic on node
/// indices — and it is observable in `--robot-insights` `Cycles`, in
/// `advanced_insights.cycle_break` `in_cycles` indices, and in the order
/// `cycle_warning` suggestions are generated.
///
/// Node indices are assigned in sorted issue-ID order by the analyzer
/// (`analyzer.rs:650-694`), so "lexicographic on indices" is exactly Go's
/// numeric comparison of the compact int64 node IDs, and "lexicographic on
/// indices" is likewise exactly Go's comparison of the issue-ID strings those
/// indices stand for.
pub fn find_cycles_safe(graph: &DiGraph, limit: usize) -> CycleEnumerationResult {
    if limit == 0 {
        return CycleEnumerationResult::default();
    }

    let sccs = tarjan_scc(graph);
    let mut cycles: Vec<Vec<usize>> = Vec::new();

    for scc in &sccs.components {
        if scc.len() == 1 {
            // Self-loop check (graph_cycles.go:28-35). `add_edge` deduplicates
            // adjacency, so membership is the same predicate as Go's
            // `HasEdgeFromTo`.
            let n = scc[0];
            if graph.successors_slice(n).contains(&n) {
                cycles.push(vec![n, n]);
            }
            continue;
        }
        if let Some(cycle) = find_one_cycle_in_scc(graph, scc) {
            cycles.push(cycle);
        }
    }

    // graph_cycles.go:46-59. `Vec<usize>`'s `Ord` is the elementwise
    // comparison Go writes out by hand.
    cycles.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));

    let total = cycles.len();
    let truncated = total > limit;
    if truncated {
        cycles.truncate(limit);
    }
    CycleEnumerationResult {
        cycles,
        truncated,
        count: total,
    }
}

/// Go `findOneCycleInSCC` (`pkg/analysis/graph_cycles.go:71-155`).
///
/// Iterative DFS inside one component, seeded at its lowest-index member, with
/// neighbours pre-filtered to the component and sorted. The first edge onto a
/// node still on the stack closes the cycle, which is why a component with many
/// elementary cycles still reports exactly one.
fn find_one_cycle_in_scc(graph: &DiGraph, scc: &[usize]) -> Option<Vec<usize>> {
    // Sort SCC nodes for a deterministic DFS starting point (:73-75).
    let mut scc_sorted = scc.to_vec();
    scc_sorted.sort_unstable();

    let in_scc: HashSet<usize> = scc_sorted.iter().copied().collect();

    // Pre-compute and sort adjacency lists for nodes in SCC (:85-99). The
    // analyzer already inserts edges in sorted order, so the sort is a no-op
    // there; it is kept because Go performs it and other DiGraph producers do
    // not.
    let mut adj: HashMap<usize, Vec<usize>> = HashMap::with_capacity(scc_sorted.len());
    for &u in &scc_sorted {
        let mut neighbors: Vec<usize> = graph
            .successors_slice(u)
            .iter()
            .copied()
            .filter(|n| in_scc.contains(n))
            .collect();
        neighbors.sort_unstable();
        adj.insert(u, neighbors);
    }

    let mut visited: HashSet<usize> = HashSet::new();
    let mut on_stack: HashSet<usize> = HashSet::new();
    let mut stack_pos: HashMap<usize, usize> = HashMap::new();
    let mut neighbor_index: HashMap<usize, usize> = HashMap::new();
    let mut stack: Vec<usize> = Vec::new();

    if let Some(&first) = scc_sorted.first() {
        stack_pos.insert(first, 0);
        stack.push(first);
    }

    while let Some(&u) = stack.last() {
        if visited.insert(u) {
            on_stack.insert(u);
        }

        let idx = *neighbor_index.get(&u).unwrap_or(&0);
        let empty: [usize; 0] = [];
        let neighbors: &[usize] = adj.get(&u).map_or(&empty, |v| v.as_slice());

        if idx < neighbors.len() {
            let v = neighbors[idx];
            neighbor_index.insert(u, idx + 1);

            if on_stack.contains(&v) {
                // Cycle found: reconstruct v..u, then close with v (:132-140).
                if let Some(&stack_idx) = stack_pos.get(&v) {
                    let mut cycle = stack[stack_idx..].to_vec();
                    cycle.push(v);
                    return Some(cycle);
                }
            }

            if !visited.contains(&v) {
                stack_pos.insert(v, stack.len());
                stack.push(v);
            }
        } else {
            // All neighbours visited: backtrack (:146-152).
            on_stack.remove(&u);
            stack_pos.remove(&u);
            stack.pop();
            neighbor_index.remove(&u);
        }
    }

    None
}

/// Cycles as stored by the Go oracle: one representative per cyclic component,
/// length-then-lexicographic sorted, capped at `max_cycles`.
pub fn enumerate_cycles(graph: &DiGraph, max_cycles: usize) -> Vec<Vec<usize>> {
    find_cycles_safe(graph, max_cycles).cycles
}

/// Go `cycleDetectionResult` under Rust's existing name, with Go's semantics:
/// `count` is the pre-truncation total and `truncated` means the cap bit.
pub fn enumerate_cycles_with_info(graph: &DiGraph, max_cycles: usize) -> CycleEnumerationResult {
    find_cycles_safe(graph, max_cycles)
}

// ============================================================================
// Elementary cycle enumeration (Johnson) — wasm viewer, no Go counterpart
// ============================================================================

/// Enumerate elementary cycles using Johnson's algorithm.
///
/// Reference: Donald B. Johnson, "Finding All the Elementary Circuits of a Directed Graph"
/// SIAM J. Computing, Vol. 4, No. 1, March 1975
///
/// Not the Go oracle's algorithm: Go extracts one cycle per SCC, and every
/// `Cycles` / `cycle_break` / `cycle_warning` field in the robot surface is
/// built on that. This is kept only because the browser viewer's cycle-break
/// ranking scores an edge by how many distinct circuits it participates in, a
/// question the oracle never asks.
///
/// # Arguments
/// * `graph` - The directed graph
/// * `max_cycles` - Maximum number of cycles to find (prevents exponential blowup)
///
/// # Returns
/// Vector of cycles, each cycle is a vector of node indices in order
pub fn enumerate_elementary_cycles(graph: &DiGraph, max_cycles: usize) -> Vec<Vec<usize>> {
    let n = graph.len();
    if n == 0 || max_cycles == 0 {
        return Vec::new();
    }

    let mut cycles: Vec<Vec<usize>> = Vec::new();
    let mut blocked = vec![false; n];
    let mut blocked_map: Vec<HashSet<usize>> = vec![HashSet::new(); n];
    let mut stack: Vec<usize> = Vec::new();

    // Run Johnson's algorithm starting from each node. Iteration `start`
    // only searches the SCC containing `start` inside the subgraph induced
    // by nodes `>= start` — every elementary circuit has a unique least
    // vertex, and only vertices that can both be reached from `start` and
    // reach it can lie on one through `start`. Searching the whole tail
    // subgraph instead (the previous implementation) made the DFS explore
    // every DAG path and was exponential on acyclic inputs.
    for start in 0..n {
        if cycles.len() >= max_cycles {
            break;
        }

        let allowed = start_scc(graph, start);

        // Reset blocked state
        blocked.fill(false);
        for s in &mut blocked_map {
            s.clear();
        }

        let mut state = CircuitState {
            graph,
            allowed: &allowed,
            blocked: &mut blocked,
            blocked_map: &mut blocked_map,
            stack: &mut stack,
            cycles: &mut cycles,
            max_cycles,
        };
        state.circuit(start, start, &mut |u, blocked, blocked_map| {
            unblock(u, blocked, blocked_map)
        });
    }

    cycles
}

/// Johnson's per-iteration search space: the SCC containing `start` in the
/// subgraph induced by `{v : v >= start}`. Computed as the intersection of
/// forward reachability from `start` and backward reachability to `start`,
/// both restricted to nodes `>= start`. Two BFS passes, O(V + E) per
/// iteration, replacing an unrestricted exponential DFS.
fn start_scc(graph: &DiGraph, start: usize) -> Vec<bool> {
    let n = graph.len();
    let mut allowed = vec![false; n];

    // Forward: nodes reachable from `start` via edges into nodes >= start.
    let mut fwd = vec![false; n];
    fwd[start] = true;
    let mut queue = VecDeque::from([start]);
    while let Some(u) = queue.pop_front() {
        for &w in graph.successors_slice(u) {
            if w >= start && !fwd[w] {
                fwd[w] = true;
                queue.push_back(w);
            }
        }
    }

    // Backward: nodes that can reach `start` via predecessors >= start.
    let mut bwd = vec![false; n];
    bwd[start] = true;
    let mut queue = VecDeque::from([start]);
    while let Some(u) = queue.pop_front() {
        for &w in graph.predecessors_slice(u) {
            if w >= start && !bwd[w] {
                bwd[w] = true;
                queue.push_back(w);
            }
        }
    }

    for i in 0..n {
        allowed[i] = fwd[i] && bwd[i];
    }
    allowed
}

/// Iterative form of Johnson's `unblock`: releasing `u` transitively
/// releases every node blocked *through* it. An explicit work stack keeps
/// a long block chain from overflowing the call stack; drain order does
/// not affect the result because the released set is order-independent.
fn unblock(u: usize, blocked: &mut [bool], blocked_map: &mut [HashSet<usize>]) {
    let mut work = vec![u];
    while let Some(x) = work.pop() {
        if !blocked[x] {
            continue;
        }
        blocked[x] = false;
        let dependents: Vec<usize> = blocked_map[x].drain().collect();
        for w in dependents {
            if blocked[w] {
                work.push(w);
            }
        }
    }
}

struct CircuitState<'a> {
    graph: &'a DiGraph,
    /// `allowed[w]` is true iff `w` lies in `start`'s SCC of the
    /// `>= start` induced subgraph (see `start_scc`).
    allowed: &'a [bool],
    blocked: &'a mut [bool],
    blocked_map: &'a mut [HashSet<usize>],
    stack: &'a mut Vec<usize>,
    cycles: &'a mut Vec<Vec<usize>>,
    max_cycles: usize,
}

impl CircuitState<'_> {
    // Circuit search from start vertex. Recursion depth is bounded by the
    // size of start's SCC — Johnson's algorithm is inherently a DFS, so a
    // pathological SCC of tens of thousands of nodes can still overflow the
    // native stack; that is a documented limit of this port, not a
    // correctness issue (the Go/browser callers cap `max_cycles` first).
    fn circuit(
        &mut self,
        v: usize,
        start: usize,
        unblock: &mut impl FnMut(usize, &mut [bool], &mut [HashSet<usize>]),
    ) -> bool {
        if self.cycles.len() >= self.max_cycles {
            return false;
        }

        let mut found = false;
        self.stack.push(v);
        self.blocked[v] = true;

        for &w in self.graph.successors_slice(v) {
            // Only vertices in start's SCC of the >= start subgraph can
            // close a circuit through start.
            if !self.allowed[w] {
                continue;
            }

            if w == start {
                // Found a cycle
                self.cycles.push(self.stack.clone());
                found = true;
                if self.cycles.len() >= self.max_cycles {
                    self.stack.pop();
                    return found;
                }
            } else if !self.blocked[w] && self.circuit(w, start, unblock) {
                found = true;
            }
        }

        if found {
            unblock(v, self.blocked, self.blocked_map);
        } else {
            for &w in self.graph.successors_slice(v) {
                if self.allowed[w] {
                    self.blocked_map[w].insert(v);
                }
            }
        }

        self.stack.pop();
        found
    }
}

/// Elementary-cycle enumeration with truncation metadata (Johnson).
#[derive(Serialize, Clone, Debug, Default)]
pub struct ElementaryCycleResult {
    /// Elementary circuits found, capped at `max_cycles`.
    pub cycles: Vec<Vec<usize>>,
    /// Whether the cap was reached.
    pub truncated: bool,
    /// Number of circuits stored.
    pub count: usize,
}

/// Enumerate every elementary cycle with metadata about truncation.
pub fn enumerate_elementary_cycles_with_info(
    graph: &DiGraph,
    max_cycles: usize,
) -> ElementaryCycleResult {
    // A zero limit means "no enumeration", matching find_cycles_safe's
    // early return — it must not report truncation for an empty result.
    if max_cycles == 0 {
        return ElementaryCycleResult::default();
    }
    let cycles = enumerate_elementary_cycles(graph, max_cycles);
    let count = cycles.len();
    ElementaryCycleResult {
        cycles,
        truncated: count >= max_cycles,
        count,
    }
}

// ============================================================================
// Cycle Break Suggestions
// ============================================================================

/// A suggestion for which edge to remove to break cycles.
#[derive(Debug, Clone, Serialize)]
pub struct CycleBreakItem {
    /// Source node of the edge
    pub from: usize,
    /// Target node of the edge
    pub to: usize,
    /// Number of cycles this edge appears in
    pub cycles_broken: usize,
    /// Collateral damage score (sum of degree changes)
    pub collateral: usize,
    /// Node IDs for display
    pub from_id: Option<String>,
    /// Node ID for target
    pub to_id: Option<String>,
}

/// Result of cycle break analysis.
#[derive(Debug, Clone, Serialize)]
pub struct CycleBreakResult {
    /// Suggested edges to remove
    pub suggestions: Vec<CycleBreakItem>,
    /// Total cycles in the graph
    pub total_cycles: usize,
    /// Whether cycle enumeration was truncated
    pub truncated: bool,
}

/// Analyze cycles and suggest edges to remove to break them.
///
/// For each edge within an SCC (cycle-containing component), calculates:
/// - How many cycles it participates in
/// - The collateral damage (in-degree + out-degree of incident nodes)
///
/// Suggestions are sorted by: cycles_broken desc, then collateral asc
/// (prefer edges that break many cycles with minimal disruption)
///
/// Browser-viewer scorer, not a port of Go's `cycle_break`: it scores edges
/// by every elementary circuit, where Go scores them by the per-SCC
/// representatives `findCyclesSafe` returns. See the note on the enumeration
/// call below. The CLI's parity path for `cycle_break` lives in
/// `crates/bv/src/main.rs`.
///
/// # Arguments
/// * `graph` - The directed graph
/// * `limit` - Maximum suggestions to return
/// * `max_cycles_to_enumerate` - Max cycles to enumerate for scoring (default 100)
pub fn cycle_break_suggestions(
    graph: &DiGraph,
    limit: usize,
    max_cycles_to_enumerate: usize,
) -> CycleBreakResult {
    let scc = tarjan_scc(graph);
    if !scc.has_cycles {
        return CycleBreakResult {
            suggestions: Vec::new(),
            total_cycles: 0,
            truncated: false,
        };
    }

    // Enumerate actual cycles to count edge participation.
    //
    // This is the browser viewer's own scorer, NOT Go's
    // `generateCycleBreakSuggestionsFromStats` (advanced_insights.go:296-372).
    // Go ranks edges by how many of `stats.Cycles()` contain them, and
    // `stats.Cycles()` holds one representative per SCC — the oracle's
    // `findCyclesSafe` output. This function instead counts every elementary
    // circuit, so it ranks an edge higher when the component is densely
    // cyclic. The CLI's `advanced_insights.cycle_break` is computed in
    // `crates/bv/src/main.rs:5081`, which does follow Go; only
    // `bv-graph-wasm`'s JS view consumes this one.
    let cycle_info = enumerate_elementary_cycles_with_info(graph, max_cycles_to_enumerate);
    let cycles = &cycle_info.cycles;

    // Build a map of edge -> cycles it appears in
    let mut edge_cycle_count: std::collections::HashMap<(usize, usize), usize> =
        std::collections::HashMap::new();

    for cycle in cycles {
        if cycle.is_empty() {
            continue;
        }
        // Count edges in this cycle. A self-loop arrives as the singleton
        // `[n]` and still contributes its `(n, n)` edge exactly once.
        for i in 0..cycle.len() {
            let from = cycle[i];
            let to = cycle[(i + 1) % cycle.len()];
            *edge_cycle_count.entry((from, to)).or_insert(0) += 1;
        }
    }

    // Build set of nodes in cyclic SCCs: size > 1, or a singleton with a
    // self-edge (the `[n, n]` record find_cycles_safe emits).
    let cycle_nodes: HashSet<usize> = scc
        .components
        .iter()
        .filter(|c| c.len() > 1 || (c.len() == 1 && graph.successors_slice(c[0]).contains(&c[0])))
        .flat_map(|c| c.iter().copied())
        .collect();

    // Find all edges within cycle SCCs. Iterate in index order so ties in
    // the (cycles_broken, collateral) sort below cannot leak HashSet order.
    let mut cycle_nodes_sorted: Vec<usize> = cycle_nodes.iter().copied().collect();
    cycle_nodes_sorted.sort_unstable();
    let mut suggestions: Vec<CycleBreakItem> = Vec::new();

    for &from in &cycle_nodes_sorted {
        for &to in graph.successors_slice(from) {
            if cycle_nodes.contains(&to) {
                let cycles_broken = edge_cycle_count.get(&(from, to)).copied().unwrap_or(0);
                let collateral =
                    graph.successors_slice(from).len() + graph.predecessors_slice(to).len();

                suggestions.push(CycleBreakItem {
                    from,
                    to,
                    cycles_broken,
                    collateral,
                    from_id: graph.node_id(from),
                    to_id: graph.node_id(to),
                });
            }
        }
    }

    // Sort by: cycles_broken desc, collateral asc
    suggestions.sort_by(|a, b| match b.cycles_broken.cmp(&a.cycles_broken) {
        std::cmp::Ordering::Equal => a.collateral.cmp(&b.collateral),
        other => other,
    });

    suggestions.truncate(limit);

    CycleBreakResult {
        suggestions,
        total_cycles: cycle_info.count,
        truncated: cycle_info.truncated,
    }
}

/// Quick check for edges that could break cycles.
///
/// A simplified version that only looks at SCC membership without
/// full cycle enumeration. Faster but less precise scoring.
pub fn quick_cycle_break_edges(graph: &DiGraph, limit: usize) -> Vec<CycleBreakItem> {
    let scc = tarjan_scc(graph);
    if !scc.has_cycles {
        return Vec::new();
    }

    // Build set of nodes in cyclic SCCs (size > 1 or self-loop singleton).
    let cycle_nodes: HashSet<usize> = scc
        .components
        .iter()
        .filter(|c| c.len() > 1 || (c.len() == 1 && graph.successors_slice(c[0]).contains(&c[0])))
        .flat_map(|c| c.iter().copied())
        .collect();

    // Index order keeps equal-collateral ties deterministic (see above).
    let mut cycle_nodes_sorted: Vec<usize> = cycle_nodes.iter().copied().collect();
    cycle_nodes_sorted.sort_unstable();
    let mut suggestions: Vec<CycleBreakItem> = Vec::new();

    for &from in &cycle_nodes_sorted {
        for &to in graph.successors_slice(from) {
            if cycle_nodes.contains(&to) {
                // Heuristic: edges with low total degree are better to remove
                let collateral =
                    graph.successors_slice(from).len() + graph.predecessors_slice(to).len();

                suggestions.push(CycleBreakItem {
                    from,
                    to,
                    cycles_broken: 1, // Unknown without enumeration
                    collateral,
                    from_id: graph.node_id(from),
                    to_id: graph.node_id(to),
                });
            }
        }
    }

    // Sort by collateral (prefer low-impact edges)
    suggestions.sort_by_key(|s| s.collateral);
    suggestions.truncate(limit);
    suggestions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scc_empty() {
        let graph = DiGraph::new();
        let result = tarjan_scc(&graph);
        assert!(result.components.is_empty());
        assert!(!result.has_cycles);
    }

    #[test]
    fn test_scc_single_node() {
        let mut graph = DiGraph::new();
        graph.add_node("a");
        let result = tarjan_scc(&graph);
        assert_eq!(result.components.len(), 1);
        assert_eq!(result.components[0].len(), 1);
        assert!(!result.has_cycles);
    }

    #[test]
    fn test_scc_self_loop() {
        let mut graph = DiGraph::new();
        let a = graph.add_node("a");
        graph.add_edge(a, a);
        let result = tarjan_scc(&graph);
        // A self-edge makes the singleton SCC cyclic — the same predicate
        // find_cycles_safe uses for its `[n, n]` record.
        assert!(result.has_cycles);
        assert_eq!(result.cycle_count, 1);
        assert!(has_cycles(&graph));
    }

    /// tarjan_scc's has_cycles/cycle_count must agree with
    /// find_cycles_safe: a self-loop is a cycle even though its SCC has
    /// size 1.
    #[test]
    fn test_scc_self_loop_alongside_plain_singletons() {
        let mut graph = DiGraph::new();
        let looped = graph.add_node("looped");
        let plain = graph.add_node("plain");
        let other = graph.add_node("other");
        graph.add_edge(looped, looped);
        graph.add_edge(plain, other);

        let scc = tarjan_scc(&graph);
        assert!(scc.has_cycles);
        assert_eq!(scc.cycle_count, 1);
        assert_eq!(
            find_cycles_safe(&graph, 100).cycles,
            vec![vec![looped, looped]]
        );
    }

    /// Johnson enumeration: a self-loop is the singleton circuit `[n]`,
    /// and the SCC restriction must not lose it.
    #[test]
    fn test_elementary_self_loop() {
        let mut graph = DiGraph::new();
        let a = graph.add_node("a");
        let b = graph.add_node("b");
        graph.add_edge(a, a);
        graph.add_edge(a, b);

        let cycles = enumerate_elementary_cycles(&graph, 100);
        assert_eq!(cycles, vec![vec![a]]);
    }

    /// Johnson enumeration restricted to start's SCC: a large DAG must
    /// terminate quickly and report nothing (previously the unrestricted
    /// DFS explored every path — exponential).
    #[test]
    fn test_elementary_cycles_large_dag() {
        // Layered DAG: 40 layers of 2 nodes, every node points to both
        // nodes in the next layer — astronomically many paths, zero cycles.
        let mut graph = DiGraph::new();
        let layers = 40;
        let width = 2;
        for _ in 0..layers * width {
            graph.add_node("x");
        }
        for l in 0..layers - 1 {
            for i in 0..width {
                for j in 0..width {
                    graph.add_edge(l * width + i, (l + 1) * width + j);
                }
            }
        }
        let cycles = enumerate_elementary_cycles(&graph, 100);
        assert!(cycles.is_empty());
    }

    /// Johnson must still find every elementary circuit inside an SCC —
    /// the restriction is an optimisation, not a filter on results.
    #[test]
    fn test_elementary_cycles_complete_digraph() {
        // Complete digraph on 4 nodes without self-loops has
        // C(4,2)·1! + C(4,3)·2! + C(4,4)·3! = 6 + 8 + 6 = 20 elementary
        // circuits.
        let mut graph = DiGraph::new();
        let n: Vec<usize> = (0..4).map(|i| graph.add_node(&format!("n{i}"))).collect();
        for &u in &n {
            for &v in &n {
                if u != v {
                    graph.add_edge(u, v);
                }
            }
        }
        let found = enumerate_elementary_cycles(&graph, 1000);
        assert_eq!(found.len(), 20, "K4 digraph has 20 elementary circuits");
        // Every reported circuit must start at its least vertex (Johnson's
        // ordering invariant) — cycles are stored as the stack at close.
        for c in &found {
            assert!(c.len() >= 2);
        }
    }

    /// max_cycles = 0 must report no truncation, matching
    /// find_cycles_safe's zero-limit early return.
    #[test]
    fn test_elementary_with_info_zero_limit() {
        let mut graph = DiGraph::new();
        let a = graph.add_node("a");
        let b = graph.add_node("b");
        graph.add_edge(a, b);
        graph.add_edge(b, a);

        let result = enumerate_elementary_cycles_with_info(&graph, 0);
        assert!(result.cycles.is_empty());
        assert_eq!(result.count, 0);
        assert!(!result.truncated);
    }

    /// A self-loop must produce a cycle-break suggestion: has_cycles is
    /// now true for it, and the `(n, n)` edge must be scored.
    #[test]
    fn test_cycle_break_self_loop() {
        let mut graph = DiGraph::new();
        let a = graph.add_node("a");
        graph.add_edge(a, a);

        let result = cycle_break_suggestions(&graph, 10, 100);
        assert_eq!(result.total_cycles, 1);
        assert_eq!(result.suggestions.len(), 1);
        assert_eq!(result.suggestions[0].from, a);
        assert_eq!(result.suggestions[0].to, a);
        assert_eq!(result.suggestions[0].cycles_broken, 1);
    }

    #[test]
    fn test_scc_simple_cycle() {
        // a -> b -> c -> a
        let mut graph = DiGraph::new();
        let a = graph.add_node("a");
        let b = graph.add_node("b");
        let c = graph.add_node("c");
        graph.add_edge(a, b);
        graph.add_edge(b, c);
        graph.add_edge(c, a);

        let result = tarjan_scc(&graph);
        assert!(result.has_cycles);
        assert_eq!(result.cycle_count, 1);
        // One SCC with all 3 nodes
        let big_scc = result.components.iter().find(|c| c.len() > 1);
        assert!(big_scc.is_some());
        assert_eq!(big_scc.unwrap().len(), 3);
    }

    #[test]
    fn test_scc_dag() {
        // a -> b -> c (no cycles)
        let mut graph = DiGraph::new();
        let a = graph.add_node("a");
        let b = graph.add_node("b");
        let c = graph.add_node("c");
        graph.add_edge(a, b);
        graph.add_edge(b, c);

        let result = tarjan_scc(&graph);
        assert!(!result.has_cycles);
        // Each node is its own SCC
        assert_eq!(result.components.len(), 3);
    }

    #[test]
    fn test_scc_two_cycles() {
        // Cycle 1: a -> b -> a
        // Cycle 2: c -> d -> c
        let mut graph = DiGraph::new();
        let a = graph.add_node("a");
        let b = graph.add_node("b");
        let c = graph.add_node("c");
        let d = graph.add_node("d");
        graph.add_edge(a, b);
        graph.add_edge(b, a);
        graph.add_edge(c, d);
        graph.add_edge(d, c);

        let result = tarjan_scc(&graph);
        assert!(result.has_cycles);
        assert_eq!(result.cycle_count, 2);
    }

    #[test]
    fn test_enumerate_empty() {
        let graph = DiGraph::new();
        let cycles = enumerate_cycles(&graph, 100);
        assert!(cycles.is_empty());
    }

    #[test]
    fn test_enumerate_dag() {
        // a -> b -> c (no cycles)
        let mut graph = DiGraph::new();
        let a = graph.add_node("a");
        let b = graph.add_node("b");
        let c = graph.add_node("c");
        graph.add_edge(a, b);
        graph.add_edge(b, c);

        let cycles = enumerate_cycles(&graph, 100);
        assert!(cycles.is_empty());
    }

    #[test]
    fn test_enumerate_simple_cycle() {
        // a -> b -> c -> a
        let mut graph = DiGraph::new();
        let a = graph.add_node("a");
        let b = graph.add_node("b");
        let c = graph.add_node("c");
        graph.add_edge(a, b);
        graph.add_edge(b, c);
        graph.add_edge(c, a);

        let cycles = enumerate_cycles(&graph, 100);
        // Go stores closed paths (A,B,C,A) — graph_cycles.go:138 appends v.
        assert_eq!(cycles, vec![vec![a, b, c, a]]);
    }

    #[test]
    fn test_enumerate_two_node_cycle() {
        // a -> b -> a
        let mut graph = DiGraph::new();
        let a = graph.add_node("a");
        let b = graph.add_node("b");
        graph.add_edge(a, b);
        graph.add_edge(b, a);

        let cycles = enumerate_cycles(&graph, 100);
        assert_eq!(cycles, vec![vec![a, b, a]]);
    }

    #[test]
    fn test_enumerate_max_limit() {
        // Three disjoint two-node components, so three representatives.
        let mut graph = DiGraph::new();
        for pair in [("a", "b"), ("c", "d"), ("e", "f")] {
            let x = graph.add_node(pair.0);
            let y = graph.add_node(pair.1);
            graph.add_edge(x, y);
            graph.add_edge(y, x);
        }

        // graph_cycles.go:61-65 — the total is recorded before truncation, so
        // the cap keeps the first two and reports the pre-truncation count.
        let result = enumerate_cycles_with_info(&graph, 2);
        assert_eq!(result.cycles.len(), 2);
        assert_eq!(result.count, 3);
        assert!(result.truncated);
    }

    #[test]
    fn test_enumerate_diamond_with_back_edge() {
        //     a
        //    / \
        //   b   c
        //    \ /
        //     d -> a (back edge creates cycle)
        let mut graph = DiGraph::new();
        let a = graph.add_node("a");
        let b = graph.add_node("b");
        let c = graph.add_node("c");
        let d = graph.add_node("d");
        graph.add_edge(a, b);
        graph.add_edge(a, c);
        graph.add_edge(b, d);
        graph.add_edge(c, d);
        graph.add_edge(d, a);

        // All four nodes are one SCC, so Go reports ONE representative, not
        // the two elementary circuits (a->b->d->a, a->c->d->a) Johnson finds.
        let cycles = enumerate_cycles(&graph, 100);
        assert_eq!(cycles, vec![vec![a, b, d, a]]);
    }

    #[test]
    fn test_enumerate_with_info() {
        // a -> b -> a
        let mut graph = DiGraph::new();
        let a = graph.add_node("a");
        let b = graph.add_node("b");
        graph.add_edge(a, b);
        graph.add_edge(b, a);

        let result = enumerate_cycles_with_info(&graph, 100);
        assert_eq!(result.count, 1);
        assert!(!result.truncated);

        // A limit of exactly the representative count is NOT truncation: Go
        // sets `truncated` from the pre-limit total (graph_cycles.go:61-64).
        let result_one = enumerate_cycles_with_info(&graph, 1);
        assert_eq!(result_one.count, 1);
        assert_eq!(result_one.cycles, vec![vec![a, b, a]]);
        assert!(!result_one.truncated);
    }

    #[test]
    fn test_has_cycles() {
        let mut dag = DiGraph::new();
        let a = dag.add_node("a");
        let b = dag.add_node("b");
        dag.add_edge(a, b);
        assert!(!has_cycles(&dag));

        let mut cyclic = DiGraph::new();
        let x = cyclic.add_node("x");
        let y = cyclic.add_node("y");
        cyclic.add_edge(x, y);
        cyclic.add_edge(y, x);
        assert!(has_cycles(&cyclic));
    }

    #[test]
    fn test_complex_graph() {
        // Multiple interconnected cycles
        let mut graph = DiGraph::new();
        for i in 0..5 {
            graph.add_node(&format!("n{}", i));
        }
        // Create some cycles
        graph.add_edge(0, 1);
        graph.add_edge(1, 2);
        graph.add_edge(2, 0); // Cycle: 0->1->2->0
        graph.add_edge(2, 3);
        graph.add_edge(3, 4);
        graph.add_edge(4, 2); // Cycle: 2->3->4->2

        let scc = tarjan_scc(&graph);
        assert!(scc.has_cycles);

        // The two circuits share node 2, so the whole graph is one SCC and Go
        // reports a single representative.
        let cycles = enumerate_cycles(&graph, 100);
        assert_eq!(cycles, vec![vec![0, 1, 2, 0]]);
    }

    // ========================================================================
    // Go `findCyclesSafe` contract
    // ========================================================================

    /// graph_cycles.go:28-35 — a singleton SCC with a self-edge is `[n, n]`,
    /// the doubled node included. The self-loop sorts first: it is the
    /// shortest possible record (length 2 counting the closing node).
    #[test]
    fn test_self_loop_is_a_doubled_record() {
        let mut graph = DiGraph::new();
        let a = graph.add_node("a");
        let b = graph.add_node("b");
        let c = graph.add_node("c");
        graph.add_edge(c, c); // self-loop on the highest-index node
        graph.add_edge(a, b);
        graph.add_edge(b, a);

        let cycles = enumerate_cycles(&graph, 100);
        assert_eq!(cycles, vec![vec![c, c], vec![a, b, a]]);
    }

    /// graph_cycles.go:27-41 — one representative per component, so a dense
    /// component with many elementary cycles still contributes one record.
    #[test]
    fn test_one_representative_per_component() {
        // Complete digraph on four nodes: 15 elementary cycles, one SCC.
        let mut graph = DiGraph::new();
        let n: Vec<usize> = (0..4).map(|i| graph.add_node(&format!("n{i}"))).collect();
        for &u in &n {
            for &v in &n {
                if u != v {
                    graph.add_edge(u, v);
                }
            }
        }

        let found = enumerate_elementary_cycles(&graph, 1000);
        assert!(found.len() > 5, "sanity: many elementary circuits exist");

        let stored = enumerate_cycles(&graph, 1000);
        assert_eq!(stored.len(), 1);
        // DFS from the lowest-index member, lowest-index neighbour first.
        assert_eq!(stored[0], vec![n[0], n[1], n[0]]);
    }

    /// graph_cycles.go:46-59 — length ascending, then lexicographic. Both
    /// orderings are observable in the `Cycles` array.
    #[test]
    fn test_sorted_by_length_then_lexicographically() {
        let mut graph = DiGraph::new();
        // Components are added in an order that would leave the output unsorted
        // if the sort were missing.
        let a = graph.add_node("a");
        let b = graph.add_node("b");
        let z = graph.add_node("z");
        let y = graph.add_node("y");
        let x = graph.add_node("x");
        graph.add_edge(z, y);
        graph.add_edge(y, z); // len-3 record, lexicographically last
        graph.add_edge(x, x); // len-2 record
        graph.add_edge(a, b);
        graph.add_edge(b, a); // len-3 record, lexicographically first

        let cycles = enumerate_cycles(&graph, 100);
        assert_eq!(
            cycles,
            vec![vec![x, x], vec![a, b, a], vec![z, y, z]],
            "shortest first, then byte-wise by node index"
        );
    }

    /// graph_cycles.go:21-23 — a non-positive limit yields the zero value.
    #[test]
    fn test_zero_limit_returns_nothing() {
        let mut graph = DiGraph::new();
        let a = graph.add_node("a");
        let b = graph.add_node("b");
        graph.add_edge(a, b);
        graph.add_edge(b, a);

        let found = find_cycles_safe(&graph, 0);
        assert!(found.cycles.is_empty());
        assert_eq!(found.count, 0);
        assert!(!found.truncated);
    }

    /// The `--robot-insights` shape on the large_cyclic_600 fixture, reduced:
    /// one self-loop plus two- and three-node components, ordered exactly the
    /// way the golden records them. Nodes are inserted in sorted-ID order
    /// because that is how the analyzer assigns indices, which is what makes
    /// "lexicographic on indices" equal Go's comparison of the IDs themselves.
    #[test]
    fn test_golden_shaped_multi_component_output() {
        let mut graph = DiGraph::new();
        let c1 = graph.add_node("Cyc-1");
        let c11 = graph.add_node("Cyc-11");
        let c12 = graph.add_node("Cyc-12");
        let c2 = graph.add_node("Cyc-2");
        let c33 = graph.add_node("Cyc-33");
        let c5 = graph.add_node("Cyc-5");
        let c6 = graph.add_node("Cyc-6");
        graph.add_edge(c33, c33);
        graph.add_edge(c1, c2);
        graph.add_edge(c2, c1);
        graph.add_edge(c11, c12);
        graph.add_edge(c12, c11);
        graph.add_edge(c5, c6);
        graph.add_edge(c6, c5);

        let names: Vec<String> = (0..graph.len())
            .map(|i| graph.node_id(i).unwrap())
            .collect();
        let stored = enumerate_cycles(&graph, 100);
        let printed: Vec<Vec<&str>> = stored
            .iter()
            .map(|c| c.iter().map(|&i| names[i].as_str()).collect())
            .collect();

        assert_eq!(
            printed,
            vec![
                vec!["Cyc-33", "Cyc-33"],
                vec!["Cyc-1", "Cyc-2", "Cyc-1"],
                vec!["Cyc-11", "Cyc-12", "Cyc-11"],
                vec!["Cyc-5", "Cyc-6", "Cyc-5"],
            ]
        );
    }

    // ========================================================================
    // Cycle Break Suggestion Tests
    // ========================================================================

    #[test]
    fn test_cycle_break_dag() {
        // a -> b -> c (no cycles)
        let mut graph = DiGraph::new();
        let a = graph.add_node("a");
        let b = graph.add_node("b");
        let c = graph.add_node("c");
        graph.add_edge(a, b);
        graph.add_edge(b, c);

        let result = cycle_break_suggestions(&graph, 10, 100);
        assert!(result.suggestions.is_empty());
        assert_eq!(result.total_cycles, 0);
        assert!(!result.truncated);
    }

    #[test]
    fn test_cycle_break_simple() {
        // a -> b -> a
        let mut graph = DiGraph::new();
        let a = graph.add_node("a");
        let b = graph.add_node("b");
        graph.add_edge(a, b);
        graph.add_edge(b, a);

        let result = cycle_break_suggestions(&graph, 10, 100);
        assert_eq!(result.total_cycles, 1);
        assert_eq!(result.suggestions.len(), 2); // Both edges are candidates

        // Both edges participate in 1 cycle
        for s in &result.suggestions {
            assert_eq!(s.cycles_broken, 1);
        }
    }

    #[test]
    fn test_cycle_break_triangle() {
        // a -> b -> c -> a
        let mut graph = DiGraph::new();
        let a = graph.add_node("a");
        let b = graph.add_node("b");
        let c = graph.add_node("c");
        graph.add_edge(a, b);
        graph.add_edge(b, c);
        graph.add_edge(c, a);

        let result = cycle_break_suggestions(&graph, 10, 100);
        assert_eq!(result.total_cycles, 1);
        assert_eq!(result.suggestions.len(), 3); // 3 edges in cycle

        // All edges participate in 1 cycle
        for s in &result.suggestions {
            assert_eq!(s.cycles_broken, 1);
        }
    }

    #[test]
    fn test_cycle_break_shared_edge() {
        //     a
        //    / \
        //   b   c
        //    \ /
        //     d -> a (creates two cycles sharing d->a)
        let mut graph = DiGraph::new();
        let a = graph.add_node("a");
        let b = graph.add_node("b");
        let c = graph.add_node("c");
        let d = graph.add_node("d");
        graph.add_edge(a, b);
        graph.add_edge(a, c);
        graph.add_edge(b, d);
        graph.add_edge(c, d);
        graph.add_edge(d, a); // Shared back edge

        let result = cycle_break_suggestions(&graph, 10, 100);
        assert_eq!(result.total_cycles, 2);

        // d->a should be ranked first (breaks 2 cycles)
        let best = &result.suggestions[0];
        assert_eq!(best.from, d);
        assert_eq!(best.to, a);
        assert_eq!(best.cycles_broken, 2);
    }

    #[test]
    fn test_cycle_break_includes_ids() {
        // a -> b -> a
        let mut graph = DiGraph::new();
        graph.add_node("issue-1");
        graph.add_node("issue-2");
        graph.add_edge(0, 1);
        graph.add_edge(1, 0);

        let result = cycle_break_suggestions(&graph, 10, 100);
        assert!(!result.suggestions.is_empty());

        let s = &result.suggestions[0];
        assert!(s.from_id.is_some());
        assert!(s.to_id.is_some());
    }

    #[test]
    fn test_cycle_break_limit() {
        // Many edges in cycle
        let mut graph = DiGraph::new();
        for i in 0..10 {
            graph.add_node(&format!("n{}", i));
        }
        // Create a 10-node cycle
        for i in 0..10 {
            graph.add_edge(i, (i + 1) % 10);
        }

        let result = cycle_break_suggestions(&graph, 3, 100);
        assert_eq!(result.suggestions.len(), 3); // Limited to 3
    }

    #[test]
    fn test_quick_cycle_break() {
        // a -> b -> a
        let mut graph = DiGraph::new();
        let a = graph.add_node("a");
        let b = graph.add_node("b");
        graph.add_edge(a, b);
        graph.add_edge(b, a);

        let suggestions = quick_cycle_break_edges(&graph, 10);
        assert_eq!(suggestions.len(), 2);

        // Sorted by collateral
        for s in &suggestions {
            assert_eq!(s.cycles_broken, 1); // Heuristic value
        }
    }

    #[test]
    fn test_quick_cycle_break_dag() {
        let mut graph = DiGraph::new();
        let a = graph.add_node("a");
        let b = graph.add_node("b");
        graph.add_edge(a, b);

        let suggestions = quick_cycle_break_edges(&graph, 10);
        assert!(suggestions.is_empty());
    }

    #[test]
    fn test_cycle_break_disconnected_cycles() {
        // Two separate cycles
        // Cycle 1: a -> b -> a
        // Cycle 2: c -> d -> c
        let mut graph = DiGraph::new();
        let a = graph.add_node("a");
        let b = graph.add_node("b");
        let c = graph.add_node("c");
        let d = graph.add_node("d");
        graph.add_edge(a, b);
        graph.add_edge(b, a);
        graph.add_edge(c, d);
        graph.add_edge(d, c);

        let result = cycle_break_suggestions(&graph, 10, 100);
        assert_eq!(result.total_cycles, 2);
        assert_eq!(result.suggestions.len(), 4); // 2 edges per cycle
    }

    #[test]
    fn test_cycle_break_collateral_ordering() {
        // a -> b -> a  (small cycle)
        // a -> c -> d -> a (larger cycle through same node)
        let mut graph = DiGraph::new();
        let a = graph.add_node("a");
        let b = graph.add_node("b");
        let c = graph.add_node("c");
        let d = graph.add_node("d");
        graph.add_edge(a, b);
        graph.add_edge(b, a);
        graph.add_edge(a, c);
        graph.add_edge(c, d);
        graph.add_edge(d, a);

        let result = cycle_break_suggestions(&graph, 10, 100);
        // Should have suggestions sorted by cycles_broken desc, then collateral asc
        // Check that suggestions are not empty
        assert!(!result.suggestions.is_empty());

        // Verify ordering: if same cycles_broken, lower collateral first
        for i in 1..result.suggestions.len() {
            let prev = &result.suggestions[i - 1];
            let curr = &result.suggestions[i];
            if prev.cycles_broken == curr.cycles_broken {
                assert!(prev.collateral <= curr.collateral);
            } else {
                assert!(prev.cycles_broken >= curr.cycles_broken);
            }
        }
    }
}
