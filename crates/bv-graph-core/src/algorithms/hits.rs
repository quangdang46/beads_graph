//! HITS (Hyperlink-Induced Topic Search) algorithm.
//!
//! Computes hub and authority scores for nodes.
//! - Hubs: nodes that point to many good authorities
//! - Authorities: nodes pointed to by many good hubs
//!
//! Useful for identifying key "hub" issues that coordinate work
//! and "authority" issues that many others depend on.

use crate::graph::DiGraph;
use serde::Serialize;

/// Configuration for HITS computation.
pub struct HITSConfig {
    /// Convergence tolerance
    pub tolerance: f64,
    /// Maximum iterations
    pub max_iterations: u32,
}

impl Default for HITSConfig {
    fn default() -> Self {
        HITSConfig {
            tolerance: 1e-3,
            max_iterations: 100,
        }
    }
}

/// Result of HITS computation.
#[derive(Serialize)]
pub struct HITSResult {
    /// Hub scores (nodes that point to authorities)
    pub hubs: Vec<f64>,
    /// Authority scores (nodes pointed to by hubs)
    pub authorities: Vec<f64>,
    /// Number of iterations until convergence
    pub iterations: u32,
}

/// Compute HITS hub and authority scores.
///
/// HITS is an iterative algorithm:
/// 1. Authority(v) = sum of Hub(u) for all u → v
/// 2. Hub(u) = sum of Authority(v) for all u → v
/// 3. Normalize both vectors
/// 4. Repeat until convergence
///
/// Byte-compatibility with `gonum/graph/network.HITS` rests on two details
/// that a reader would otherwise "clean up":
///
/// * **The two normalization norms are fused multiply-adds.** See the
///   `mul_add` comments in the loop body; a plain `norm += a * a` puts every
///   score 1 ULP away from Go's.
/// * **`l2_norm` is gonum's scaled `dnrm2`, not a naive sum of squares.**
///   See its own doc comment.
///
/// # Arguments
/// * `graph` - The directed graph
/// * `config` - HITS configuration parameters
///
/// # Returns
/// HITSResult containing hub and authority scores
pub fn hits(graph: &DiGraph, config: &HITSConfig) -> HITSResult {
    let n = graph.len();
    if n == 0 {
        return HITSResult {
            hubs: Vec::new(),
            authorities: Vec::new(),
            iterations: 0,
        };
    }

    // gonum's network.HITS (vendor/.../network/hits.go): both vectors start
    // at 1, each iteration normalizes authority then hub on their own, and
    // the loop ends when the 2-norm of BOTH delta vectors falls below tol.
    // It has no iteration cap; `max_iterations` is kept only as a safety
    // bound for a graph that fails to converge.
    let mut auth = vec![1.0_f64; n];
    let mut hubs = vec![1.0_f64; n];
    let mut delta_auth = vec![0.0_f64; n];
    let mut delta_hub = vec![0.0_f64; n];
    let mut iterations = 0;

    // Zero-cap convention shared with pagerank/eigenvector (`for _ in
    // 0..max_iterations`): a cap of 0 means ZERO iterations and returns the
    // initial vectors — the loop below would otherwise always run once.
    if config.max_iterations == 0 {
        return HITSResult {
            hubs,
            authorities: auth,
            iterations: 0,
        };
    }

    loop {
        iterations += 1;

        // auth(v) = sum over u -> v of hub(u), then normalize.
        let mut norm = 0.0_f64;
        for v in 0..n {
            let mut a = 0.0_f64;
            for &u in graph.predecessors_slice(v) {
                a += hubs[u];
            }
            // gonum stores the previous value, then takes the difference
            // AFTER normalizing (hits.go:63-70) — the delta is against the
            // normalized vector, not the raw one.
            delta_auth[v] = auth[v];
            auth[v] = a;
            // FUSED, deliberately: `norm += a * a` in Go compiles to a single
            // FMADDD on arm64 (`1f410020 FMADDD F1, F0, F1, F0` attributed to
            // hits.go:68 in the shipped bv binary), so Go rounds `a*a + norm`
            // ONCE. A plain `norm += a * a` rounds twice and drifts the
            // normalization by 1 ULP, which propagates into every hub and
            // authority score. Do not "simplify" this back — see the note on
            // this function's doc comment.
            norm = a.mul_add(a, norm);
        }
        norm = norm.sqrt();
        // gonum divides unconditionally (hits.go:73-76). The `norm > 0.0`
        // guard is a deliberate deviation: Go's only caller gates on
        // `Edges().Len() > 0` (pkg/analysis/graph.go:2118), which makes
        // `norm == 0` unreachable there, whereas Rust's `analyzer.rs:867`
        // path has no such gate. Dividing by zero here would emit NaN
        // scores instead of zeros, so the guard is kept.
        if norm > 0.0 {
            for v in 0..n {
                auth[v] /= norm;
                delta_auth[v] -= auth[v];
            }
        }

        // hub(u) = sum over u -> v of auth(v), then normalize.
        let mut norm = 0.0_f64;
        for u in 0..n {
            let mut h = 0.0_f64;
            for &v in graph.successors_slice(u) {
                h += auth[v];
            }
            delta_hub[u] = hubs[u];
            hubs[u] = h;
            // Fused for the same reason as the authority norm above; Go
            // compiles hits.go:85 to `1f410020 FMADDD F1, F0, F1, F0`.
            norm = h.mul_add(h, norm);
        }
        norm = norm.sqrt();
        if norm > 0.0 {
            for u in 0..n {
                hubs[u] /= norm;
                delta_hub[u] -= hubs[u];
            }
        }

        let auth_diff = l2_norm(&delta_auth);
        let hub_diff = l2_norm(&delta_hub);
        if auth_diff < config.tolerance && hub_diff < config.tolerance {
            break;
        }
        if iterations >= config.max_iterations {
            break;
        }
    }

    HITSResult {
        hubs,
        authorities: auth,
        iterations,
    }
}

