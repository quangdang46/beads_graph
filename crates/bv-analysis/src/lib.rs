//! bv-analysis: two-phase analyzer, cache, scoring, drift.
//! Graph algorithms come from bv-graph-core (extracted from upstream wasm crate).

pub mod analyzer;
pub mod blocker_chain;
pub mod cache;
pub mod diff;
pub mod drift;
pub mod feedback;
pub mod file_impact;
pub mod go_math;
pub mod impact;
pub mod label_health;
pub mod metrics;
pub mod scoring;
pub mod snapshot_diff;
pub mod suggestions;
pub mod triage;

pub use bv_graph_core::algorithms;
pub use bv_graph_core::DiGraph;

// Re-export analyzer types for downstream crates
pub use analyzer::build_graph;
pub use analyzer::{
    analyze_phase1, analyze_phase2_blocking, analyze_with_profile, full_analysis_config,
    AnalysisBudget, GraphAnalysisPhase2, MetricStatus, Phase1Stats, StartupProfile, StatusEntry,
};
pub use blocker_chain::{is_actionable, open_blockers};
pub use feedback::{load_feedback, FeedbackData, FeedbackEvent, FeedbackJson, WeightAdjustment};
pub use go_math::{go_log, go_log2};
pub use impact::{compute_impact_scores, compute_impact_scores_with_weights, ImpactInputs};
pub use scoring::{default_weights, Weights};
pub use triage::{build_triage, compute_blocked_set, compute_row_triage, RowTriage, TriageOutput};
