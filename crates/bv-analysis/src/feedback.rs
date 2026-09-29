//! Feedback loop for recommendation tuning (bv-90) — port of Go
//! `pkg/analysis/feedback.go`.
//!
//! Go stores the accepted/ignored history in `<beadsDir>/feedback.json` and
//! smooths the eight composite-score factor weights toward whatever the user
//! kept or dropped. The store is deliberately *not* self-applying: below
//! [`MIN_FEEDBACK_SAMPLES`] events the adjustments are still tracked and
//! reported, but scoring keeps using the documented defaults so a single stray
//! click cannot reorder a project's triage (Go `FeedbackData.Applies`,
//! feedback.go:306-318).

use crate::scoring::{default_weights, Weights};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

/// The name of the feedback sidecar file (Go `FeedbackFile`, feedback.go:15).
pub const FEEDBACK_FILE: &str = "feedback.json";

/// Accept/ignore events required before adjusted weights are applied to
/// scoring — Go `MinFeedbackSamples` (feedback.go:310).
pub const MIN_FEEDBACK_SAMPLES: usize = 3;

/// Exponential-smoothing learning rate (Go `smoothingAlpha`, feedback.go:242).
const SMOOTHING_ALPHA: f64 = 0.2;

/// The tracked factor names in their canonical order (Go
/// `defaultWeightAdjustments`, feedback.go:66-69). The order is the on-disk
/// `adjustments` array order, so it is part of the file format.
pub const ADJUSTMENT_NAMES: [&str; 8] = [
    "PageRank",
    "Betweenness",
    "BlockerRatio",
    "Staleness",
    "PriorityBoost",
    "TimeToImpact",
    "Urgency",
    "Risk",
];

/// The eight normalized (pre-weight) components the weight update consumes —
/// Go `ScoreBreakdown`'s `*Norm` fields (feedback.go:252-261). The weighted
/// fields of the same Go struct are not read by the update.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ScoreContributions {
    pub pagerank: f64,
    pub betweenness: f64,
    pub blocker_ratio: f64,
    pub staleness: f64,
    pub priority_boost: f64,
    pub time_to_impact: f64,
    pub urgency: f64,
    pub risk: f64,
}

impl From<&crate::impact::Breakdown> for ScoreContributions {
    fn from(b: &crate::impact::Breakdown) -> Self {
        Self {
            pagerank: b.pagerank_norm,
            betweenness: b.betweenness_norm,
            blocker_ratio: b.blocker_ratio_norm,
            staleness: b.staleness_norm,
            priority_boost: b.priority_boost_norm,
            time_to_impact: b.time_to_impact_norm,
            urgency: b.urgency_norm,
            risk: b.risk_norm,
        }
    }
}

impl ScoreContributions {
    /// The contribution of `name`, or `None` for a name this store does not
    /// track (Go's `contributions[name]` lookup miss, feedback.go:265-268).
    fn contribution(&self, name: &str) -> Option<f64> {
        Some(match name {
            "PageRank" => self.pagerank,
            "Betweenness" => self.betweenness,
            "BlockerRatio" => self.blocker_ratio,
            "Staleness" => self.staleness,
            "PriorityBoost" => self.priority_boost,
            "TimeToImpact" => self.time_to_impact,
            "Urgency" => self.urgency,
            "Risk" => self.risk,
            _ => return None,
        })
    }
}

/// A Go `time.Time` on the wire: RFC3339 with nanosecond precision and trailing
/// zeros removed. The zero value is `0001-01-01T00:00:00Z`, which is what Go
/// emits for a `time.Time` that was never assigned.
///
/// A Go `time.Time` carries the zone it was parsed in, and Go marshals that
/// offset back out — a value stored as `2026-09-26T20:00:07.367773+07:00`
/// reads back and re-emits with `+07:00`, not normalized to `Z`. A bare
/// `jiff::Timestamp` keeps only the instant and would print UTC, so the offset
/// is retained here and re-applied on the way out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GoTime(pub jiff::Timestamp, pub Option<i64>);

/// Go's zero `time.Time` (Go `time.Time{}`).
pub const GO_ZERO_TIME: &str = "0001-01-01T00:00:00Z";

impl GoTime {
    /// The Go zero time (`time.Time{}`).
    pub fn zero() -> Self {
        Self(
            GO_ZERO_TIME
                .parse()
                .expect("GO_ZERO_TIME is a valid RFC3339 stamp"),
            None,
        )
    }

    /// Go's `time.Now()` on this machine: the local zone, not UTC, so the
    /// offset Go writes is the one the wall clock is actually in.
    ///
    /// A zero offset is stored as `None` rather than `Some(0)`, because `None`
    /// is what a `Z` stamp parses back to and both render as `Z` — the same
    /// form Go writes for a UTC `time.Time`. Keeping `Some(0)` would render
    /// identically and still fail to equal its own round trip, since
    /// `parse_offset_suffix` has no `Z` case to return `Some(0)` from.
    pub fn now() -> Self {
        let ts = jiff::Timestamp::now();
        let offset = ts
            .to_zoned(jiff::tz::TimeZone::try_system().unwrap_or(jiff::tz::TimeZone::UTC))
            .offset()
            .seconds() as i64;
        Self(ts, (offset != 0).then_some(offset))
    }