/// L2 norm, matching gonum's `floats.Norm(s, 2)`.
///
/// `floats.Norm` special-cases `L == 2` to `f64.L2NormUnitary`
/// (gonum v0.17.0 floats/floats.go:604-605). On the build platforms bv
/// targets, `L2NormUnitary` is the pure-Go BLAS `dnrm2` in
/// `internal/asm/f64/l2norm_noasm.go:13-35` — the amd64 assembly is gated
/// behind `!amd64 || noasm || gccgo || safe` and is not used on darwin/arm64.
/// It accumulates a running `scale` (= max |x|) with a rescaled `sumSquares`
/// seeded at 1.0, which is what keeps the sum from overflowing or flushing to
/// zero where a naive `sqrt(sum(x*x))` would.
///
/// This value only ever feeds the `< tol` convergence test — it is never fed
/// back into the scores — so on the frozen goldens it changes no byte. It is
/// kept faithful because the two forms are not equal: on the last iteration
/// of `large_cyclic_600` gonum's scaled form yields
/// 0.00083638227391777039948 where the naive form yields
/// 0.00083638227391776964054. They are both far below the 1e-3 tolerance, so
/// the iteration count is the same.
fn l2_norm(x: &[f64]) -> f64 {
    let mut scale = 0.0_f64;
    let mut sum_squares = 1.0_f64;
    for &v in x {
        if v == 0.0 {
            continue;
        }
        let absxi = v.abs();
        if absxi.is_nan() {
            return f64::NAN;
        }
        if scale < absxi {
            let s = scale / absxi;
            sum_squares = 1.0 + sum_squares * s * s;
            scale = absxi;
        } else {
            let s = absxi / scale;
            sum_squares += s * s;
        }
    }
    if scale == f64::INFINITY {
        return f64::INFINITY;
    }
    scale * sum_squares.sqrt()
}

