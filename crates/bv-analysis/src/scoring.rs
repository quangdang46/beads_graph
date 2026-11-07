//! Scoring constants + composite impact scoring — port of Go
//! `pkg/analysis/priority.go` (weights) and triage factors.

use serde::Serialize;
use std::collections::BTreeMap;

// === Impact weights (Go priority.go:55-62) — DO NOT ALTER ===
pub const WEIGHT_PAGE_RANK: f64 = 0.22;
pub const WEIGHT_BETWEENNESS: f64 = 0.20;
pub const WEIGHT_BLOCKER_RATIO: f64 = 0.13;
pub const WEIGHT_STALENESS: f64 = 0.05;
pub const WEIGHT_PRIORITY_BOOST: f64 = 0.10;
pub const WEIGHT_TIME_TO_IMPACT: f64 = 0.10;
pub const WEIGHT_URGENCY: f64 = 0.10;
pub const WEIGHT_RISK: f64 = 0.10;

/// The set of composite-score factor weights — port of Go `Weights`
/// (pkg/analysis/priority.go:68-77).
///
/// The `WEIGHT_*` constants above are the documented defaults; feedback
/// (`feedback.rs`) and per-project configuration produce adjusted values that a
/// scoring caller installs through [`Weights::normalized`] and passes to
/// [`crate::impact::compute_impact_scores_with_weights`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Weights {
    pub pagerank: f64,
    pub betweenness: f64,
    pub blocker_ratio: f64,
    pub staleness: f64,
    pub priority_boost: f64,
    pub time_to_impact: f64,
    pub urgency: f64,
    pub risk: f64,
}

/// The documented default factor weights (sum 1.0) — Go `DefaultWeights`
/// (pkg/analysis/priority.go:80-91).
pub fn default_weights() -> Weights {
    Weights {
        pagerank: WEIGHT_PAGE_RANK,
        betweenness: WEIGHT_BETWEENNESS,
        blocker_ratio: WEIGHT_BLOCKER_RATIO,
        staleness: WEIGHT_STALENESS,
        priority_boost: WEIGHT_PRIORITY_BOOST,
        time_to_impact: WEIGHT_TIME_TO_IMPACT,
        urgency: WEIGHT_URGENCY,
        risk: WEIGHT_RISK,
    }
}

impl Weights {
    /// Total of all factor weights — Go `Weights.Sum` (priority.go:94-96).
    pub fn sum(self) -> f64 {
        self.pagerank
            + self.betweenness
            + self.blocker_ratio
            + self.staleness
            + self.priority_boost
            + self.time_to_impact
            + self.urgency
            + self.risk
    }

    /// Whether no weight has been set — Go `Weights.IsZero` (priority.go:99).
    pub fn is_zero(self) -> bool {
        self.sum() == 0.0
    }

    /// Scale the weights so they sum to 1.0. An all-zero `Weights` normalizes
    /// back to [`default_weights`] so a caller that never installed weights can
    /// never zero out every score — Go `Weights.Normalized` (priority.go:104-119).
    pub fn normalized(self) -> Weights {
        let total = self.sum();
        if total <= 0.0 {
            return default_weights();
        }
        Weights {
            pagerank: self.pagerank / total,
            betweenness: self.betweenness / total,
            blocker_ratio: self.blocker_ratio / total,
            staleness: self.staleness / total,
            priority_boost: self.priority_boost / total,
            time_to_impact: self.time_to_impact / total,
            urgency: self.urgency / total,
            risk: self.risk / total,
        }
    }

    /// The weights keyed by the factor names used in `feedback.json` — Go
    /// `Weights.AsMap` (priority.go:122-133). The `BTreeMap` reproduces Go's
    /// sorted map-key marshaling order.
    pub fn as_map(self) -> BTreeMap<String, f64> {
        BTreeMap::from([
            ("PageRank".to_string(), self.pagerank),
            ("Betweenness".to_string(), self.betweenness),
            ("BlockerRatio".to_string(), self.blocker_ratio),
            ("Staleness".to_string(), self.staleness),
            ("PriorityBoost".to_string(), self.priority_boost),
            ("TimeToImpact".to_string(), self.time_to_impact),
            ("Urgency".to_string(), self.urgency),
            ("Risk".to_string(), self.risk),
        ])
    }