    /// Whether this is Go's zero `time.Time` — Go's `t.IsZero()`.
    pub fn is_zero(&self) -> bool {
        self.0 == Self::zero().0
    }

    /// Render as Go's `RFC3339Nano`: the wall clock in the stored offset, with
    /// that offset appended. `"Z"` when the stamp was parsed as UTC.
    ///
    /// Go's `time.Time` renders the *local* wall clock for the zone it holds,
    /// not the UTC instant with a suffix — so 13:00:07Z in a +07:00 zone
    /// prints as 20:00:07+07:00. Applying the offset to the instant here is
    /// what reproduces that; appending it to the UTC form would print
    /// 13:00:07+07:00, the same instant under a label it never had.
    fn to_rfc3339(self) -> String {
        match self.1 {
            Some(secs) if secs != 0 => {
                let base = (self.0 + jiff::Span::new().seconds(secs))
                    .to_string()
                    .trim_end_matches('Z')
                    .to_string();
                let (sign, abs) = if secs < 0 { ('-', -secs) } else { ('+', secs) };
                format!("{base}{sign}{:02}:{:02}", abs / 3600, (abs % 3600) / 60)
            }
            // A zero offset is UTC, and Go renders UTC as `Z` — RFC3339 admits
            // only `Z` or `±HH:MM`. This used to trim the `Z` for *every*
            // recorded offset and re-append one only for non-zero, which left
            // the UTC case as a bare `2026-09-30T01:23:45.123456789`. That is
            // not a timestamp jiff can read back, and `GoTime::now` records
            // an offset unconditionally — so a machine at UTC+0 wrote a
            // feedback store it then refused to load:
            //
            //     Error loading feedback: failed to parse feedback file:
            //     failed to find offset component, which is required for
            //     parsing a timestamp
            //
            // Measured against the oracle at TZ=UTC: Go writes
            // `"created_at": "...113126Z"` and reads it back. Any user in UTC
            // lost their feedback store on the next run.
            _ => self.0.to_string(),
        }
    }
}

/// The seconds east of UTC in an RFC3339 stamp's trailing `±HH:MM`, or `None`
/// for a `Z` stamp and for anything that is not one. Go's `time.Parse`
/// (RFC3339) accepts only those two forms, so nothing else needs a case.
fn parse_offset_suffix(raw: &str) -> Option<i64> {
    let tail = raw.get(raw.len().checked_sub(6)?..)?;
    let (sign, digits) = match tail.as_bytes().first()? {
        b'+' => (1i64, &tail[1..]),
        b'-' => (-1i64, &tail[1..]),
        _ => return None,
    };
    let (h, m) = digits.split_once(':')?;
    let (h, m) = (h.parse::<i64>().ok()?, m.parse::<i64>().ok()?);
    Some(sign * (h * 3600 + m * 60))
}

impl fmt::Display for GoTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // jiff prints the shortest subsecond form, which is exactly Go's
        // RFC3339Nano layout; the offset Go kept is re-appended.
        f.write_str(&self.to_rfc3339())
    }
}

impl Serialize for GoTime {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_rfc3339())
    }
}

impl<'de> Deserialize<'de> for GoTime {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        // `jiff::Timestamp` normalizes an offset away, so it is read from the
        // trailing `±HH:MM` to survive the round trip the way Go's
        // `time.Time` does.
        let ts = raw
            .parse::<jiff::Timestamp>()
            .map_err(serde::de::Error::custom)?;
        Ok(Self(ts, parse_offset_suffix(&raw)))
    }
}

/// A single feedback action (Go `FeedbackEvent`, feedback.go:18-23).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeedbackEvent {
    pub issue_id: String,
    /// "accept" or "ignore".
    pub action: String,
    /// Score at the time of feedback.
    pub score: f64,
    pub timestamp: GoTime,
}

/// A smoothed weight adjustment (Go `WeightAdjustment`, feedback.go:26-31).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WeightAdjustment {
    pub name: String,
    /// Multiplier, clamped to the 0.5-2.0 range.
    pub adjustment: f64,
    /// Number of feedback events that touched this factor.
    pub samples: usize,
    pub last_updated: GoTime,
}

/// Aggregate feedback metrics (Go `FeedbackStats`, feedback.go:45-50).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct FeedbackStats {
    pub total_accepted: usize,
    pub total_ignored: usize,
    pub avg_accept_score: f64,
    pub avg_ignore_score: f64,
}

/// All feedback information for a repository (Go `FeedbackData`,
/// feedback.go:34-42). Field order is the on-disk key order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeedbackData {
    pub version: String,
    pub created_at: GoTime,
    pub updated_at: GoTime,
    #[serde(default)]
    pub events: Vec<FeedbackEvent>,
    #[serde(default)]
    pub adjustments: Vec<WeightAdjustment>,
    #[serde(default)]
    pub stats: FeedbackStats,
}

