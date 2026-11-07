//! Hybrid search scorer — port of Go `pkg/search/hybrid_scorer.go` +
//! `presets.go`: text relevance blended with graph metrics.

use serde::Serialize;

/// Preset weight configurations (Go `presets.go` — exact values).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Weights {
    pub text_relevance: f64,
    pub pagerank: f64,
    pub status: f64,
    pub impact: f64,
    pub priority: f64,
    pub recency: f64,
}

pub const PRESETS: &[(&str, Weights)] = &[
    (
        "default",
        Weights {
            text_relevance: 0.40,
            pagerank: 0.20,
            status: 0.15,
            impact: 0.10,
            priority: 0.10,
            recency: 0.05,
        },
    ),
    (
        "bug-hunting",
        Weights {
            text_relevance: 0.30,
            pagerank: 0.15,
            status: 0.15,
            impact: 0.15,
            priority: 0.20,
            recency: 0.05,
        },
    ),
    (
        "sprint-planning",
        Weights {
            text_relevance: 0.30,
            pagerank: 0.20,
            status: 0.25,
            impact: 0.15,
            priority: 0.05,
            recency: 0.05,
        },
    ),
    (
        "impact-first",
        Weights {
            text_relevance: 0.25,
            pagerank: 0.30,
            status: 0.10,
            impact: 0.20,
            priority: 0.10,
            recency: 0.05,
        },
    ),
    (
        "text-only",
        Weights {
            text_relevance: 1.00,
            pagerank: 0.00,
            status: 0.00,
            impact: 0.00,
            priority: 0.00,
            recency: 0.00,
        },
    ),
];

impl Weights {
    /// Go `Weights.Normalize` (weights.go): scale every component by `1/sum`
    /// in Go's field order; a zero sum returns the weights unchanged.
    pub fn normalized(self) -> Self {
        let sum = self.text_relevance
            + self.pagerank
            + self.status
            + self.impact
            + self.priority
            + self.recency;
        if sum == 0.0 {
            return self;
        }
        Weights {
            text_relevance: self.text_relevance / sum,
            pagerank: self.pagerank / sum,
            status: self.status / sum,
            impact: self.impact / sum,
            priority: self.priority / sum,
            recency: self.recency / sum,
        }
    }
}

/// Get preset weights by name.
pub fn get_preset(name: &str) -> Option<Weights> {
    PRESETS.iter().find(|(n, _)| *n == name).map(|(_, w)| *w)
}

/// Normalized component scores for a single candidate issue.
///
/// Port of Go `pkg/search/normalizers.go` + the `hybridScorer.Score`
/// component assembly (`hybrid_scorer_impl.go`): status/priority/impact/
/// recency normalized to [0,1], pagerank passed through from the graph
/// metrics (Go `IssueMetrics.PageRank`, already 0.0–1.0).
#[derive(Debug, Clone, Default, Serialize)]
pub struct ComponentScores {
    pub pagerank: f64,
    pub status: f64,
    pub impact: f64,
    pub priority: f64,
    pub recency: f64,
}

impl ComponentScores {
    /// Go `normalizeStatus`: open=1.0; in_progress/hooked/review=0.8;
    /// pinned=0.7; blocked=0.5; draft/deferred=0.2; closed=0.1;
    /// tombstone=0.0; anything else=0.5.
    pub fn normalize_status(status_str: &str) -> f64 {
        match status_str.trim().to_lowercase().as_str() {
            "open" => 1.0,
            "in_progress" | "hooked" | "review" => 0.8,
            "pinned" => 0.7,
            "blocked" => 0.5,
            "draft" | "deferred" => 0.2,
            "closed" => 0.1,
            "tombstone" => 0.0,
            _ => 0.5,
        }
    }

    /// Go `normalizePriority`: P0=1.0, P1=0.8, P2=0.6, P3=0.4, P4=0.2,
    /// anything else (including negative) = 0.5.
    pub fn normalize_priority(prio: i64) -> f64 {
        match prio {
            0 => 1.0,
            1 => 0.8,
            2 => 0.6,
            3 => 0.4,
            4 => 0.2,
            _ => 0.5,
        }
    }

    /// Go `normalizeImpact(blockerCount, maxBlockerCount)`: fraction of the
    /// max, 0 when the issue blocks nothing, 0.5 when there is nothing to
    /// normalize against (max == 0).
    pub fn normalize_impact(blocker_count: usize, max_blocker_count: usize) -> f64 {
        if max_blocker_count == 0 {
            return 0.5;
        }
        if blocker_count == 0 {
            return 0.0;
        }
        if blocker_count >= max_blocker_count {
            return 1.0;
        }
        blocker_count as f64 / max_blocker_count as f64
    }

    /// Go `normalizeRecencyAt`: exp(-days/30), clamped to [0,1], future
    /// dates → 1.0. Takes precomputed days (caller owns the clock, matching
    /// Go's `hybridScorerAt` pinned-reference-time design).
    pub fn normalize_recency_days(days_since_update: f64) -> f64 {
        if days_since_update < 0.0 {
            return 1.0;
        }
        let score = (-days_since_update / 30.0).exp();
        score.clamp(0.0, 1.0)
    }