    /// Build `Weights` from the factor-name map used in `feedback.json`;
    /// missing names fall back to the default for that factor — Go
    /// `WeightsFromMap` (priority.go:137-153). A negative (or NaN) value is
    /// ignored, matching Go's `v >= 0` guard.
    pub fn from_map(m: &BTreeMap<String, f64>) -> Weights {
        let mut w = default_weights();
        let pick = |name: &str, dst: &mut f64| {
            if let Some(&v) = m.get(name) {
                if v >= 0.0 {
                    *dst = v;
                }
            }
        };
        pick("PageRank", &mut w.pagerank);
        pick("Betweenness", &mut w.betweenness);
        pick("BlockerRatio", &mut w.blocker_ratio);
        pick("Staleness", &mut w.staleness);
        pick("PriorityBoost", &mut w.priority_boost);
        pick("TimeToImpact", &mut w.time_to_impact);
        pick("Urgency", &mut w.urgency);
        pick("Risk", &mut w.risk);
        w
    }
}

/// Weighted contribution of each component (Go `ScoreBreakdown`).
#[derive(Debug, Clone, Default, Serialize)]
pub struct ScoreBreakdown {
    pub pagerank: f64,
    pub betweenness: f64,
    pub blocker_ratio: f64,
    pub staleness: f64,
    pub priority_boost: f64,
    pub time_to_impact: f64,
    pub urgency: f64,
    pub risk: f64,
}

/// Composite impact score (Go `ImpactScore`).
#[derive(Debug, Clone, Default, Serialize)]
pub struct ImpactScore {
    pub score: f64,
    pub breakdown: ScoreBreakdown,
}

/// Compute weighted impact from normalized component values (each 0..1) using
/// the documented default weights — Go `computeImpact` (priority.go:252).
#[allow(clippy::too_many_arguments)]
pub fn compute_impact(
    pagerank_norm: f64,
    betweenness_norm: f64,
    blocker_ratio_norm: f64,
    staleness_norm: f64,
    priority_boost_norm: f64,
    time_to_impact_norm: f64,
    urgency_norm: f64,
    risk_norm: f64,
) -> ImpactScore {
    compute_impact_with_weights(
        pagerank_norm,
        betweenness_norm,
        blocker_ratio_norm,
        staleness_norm,
        priority_boost_norm,
        time_to_impact_norm,
        urgency_norm,
        risk_norm,
        default_weights(),
    )
}

/// Same as [`compute_impact`] with caller-supplied factor weights — the Rust
/// counterpart of Go's `a.Weights()` lookup at priority.go:252.
#[allow(clippy::too_many_arguments)]
pub fn compute_impact_with_weights(
    pagerank_norm: f64,
    betweenness_norm: f64,
    blocker_ratio_norm: f64,
    staleness_norm: f64,
    priority_boost_norm: f64,
    time_to_impact_norm: f64,
    urgency_norm: f64,
    risk_norm: f64,
    w: Weights,
) -> ImpactScore {
    let b = ScoreBreakdown {
        pagerank: pagerank_norm * w.pagerank,
        betweenness: betweenness_norm * w.betweenness,
        blocker_ratio: blocker_ratio_norm * w.blocker_ratio,
        staleness: staleness_norm * w.staleness,
        priority_boost: priority_boost_norm * w.priority_boost,
        time_to_impact: time_to_impact_norm * w.time_to_impact,
        urgency: urgency_norm * w.urgency,
        risk: risk_norm * w.risk,
    };
    ImpactScore {
        score: b.pagerank
            + b.betweenness
            + b.blocker_ratio
            + b.staleness
            + b.priority_boost
            + b.time_to_impact
            + b.urgency
            + b.risk,
        breakdown: b,
    }
}