/// The error surface of the feedback store. `Display` reproduces Go's wrapped
/// messages verbatim, because the CLI prefixes them with
/// `Error loading/saving/recording feedback: %v` (Go main.go:2449-2519).
#[derive(Debug)]
pub enum FeedbackError {
    Read(std::io::Error),
    Parse(serde_json::Error),
    Marshal(serde_json::Error),
    Write(std::io::Error),
    InvalidAction(String),
}

impl fmt::Display for FeedbackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(e) => write!(f, "failed to read feedback file: {e}"),
            Self::Parse(e) => write!(f, "failed to parse feedback file: {e}"),
            Self::Marshal(e) => write!(f, "failed to marshal feedback: {e}"),
            Self::Write(e) => write!(f, "failed to write feedback file: {e}"),
            Self::InvalidAction(a) => {
                write!(f, "invalid action: {a} (must be 'accept' or 'ignore')")
            }
        }
    }
}

impl std::error::Error for FeedbackError {}

/// The initial weight adjustments (all 1.0 = no adjustment) — Go
/// `defaultWeightAdjustments` (feedback.go:65-80).
pub fn default_weight_adjustments() -> Vec<WeightAdjustment> {
    let now = GoTime::now();
    ADJUSTMENT_NAMES
        .iter()
        .map(|name| WeightAdjustment {
            name: (*name).to_string(),
            adjustment: 1.0,
            samples: 0,
            last_updated: now,
        })
        .collect()
}

/// Initialized feedback data — Go `DefaultFeedbackData` (feedback.go:53-62).
pub fn default_feedback_data() -> FeedbackData {
    let now = GoTime::now();
    FeedbackData {
        version: "1.0".to_string(),
        created_at: now,
        updated_at: now,
        events: Vec::new(),
        adjustments: default_weight_adjustments(),
        stats: FeedbackStats::default(),
    }
}

/// Aggregate stats recomputed from the event list — Go
/// `calculateFeedbackStats` (feedback.go:152-175).
fn calculate_feedback_stats(events: &[FeedbackEvent]) -> FeedbackStats {
    let mut stats = FeedbackStats::default();
    let mut accepted_score_total = 0.0;
    let mut ignored_score_total = 0.0;
    for event in events {
        match event.action.as_str() {
            "accept" => {
                stats.total_accepted += 1;
                accepted_score_total += event.score;
            }
            "ignore" => {
                stats.total_ignored += 1;
                ignored_score_total += event.score;
            }
            _ => {}
        }
    }
    if stats.total_accepted > 0 {
        stats.avg_accept_score = accepted_score_total / stats.total_accepted as f64;
    }
    if stats.total_ignored > 0 {
        stats.avg_ignore_score = ignored_score_total / stats.total_ignored as f64;
    }
    stats
}

/// Reconcile a freshly loaded store: fill defaults, re-clamp adjustments onto
/// the eight known factor names, and recompute the stats from the events —
/// Go `normalizeLoaded` (feedback.go:105-150).
fn normalize_loaded(loaded: FeedbackData) -> FeedbackData {
    let now = GoTime::now();
    let version = if loaded.version.is_empty() {
        "1.0".to_string()
    } else {
        loaded.version
    };
    let created_at = if loaded.created_at.is_zero() {
        now
    } else {
        loaded.created_at
    };
    let updated_at = if loaded.updated_at.is_zero() {
        created_at
    } else {
        loaded.updated_at
    };

    let mut by_name: BTreeMap<&str, &WeightAdjustment> = BTreeMap::new();
    for adj in &loaded.adjustments {
        if !adj.name.is_empty() {
            by_name.insert(adj.name.as_str(), adj);
        }
    }
    let mut adjustments = default_weight_adjustments();
    for adj in adjustments.iter_mut() {
        let Some(loaded_adj) = by_name.get(adj.name.as_str()).copied() else {
            continue;
        };
        if loaded_adj.adjustment > 0.0 && loaded_adj.adjustment.is_finite() {
            adj.adjustment = loaded_adj.adjustment.clamp(0.5, 2.0);
        }
        if loaded_adj.samples > 0 {
            adj.samples = loaded_adj.samples;
        }
        if !loaded_adj.last_updated.is_zero() {
            adj.last_updated = loaded_adj.last_updated;
        } else if !updated_at.is_zero() {
            adj.last_updated = updated_at;
        }
    }

    FeedbackData {
        version,
        created_at,
        updated_at,
        stats: calculate_feedback_stats(&loaded.events),
        events: loaded.events,
        adjustments,
    }
}