/// Compute HITS with default parameters.
///
/// Go calls `network.HITS(g, 1e-3)` (pkg/analysis/graph.go:1799), so the
/// default tolerance matches gonum's call site. The iteration body now
/// follows gonum exactly: start at 1, normalize authority and hub
/// separately, and converge on the 2-norm of both delta vectors.
pub fn hits_default(graph: &DiGraph) -> HITSResult {
    hits(graph, &HITSConfig::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hits_empty() {
        let graph = DiGraph::new();
        let result = hits_default(&graph);
        assert!(result.hubs.is_empty());
        assert!(result.authorities.is_empty());
    }

    #[test]
    fn test_hits_single_node() {
        let mut graph = DiGraph::new();
        graph.add_node("a");
        let result = hits_default(&graph);
        assert_eq!(result.hubs.len(), 1);
        assert_eq!(result.authorities.len(), 1);
    }

    #[test]
    fn test_hits_zero_max_iterations() {
        // max_iterations = 0 must run zero iterations (pagerank/eigenvector
        // `0..max` convention), returning the initial all-ones vectors.
        let mut graph = DiGraph::new();
        let a = graph.add_node("a");
        let b = graph.add_node("b");
        graph.add_edge(a, b);

        let config = HITSConfig {
            tolerance: 1e-3,
            max_iterations: 0,
        };
        let result = hits(&graph, &config);
        assert_eq!(result.iterations, 0);
        assert!(result.hubs.iter().all(|&v| v == 1.0));
        assert!(result.authorities.iter().all(|&v| v == 1.0));
    }

    #[test]
    fn test_hits_chain() {
        // a -> b -> c
        // a is a hub (points to things)
        // c is an authority (pointed to)
        let mut graph = DiGraph::new();
        let a = graph.add_node("a");
        let b = graph.add_node("b");
        let c = graph.add_node("c");
        graph.add_edge(a, b);
        graph.add_edge(b, c);

        let result = hits_default(&graph);

        // In a chain: a has highest hub (points to b), c has highest authority (final target)
        // b is in the middle, moderate both
        assert!(
            result.authorities[c] > result.authorities[a],
            "c should have higher authority"
        );
        assert!(result.hubs[a] > result.hubs[c], "a should have higher hub");
    }

    #[test]
    fn test_hits_star_hub() {
        // hub -> a, hub -> b, hub -> c
        // hub is a pure hub, a/b/c are pure authorities
        let mut graph = DiGraph::new();
        let hub = graph.add_node("hub");
        let a = graph.add_node("a");
        let b = graph.add_node("b");
        let c = graph.add_node("c");
        graph.add_edge(hub, a);
        graph.add_edge(hub, b);
        graph.add_edge(hub, c);

        let result = hits_default(&graph);

        // Hub should have high hub score
        assert!(
            result.hubs[hub] > result.hubs[a],
            "hub should have higher hub score"
        );
        // a, b, c should have equal authority scores
        let auth_diff = (result.authorities[a] - result.authorities[b]).abs()
            + (result.authorities[b] - result.authorities[c]).abs();
        assert!(auth_diff < 0.01, "a, b, c should have equal authority");
    }

    #[test]
    fn test_hits_star_authority() {
        // a -> auth, b -> auth, c -> auth
        // auth is a pure authority, a/b/c are pure hubs
        let mut graph = DiGraph::new();
        let a = graph.add_node("a");
        let b = graph.add_node("b");
        let c = graph.add_node("c");
        let auth = graph.add_node("auth");
        graph.add_edge(a, auth);
        graph.add_edge(b, auth);
        graph.add_edge(c, auth);

        let result = hits_default(&graph);

        // auth should have highest authority score
        assert!(
            result.authorities[auth] > result.authorities[a],
            "auth should have higher authority"
        );
        // a, b, c should have equal hub scores
        let hub_diff =
            (result.hubs[a] - result.hubs[b]).abs() + (result.hubs[b] - result.hubs[c]).abs();
        assert!(hub_diff < 0.01, "a, b, c should have equal hub scores");
    }

    #[test]
    fn test_hits_bipartite() {
        // Hubs: h1, h2 -> Authorities: a1, a2
        // h1 -> a1, h1 -> a2
        // h2 -> a1, h2 -> a2
        let mut graph = DiGraph::new();
        let h1 = graph.add_node("h1");
        let h2 = graph.add_node("h2");
        let a1 = graph.add_node("a1");
        let a2 = graph.add_node("a2");
        graph.add_edge(h1, a1);
        graph.add_edge(h1, a2);
        graph.add_edge(h2, a1);
        graph.add_edge(h2, a2);

        let result = hits_default(&graph);

        // h1, h2 should have high hub scores
        // a1, a2 should have high authority scores
        assert!(result.hubs[h1] > result.authorities[h1]);
        assert!(result.hubs[h2] > result.authorities[h2]);
        assert!(result.authorities[a1] > result.hubs[a1]);
        assert!(result.authorities[a2] > result.hubs[a2]);
    }

    #[test]
    fn test_hits_cycle() {
        // a -> b -> c -> a
        let mut graph = DiGraph::new();
        let a = graph.add_node("a");
        let b = graph.add_node("b");
        let c = graph.add_node("c");
        graph.add_edge(a, b);
        graph.add_edge(b, c);
        graph.add_edge(c, a);

        let result = hits_default(&graph);

        // In a symmetric cycle, all nodes should have similar scores
        let hub_diff =
            (result.hubs[a] - result.hubs[b]).abs() + (result.hubs[b] - result.hubs[c]).abs();
        let auth_diff = (result.authorities[a] - result.authorities[b]).abs()
            + (result.authorities[b] - result.authorities[c]).abs();
        assert!(
            hub_diff < 0.01,
            "Cycle nodes should have similar hub scores"
        );
        assert!(
            auth_diff < 0.01,
            "Cycle nodes should have similar authority scores"
        );
    }

    #[test]
    fn test_hits_scores_normalized() {
        let mut graph = DiGraph::new();
        let a = graph.add_node("a");
        let b = graph.add_node("b");
        let c = graph.add_node("c");
        graph.add_edge(a, b);
        graph.add_edge(b, c);
        graph.add_edge(c, a);

        let result = hits_default(&graph);

        // L2 norm should be 1
        let hub_norm: f64 = result.hubs.iter().map(|v| v * v).sum::<f64>().sqrt();
        let auth_norm: f64 = result.authorities.iter().map(|v| v * v).sum::<f64>().sqrt();

        assert!(
            (hub_norm - 1.0).abs() < 0.001,
            "Hub scores should have unit L2 norm"
        );
        assert!(
            (auth_norm - 1.0).abs() < 0.001,
            "Authority scores should have unit L2 norm"
        );
    }

    #[test]
    fn test_hits_convergence() {
        // Create a non-trivial graph
        let mut graph = DiGraph::new();
        for i in 0..10 {
            graph.add_node(&format!("n{}", i));
        }
        // Add various edges
        for i in 0..5 {
            for j in 5..10 {
                graph.add_edge(i, j);
            }
        }

        let result = hits_default(&graph);

        // Should converge within max_iterations
        assert!(result.iterations <= 100);
        // Hubs (0-4) should have higher hub scores
        // Authorities (5-9) should have higher authority scores
        let avg_hub_hub: f64 = (0..5).map(|i| result.hubs[i]).sum::<f64>() / 5.0;
        let avg_auth_hub: f64 = (5..10).map(|i| result.hubs[i]).sum::<f64>() / 5.0;
        assert!(
            avg_hub_hub > avg_auth_hub,
            "Hub nodes should have higher hub scores"
        );
    }

    /// Fills `v` with a deterministic pseudo-random spread of magnitudes in
    /// (0, 1) — a stand-in for one `delta` vector of a converged HITS run.
    fn pseudo_delta(seed: u64, n: usize) -> Vec<f64> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let bits = (s >> 11) as f64 / (1u64 << 53) as f64;
                0.25 + 0.75 * bits
            })
            .collect()
    }

    /// The normalization norm is a fused multiply-add, matching the FMADDD the
    /// Go toolchain emits for `norm += a * a` (gonum network/hits.go:68 and
    /// :85). Dropping the fusion shifts the result by 1 ULP — on the frozen
    /// `large_cyclic_600` corpus that moved 47/50 hub scores and 24/50
    /// authority scores off the golden, and flipped two adjacent pairs in
    /// the emitted order. This test fails if anyone "simplifies" the mul_add.
    #[test]
    fn test_hits_norm_accumulation_is_fused() {
        // A hub/hub graph with uneven degrees so the per-node sums differ in
        // magnitude and the two rounding modes visibly disagree.
        let mut graph = DiGraph::new();
        for i in 0..24 {
            graph.add_node(&format!("n{}", i));
        }
        // Sources 0..6 fan out to sinks 12..24 with a ragged pattern.
        for i in 0..7 {
            for j in 12..24 {
                if (i * 7 + j * 3) % 5 != 0 {
                    graph.add_edge(i, j);
                }
            }
        }
        // One mid chain so authorities feed back into hubs.
        for j in 12..18 {
            graph.add_edge(7, j);
        }
        for i in 18..24 {
            graph.add_edge(i, 8);
        }

        let fused = hits_default(&graph);

        // Reference: the same loop with the naive (twice-rounded) norm, which
        // is what the port looked like before the mul_add was restored.
        let mut auth = [1.0_f64; 24];
        let mut hubs = [1.0_f64; 24];
        let mut delta_auth = [0.0_f64; 24];
        let mut delta_hub = [0.0_f64; 24];
        let mut iterations = 0;
        loop {
            iterations += 1;
            let mut norm = 0.0_f64;
            for v in 0..24 {
                let mut a = 0.0_f64;
                for &u in graph.predecessors_slice(v) {
                    a += hubs[u];
                }
                delta_auth[v] = auth[v];
                auth[v] = a;
                norm += a * a; // NOT fused — deliberately the old form
            }
            norm = norm.sqrt();
            if norm > 0.0 {
                for v in 0..24 {
                    auth[v] /= norm;
                    delta_auth[v] -= auth[v];
                }
            }
            let mut norm = 0.0_f64;
            for u in 0..24 {
                let mut h = 0.0_f64;
                for &v in graph.successors_slice(u) {
                    h += auth[v];
                }
                delta_hub[u] = hubs[u];
                hubs[u] = h;
                norm += h * h; // NOT fused — deliberately the old form
            }
            norm = norm.sqrt();
            if norm > 0.0 {
                for u in 0..24 {
                    hubs[u] /= norm;
                    delta_hub[u] -= hubs[u];
                }
            }
            if l2_norm(&delta_auth) < 1e-3 && l2_norm(&delta_hub) < 1e-3 {
                break;
            }
            if iterations >= 100 {
                break;
            }
        }

        assert_eq!(
            fused.iterations, iterations,
            "fused and unfused runs must take the same number of iterations \
             (the norm choice only gates the convergence test)"
        );

        let differs = fused
            .hubs
            .iter()
            .zip(&hubs)
            .filter(|(a, b)| a.to_bits() != b.to_bits())
            .count();
        assert_ne!(
            differs, 0,
            "fused and unfused norm accumulation produced bit-identical \
             scores on this graph, so the test no longer proves the mul_add \
             is load-bearing — pick a graph where the two disagree"
        );
    }

    /// `l2_norm` is gonum's scaled dnrm2: a running `scale` with a rescaled
    /// `sumSquares` seeded at 1.0. It must agree with the naive form to
    /// within a few ULP on well-conditioned input, and it must not overflow
    /// or underflow where the naive form would.
    #[test]
    fn test_l2_norm_matches_naive_within_ulp() {
        let v = pseudo_delta(0x5eed_1234, 512);
        let naive = v.iter().map(|x| x * x).sum::<f64>().sqrt();
        let scaled = l2_norm(&v);
        assert!(
            (naive - scaled).abs() <= 4.0 * scaled * f64::EPSILON,
            "scaled {scaled:e} vs naive {naive:e} drift by more than a few ULP"
        );
    }

    #[test]
    fn test_l2_norm_specials() {
        assert_eq!(l2_norm(&[]), 0.0, "empty vector: scale=0, 0*sqrt(1)=0");
        assert_eq!(l2_norm(&[0.0, 0.0]), 0.0);
        assert_eq!(l2_norm(&[3.0, 4.0]), 5.0);
        assert_eq!(l2_norm(&[-3.0, 4.0]), 5.0);
        assert!(l2_norm(&[f64::NAN, 1.0]).is_nan());
        assert_eq!(l2_norm(&[f64::INFINITY, 1.0]), f64::INFINITY);
    }

    /// The naive `sqrt(sum(x*x))` overflows to infinity once `sum(x*x)`
    /// exceeds f64::MAX; the scaled form does not. (It is a convergence-test
    /// input, so this cannot corrupt scores — it proves the port is the
    /// overflow-safe routine gonum actually uses.)
    #[test]
    fn test_l2_norm_does_not_overflow() {
        let v = vec![1e200_f64; 4];
        assert!(v.iter().map(|x| x * x).sum::<f64>().is_infinite());
        assert_eq!(l2_norm(&v), 2e200_f64);
    }
}