/// Go: `scoreToPriority` — impact score (0-1) -> P0..P4.
pub fn score_to_priority(score: f64) -> i32 {
    if score >= 0.7 {
        0
    } else if score >= 0.5 {
        1
    } else if score >= 0.3 {
        2
    } else if score >= 0.15 {
        3
    } else {
        4
    }
}

/// Go: `priorityToScore`.
pub fn priority_to_score(priority: i32) -> f64 {
    match priority {
        0 => 0.8,
        1 => 0.6,
        2 => 0.4,
        3 => 0.2,
        _ => 0.1,
    }
}

/// Triage combination weights (Go triage.go ~1204): base + boosts.
pub const TRIAGE_BASE_WEIGHT: f64 = 0.70;
pub const TRIAGE_UNBLOCK_BOOST_WEIGHT: f64 = 0.15;
pub const TRIAGE_QUICK_WIN_WEIGHT: f64 = 0.15;
/// unblocks threshold for the unblock boost.
pub const TRIAGE_UNBLOCK_THRESHOLD: usize = 5;

/// QuickWin sub-score (Go triage.go ~899 `buildQuickWins`):
/// `log2(unblocks+1)*.4 + simplicity*.4 + priorityBonus*.2`.
///
/// NOTE: currently uncalled — the live quick-win ranking is
/// `triage::compute_row_triage`, which implements the same Go function. This
/// copy had drifted from it in two places (an extra `.min(1.0)` cap on the
/// unblock term Go does not apply, and a three-tier priority bonus); it is
/// aligned here so the two can never disagree if a caller is added.
pub fn quickwin_score(unblocks: usize, blocker_ratio_norm: f64, priority: i32) -> f64 {
    // `go_log2`, not `f64::log2` — same bit-exact log Go's math.Log2 emits,
    // matching `compute_row_triage`.
    let unblock_impact = crate::go_math::go_log2(unblocks as f64 + 1.0);
    let simplicity = if blocker_ratio_norm < 0.2 {
        1.0
    } else if blocker_ratio_norm < 0.4 {
        0.5
    } else {
        0.0
    };
    // triage.rs `compute_row_triage`: P0/P1 get the bonus, everything else 0.
    let prio_bonus = if priority <= 1 { 0.5 } else { 0.0 };
    (unblock_impact * 0.4) + (simplicity * 0.4) + (prio_bonus * 0.2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    /// Go's zero `Weights` — deliberately not a `Default` impl, so a caller who
    /// means "the documented defaults" has to say [`default_weights`].
    fn zero_weights() -> Weights {
        Weights {
            pagerank: 0.0,
            betweenness: 0.0,
            blocker_ratio: 0.0,
            staleness: 0.0,
            priority_boost: 0.0,
            time_to_impact: 0.0,
            urgency: 0.0,
            risk: 0.0,
        }
    }

    #[test]
    fn weights_sum_to_one() {
        let total = WEIGHT_PAGE_RANK
            + WEIGHT_BETWEENNESS
            + WEIGHT_BLOCKER_RATIO
            + WEIGHT_STALENESS
            + WEIGHT_PRIORITY_BOOST
            + WEIGHT_TIME_TO_IMPACT
            + WEIGHT_URGENCY
            + WEIGHT_RISK;
        assert!(close(total, 1.0), "weights sum to {total}");
        assert!(close(default_weights().sum(), 1.0));
    }

    #[test]
    fn weights_normalized_scales_to_one() {
        // Go priority.go:104-119 — every factor divides by the same total, and
        // a non-positive total falls back to the documented defaults.
        let w = Weights {
            pagerank: 0.44,
            betweenness: 0.40,
            blocker_ratio: 0.26,
            staleness: 0.10,
            priority_boost: 0.20,
            time_to_impact: 0.20,
            urgency: 0.20,
            risk: 0.20,
        };
        let n = w.normalized();
        assert!(close(n.sum(), 1.0));
        assert!(close(n.pagerank, 0.22));
        assert!(close(n.staleness, 0.05));

        let zero = zero_weights();
        assert!(zero.is_zero());
        assert_eq!(zero.normalized(), default_weights());
    }

    #[test]
    fn weights_map_round_trip_uses_go_factor_names() {
        // Go priority.go:122-153 — the map keys are the factor names that
        // feedback.json stores, and missing/negative entries fall back to the
        // documented default for that factor.
        let m = BTreeMap::from([
            ("PageRank".to_string(), 0.5),
            ("Betweenness".to_string(), -1.0),
        ]);
        let w = Weights::from_map(&m);
        assert_eq!(w.pagerank, 0.5);
        assert_eq!(w.betweenness, WEIGHT_BETWEENNESS);
        assert_eq!(w.as_map().get("TimeToImpact"), Some(&WEIGHT_TIME_TO_IMPACT));
        // as_map keys serialize in sorted order, exactly like a Go map.
        let as_map = w.as_map();
        let keys: Vec<&str> = as_map.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            [
                "Betweenness",
                "BlockerRatio",
                "PageRank",
                "PriorityBoost",
                "Risk",
                "Staleness",
                "TimeToImpact",
                "Urgency"
            ]
        );
    }

    #[test]
    fn compute_impact_with_weights_differs_from_defaults() {
        // Go priority.go:252 reads a.Weights() instead of the constants, so a
        // caller-installed weighting must actually move the score.
        let custom = Weights {
            pagerank: 1.0,
            ..zero_weights()
        }
        .normalized();
        let r = compute_impact_with_weights(1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, custom);
        assert!(close(r.score, 1.0));
        assert!(close(r.breakdown.pagerank, 1.0));
        let d = compute_impact(1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        assert!(close(d.score, WEIGHT_PAGE_RANK));
    }

    #[test]
    fn compute_impact_weighting_exact() {
        let r = compute_impact(1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0);
        assert!(close(r.score, 1.0));
        assert!(close(r.breakdown.pagerank, 0.22));
        assert!(close(r.breakdown.betweenness, 0.20));
        assert!(close(r.breakdown.blocker_ratio, 0.13));
        assert!(close(r.breakdown.staleness, 0.05));
        // all-zero input
        let z = compute_impact(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        assert!(close(z.score, 0.0));
    }

    #[test]
    fn score_to_priority_thresholds() {
        assert_eq!(score_to_priority(0.70), 0);
        assert_eq!(score_to_priority(0.9), 0);
        assert_eq!(score_to_priority(0.5), 1);
        assert_eq!(score_to_priority(0.3), 2);
        assert_eq!(score_to_priority(0.15), 3);
        assert_eq!(score_to_priority(0.14), 4);
        assert_eq!(score_to_priority(0.0), 4);
    }

    #[test]
    fn priority_to_score_monotone() {
        for p in 0..4 {
            assert!(priority_to_score(p) > priority_to_score(p + 1));
        }
    }

    #[test]
    fn quickwin_formula_matches_go() {
        // Matches `triage::compute_row_triage`'s live formula: log2(unblocks+1)
        // uncapped, simplicity by blocker ratio, P<=1 priority bonus of 0.5.
        // unblocks=0 -> log2(1)=0; simplicity=1 (ratio 0.1); P0 bonus 0.5:
        // qw = 0*.4 + 1*.4 + 0.5*.2 = 0.5
        let qw = quickwin_score(0, 0.1, 0);
        assert!(close(qw, 0.5));
        // unblocks=15 -> log2(16)=4 (uncapped) -> 1.6; simplicity=0.5 (0.3);
        // P3 bonus 0 -> 1.6 + 0.2 = 1.8
        let qw2 = quickwin_score(15, 0.3, 3);
        assert!(close(qw2, 1.8));
    }
}