/// Load feedback data from the beads directory — Go `LoadFeedback`
/// (feedback.go:83-103). A missing file yields fresh defaults without an
/// Where the recommendation-weight store lives.
///
/// `BV_FEEDBACK_PATH` overrides it, for the same reason `BV_BASELINE_PATH`
/// exists. `.beads/feedback.json` is gitignored, so a machine that has ever
/// run a `--feedback-*` flag carries a store that a clean clone and CI do not —
/// and the store is not inert: it is the `feedback` block of every robot
/// payload, including the tuned `weight_adjustments` and `effective_weights`
/// that feed scoring. A gate that reads whichever store it finds is a gate
/// that follows the machine.
///
/// That is not hypothetical. `golden/selfrepo____robot_triage.json` had one
/// machine's 2 events and weight adjustments baked into it, so it matched here
/// and diverged on every runner. Point the override at a path that does not
/// exist and the command takes Go's clean-checkout path, which returns
/// defaults and omits the block entirely.
pub fn feedback_path(beads_dir: &Path) -> std::path::PathBuf {
    match std::env::var_os("BV_FEEDBACK_PATH") {
        Some(p) if !p.is_empty() => std::path::PathBuf::from(p),
        _ => beads_dir.join(FEEDBACK_FILE),
    }
}

/// error, which is what lets `--feedback-reset` work on a clean checkout.
///
/// Only a *not-found* stat takes that path. Go's `os.Stat` +
/// `os.IsNotExist` check (feedback.go:87) leaves every other stat error to fall
/// through to `os.ReadFile`, which then fails; `Path::exists` would swallow a
/// permission error and silently hand back defaults instead.
pub fn load_feedback(beads_dir: &Path) -> Result<FeedbackData, FeedbackError> {
    let path = feedback_path(beads_dir);
    match std::fs::metadata(&path) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(default_feedback_data());
        }
        // Let the read below produce the same "failed to read feedback file"
        // error Go would.
        Err(_) => {}
    }
    let data = std::fs::read_to_string(&path).map_err(FeedbackError::Read)?;
    let loaded: FeedbackData = serde_json::from_str(&data).map_err(FeedbackError::Parse)?;
    Ok(normalize_loaded(loaded))
}