    /// Legacy constructor kept for existing callers (CLI text-mode path):
    /// status/priority/recency via the Go-exact normalizers above,
    /// pagerank/impact default to 0 (no graph context at the call site).
    pub fn new(status_str: &str, prio: i64, days_since_update: f64) -> Self {
        ComponentScores {
            pagerank: 0.0,
            status: Self::normalize_status(status_str),
            impact: 0.0,
            priority: Self::normalize_priority(prio),
            recency: Self::normalize_recency_days(days_since_update),
        }
    }
}

/// Hybrid score for a single result.
///
/// Serializes to Go's `HybridScore` shape (`hybrid_scorer.go`):
/// `component_scores` is a flat string→float map with exactly the keys
/// `pagerank/status/impact/priority/recency` (Go omits the map only when
/// there is no metrics cache at all — i.e. pure-text fallback, handled by
/// the caller using `text_score` alone, not by emitting a partial map).
#[derive(Debug, Clone, Serialize)]
pub struct HybridResult {
    pub issue_id: String,
    pub score: f64,
    #[serde(rename = "text_score")]
    pub text_score: f64,
    pub component_scores: std::collections::BTreeMap<String, f64>,
}

impl HybridResult {
    pub fn new(
        issue_id: String,
        text_score: f64,
        weights: &Weights,
        components: &ComponentScores,
    ) -> Self {
        let score = hybrid_score(text_score, weights, components);
        let mut map = std::collections::BTreeMap::new();
        map.insert("pagerank".into(), components.pagerank);
        map.insert("status".into(), components.status);
        map.insert("impact".into(), components.impact);
        map.insert("priority".into(), components.priority);
        map.insert("recency".into(), components.recency);
        HybridResult {
            issue_id,
            score,
            text_score,
            component_scores: map,
        }
    }
}

/// Compute hybrid score (Go `hybridScorer.Score` final assembly):
/// `text*w_text + pagerank*w_pr + status*w_status + impact*w_impact +
/// priority*w_prio + recency*w_rec`, where every component is already
/// normalized to [0,1] by the caller (see `ComponentScores`).
pub fn hybrid_score(text_score: f64, weights: &Weights, components: &ComponentScores) -> f64 {
    weights.text_relevance * text_score
        + weights.pagerank * components.pagerank
        + weights.status * components.status
        + weights.impact * components.impact
        + weights.priority * components.priority
        + weights.recency * components.recency
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_preset_weights() {
        let w = get_preset("default").unwrap();
        assert!((w.text_relevance - 0.40).abs() < 1e-9);
        assert!((w.pagerank - 0.20).abs() < 1e-9);
        assert!((w.status - 0.15).abs() < 1e-9);
        assert!((w.impact - 0.10).abs() < 1e-9);
        assert!((w.priority - 0.10).abs() < 1e-9);
        assert!((w.recency - 0.05).abs() < 1e-9);
    }

    #[test]
    fn all_presets_sum_to_one() {
        for (name, w) in PRESETS {
            let total =
                w.text_relevance + w.pagerank + w.status + w.impact + w.priority + w.recency;
            assert!((total - 1.0).abs() < 1e-6, "preset {name} sums to {total}");
        }
    }

    #[test]
    fn unknown_preset_returns_none() {
        assert!(get_preset("nonexistent").is_none());
    }

    #[test]
    fn all_five_preset_names_exist() {
        let names: Vec<&str> = PRESETS.iter().map(|(n, _)| *n).collect();
        assert_eq!(names.len(), 5);
        for expected in [
            "default",
            "bug-hunting",
            "sprint-planning",
            "impact-first",
            "text-only",
        ] {
            assert!(names.contains(&expected), "missing preset: {expected}");
        }
    }

    #[test]
    fn text_only_weights_zero_out_graph_signals() {
        let w = get_preset("text-only").unwrap();
        assert_eq!(w.pagerank, 0.0);
        assert_eq!(w.status, 0.0);
        assert_eq!(w.impact, 0.0);
    }

    #[test]
    fn hybrid_score_with_text_only_uses_text_exclusively() {
        let w = get_preset("text-only").unwrap();
        let comps = ComponentScores::new("open", 2, 5.0);
        let score = hybrid_score(0.8, &w, &comps);
        assert!((score - 0.8).abs() < 1e-9);
    }

    #[test]
    fn component_normalizers_match_go() {
        let c = ComponentScores::new("open", 0, 0.0);
        assert!((c.status - 1.0).abs() < 1e-9);
        assert!((c.priority - 1.0).abs() < 1e-9);
        assert!((c.recency - 1.0).abs() < 1e-9);

        let c2 = ComponentScores::new("tombstone", 4, 90.0);
        assert!(c2.status.abs() < 1e-9);
        assert!((c2.priority - 0.2).abs() < 1e-9);
        assert!(c2.recency < 0.05);
    }
}