impl FeedbackData {
    /// Persist to `<beadsDir>/feedback.json` with Go's 2-space indent and 0644
    /// mode — Go `FeedbackData.Save` (feedback.go:178-195). Go stamps
    /// `UpdatedAt` under the same lock that guards the write, so do it here.
    pub fn save(&mut self, beads_dir: &Path) -> Result<(), FeedbackError> {
        self.updated_at = GoTime::now();
        let data = serde_json::to_string_pretty(self).map_err(FeedbackError::Marshal)?;
        let path = feedback_path(beads_dir);
        std::fs::write(&path, data).map_err(FeedbackError::Write)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644));
        }
        Ok(())
    }

    /// Add a feedback event and smooth the weight adjustments using the
    /// breakdown — Go `RecordFeedback` (feedback.go:198-230).
    pub fn record_feedback(
        &mut self,
        issue_id: &str,
        action: &str,
        score: f64,
        contributions: ScoreContributions,
    ) -> Result<(), FeedbackError> {
        if action != "accept" && action != "ignore" {
            return Err(FeedbackError::InvalidAction(action.to_string()));
        }

        self.events.push(FeedbackEvent {
            issue_id: issue_id.to_string(),
            action: action.to_string(),
            score,
            timestamp: GoTime::now(),
        });

        if action == "accept" {
            self.stats.total_accepted += 1;
            self.stats.avg_accept_score = update_running_average(
                self.stats.avg_accept_score,
                score,
                self.stats.total_accepted,
            );
        } else {
            self.stats.total_ignored += 1;
            self.stats.avg_ignore_score = update_running_average(
                self.stats.avg_ignore_score,
                score,
                self.stats.total_ignored,
            );
        }

        self.update_weight_adjustments(action, contributions);
        Ok(())
    }

    /// Whether enough feedback exists for the adjusted weights to be used in
    /// scoring — Go `FeedbackData.Applies` (feedback.go:314-318).
    pub fn applies(&self) -> bool {
        self.events.len() >= MIN_FEEDBACK_SAMPLES
    }

    /// The current weight adjustments keyed by factor name — Go
    /// `GetAdjustedWeights` (feedback.go:291-304).
    pub fn adjusted_weights(&self) -> BTreeMap<String, f64> {
        self.adjustments
            .iter()
            .map(|adj| (adj.name.clone(), adj.adjustment))
            .collect()
    }

    /// The original weights multiplied by their adjustments, renormalized to
    /// sum ~1.0 — Go `GetEffectiveWeights` (feedback.go:328-370). The
    /// multiply-then-divide order is load-bearing: Go's output carries the
    /// resulting float noise (e.g. `0.22000000000000003`) and a differential
    /// gate compares the exact digits.
    pub fn effective_weights(&self) -> BTreeMap<String, f64> {
        let base = default_weights().as_map();
        let adjustments = self.adjusted_weights();

        // Go iterates `baseWeights`, a `map[string]float64`, and Go randomises
        // map iteration order per run: the oracle returns a different
        // `effective_weights` ulp on consecutive runs of the same command
        // (0.19876168474138706 then 0.1987616847413871). No fixed order can
        // reproduce it, and the golden corpus predates the `feedback` block
        // entirely, so the last-ulp difference is not a defect. The declared
        // order below is a deliberate, stable choice.
        const ORDER: [&str; 8] = [
            "PageRank",
            "Betweenness",
            "BlockerRatio",
            "Staleness",
            "PriorityBoost",
            "TimeToImpact",
            "Urgency",
            "Risk",
        ];
        let mut pairs: Vec<(String, f64)> = Vec::with_capacity(ORDER.len());
        for name in ORDER {
            let w = base
                .get(name)
                .copied()
                .expect("every base weight is present in as_map");
            let adjusted = adjustments.get(name).copied().unwrap_or(w);
            pairs.push((name.to_string(), w * adjusted));
        }
        let mut effective: BTreeMap<String, f64> = BTreeMap::new();
        for (name, w) in pairs {
            effective.insert(name, w);
        }

        // The normalisation must walk the same order the total was summed in.
        let mut total = 0.0;
        for name in ORDER {
            total += effective[name];
        }
        if total > 0.0 {
            for name in ORDER {
                if let Some(w) = effective.get_mut(name) {
                    *w /= total;
                }
            }
        }
        effective
    }

    /// The effective weights as the struct a scoring caller applies — Go
    /// `FeedbackData.Weights` (feedback.go:323-325). It does not check
    /// [`Self::applies`]; callers decide whether the sample size justifies it.
    ///
    /// Go hands the result to `Analyzer.SetWeights` (priority.go:158-161).
    /// Rust has no long-lived analyzer object, so the equivalent wiring is to
    /// pass it to
    /// [`crate::impact::compute_impact_scores_with_weights`] — which is exactly
    /// where Go reads `a.Weights()` (priority.go:252).
    pub fn weights(&self) -> Weights {
        Weights::from_map(&self.effective_weights()).normalized()
    }

    /// Clear all feedback data, returning to defaults — Go
    /// `FeedbackData.Reset` (feedback.go:373-381). `CreatedAt` is preserved.
    pub fn reset(&mut self) {
        self.events = Vec::new();
        self.adjustments = default_weight_adjustments();
        self.stats = FeedbackStats::default();
        self.updated_at = GoTime::now();
    }

    /// A human-readable summary of the feedback state — Go
    /// `FeedbackData.Summary` (feedback.go:384-398).
    pub fn summary(&self) -> String {
        if self.events.is_empty() {
            return "No feedback recorded yet. Use --feedback-accept or --feedback-ignore to provide feedback."
                .to_string();
        }
        format!(
            "Feedback: {} accepted (avg score {:.2}), {} ignored (avg score {:.2}), {} total events",
            self.stats.total_accepted,
            self.stats.avg_accept_score,
            self.stats.total_ignored,
            self.stats.avg_ignore_score,
            self.events.len()
        )
    }

    /// The store formatted for robot output — Go `FeedbackData.ToJSON`
    /// (feedback.go:418-435). Field order is Go's struct order; the two maps
    /// marshal in sorted key order, which the `BTreeMap`s reproduce.
    pub fn to_json(&self) -> FeedbackJson {
        FeedbackJson {
            enabled: !self.events.is_empty(),
            applied: self.events.len() >= MIN_FEEDBACK_SAMPLES,
            min_samples: MIN_FEEDBACK_SAMPLES,
            total_events: self.events.len(),
            accepted_count: self.stats.total_accepted,
            ignored_count: self.stats.total_ignored,
            avg_accept_score: self.stats.avg_accept_score,
            avg_ignore_score: self.stats.avg_ignore_score,
            weight_adjustments: self.adjusted_weights(),
            effective_weights: self.effective_weights(),
            updated_at: self.updated_at,
        }
    }

    /// Exponential smoothing toward the observed adjustment — Go
    /// `updateWeightAdjustments` (feedback.go:244-288). Every tracked factor is
    /// touched, including one whose contribution was zero: Go still runs
    /// `alpha*target + (1-alpha)*old` for it, and the resulting float noise is
    /// part of the stored value.
    fn update_weight_adjustments(&mut self, action: &str, contributions: ScoreContributions) {
        let direction = if action == "ignore" { -1.0 } else { 1.0 };
        let now = GoTime::now();
        for adj in self.adjustments.iter_mut() {
            let Some(contribution) = contributions.contribution(&adj.name) else {
                continue;
            };
            // Max 10% change per feedback.
            let delta = direction * contribution * 0.1;
            let target = (adj.adjustment + delta).clamp(0.5, 2.0);
            adj.adjustment = SMOOTHING_ALPHA * target + (1.0 - SMOOTHING_ALPHA) * adj.adjustment;
            adj.samples += 1;
            adj.last_updated = now;
        }
    }
}

/// Go `updateRunningAverage` (feedback.go:233-238).
fn update_running_average(current_avg: f64, new_value: f64, count: usize) -> f64 {
    if count <= 1 {
        return new_value;
    }
    current_avg + (new_value - current_avg) / count as f64
}

/// Robot-output view of the store — Go `FeedbackJSON` (feedback.go:401-415).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FeedbackJson {
    pub enabled: bool,
    /// True when `total_events >= min_samples`, i.e. the effective weights
    /// below were actually used to score this output.
    pub applied: bool,
    pub min_samples: usize,
    pub total_events: usize,
    pub accepted_count: usize,
    pub ignored_count: usize,
    pub avg_accept_score: f64,
    pub avg_ignore_score: f64,
    pub weight_adjustments: BTreeMap<String, f64>,
    pub effective_weights: BTreeMap<String, f64>,
    pub updated_at: GoTime,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("bvr-feedback-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn contributions(v: f64) -> ScoreContributions {
        ScoreContributions {
            pagerank: v,
            betweenness: v,
            blocker_ratio: v,
            staleness: v,
            priority_boost: v,
            time_to_impact: v,
            urgency: v,
            risk: v,
        }
    }

    #[test]
    fn default_store_has_eight_adjustments_in_declared_order() {
        let fb = default_feedback_data();
        assert_eq!(fb.version, "1.0");
        assert_eq!(fb.adjustments.len(), 8);
        let names: Vec<&str> = fb.adjustments.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ADJUSTMENT_NAMES.to_vec());
        assert!(fb.adjustments.iter().all(|a| a.adjustment == 1.0));
        assert!(fb.adjustments.iter().all(|a| a.samples == 0));
        assert!(fb.events.is_empty());
    }

    #[test]
    fn load_missing_file_returns_defaults_without_error() {
        // Go feedback.go:87-89 — a missing file is not an error, which is what
        // lets --feedback-reset work on a clean checkout.
        let dir = temp_dir("missing");
        let fb = load_feedback(&dir).unwrap();
        assert_eq!(fb.adjustments.len(), 8);
        assert!(fb.events.is_empty());
        assert!(!fb.applies());
        assert!(!fb.to_json().enabled);
    }

    #[test]
    fn save_then_load_round_trips_through_the_file() {
        let dir = temp_dir("roundtrip");
        let mut fb = default_feedback_data();
        fb.record_feedback("a1", "accept", 0.397, contributions(0.5))
            .unwrap();
        fb.save(&dir).unwrap();

        let raw = std::fs::read_to_string(dir.join(FEEDBACK_FILE)).unwrap();
        // Go MarshalIndent(f, "", "  ").
        assert!(raw.contains("\n  \"version\": \"1.0\""), "{raw}");
        // Go writes the declared name order, not a sorted map order.
        let page_rank = raw.find("\"PageRank\"").unwrap();
        let risk = raw.find("\"Risk\"").unwrap();
        assert!(page_rank < risk);

        let back = load_feedback(&dir).unwrap();
        assert_eq!(back.events.len(), 1);
        assert_eq!(back.events[0].issue_id, "a1");
        assert_eq!(back.events[0].action, "accept");
        assert_eq!(back.stats.total_accepted, 1);
        assert!((back.stats.avg_accept_score - 0.397).abs() < 1e-12);
        assert_eq!(back.adjustments.len(), 8);
    }

    #[test]
    fn record_feedback_rejects_an_unknown_action() {
        let mut fb = default_feedback_data();
        let err = fb
            .record_feedback("a1", "maybe", 0.1, contributions(0.0))
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid action: maybe (must be 'accept' or 'ignore')"
        );
        assert!(fb.events.is_empty());
    }

    #[test]
    fn accept_boosts_and_ignore_shrinks_the_contributing_weights() {
        // Go feedback.go:244-288: delta = direction * contribution * 0.1, then
        // adj = 0.2*clamp(adj+delta) + 0.8*adj.
        let mut fb = default_feedback_data();
        fb.record_feedback("a1", "accept", 0.5, contributions(1.0))
            .unwrap();
        // target = 1.1 -> 0.2*1.1 + 0.8*1.0 = 1.02
        for adj in &fb.adjustments {
            assert!((adj.adjustment - 1.02).abs() < 1e-12, "{}", adj.name);
            assert_eq!(adj.samples, 1);
        }

        let mut fb2 = default_feedback_data();
        fb2.record_feedback("a2", "ignore", 0.5, contributions(1.0))
            .unwrap();
        // target = 0.9 -> 0.2*0.9 + 0.8*1.0 = 0.98
        for adj in &fb2.adjustments {
            assert!((adj.adjustment - 0.98).abs() < 1e-12, "{}", adj.name);
        }
    }

    #[test]
    fn weight_adjustment_clamps_to_the_go_range() {
        // Go feedback.go:281 — clamp(0.5, 2.0) happens before smoothing, so
        // repeated accepts saturate at 0.2*2.0 + 0.8*adj, never above 2.0.
        let mut fb = default_feedback_data();
        for _ in 0..200 {
            fb.record_feedback("a1", "accept", 1.0, contributions(1.0))
                .unwrap();
        }
        for adj in &fb.adjustments {
            assert!(adj.adjustment <= 2.0, "{} = {}", adj.name, adj.adjustment);
            assert!(adj.adjustment >= 0.5);
        }
    }

    #[test]
    fn applies_only_after_three_events() {
        // Go feedback.go:310, 317.
        let mut fb = default_feedback_data();
        assert!(!fb.applies());
        fb.record_feedback("a1", "accept", 0.1, contributions(0.0))
            .unwrap();
        assert!(!fb.applies());
        fb.record_feedback("a2", "accept", 0.1, contributions(0.0))
            .unwrap();
        assert!(!fb.applies());
        fb.record_feedback("a3", "accept", 0.1, contributions(0.0))
            .unwrap();
        assert!(fb.applies());
        assert!(fb.to_json().applied);
    }

    #[test]
    fn running_average_matches_go() {
        // Go feedback.go:233-238.
        let mut fb = default_feedback_data();
        fb.record_feedback("a", "accept", 1.0, contributions(0.0))
            .unwrap();
        assert_eq!(fb.stats.avg_accept_score, 1.0);
        fb.record_feedback("b", "accept", 0.0, contributions(0.0))
            .unwrap();
        assert!((fb.stats.avg_accept_score - 0.5).abs() < 1e-12);
        fb.record_feedback("c", "ignore", 0.25, contributions(0.0))
            .unwrap();
        assert!((fb.stats.avg_ignore_score - 0.25).abs() < 1e-12);
        assert_eq!(fb.stats.total_accepted, 2);
        assert_eq!(fb.stats.total_ignored, 1);
    }

    #[test]
    fn summary_matches_go_wording() {
        let fb = default_feedback_data();
        assert_eq!(
            fb.summary(),
            "No feedback recorded yet. Use --feedback-accept or --feedback-ignore to provide feedback."
        );
        let mut fb = fb;
        fb.record_feedback("a1", "accept", 0.397, contributions(0.5))
            .unwrap();
        assert_eq!(
            fb.summary(),
            "Feedback: 1 accepted (avg score 0.40), 0 ignored (avg score 0.00), 1 total events"
        );
    }

    #[test]
    fn reset_clears_events_and_restores_default_adjustments() {
        let dir = temp_dir("reset");
        let mut fb = default_feedback_data();
        fb.record_feedback("a1", "accept", 0.4, contributions(1.0))
            .unwrap();
        fb.record_feedback("a2", "ignore", 0.3, contributions(1.0))
            .unwrap();
        let created = fb.created_at;
        fb.reset();
        assert!(fb.events.is_empty());
        assert_eq!(fb.stats, FeedbackStats::default());
        assert!(fb.adjustments.iter().all(|a| a.adjustment == 1.0));
        assert_eq!(fb.created_at, created, "Reset preserves CreatedAt");
        // Go's --feedback-reset writes a fresh file even when none existed.
        fb.save(&dir).unwrap();
        assert!(dir.join(FEEDBACK_FILE).exists());
        let back = load_feedback(&dir).unwrap();
        assert!(back.events.is_empty());
        assert_eq!(back.adjustments.len(), 8);
    }

    #[test]
    fn to_json_key_order_matches_go_struct_order() {
        let fb = default_feedback_data();
        let raw = serde_json::to_string(&fb.to_json()).unwrap();
        let expected = [
            "\"enabled\"",
            "\"applied\"",
            "\"min_samples\"",
            "\"total_events\"",
            "\"accepted_count\"",
            "\"ignored_count\"",
            "\"avg_accept_score\"",
            "\"avg_ignore_score\"",
            "\"weight_adjustments\"",
            "\"effective_weights\"",
            "\"updated_at\"",
        ];
        let mut at = 0usize;
        for key in expected {
            let found = raw[at..]
                .find(key)
                .unwrap_or_else(|| panic!("{key} missing or out of order in {raw}"));
            at += found + key.len();
        }
        let j = fb.to_json();
        assert_eq!(j.min_samples, 3);
        assert!(!j.enabled);
        assert!(!j.applied);
    }

    #[test]
    fn effective_weights_use_go_multiply_then_divide() {
        // Go feedback.go:336-367 multiplies each base by its adjustment, sums,
        // then divides. Rust keeps those operations but iterates the maps in
        // sorted order, which is what Go does on most runs: Go ranges over a
        // `map[string]float64`, so its summation order is randomized per
        // process. Six runs of the Go snippet on an untouched store printed
        // these clean digits five times and the `0.22000000000000003` variant
        // once, so there is no single Go output to match and the deterministic
        // reading is the one to assert.
        let fb = default_feedback_data();
        let eff = fb.effective_weights();
        assert_eq!(eff["PageRank"], 0.22);
        assert_eq!(eff["Betweenness"], 0.2);
        assert_eq!(eff["BlockerRatio"], 0.13);
        assert_eq!(eff["Staleness"], 0.05);
        let total: f64 = eff.values().sum();
        assert!((total - 1.0).abs() < 1e-12, "{total}");

        // A non-uniform boost does move the proportions, and the renormalization
        // is what keeps the sum at 1.0.
        let mut boosted = default_feedback_data();
        boosted.adjustments[0].adjustment = 1.5;
        let eff = boosted.effective_weights();
        let base = default_weights();
        let expected_ratio = 1.5 * base.pagerank / base.risk;
        assert!((eff["PageRank"] / eff["Risk"] - expected_ratio).abs() < 1e-12);
    }

    #[test]
    fn weights_are_normalized_and_sum_to_one() {
        let mut fb = default_feedback_data();
        fb.record_feedback("a1", "accept", 0.5, contributions(1.0))
            .unwrap();
        let w = fb.weights();
        assert!((w.sum() - 1.0).abs() < 1e-12, "{}", w.sum());
        // Every factor was boosted by the same 1.02, so the effective weights
        // are still the documented proportions.
        assert!((w.pagerank - 0.22).abs() < 1e-12, "{}", w.pagerank);
        assert!((w.staleness - 0.05).abs() < 1e-12, "{}", w.staleness);
    }

    #[test]
    fn loading_reclamps_and_recomputes_stats() {
        // Go normalizeLoaded (feedback.go:105-150) repairs a hand-edited file:
        // unknown names drop out, out-of-range multipliers are clamped, and the
        // stats are recomputed from the events rather than trusted.
        let dir = temp_dir("reclamp");
        let raw = r#"{
  "version": "",
  "created_at": "0001-01-01T00:00:00Z",
  "updated_at": "0001-01-01T00:00:00Z",
  "events": [
    {"issue_id": "a", "action": "accept", "score": 0.4, "timestamp": "2024-01-01T00:00:00Z"},
    {"issue_id": "b", "action": "ignore", "score": 0.2, "timestamp": "2024-01-01T00:00:00Z"}
  ],
  "adjustments": [
    {"name": "PageRank", "adjustment": 9.0, "samples": 4, "last_updated": "0001-01-01T00:00:00Z"},
    {"name": "NotAFactor", "adjustment": 3.0, "samples": 9, "last_updated": "0004-01-01T00:00:00Z"}
  ],
  "stats": {"total_accepted": 99, "total_ignored": 99, "avg_accept_score": 9.0, "avg_ignore_score": 9.0}
}"#;
        std::fs::write(dir.join(FEEDBACK_FILE), raw).unwrap();

        let fb = load_feedback(&dir).unwrap();
        assert_eq!(fb.version, "1.0");
        assert_eq!(fb.stats.total_accepted, 1);
        assert_eq!(fb.stats.total_ignored, 1);
        assert!((fb.stats.avg_accept_score - 0.4).abs() < 1e-12);
        assert!((fb.stats.avg_ignore_score - 0.2).abs() < 1e-12);
        let names: BTreeSet<&str> = fb.adjustments.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, BTreeSet::from(ADJUSTMENT_NAMES));
        let pr = fb
            .adjustments
            .iter()
            .find(|a| a.name == "PageRank")
            .unwrap();
        assert_eq!(pr.adjustment, 2.0, "9.0 clamps down to the 2.0 ceiling");
        assert_eq!(pr.samples, 4);
        let risk = fb.adjustments.iter().find(|a| a.name == "Risk").unwrap();
        assert_eq!(risk.adjustment, 1.0, "unlisted factors reset to 1.0");
        assert_eq!(risk.samples, 0);
    }

    #[test]
    fn go_time_round_trips_and_uses_the_go_zero_value() {
        let zero = GoTime::zero();
        assert!(zero.is_zero());
        assert_eq!(zero.to_string(), GO_ZERO_TIME);
        let raw = serde_json::to_string(&zero).unwrap();
        assert_eq!(raw, format!("\"{GO_ZERO_TIME}\""));
        let back: GoTime = serde_json::from_str(&raw).unwrap();
        assert_eq!(back, zero);

        let now = GoTime::now();
        let raw = serde_json::to_string(&now).unwrap();
        // `GoTime::now()` stamps the LOCAL zone, the way Go's `time.Now()` does
        // — it does not force UTC. So the suffix is "Z" only on a machine whose
        // zone offset is zero; in +07:00 it is "+07:00". Asserting "Z" here
        // made the test fail on every non-UTC developer machine while passing on
        // CI, which is configured for UTC. What matters for parity is that the
        // rendered offset matches the machine's, and that it round-trips.
        let offset = jiff::Timestamp::now()
            .to_zoned(jiff::tz::TimeZone::try_system().unwrap_or(jiff::tz::TimeZone::UTC))
            .offset()
            .seconds();
        if offset == 0 {
            assert!(raw.ends_with("Z\""), "{raw}");
        } else {
            let (sign, abs) = if offset < 0 {
                ('-', -offset)
            } else {
                ('+', offset)
            };
            let tail = format!("{sign}{:02}:{:02}\"", abs / 3600, (abs % 3600) / 60);
            assert!(
                raw.ends_with(&tail),
                "expected local offset {tail}, got {raw}"
            );
        }
        let back: GoTime = serde_json::from_str(&raw).unwrap();
        assert_eq!(back, now);
        assert!(!back.is_zero());
    }
}
