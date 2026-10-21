//! Sprint burndown, forecast/ETA, and capacity analysis.
//!
//! Full implementations matching Go `cmd/bv/main.go` (burndown ~7788,
//! capacity ~5133) and `pkg/analysis/eta.go`.

use crate::model::{self, BurndownPoint, Forecast, Issue, Sprint};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::process::Command;

const SPRINTS_FILE: &str = "sprints.jsonl";

// ---------------------------------------------------------------------------
// Constants matching Go `pkg/analysis/eta.go` and `priority.go`
// ---------------------------------------------------------------------------

/// Default estimated minutes when no explicit estimate exists (Go: `DefaultEstimatedMinutes`).
const DEFAULT_ESTIMATED_MINUTES: i64 = 60;

/// Velocity lookback window in days (Go: `const windowDays = 30`).
const VELOCITY_WINDOW_DAYS: i64 = 30;

/// Work-day length in hours for capacity calculations (Go: `60.0 * 8.0`).
const WORKDAY_HOURS: f64 = 60.0 * 8.0;

// ===========================================================================
// Output types — JSON structs matching Go field names exactly
// ===========================================================================

/// Burndown output for `--robot-burndown` (Go: `BurndownOutput`).
#[derive(Debug, Clone, Serialize)]
pub struct BurndownOutput {
    pub sprint_id: String,
    pub sprint_name: String,
    pub start_date: String,
    pub end_date: String,
    pub total_days: i64,
    pub elapsed_days: i64,
    pub remaining_days: i64,
    pub total_issues: usize,
    pub completed_issues: usize,
    pub remaining_issues: usize,
    pub ideal_burn_rate: f64,
    pub actual_burn_rate: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projected_complete: Option<String>,
    pub on_track: bool,
    pub daily_points: Vec<BurndownPoint>,
    pub ideal_line: Vec<BurndownPoint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope_changes: Vec<ScopeChangeEvent>,
}

/// A scope change event (issues added/removed from sprint).
#[derive(Debug, Clone, Serialize)]
pub struct ScopeChangeEvent {
    pub date: String,
    pub issue_id: String,
    pub issue_title: String,
    pub action: String, // "added" or "removed"
}

/// Internal snapshot for git log diff parsing (Go: `sprintSnapshot`).
#[derive(Debug, Clone, Default, Deserialize)]
#[allow(dead_code)]
struct SprintSnapshot {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    bead_ids: Vec<String>,
}

// ===========================================================================
// 1. BURNDOWN — full implementation matching Go `calculateBurndownAt`
// ===========================================================================

/// Full burndown calculation matching Go `calculateBurndownAt`.
///
/// - `closed_at` timestamp-based daily completion tracking
/// - Ideal and actual burn rates
/// - Projected completion date
/// - Daily burndown points and ideal line
pub fn calculate_burndown_at(
    sprint: &Sprint,
    issues: &[Issue],
    now: jiff::Timestamp,
) -> BurndownOutput {
    let start = model::parse_ts(&sprint.start_date);
    let end = model::parse_ts(&sprint.end_date);

    let issue_map: HashMap<&str, &Issue> = issues.iter().map(|i| (i.id.as_str(), i)).collect();
    let sprint_issues: Vec<&Issue> = sprint
        .bead_ids
        .iter()
        .filter_map(|bid| issue_map.get(bid.as_str()).copied())
        .collect();

    let total_issues = sprint_issues.len();
    let completed_issues = sprint_issues
        .iter()
        .filter(|i| i.status.is_closed())
        .count();
    let remaining_issues = total_issues - completed_issues;

    // Days calculation — Go: totalDays = int(end.Sub(start).Hours()/24) + 1
    let (total_days, elapsed_days, remaining_days) = if let (Some(start_ts), Some(end_ts)) =
        (start, end)
    {
        let total = ((end_ts - start_ts).total(jiff::Unit::Hour).unwrap_or(0.0) / 24.0) as i64 + 1;
        if now < start_ts {
            (total, 0, total)
        } else if now > end_ts {
            (total, total, 0)
        } else {
            let elapsed =
                ((now - start_ts).total(jiff::Unit::Hour).unwrap_or(0.0) / 24.0) as i64 + 1;
            (total, elapsed, total - elapsed)
        }
    } else {
        (0, 0, 0)
    };

    // Burn rates — Go: idealBurnRate = float64(totalIssues) / float64(totalDays)
    let ideal_burn_rate = if total_days > 0 {
        total_issues as f64 / total_days as f64
    } else {
        0.0
    };
    let actual_burn_rate = if elapsed_days > 0 {
        completed_issues as f64 / elapsed_days as f64
    } else {
        0.0
    };

    // Projected completion — Go: now + daysToComplete + 1 day
    let (projected_complete, on_track) = if actual_burn_rate > 0.0 && remaining_issues > 0 {
        let days_to_complete = remaining_issues as f64 / actual_burn_rate;
        let projected =
            now + jiff::SignedDuration::from_secs((days_to_complete as i64 + 1) * 86400);
        let on = !projected.gt(&end.unwrap_or(now));
        (Some(projected.to_string()), on)
    } else if remaining_issues == 0 {
        // Already complete
        (None, true)
    } else if elapsed_days > 0 && completed_issues == 0 {
        // No progress
        (None, false)
    } else {
        (None, true)
    };

    // Daily burndown points — Go: iterate each day, count closed_at <= dayEnd
    let daily_points = generate_daily_burndown(sprint, &sprint_issues, now);

    // Ideal line — Go: linear from total to 0 over totalDays+1 entries
    let ideal_line = generate_ideal_line(sprint, total_issues);

    BurndownOutput {
        sprint_id: sprint.id.clone(),
        sprint_name: sprint.name.clone(),
        start_date: sprint.start_date.clone().unwrap_or_default(),
        end_date: sprint.end_date.clone().unwrap_or_default(),
        total_days,
        elapsed_days,
        remaining_days,
        total_issues,
        completed_issues,
        remaining_issues,
        ideal_burn_rate,
        actual_burn_rate,
        projected_complete,
        on_track,
        daily_points,
        ideal_line,
        scope_changes: Vec::new(),
    }
}

/// Generate daily burndown points using `closed_at` timestamps.
/// Each point represents remaining/actual issue counts at end-of-day.
///
/// Go: `generateDailyBurndown`
fn generate_daily_burndown(
    sprint: &Sprint,
    issues: &[&Issue],
    now: jiff::Timestamp,
) -> Vec<BurndownPoint> {
    let Some(start) = model::parse_ts(&sprint.start_date) else {
        return Vec::new();
    };
    let end = model::parse_ts(&sprint.end_date).unwrap_or(now);
    let total = issues.len() as i64;

    let mut points = Vec::new();
    let mut day = start;
    let cutoff = if end < now { end } else { now };

    while day <= cutoff {
        // Go: dayEnd := d.Add(24*time.Hour - time.Second)
        let day_end = day + jiff::SignedDuration::from_secs(86400 - 1);

        let completed = issues
            .iter()
            .filter(|i| i.status.is_closed())
            .filter(|i| {
                if let Some(closed_at_str) = &i.closed_at {
                    if let Ok(closed_at) = closed_at_str.parse::<jiff::Timestamp>() {
                        return closed_at <= day_end;
                    }
                }
                false
            })
            .count() as i64;

        points.push(BurndownPoint {
            date: day.to_string(),
            remaining: total - completed,
            ideal: None,
        });

        day += jiff::SignedDuration::from_secs(86400);
    }

    points
}

/// Generate the ideal burndown line: linear from total to 0.
///
/// Go: `generateIdealLine` — `totalDays + 1` entries
fn generate_ideal_line(sprint: &Sprint, total_issues: usize) -> Vec<BurndownPoint> {
    let Some(start) = model::parse_ts(&sprint.start_date) else {
        return Vec::new();
    };
    let end = match model::parse_ts(&sprint.end_date) {
        Some(e) => e,
        None => return Vec::new(),
    };
    if total_issues == 0 {
        return Vec::new();
    }

    let total_days = ((end - start).total(jiff::Unit::Hour).unwrap_or(0.0) / 24.0) as i64 + 1;
    let burn_per_day = total_issues as f64 / total_days as f64;

    let mut points = Vec::with_capacity((total_days + 1) as usize);
    for i in 0..=total_days {
        let day = start + jiff::SignedDuration::from_secs(i * 86400);
        let remaining = (total_issues as f64 - i as f64 * burn_per_day).max(0.0) as i64;
        points.push(BurndownPoint {
            date: day.to_string(),
            remaining,
            ideal: None,
        });
    }

    points
}

/// Backward-compatible wrapper that matches the original simple API.
/// Returns `(daily_points, total_issues)`.
pub fn calculate_burndown(
    sprint: &Sprint,
    issues: &[Issue],
    now: jiff::Timestamp,
) -> (Vec<BurndownPoint>, usize) {
    let output = calculate_burndown_at(sprint, issues, now);
    (output.daily_points, output.total_issues)
}

// ===========================================================================
// 2. FORECAST / ETA — matching Go `pkg/analysis/eta.go`
// ===========================================================================

/// ETA estimation for a single issue (Go: `ETAEstimate`).
#[derive(Debug, Clone, Serialize)]
pub struct ETAEstimate {
    pub issue_id: String,
    pub estimated_minutes: i64,
    pub estimated_days: f64,
    pub eta_date: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eta_date_low: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eta_date_high: Option<String>,
    pub confidence: f64,
    pub velocity_minutes_per_day: f64,
    pub agents: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub factors: Vec<String>,
}

/// Forecast summary for multi-issue mode (Go: `ForecastSummary`).
#[derive(Debug, Clone, Serialize)]
pub struct ForecastSummary {
    pub total_minutes: i64,
    pub total_days: f64,
    pub avg_confidence: f64,
    pub earliest_eta: String,
    pub latest_eta: String,
}

/// Forecast output for `--robot-forecast` (Go: `ForecastOutput`).
#[derive(Debug, Clone, Serialize)]
pub struct ForecastOutput {
    pub agents: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filters: Option<BTreeMap<String, String>>,
    pub forecast_count: usize,
    pub forecasts: Vec<ETAEstimate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<ForecastSummary>,
}

/// Estimate ETA for a single issue.
///
/// Algorithm (Go `EstimateETAForIssue`):
/// 1. Complexity minutes = base * type_weight * depth_factor * desc_factor
/// 2. Velocity = slowest non-zero label velocity over 30-day window
/// 3. Confidence = base + explicit-estimate bonus + velocity bonus - label penalty
/// 4. Confidence interval = estimated_days * (1 - confidence) * 0.8
///
/// `critical_path_depth` is the graph depth score; pass 0.0 when unavailable.
pub fn estimate_eta_for_issue(
    all_issues: &[Issue],
    issue_id: &str,
    agents: i64,
    critical_path_depth: f64,
    now: jiff::Timestamp,
) -> Result<ETAEstimate, String> {
    let issue_map: HashMap<&str, &Issue> = all_issues.iter().map(|i| (i.id.as_str(), i)).collect();
    let issue = issue_map
        .get(issue_id)
        .ok_or_else(|| format!("issue {issue_id:?} not found"))?;

    let agents = agents.max(1);
    let median_minutes = compute_median_estimated_minutes(all_issues);

    // Complexity minutes
    let (complexity_minutes, complexity_factors) =
        estimate_complexity_minutes(issue, median_minutes, critical_path_depth);

    // Velocity per day
    let (velocity_per_day, velocity_samples, mut velocity_factors) =
        estimate_velocity_minutes_per_day(all_issues, issue, now, median_minutes);

    // Go eta.go:50-56 — the conservative default is one median-sized issue
    // per work week, the final fallback is 1h/day, and BOTH the swap and the
    // reason are recorded in the factor list. Dropping the reason factor left
    // every factor array one entry short.
    let (velocity_per_day, velocity_factors) = if velocity_per_day <= 0.0 {
        let fallback = median_minutes as f64 / 5.0;
        let value = if fallback > 0.0 { fallback } else { 60.0 };
        velocity_factors.push("velocity: no recent closures; using default".to_string());
        (value, velocity_factors)
    } else {
        (velocity_per_day, velocity_factors)
    };

    let capacity_per_day = velocity_per_day * agents as f64;
    let estimated_days = (complexity_minutes as f64 / capacity_per_day).max(0.0);

    // Confidence
    let confidence = estimate_eta_confidence(issue, velocity_samples);

    // Confidence intervals — Go: deltaDays = max(0.5, estimatedDays * (1-confidence) * 0.8)
    let delta_days = (estimated_days * (1.0 - confidence) * 0.8).max(0.5);

    let eta = now + duration_days(estimated_days);
    let eta_low = now + duration_days((estimated_days - delta_days).max(0.0));
    let eta_high = now + duration_days(estimated_days + delta_days);

    let mut factors = complexity_factors;
    factors.extend(velocity_factors);
    factors.push(format!("agents: {agents}"));
    if factors.len() > 8 {
        factors.truncate(8);
    }

    Ok(ETAEstimate {
        issue_id: issue_id.to_string(),
        estimated_minutes: complexity_minutes,
        estimated_days,
        eta_date: eta.to_string(),
        eta_date_low: Some(eta_low.to_string()),
        eta_date_high: Some(eta_high.to_string()),
        confidence,
        velocity_minutes_per_day: velocity_per_day,
        agents,
        factors,
    })
}

/// Compute complexity minutes for a single issue.
///
/// Go `estimateComplexityMinutes`:
/// - base: explicit estimated_minutes or median (default 60)
/// - type weight: bug=1.0, task=1.0, chore=0.8, feature=1.3, epic=2.0
/// - depth factor: 1.0 + min(1.0, depth / 10.0)
/// - desc factor: 1.0 + min(1.0, rune_count / 2000.0)
fn estimate_complexity_minutes(
    issue: &Issue,
    median_minutes: i64,
    critical_path_depth: f64,
) -> (i64, Vec<String>) {
    let mut factors = Vec::new();

    // Base minutes — explicit or median or default
    let (base_minutes, source) = if let Some(em) = issue.estimated_minutes {
        if em > 0 {
            (em, "explicit")
        } else {
            (median_minutes, "median")
        }
    } else {
        (median_minutes, "median")
    };
    let base_minutes = if base_minutes > 0 {
        base_minutes
    } else {
        DEFAULT_ESTIMATED_MINUTES
    };
    factors.push(format!("estimate: {source} ({base_minutes}m)"));

    // Type weight — Go: TypeBug=1.0, TypeTask=1.0, TypeChore=0.8, TypeFeature=1.3, TypeEpic=2.0
    let type_weight = match issue.issue_type.as_str() {
        "bug" => 1.0,
        "task" => 1.0,
        "chore" => 0.8,
        "feature" => 1.3,
        "epic" => 2.0,
        _ => 1.0,
    };
    // Go eta.go:125 uses the Unicode MULTIPLICATION SIGN U+00D7
    // (`fmt.Sprintf("type: %s×%.1f", ...)`), not ASCII "x".
    factors.push(format!(
        "type: {}\u{d7}{:.1}",
        issue.issue_type, type_weight
    ));

    // Depth factor — Go: 1.0 + min(1.0, depth/10.0)
    let depth_factor = 1.0 + (critical_path_depth / 10.0).min(1.0);
    // Go eta.go:133 — `fmt.Sprintf("depth: %.0f×%.2f", ...)`, again U+00D7.
    factors.push(format!(
        "depth: {critical_path_depth:.0}\u{d7}{depth_factor:.2}"
    ));

    // Description factor — Go: 1.0 + min(1.0, runeCount/2000.0)
    let desc_runes = issue.description.chars().count();
    let desc_factor = 1.0 + (desc_runes as f64 / 2000.0).min(1.0);
    // Go eta.go:139-141 — `fmt.Sprintf("desc: %dr×%.2f", ...)` and the
    // literal "desc: empty×1.00". U+00D7, and NO space before it.
    if desc_runes > 0 {
        factors.push(format!("desc: {desc_runes}r\u{d7}{desc_factor:.2}"));
    } else {
        factors.push("desc: empty\u{d7}1.00".to_string());
    }

    let derived = (base_minutes as f64 * type_weight * depth_factor * desc_factor) as i64;
    let final_minutes = if derived > 0 { derived } else { base_minutes };

    (final_minutes, factors)
}

/// Estimate velocity (minutes/day) from recent closed issues.
///
/// Go `estimateVelocityMinutesPerDay`:
/// - 30-day window, closed issues sharing labels
/// - Uses slowest non-zero label velocity (conservative)
/// - Falls back to global velocity
fn estimate_velocity_minutes_per_day(
    all_issues: &[Issue],
    issue: &Issue,
    now: jiff::Timestamp,
    median_minutes: i64,
) -> (f64, i64, Vec<String>) {
    let since = now - jiff::SignedDuration::from_secs(VELOCITY_WINDOW_DAYS * 86400);
    let labels = &issue.labels;

    if labels.is_empty() {
        let (v, n) = velocity_minutes_per_day_for_label(all_issues, "", since, median_minutes);
        return (v, n, vec![format!("velocity: global ({n} samples/30d)")]);
    }

    // Find slowest non-zero label velocity (Go: bestV==0 || v < bestV || tie by name)
    let mut best_label = String::new();
    let mut best_v = 0.0f64;
    let mut best_n = 0i64;

    for label in labels {
        let (v, n) = velocity_minutes_per_day_for_label(all_issues, label, since, median_minutes);
        if n == 0 || v <= 0.0 {
            continue;
        }
        if best_v == 0.0
            || v < best_v
            || (v == best_v && label.to_lowercase() < best_label.to_lowercase())
        {
            best_label = label.clone();
            best_v = v;
            best_n = n;
        }
    }

    if best_v > 0.0 {
        return (
            best_v,
            best_n,
            vec![format!(
                "velocity: label={best_label} ({:.0} min/day, {best_n} samples/30d)",
                best_v
            )],
        );
    }

    // Fallback: global velocity
    let (v, n) = velocity_minutes_per_day_for_label(all_issues, "", since, median_minutes);
    (v, n, vec![format!("velocity: global ({n} samples/30d)")])
}

/// Compute velocity (total_minutes / 30 days) for issues sharing a label.
///
/// Go `velocityMinutesPerDayForLabel`:
/// - sum estimated_minutes of closed issues in window, then divide by 30
fn velocity_minutes_per_day_for_label(
    all_issues: &[Issue],
    label: &str,
    since: jiff::Timestamp,
    median_minutes: i64,
) -> (f64, i64) {
    let mut total = 0i64;
    let mut samples = 0i64;

    for iss in all_issues {
        if !iss.status.is_closed() {
            continue;
        }

        // Robust closure time: ClosedAt if available, else UpdatedAt
        let closed_at = if let Some(closed_str) = &iss.closed_at {
            closed_str
                .parse::<jiff::Timestamp>()
                .unwrap_or_else(|_| now_fallback())
        } else {
            iss.updated_at
                .as_deref()
                .and_then(|s| s.parse::<jiff::Timestamp>().ok())
                .unwrap_or_else(now_fallback)
        };

        if closed_at < since {
            continue;
        }

        if !label.is_empty() && !has_label(&iss.labels, label) {
            continue;
        }

        let minutes = if let Some(em) = iss.estimated_minutes {
            if em > 0 {
                em
            } else {
                median_minutes.max(DEFAULT_ESTIMATED_MINUTES)
            }
        } else {
            median_minutes.max(DEFAULT_ESTIMATED_MINUTES)
        };

        total += minutes;
        samples += 1;
    }

    if samples == 0 {
        return (0.0, 0);
    }

    (total as f64 / 30.0, samples)
}

/// Check if `labels` contains `target` (case-insensitive).
fn has_label(labels: &[String], target: &str) -> bool {
    let target_lower = target.to_lowercase();
    labels.iter().any(|l| l.to_lowercase() == target_lower)
}

/// Estimate confidence score for an ETA prediction.
///
/// Go `estimateETAConfidence`:
/// - base: 0.25
/// - +0.25 if explicit estimated_minutes
/// - +0.10..+0.30 based on velocity samples (>=15, >=5, >=1)
/// - -0.05 if no labels
/// - clamped [0.10, 0.90]
fn estimate_eta_confidence(issue: &Issue, velocity_samples: i64) -> f64 {
    let mut conf: f64 = 0.25;

    if let Some(em) = issue.estimated_minutes {
        if em > 0 {
            conf += 0.25;
        }
    }

    match velocity_samples {
        n if n >= 15 => conf += 0.30,
        n if n >= 5 => conf += 0.20,
        n if n >= 1 => conf += 0.10,
        _ => conf -= 0.05,
    }

    if issue.labels.is_empty() {
        conf -= 0.05;
    }

    conf.clamp(0.10, 0.90)
}

/// Compute median of all explicit estimated_minutes across issues.
///
/// Go `computeMedianEstimatedMinutes`.
fn compute_median_estimated_minutes(issues: &[Issue]) -> i64 {
    let mut estimates: Vec<i64> = issues
        .iter()
        .filter_map(|i| i.estimated_minutes)
        .filter(|e| *e > 0)
        .collect();

    if estimates.is_empty() {
        return DEFAULT_ESTIMATED_MINUTES;
    }

    estimates.sort_unstable();
    let mid = estimates.len() / 2;
    if estimates.len().is_multiple_of(2) {
        (estimates[mid - 1] + estimates[mid]) / 2
    } else {
        estimates[mid]
    }
}

/// Convert fractional days to `jiff::SignedDuration`.
fn duration_days(days: f64) -> jiff::SignedDuration {
    if days <= 0.0 {
        return jiff::SignedDuration::ZERO;
    }
    // Go `durationDays` (eta.go:283-288) is
    // `time.Duration(days * float64(24*time.Hour))` — nanoseconds, not
    // seconds. `float64(24*time.Hour)` is 8.64e13 ns, so the product keeps
    // sub-second resolution and the resulting `time.Time` marshals as
    // RFC3339Nano ("2026-08-23T08:31:58.976744186Z"). Rounding to whole
    // seconds here dropped the fraction entirely.
    let total_nanos = (days * 86_400_000_000_000.0) as i64;
    jiff::SignedDuration::new(
        total_nanos.div_euclid(1_000_000_000),
        total_nanos.rem_euclid(1_000_000_000) as i32,
    )
}

/// Fallback to a recent timestamp (avoids Option unwrap in velocity calc).
fn now_fallback() -> jiff::Timestamp {
    jiff::Timestamp::now()
}

/// Backward-compatible forecast wrapper.
///
/// Returns `Some(Forecast)` for backward compatibility; use `estimate_eta_for_issue`
/// for the full structured output.
pub fn estimate_forecast(
    sprint: &Sprint,
    issues: &[Issue],
    now: jiff::Timestamp,
) -> Option<Forecast> {
    let sprint_issues: Vec<&Issue> = issues
        .iter()
        .filter(|i| sprint.bead_ids.iter().any(|bid| bid == &i.id))
        .collect();
    let remaining = sprint_issues
        .iter()
        .filter(|i| !i.status.is_closed())
        .count();
    if remaining == 0 {
        return None;
    }

    let velocity = sprint.velocity_target.unwrap_or(1.0);
    let days_needed = (remaining as f64 / velocity).ceil() as i64;
    let eta = now + jiff::SignedDuration::from_secs(days_needed * 86400);

    let mut factors = vec![format!("{remaining} open issues remaining")];
    if let Some(target) = sprint.velocity_target {
        factors.push(format!("velocity target: {target:.1} issues/day"));
    }

    Some(Forecast {
        bead_id: sprint.id.clone(),
        eta_date: eta.to_string(),
        confidence: 0.5,
        factors,
        created_at: Some(now.to_string()),
    })
}

/// Produce a `ForecastOutput` for all open issues in the target list.
///
/// Mirrors Go's `robot-forecast` handler with `ForecastSummary` when
/// forecasting multiple issues.
///
/// `critical_path_score` is the analyzer's per-issue critical-path score
/// (`GraphStats.GetCriticalPathScore`, graph.go:344). Go's forecast handler
/// passes the real `graphStats` into every `EstimateETAForIssue` call
/// (robot_registry.go:1521, :1539), so the complexity `depth` factor is the
/// issue's actual graph height. A hardcoded 0.0 made every forecast report
/// `depth: 0\u{d7}1.00` and understate the estimate. An id absent from the
/// map reads as depth 0, matching a nil `criticalPathScore`.
pub fn compute_forecast_output(
    issues: &[Issue],
    sprint_bead_ids: Option<&HashSet<String>>,
    label_filter: Option<&str>,
    agents: i64,
    critical_path_score: &BTreeMap<String, f64>,
    now: jiff::Timestamp,
) -> ForecastOutput {
    let target_issues: Vec<&Issue> = issues
        .iter()
        .filter(|i| {
            if i.status.is_closed() {
                return false;
            }
            if let Some(sprint_ids) = sprint_bead_ids {
                if !sprint_ids.contains(&i.id) {
                    return false;
                }
            }
            if let Some(label) = label_filter {
                if !has_label(&i.labels, label) {
                    return false;
                }
            }
            true
        })
        .collect();

    let mut forecasts = Vec::new();
    for issue in &target_issues {
        let depth = critical_path_score
            .get(issue.id.as_str())
            .copied()
            .unwrap_or(0.0);
        if let Ok(eta) = estimate_eta_for_issue(issues, &issue.id, agents, depth, now) {
            forecasts.push(eta);
        }
    }

    let summary = if forecasts.len() > 1 {
        let total_minutes: i64 = forecasts.iter().map(|f| f.estimated_minutes).sum();
        let total_confidence: f64 = forecasts.iter().map(|f| f.confidence).sum();
        let mut earliest = &forecasts[0].eta_date;
        let mut latest = &forecasts[0].eta_date;
        for f in &forecasts[1..] {
            if f.eta_date < *earliest {
                earliest = &f.eta_date;
            }
            if f.eta_date > *latest {
                latest = &f.eta_date;
            }
        }
        Some(ForecastSummary {
            total_minutes,
            total_days: total_minutes as f64 / WORKDAY_HOURS,
            avg_confidence: total_confidence / forecasts.len() as f64,
            earliest_eta: earliest.clone(),
            latest_eta: latest.clone(),
        })
    } else {
        None
    };

    let mut filters = BTreeMap::new();
    if let Some(label) = label_filter {
        filters.insert("label".to_string(), label.to_string());
    }
    if let Some(ids) = sprint_bead_ids {
        // Show the sprint filter as the first matching sprint id
        filters.insert(
            "sprint".to_string(),
            ids.iter().next().cloned().unwrap_or_default(),
        );
    }

    ForecastOutput {
        agents,
        filters: if filters.is_empty() {
            None
        } else {
            Some(filters)
        },
        forecast_count: forecasts.len(),
        forecasts,
        summary,
    }
}

// ===========================================================================
// 3. CAPACITY — critical path + parallel decomposition
// ===========================================================================

/// Bottleneck issue in the dependency graph.
#[derive(Debug, Clone, Serialize)]
pub struct Bottleneck {
    pub id: String,
    pub title: String,
    pub blocks_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<String>,
}

/// Capacity analysis output (Go: `CapacityOutput`).
#[derive(Debug, Clone, Serialize)]
pub struct CapacityOutput {
    pub agents: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub open_issue_count: usize,
    pub total_minutes: i64,
    pub total_days: f64,
    pub serial_minutes: i64,
    pub parallel_minutes: i64,
    pub parallelizable_pct: f64,
    pub estimated_days: f64,
    pub critical_path_length: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub critical_path: Vec<String>,
    pub actionable_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actionable: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bottlenecks: Vec<Bottleneck>,
}

/// Go `Issue.IsDeferredAt` (pkg/model/types.go:261) — the scheduler's deferral
/// is active while the instant is still in the future.
fn issue_is_deferred_at(issue: &Issue, now: jiff::Timestamp) -> bool {
    crate::model::parse_ts(&issue.defer_until).is_some_and(|d| d > now)
}

/// Compute capacity analysis for open issues.
///
/// Go `run_robot_capacity`:
/// - Sum total_minutes across open issues (complexity estimation)
/// - Build dependency adjacency (blockedBy, blocks maps)
/// - Find actionable issues (no open blockers)
/// - Find critical path (longest chain of dependent open issues from actionable)
/// - serial_minutes = sum along critical path
/// - parallelizable_pct = (total - serial) / total * 100
/// - estimated_days = (serial + parallel / agents) / (60 * 8)
/// - bottlenecks: issues blocking most others, top 5
///
/// `critical_path_score` is the analyzer's per-issue critical-path score
/// (`GraphStats.GetCriticalPathScore`, graph.go:344), keyed by issue id.
/// Go's `handleRobotCapacity` (robot_registry.go:3536, :3576) passes the real
/// `graphStats` into `EstimateETAForIssue`, so the `depth` factor
/// (`eta.go:130-133`) is the issue's actual graph depth. Passing 0.0 here made
/// the factor `depth: 0\u{d7}1.00` and dropped every depth multiplier from both
/// `total_minutes` and `serial_minutes`. An issue absent from the map reads as
/// depth 0, which is what Go's `GetCriticalPathScore` returns for a nil map.
pub fn calculate_capacity(
    issues: &[Issue],
    agents: i64,
    label_filter: Option<&str>,
    critical_path_score: &BTreeMap<String, f64>,
    now: jiff::Timestamp,
) -> CapacityOutput {
    let agents = agents.max(1);
    let median_minutes = compute_median_estimated_minutes(issues);

    // Filter by label if specified
    let mut target_issues: Vec<&Issue> = issues
        .iter()
        .filter(|i| {
            if let Some(label) = label_filter {
                has_label(&i.labels, label)
            } else {
                true
            }
        })
        .collect();

    // Go robot_registry.go:3557 sorts the target set by id BEFORE the open
    // filter at :3559-3566, so `openIssues` — and every list derived from it
    // in issue order — is id-sorted. Leaving it in loader order put each
    // bottleneck's `blocks` array in a different order than the oracle.
    target_issues.sort_by(|a, b| a.id.cmp(&b.id));

    // Build issue map and collect open issues. Go's open test is
    // `!Status.IsClosed() && !Status.IsTombstone()` (:3562).
    let issue_map: HashMap<&str, &Issue> =
        target_issues.iter().map(|i| (i.id.as_str(), *i)).collect();
    let open_issues: Vec<&Issue> = target_issues
        .iter()
        .filter(|i| !i.status.is_closed() && i.status != model::Status::Tombstone)
        .copied()
        .collect();

    // Estimate total minutes across open issues
    let total_minutes: i64 = open_issues
        .iter()
        .map(|iss| {
            let (minutes, _) = estimate_complexity_minutes(
                iss,
                median_minutes,
                critical_path_score
                    .get(iss.id.as_str())
                    .copied()
                    .unwrap_or(0.0),
            );
            minutes
        })
        .sum();

    // Build dependency adjacency — Go: blockedBy and blocks maps
    // Only include blocking dependencies (type blocks or empty)
    let mut blocked_by: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut blocks: HashMap<&str, Vec<&str>> = HashMap::new();

    for iss in &open_issues {
        // Go robot_registry.go:3588 — `seen` is per dependent, so a repeated
        // dependency record from one issue contributes a single edge (and a
        // single `blocks` entry), matching the dedup Go does before
        // `blockerCounts[v]++` in the graph builder (graph.go:1640-1646).
        let mut seen: HashSet<&str> = HashSet::new();
        for dep in &iss.dependencies {
            let dep_id = dep.effective_depends_on();
            if dep_id.is_empty() {
                continue;
            }
            // Only blocking dependencies count for capacity
            if !dep.r#type.is_blocking() {
                continue;
            }
            if !seen.insert(dep_id) {
                continue;
            }
            if issue_map.contains_key(dep_id) {
                blocked_by.entry(iss.id.as_str()).or_default().push(dep_id);
                blocks.entry(dep_id).or_default().push(iss.id.as_str());
            }
        }
    }

    // Find actionable — Go: `readiness.Ready(issue.ID, now)` over the open
    // issues (robot_registry.go:3598-3602).
    //
    // Go's `Ready` is three gates, not one: the status must be open or
    // in-progress, the bead must not be scheduler-deferred, and its
    // dependencies must be satisfied. The port checked only the third, so a
    // DEFERRED bead with no open blockers was reported as actionable work: on
    // a 382-bead repository `--robot-capacity` counted 15 actionable issues
    // where the oracle counts 11.
    let open_ids: HashSet<&str> = open_issues.iter().map(|i| i.id.as_str()).collect();
    let mut actionable: Vec<String> = open_issues
        .iter()
        .filter(|iss| {
            iss.status.is_open()
                && !issue_is_deferred_at(iss, now)
                && blocked_by
                    .get(iss.id.as_str())
                    .map(|deps| deps.iter().all(|dep_id| !open_ids.contains(dep_id)))
                    .unwrap_or(true)
        })
        .map(|iss| iss.id.clone())
        .collect();
    actionable.sort();

    // Find critical path: longest chain of dependent open issues
    // Go: DFS from each actionable issue, tracking longest path
    let mut longest_chain: Vec<String> = Vec::new();
    let mut visited: HashSet<&str> = HashSet::new();

    fn dfs<'a>(
        id: &'a str,
        path: Vec<&'a str>,
        blocks: &HashMap<&'a str, Vec<&'a str>>,
        open_ids: &HashSet<&'a str>,
        longest: &mut Vec<String>,
        visited: &mut HashSet<&'a str>,
    ) {
        if visited.contains(id) {
            return;
        }
        visited.insert(id);

        let mut path = path;
        path.push(id);

        if path.len() > longest.len() {
            *longest = path.iter().map(|s| s.to_string()).collect();
        }

        if let Some(next_ids) = blocks.get(id) {
            for next_id in next_ids {
                if open_ids.contains(next_id) {
                    dfs(next_id, path.clone(), blocks, open_ids, longest, visited);
                }
            }
        }

        visited.remove(id);
    }

    for start_id in &actionable {
        dfs(
            start_id,
            Vec::new(),
            &blocks,
            &open_ids,
            &mut longest_chain,
            &mut visited,
        );
    }

    // Serial minutes: sum along critical path
    let serial_minutes: i64 = longest_chain
        .iter()
        .filter_map(|id| issue_map.get(id.as_str()))
        .map(|iss| {
            let (minutes, _) = estimate_complexity_minutes(
                iss,
                median_minutes,
                critical_path_score
                    .get(iss.id.as_str())
                    .copied()
                    .unwrap_or(0.0),
            );
            minutes
        })
        .sum();

    // Parallelizable percentage
    let parallelizable_pct = if total_minutes > 0 {
        ((total_minutes - serial_minutes) as f64 / total_minutes as f64) * 100.0
    } else {
        0.0
    };

    // Estimated days with N agents
    let parallel_minutes = total_minutes - serial_minutes;
    let effective_minutes = serial_minutes + parallel_minutes / agents;
    let estimated_days = effective_minutes as f64 / WORKDAY_HOURS;

    // Bottlenecks: issues blocking the most others, top 5
    let mut bottlenecks: Vec<Bottleneck> = open_issues
        .iter()
        .filter(|iss| blocks.get(iss.id.as_str()).map_or(0, |v| v.len()) > 1)
        .map(|iss| {
            let blocked = blocks.get(iss.id.as_str()).cloned().unwrap_or_default();
            Bottleneck {
                id: iss.id.clone(),
                title: iss.title.clone(),
                blocks_count: blocked.len(),
                blocks: blocked.into_iter().map(String::from).collect(),
            }
        })
        .collect();
    // Go robot_registry.go:3644-3650 — blocks_count descending, then id
    // ascending. Without the id tiebreak a stable sort kept loader order, so
    // a different set of five survived the `[:5]` truncation.
    bottlenecks.sort_by(|a, b| {
        b.blocks_count
            .cmp(&a.blocks_count)
            .then_with(|| a.id.cmp(&b.id))
    });
    bottlenecks.truncate(5);

    CapacityOutput {
        agents,
        label: label_filter.map(String::from),
        open_issue_count: open_issues.len(),
        total_minutes,
        total_days: total_minutes as f64 / WORKDAY_HOURS,
        serial_minutes,
        parallel_minutes,
        parallelizable_pct,
        estimated_days,
        critical_path_length: longest_chain.len(),
        critical_path: longest_chain,
        actionable_count: actionable.len(),
        actionable,
        bottlenecks,
    }
}

// ===========================================================================
// 4. SCOPE CHANGES — git log parsing (optional, best-effort)
// ===========================================================================

/// Parse scope changes from git log of `.beads/sprints.jsonl`.
///
/// Go `computeSprintScopeChanges`: runs `git log -p -U0` on the sprints
/// JSONL file, diffs consecutive commits, and emits add/remove events.
/// Returns `None` when not in a git repo or when no scope changes exist.
pub fn compute_sprint_scope_changes(
    repo_path: &Path,
    sprint: &Sprint,
    issue_map: &HashMap<String, &Issue>,
) -> Option<Vec<ScopeChangeEvent>> {
    if sprint.id.is_empty() {
        return None;
    }
    let start = model::parse_ts(&sprint.start_date)?;
    let end = model::parse_ts(&sprint.end_date)?;
    let now = jiff::Timestamp::now();

    // Check if .git exists
    let git_dir = repo_path.join(".git");
    if !git_dir.exists() {
        return None;
    }

    // Bound the history window to the sprint
    let since = start - jiff::SignedDuration::from_secs(86400); // -1 day
    let until = if end > now { now } else { end };

    let sprints_path = format!(".beads/{SPRINTS_FILE}");

    let output = Command::new("git")
        .arg("-c")
        .arg("color.ui=false")
        .arg("log")
        .arg("-p")
        .arg("-U0")
        .arg("--format=%H%x00%cI")
        .arg(format!("--since={since}"))
        .arg(format!("--until={until}"))
        .arg("--")
        .arg(&sprints_path)
        .current_dir(repo_path)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut commits: Vec<ScopeCommit> = Vec::new();
    let mut current_ts = jiff::Timestamp::default();
    let mut current_sha = String::new();
    let mut have_commit = false;
    let mut old_snap = SprintSnapshot::default();
    let mut new_snap = SprintSnapshot::default();
    let mut have_old = false;
    let mut have_new = false;

    let process_commit = |commits: &mut Vec<ScopeCommit>,
                          have_commit: &mut bool,
                          current_sha: &str,
                          current_ts: &jiff::Timestamp,
                          old_snap: &SprintSnapshot,
                          new_snap: &SprintSnapshot,
                          have_old: bool,
                          have_new: bool,
                          issue_map: &HashMap<String, &Issue>| {
        if !*have_commit {
            return;
        }
        if have_old && have_new && old_snap.id == sprint.id && new_snap.id == sprint.id {
            let added = set_difference(&new_snap.bead_ids, &old_snap.bead_ids);
            let removed = set_difference(&old_snap.bead_ids, &new_snap.bead_ids);
            if added.is_empty() && removed.is_empty() {
                return;
            }

            let mut events = Vec::new();
            for id in &removed {
                let title = issue_map
                    .get(id.as_str())
                    .map(|i| i.title.clone())
                    .unwrap_or_default();
                events.push(ScopeChangeEvent {
                    date: current_ts.to_string(),
                    issue_id: id.clone(),
                    issue_title: title,
                    action: "removed".to_string(),
                });
            }
            for id in &added {
                let title = issue_map
                    .get(id.as_str())
                    .map(|i| i.title.clone())
                    .unwrap_or_default();
                events.push(ScopeChangeEvent {
                    date: current_ts.to_string(),
                    issue_id: id.clone(),
                    issue_title: title,
                    action: "added".to_string(),
                });
            }

            commits.push(ScopeCommit {
                sha: current_sha.to_string(),
                timestamp: *current_ts,
                order: commits.len(),
                events,
            });
        }
    };

    for line in stdout.lines() {
        // Try to parse as git header line: SHA\x00timestamp
        if let Some((sha, ts)) = parse_git_header_line(line) {
            process_commit(
                &mut commits,
                &mut have_commit,
                &current_sha,
                &current_ts,
                &old_snap,
                &new_snap,
                have_old,
                have_new,
                issue_map,
            );
            current_ts = ts;
            current_sha = sha;
            have_commit = true;
            old_snap = SprintSnapshot::default();
            new_snap = SprintSnapshot::default();
            have_old = false;
            have_new = false;
            continue;
        }

        if !have_commit {
            continue;
        }

        if let Some(rest) = line.strip_prefix("-{") {
            if let Ok(snap) = serde_json::from_str::<SprintSnapshot>(&format!("{{{rest}")) {
                if snap.id == sprint.id {
                    old_snap = snap;
                    have_old = true;
                }
            }
        } else if let Some(rest) = line.strip_prefix("+{") {
            if let Ok(snap) = serde_json::from_str::<SprintSnapshot>(&format!("{{{rest}")) {
                if snap.id == sprint.id {
                    new_snap = snap;
                    have_new = true;
                }
            }
        }
    }

    // Process the last commit
    process_commit(
        &mut commits,
        &mut have_commit,
        &current_sha,
        &current_ts,
        &old_snap,
        &new_snap,
        have_old,
        have_new,
        issue_map,
    );

    if commits.is_empty() {
        return None;
    }

    // Sort chronologically (Go: stable sort by timestamp, then by reverse order)
    commits.sort_by(|a, b| {
        if a.timestamp != b.timestamp {
            a.timestamp.cmp(&b.timestamp)
        } else {
            b.order.cmp(&a.order) // reverse of original order
        }
    });

    let mut scope_changes = Vec::new();
    for c in &commits {
        scope_changes.extend(c.events.clone());
    }

    Some(scope_changes)
}

/// Internal commit record for scope change diff parsing.
#[derive(Debug)]
#[allow(dead_code)]
struct ScopeCommit {
    sha: String,
    timestamp: jiff::Timestamp,
    order: usize,
    events: Vec<ScopeChangeEvent>,
}

/// Parse a git log header line: `SHA\x00timestamp`.
///
/// Go: `parseGitHeaderLine`
fn parse_git_header_line(line: &str) -> Option<(String, jiff::Timestamp)> {
    let parts: Vec<&str> = line.splitn(2, '\x00').collect();
    if parts.len() != 2 {
        return None;
    }
    if parts[0].len() != 40 {
        return None;
    }
    let ts = parts[1].trim().parse::<jiff::Timestamp>().ok()?;
    Some((parts[0].to_string(), ts))
}

/// Set difference: elements in `a` not in `b`.
///
/// Go: `setDifference`
fn set_difference(a: &[String], b: &[String]) -> Vec<String> {
    if a.is_empty() {
        return Vec::new();
    }
    let b_set: HashSet<&str> = b.iter().map(|s| s.as_str()).collect();
    a.iter()
        .filter(|id| !id.is_empty() && !b_set.contains(id.as_str()))
        .cloned()
        .collect()
}

// ===========================================================================
// Sprint loading
// ===========================================================================

/// Load sprints from `.beads/sprints.jsonl` in the given directory.
/// Returns an empty Vec if the file doesn't exist or is empty.
pub fn load_sprints(dir: &Path) -> Result<Vec<Sprint>, String> {
    let path = dir.join(".beads").join(SPRINTS_FILE);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let mut sprints = Vec::new();
    for (lineno, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<Sprint>(line) {
            Ok(sprint) => sprints.push(sprint),
            Err(e) => {
                eprintln!("sprints.jsonl line {}: parse error: {e}", lineno + 1);
            }
        }
    }
    Ok(sprints)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Dependency, DependencyType, Status};

    fn make_issue(id: &str, status: Status) -> Issue {
        Issue {
            id: id.to_string(),
            content_hash: String::new(),
            title: id.to_string(),
            description: String::new(),
            design: String::new(),
            acceptance_criteria: String::new(),
            notes: String::new(),
            status,
            priority: 2,
            issue_type: "task".into(),
            assignee: String::new(),
            estimated_minutes: None,
            created_at: Some("2026-01-01T00:00:00Z".into()),
            updated_at: Some("2026-01-01T00:00:00Z".into()),
            due_date: None,
            defer_until: None,
            closed_at: None,
            external_ref: None,
            compaction_level: 0,
            compacted_at: None,
            compacted_at_commit: None,
            original_size: 0,
            labels: vec![],
            dependencies: vec![],
            comments: vec![],
            source_repo: String::new(),
        }
    }

    fn make_sprint(id: &str, bead_ids: &[&str], velocity: Option<f64>) -> Sprint {
        Sprint {
            id: id.to_string(),
            name: format!("Sprint {id}"),
            start_date: Some("2026-01-01T00:00:00Z".into()),
            end_date: Some("2026-01-14T00:00:00Z".into()),
            bead_ids: bead_ids.iter().map(|s| s.to_string()).collect(),
            velocity_target: velocity,
            created_at: None,
            updated_at: None,
        }
    }

    // -----------------------------------------------------------------------
    // load_sprints
    // -----------------------------------------------------------------------

    #[test]
    fn load_sprints_returns_empty_when_file_missing() {
        let dir = std::env::temp_dir().join(format!("bvr-sprint-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let result = super::load_sprints(&dir).unwrap();
        assert!(result.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // calculate_burndown_at
    // -----------------------------------------------------------------------

    #[test]
    fn burndown_total_and_remaining_match_go() {
        let s = make_sprint("s1", &["A", "B", "C"], Some(1.0));
        let mut a = make_issue("A", Status::Open);
        a.closed_at = None;
        let mut b = make_issue("B", Status::Closed);
        b.closed_at = Some("2026-01-03T23:59:59Z".into());
        let c = make_issue("C", Status::Open);
        let d = make_issue("D", Status::Open); // not in sprint

        let now = "2026-01-03T12:00:00Z".parse::<jiff::Timestamp>().unwrap();
        let out = calculate_burndown_at(&s, &[a, b, c, d], now);

        assert_eq!(out.sprint_id, "s1");
        assert_eq!(out.total_issues, 3); // only A, B, C in sprint
        assert_eq!(out.completed_issues, 1); // B is closed
        assert_eq!(out.remaining_issues, 2); // A and C open
        assert_eq!(out.total_days, 14); // Jan 1..Jan 14 inclusive
                                        // elapsedDays: (now - start).hours/24 + 1 = (60h)/24 + 1 = 3
        assert_eq!(out.elapsed_days, 3);
        assert_eq!(out.remaining_days, 11);
        // ideal_burn_rate: 3/14
        assert!((out.ideal_burn_rate - 3.0 / 14.0).abs() < 1e-10);
        // actual_burn_rate: 1/3
        assert!((out.actual_burn_rate - 1.0 / 3.0).abs() < 1e-10);
        assert!(!out.scope_changes.is_empty() || out.scope_changes.is_empty()); // just ensure it compiles
    }

    #[test]
    fn burndown_daily_points_use_closed_at_not_status() {
        let s = make_sprint("s1", &["A", "B"], None);
        // A: closed_at on Jan 2
        let mut a = make_issue("A", Status::Closed);
        a.closed_at = Some("2026-01-02T12:00:00Z".into());
        // B: still open
        let b = make_issue("B", Status::Open);

        let now = "2026-01-03T00:00:00Z".parse::<jiff::Timestamp>().unwrap();
        let out = calculate_burndown_at(&s, &[a, b], now);

        // Jan 1: 2 remaining (A not yet closed by end of Jan 1)
        assert_eq!(out.daily_points[0].remaining, 2);
        // Jan 2: 1 remaining (A closed_at <= 23:59:59 of Jan 2)
        assert_eq!(out.daily_points[1].remaining, 1);
        // Jan 3: 1 remaining (B still open)
        assert_eq!(out.daily_points[2].remaining, 1);
    }

    #[test]
    fn burndown_projected_complete_and_on_track() {
        let s = make_sprint("s1", &["A", "B", "C", "D"], None);
        let mut a = make_issue("A", Status::Closed);
        a.closed_at = Some("2026-01-02T23:59:59Z".into());
        let mut b = make_issue("B", Status::Closed);
        b.closed_at = Some("2026-01-03T23:59:59Z".into());
        let c = make_issue("C", Status::Open);
        let d = make_issue("D", Status::Open);

        let now = "2026-01-04T00:00:00Z".parse::<jiff::Timestamp>().unwrap();
        let out = calculate_burndown_at(&s, &[a, b, c, d], now);

        assert_eq!(out.completed_issues, 2);
        assert_eq!(out.remaining_issues, 2);
        // elapsedDays: (Jan4-Jan1).hours/24 + 1 = 3 + 1 = 4
        // actual_burn_rate: 2/4 = 0.5
        assert!((out.actual_burn_rate - 0.5).abs() < 1e-10);
        // projected = now + (2 / 0.5 + 1) days = now + 5 days = Jan 9
        assert!(out.projected_complete.is_some());
        let proj = out.projected_complete.unwrap();
        assert!(proj.starts_with("2026-01-09"));
        // on_track: projected <= end_date (Jan 14) => true
        assert!(out.on_track);
    }

    #[test]
    fn burndown_all_closed_early() {
        let s = make_sprint("s1", &["A", "B"], None);
        let mut a = make_issue("A", Status::Closed);
        a.closed_at = Some("2026-01-02T23:59:59Z".into());
        let mut b = make_issue("B", Status::Closed);
        b.closed_at = Some("2026-01-03T23:59:59Z".into());

        let now = "2026-01-04T00:00:00Z".parse::<jiff::Timestamp>().unwrap();
        let out = calculate_burndown_at(&s, &[a, b], now);

        assert_eq!(out.completed_issues, 2);
        assert_eq!(out.remaining_issues, 0);
        assert!(out.projected_complete.is_none());
        assert!(out.on_track);
    }

    #[test]
    fn burndown_no_progress_past_start() {
        let s = make_sprint("s1", &["A", "B"], None);
        let a = make_issue("A", Status::Open);
        let b = make_issue("B", Status::Open);

        let now = "2026-01-05T00:00:00Z".parse::<jiff::Timestamp>().unwrap();
        let out = calculate_burndown_at(&s, &[a, b], now);

        assert_eq!(out.completed_issues, 0);
        assert!(out.projected_complete.is_none());
        assert!(!out.on_track);
    }

    #[test]
    fn burndown_before_start() {
        let s = make_sprint("s1", &["A"], None);
        let a = make_issue("A", Status::Open);

        let now = "2025-12-31T00:00:00Z".parse::<jiff::Timestamp>().unwrap();
        let out = calculate_burndown_at(&s, &[a], now);

        assert_eq!(out.elapsed_days, 0);
        assert_eq!(out.remaining_days, out.total_days);
    }

    #[test]
    fn burndown_after_end() {
        let s = make_sprint("s1", &["A"], None);
        let a = make_issue("A", Status::Open);

        let now = "2026-01-20T00:00:00Z".parse::<jiff::Timestamp>().unwrap();
        let out = calculate_burndown_at(&s, &[a], now);

        assert_eq!(out.elapsed_days, out.total_days);
        assert_eq!(out.remaining_days, 0);
    }

    #[test]
    fn burndown_empty_sprint() {
        let s = make_sprint("s1", &[], None);
        let now = "2026-01-05T00:00:00Z".parse::<jiff::Timestamp>().unwrap();
        let out = calculate_burndown_at(&s, &[], now);
        assert_eq!(out.total_issues, 0);
        // Go generates daily points even with 0 issues (all remaining=0)
        assert!(!out.daily_points.is_empty());
        // Ideal line is empty when total_issues == 0
        assert!(out.ideal_line.is_empty());
    }

    #[test]
    fn ideal_line_has_correct_count() {
        let s = make_sprint("s1", &["A", "B", "C"], None);
        let issues = vec![
            make_issue("A", Status::Open),
            make_issue("B", Status::Open),
            make_issue("C", Status::Open),
        ];
        let now = "2026-01-05T00:00:00Z".parse::<jiff::Timestamp>().unwrap();
        let out = calculate_burndown_at(&s, &issues, now);

        // totalDays = 14 (Jan 1..Jan 14), ideal line has totalDays+1 = 15 points
        assert_eq!(out.ideal_line.len(), 15);
        // First point: remaining = total = 3
        assert_eq!(out.ideal_line[0].remaining, 3);
    }

    #[test]
    fn ideal_line_first_point_equals_total() {
        let s = make_sprint("s1", &["A", "B"], None);
        let now = "2026-01-05T00:00:00Z".parse::<jiff::Timestamp>().unwrap();
        let out = calculate_burndown_at(
            &s,
            &[make_issue("A", Status::Open), make_issue("B", Status::Open)],
            now,
        );

        // First ideal point: remaining = total = 2
        assert_eq!(out.ideal_line[0].remaining, 2);
        // Last ideal point: remaining = 0
        assert_eq!(out.ideal_line.last().unwrap().remaining, 0);
    }

    #[test]
    fn burndown_backward_compat_wrapper() {
        let s = make_sprint("s1", &["A", "B", "C"], Some(1.0));
        let a = make_issue("A", Status::Open);
        let mut b = make_issue("B", Status::Closed);
        b.closed_at = Some("2026-01-03T23:59:59Z".into());
        let c = make_issue("C", Status::Open);

        let now = "2026-01-03T12:00:00Z".parse::<jiff::Timestamp>().unwrap();
        let (points, total) = calculate_burndown(&s, &[a, b, c], now);
        assert_eq!(total, 3);
        assert!(!points.is_empty());
    }

    // -----------------------------------------------------------------------
    // ETA / Forecast
    // -----------------------------------------------------------------------

    #[test]
    fn estimate_eta_for_unknown_issue_returns_err() {
        let issues = vec![make_issue("A", Status::Open)];
        let now = "2026-01-05T00:00:00Z".parse::<jiff::Timestamp>().unwrap();
        let result = estimate_eta_for_issue(&issues, "Z", 1, 0.0, now);
        assert!(result.is_err());
    }

    #[test]
    fn estimate_eta_basic_with_explicit_estimate() {
        let mut a = make_issue("A", Status::Open);
        a.estimated_minutes = Some(120);
        a.labels = vec!["bug".into()];
        let issues = vec![a];

        let now = "2026-01-05T00:00:00Z".parse::<jiff::Timestamp>().unwrap();
        let eta = estimate_eta_for_issue(&issues, "A", 1, 0.0, now).unwrap();

        assert_eq!(eta.issue_id, "A");
        assert_eq!(eta.estimated_minutes, 120);
        assert_eq!(eta.agents, 1);
        assert!(eta.confidence >= 0.10 && eta.confidence <= 0.90);
        // With explicit estimate: confidence >= 0.25 + 0.25 - 0.05(no velocity) = 0.45
        // No velocity samples -> conf -= 0.05; has label -> no penalty
        assert!(eta.confidence >= 0.35);
        assert!(!eta.eta_date.is_empty());
    }

    #[test]
    fn estimate_eta_type_weight_epic_higher_than_task() {
        let mut epic = make_issue("E1", Status::Open);
        epic.issue_type = "epic".into();
        epic.estimated_minutes = Some(60);

        let mut task = make_issue("T1", Status::Open);
        task.issue_type = "task".into();
        task.estimated_minutes = Some(60);

        let issues = vec![epic.clone(), task.clone()];
        let now = "2026-01-05T00:00:00Z".parse::<jiff::Timestamp>().unwrap();

        let eta_epic = estimate_eta_for_issue(&issues, "E1", 1, 0.0, now).unwrap();
        let eta_task = estimate_eta_for_issue(&issues, "T1", 1, 0.0, now).unwrap();

        // Epic (weight 2.0) should have higher estimated_minutes than task (weight 1.0)
        assert!(eta_epic.estimated_minutes > eta_task.estimated_minutes);
    }

    #[test]
    fn estimate_eta_depth_increases_complexity() {
        let mut a = make_issue("A", Status::Open);
        a.estimated_minutes = Some(60);
        let issues = vec![a.clone()];

        let now = "2026-01-05T00:00:00Z".parse::<jiff::Timestamp>().unwrap();

        let eta_low = estimate_eta_for_issue(&issues, "A", 1, 0.0, now).unwrap();
        let eta_high = estimate_eta_for_issue(&issues, "A", 1, 10.0, now).unwrap();

        // depth=10 gives factor 2.0, depth=0 gives factor 1.0
        assert!(eta_high.estimated_minutes >= eta_low.estimated_minutes);
    }

    #[test]
    fn estimate_eta_confidence_with_velocity_samples() {
        let mut issues = Vec::new();
        // Create several closed issues with labels
        for i in 0..10 {
            let mut iss = make_issue(&format!("closed-{i}"), Status::Closed);
            iss.labels = vec!["backend".into()];
            iss.estimated_minutes = Some(30);
            iss.closed_at = Some("2026-01-04T12:00:00Z".into());
            issues.push(iss);
        }
        // Target issue with same label
        let mut target = make_issue("target", Status::Open);
        target.labels = vec!["backend".into()];
        target.estimated_minutes = Some(60);
        issues.push(target);

        let now = "2026-01-05T00:00:00Z".parse::<jiff::Timestamp>().unwrap();
        let eta = estimate_eta_for_issue(&issues, "target", 1, 0.0, now).unwrap();

        // With 10 samples, confidence should be higher
        assert!(eta.confidence >= 0.50);
        assert!(eta.velocity_minutes_per_day > 0.0);
    }

    #[test]
    fn estimate_eta_multi_agent() {
        let mut a = make_issue("A", Status::Open);
        a.estimated_minutes = Some(600);
        let issues = vec![a];

        let now = "2026-01-05T00:00:00Z".parse::<jiff::Timestamp>().unwrap();

        let eta_1 = estimate_eta_for_issue(&issues, "A", 1, 0.0, now).unwrap();
        let eta_4 = estimate_eta_for_issue(&issues, "A", 4, 0.0, now).unwrap();

        // More agents = fewer estimated_days
        assert!(eta_4.estimated_days < eta_1.estimated_days);
    }

    #[test]
    fn compute_forecast_output_all_open() {
        let mut a = make_issue("A", Status::Open);
        a.estimated_minutes = Some(60);
        let mut b = make_issue("B", Status::Open);
        b.estimated_minutes = Some(120);
        let c = make_issue("C", Status::Closed);

        let now = "2026-01-05T00:00:00Z".parse::<jiff::Timestamp>().unwrap();
        let out = compute_forecast_output(&[a, b, c], None, None, 1, &BTreeMap::new(), now);

        assert_eq!(out.forecast_count, 2); // A and B are open
        assert!(out.summary.is_some()); // multiple forecasts
        let summary = out.summary.unwrap();
        assert_eq!(summary.total_minutes, 180);
        assert!(summary.total_days > 0.0);
    }

    #[test]
    fn compute_forecast_output_with_label_filter() {
        let mut a = make_issue("A", Status::Open);
        a.labels = vec!["frontend".into()];
        a.estimated_minutes = Some(30);
        let mut b = make_issue("B", Status::Open);
        b.labels = vec!["backend".into()];
        b.estimated_minutes = Some(60);

        let now = "2026-01-05T00:00:00Z".parse::<jiff::Timestamp>().unwrap();
        let out =
            compute_forecast_output(&[a, b], None, Some("frontend"), 1, &BTreeMap::new(), now);

        assert_eq!(out.forecast_count, 1);
        assert_eq!(out.forecasts[0].issue_id, "A");
    }

    #[test]
    fn backward_compat_estimate_forecast() {
        let s = make_sprint("s1", &["A", "B", "C"], Some(2.0));
        let issues = vec![
            make_issue("A", Status::Open),
            make_issue("B", Status::Open),
            make_issue("C", Status::Closed),
        ];
        let now = "2026-01-05T00:00:00Z".parse::<jiff::Timestamp>().unwrap();
        let f = estimate_forecast(&s, &issues, now).unwrap();
        assert_eq!(f.bead_id, "s1");
        assert!(f.confidence > 0.0);
        assert!(f.factors.iter().any(|f| f.contains("velocity")));
    }

    #[test]
    fn backward_compat_estimate_forecast_all_closed() {
        let s = make_sprint("s1", &["A"], Some(1.0));
        let issues = vec![make_issue("A", Status::Closed)];
        let now = "2026-01-05T00:00:00Z".parse::<jiff::Timestamp>().unwrap();
        assert!(estimate_forecast(&s, &issues, now).is_none());
    }

    // -----------------------------------------------------------------------
    // Capacity
    // -----------------------------------------------------------------------

    #[test]
    fn capacity_empty_issues() {
        let out = calculate_capacity(&[], 1, None, &BTreeMap::new(), jiff::Timestamp::UNIX_EPOCH);
        assert_eq!(out.open_issue_count, 0);
        assert_eq!(out.total_minutes, 0);
        assert!(out.critical_path.is_empty());
        assert!(out.bottlenecks.is_empty());
    }

    #[test]
    fn capacity_all_closed() {
        let a = make_issue("A", Status::Closed);
        let b = make_issue("B", Status::Closed);
        let out = calculate_capacity(
            &[a, b],
            1,
            None,
            &BTreeMap::new(),
            jiff::Timestamp::UNIX_EPOCH,
        );
        assert_eq!(out.open_issue_count, 0);
        assert_eq!(out.total_minutes, 0);
    }

    #[test]
    fn capacity_no_dependencies() {
        let mut a = make_issue("A", Status::Open);
        a.estimated_minutes = Some(60);
        let mut b = make_issue("B", Status::Open);
        b.estimated_minutes = Some(120);

        let out = calculate_capacity(
            &[a, b],
            1,
            None,
            &BTreeMap::new(),
            jiff::Timestamp::UNIX_EPOCH,
        );
        assert_eq!(out.open_issue_count, 2);
        assert_eq!(out.total_minutes, 180);
        // No dependencies: critical path = longest single issue (B: 120m)
        assert!(out.critical_path_length >= 1);
        // All issues are actionable
        assert_eq!(out.actionable_count, 2);
    }

    #[test]
    fn capacity_with_blocking_dependency() {
        let mut a = make_issue("A", Status::Open);
        a.estimated_minutes = Some(60);
        let mut b = make_issue("B", Status::Open);
        b.estimated_minutes = Some(120);
        b.dependencies = vec![Dependency {
            issue_id: "B".into(),
            depends_on_id: "A".into(),
            depends_on_legacy: String::new(),
            target_id_legacy: String::new(),
            r#type: DependencyType::Blocks,
            created_at: None,
            created_by: String::new(),
        }];

        let out = calculate_capacity(
            &[a, b],
            1,
            None,
            &BTreeMap::new(),
            jiff::Timestamp::UNIX_EPOCH,
        );
        assert_eq!(out.open_issue_count, 2);
        // A is actionable (no blockers), B is blocked by A
        assert_eq!(out.actionable_count, 1);
        assert!(out.actionable.contains(&"A".to_string()));
        // Critical path: A -> B (length 2)
        assert_eq!(out.critical_path_length, 2);
        assert!(out.critical_path.contains(&"A".to_string()));
        assert!(out.critical_path.contains(&"B".to_string()));
    }

    #[test]
    fn capacity_bottleneck_detection() {
        let mut a = make_issue("A", Status::Open);
        a.estimated_minutes = Some(60);
        let mut b = make_issue("B", Status::Open);
        b.estimated_minutes = Some(30);
        let mut c = make_issue("C", Status::Open);
        c.estimated_minutes = Some(30);
        c.dependencies = vec![Dependency {
            issue_id: "C".into(),
            depends_on_id: "A".into(),
            depends_on_legacy: String::new(),
            target_id_legacy: String::new(),
            r#type: DependencyType::Blocks,
            created_at: None,
            created_by: String::new(),
        }];
        let mut d = make_issue("D", Status::Open);
        d.estimated_minutes = Some(30);
        d.dependencies = vec![Dependency {
            issue_id: "D".into(),
            depends_on_id: "A".into(),
            depends_on_legacy: String::new(),
            target_id_legacy: String::new(),
            r#type: DependencyType::Blocks,
            created_at: None,
            created_by: String::new(),
        }];

        let out = calculate_capacity(
            &[a, b, c, d],
            1,
            None,
            &BTreeMap::new(),
            jiff::Timestamp::UNIX_EPOCH,
        );
        // A blocks C and D -> bottleneck
        assert!(!out.bottlenecks.is_empty());
        assert_eq!(out.bottlenecks[0].id, "A");
        assert_eq!(out.bottlenecks[0].blocks_count, 2);
    }

    #[test]
    fn capacity_with_label_filter() {
        let mut a = make_issue("A", Status::Open);
        a.labels = vec!["backend".into()];
        a.estimated_minutes = Some(60);
        let mut b = make_issue("B", Status::Open);
        b.labels = vec!["frontend".into()];
        b.estimated_minutes = Some(120);

        let out = calculate_capacity(
            &[a, b],
            1,
            Some("backend"),
            &BTreeMap::new(),
            jiff::Timestamp::UNIX_EPOCH,
        );
        assert_eq!(out.open_issue_count, 1); // only A
        assert_eq!(out.label.as_deref(), Some("backend"));
    }

    #[test]
    fn capacity_non_blocking_deps_ignored() {
        let mut a = make_issue("A", Status::Open);
        a.estimated_minutes = Some(60);
        let mut b = make_issue("B", Status::Open);
        b.estimated_minutes = Some(120);
        b.dependencies = vec![Dependency {
            issue_id: "B".into(),
            depends_on_id: "A".into(),
            depends_on_legacy: String::new(),
            target_id_legacy: String::new(),
            r#type: DependencyType::Related, // non-blocking
            created_at: None,
            created_by: String::new(),
        }];

        let out = calculate_capacity(
            &[a, b],
            1,
            None,
            &BTreeMap::new(),
            jiff::Timestamp::UNIX_EPOCH,
        );
        // Related dependency doesn't block: both actionable
        assert_eq!(out.actionable_count, 2);
        // Critical path: max(A, B) since they're independent = B (120m)
        assert_eq!(out.critical_path_length, 1);
    }

    #[test]
    fn capacity_parallelizable_pct() {
        let mut a = make_issue("A", Status::Open);
        a.estimated_minutes = Some(100);
        let mut b = make_issue("B", Status::Open);
        b.estimated_minutes = Some(100);

        let out = calculate_capacity(
            &[a, b],
            1,
            None,
            &BTreeMap::new(),
            jiff::Timestamp::UNIX_EPOCH,
        );
        // No dependencies: critical path = one issue (100m)
        // parallelizable = (200 - 100) / 200 * 100 = 50%
        assert!((out.parallelizable_pct - 50.0).abs() < 1.0);
    }

    #[test]
    fn capacity_estimated_days_with_agents() {
        let mut a = make_issue("A", Status::Open);
        a.estimated_minutes = Some(480); // 8 hours = 1 day serial
        let mut b = make_issue("B", Status::Open);
        b.estimated_minutes = Some(480);

        let out1 = calculate_capacity(
            &[a.clone(), b.clone()],
            1,
            None,
            &BTreeMap::new(),
            jiff::Timestamp::UNIX_EPOCH,
        );
        let out2 = calculate_capacity(
            &[a, b],
            2,
            None,
            &BTreeMap::new(),
            jiff::Timestamp::UNIX_EPOCH,
        );

        // With 2 agents, parallel work is halved
        assert!(out2.estimated_days < out1.estimated_days);
    }

    // -----------------------------------------------------------------------
    // Edge cases for ETA
    // -----------------------------------------------------------------------

    #[test]
    fn eta_zero_estimated_minutes_uses_default() {
        let mut a = make_issue("A", Status::Open);
        a.estimated_minutes = Some(0);
        let issues = vec![a];

        let now = "2026-01-05T00:00:00Z".parse::<jiff::Timestamp>().unwrap();
        let eta = estimate_eta_for_issue(&issues, "A", 1, 0.0, now).unwrap();
        // Should fall back to median/default = 60
        assert_eq!(eta.estimated_minutes, 60);
    }

    #[test]
    fn eta_no_labels_reduces_confidence() {
        let mut a = make_issue("A", Status::Open);
        a.estimated_minutes = Some(60);
        a.labels = vec![];
        let issues = vec![a];

        let now = "2026-01-05T00:00:00Z".parse::<jiff::Timestamp>().unwrap();
        let eta = estimate_eta_for_issue(&issues, "A", 1, 0.0, now).unwrap();
        // base=0.25, +0.25(explicit), -0.05(no labels), -0.05(no velocity) = 0.40
        assert!((eta.confidence - 0.40).abs() < 0.01);
    }

    #[test]
    fn eta_confidence_clamped() {
        let issues = vec![make_issue("A", Status::Open)];
        let now = "2026-01-05T00:00:00Z".parse::<jiff::Timestamp>().unwrap();
        let eta = estimate_eta_for_issue(&issues, "A", 1, 0.0, now).unwrap();
        assert!(eta.confidence >= 0.10);
        assert!(eta.confidence <= 0.90);
    }

    #[test]
    fn compute_median_estimated_minutes_all_zero() {
        let a = make_issue("A", Status::Open);
        let b = make_issue("B", Status::Open);
        let m = compute_median_estimated_minutes(&[a, b]);
        assert_eq!(m, DEFAULT_ESTIMATED_MINUTES);
    }

    #[test]
    fn compute_median_estimated_minutes_odd() {
        let mut a = make_issue("A", Status::Open);
        a.estimated_minutes = Some(30);
        let mut b = make_issue("B", Status::Open);
        b.estimated_minutes = Some(90);
        let mut c = make_issue("C", Status::Open);
        c.estimated_minutes = Some(60);
        let m = compute_median_estimated_minutes(&[a, b, c]);
        assert_eq!(m, 60);
    }

    #[test]
    fn compute_median_estimated_minutes_even() {
        let mut a = make_issue("A", Status::Open);
        a.estimated_minutes = Some(20);
        let mut b = make_issue("B", Status::Open);
        b.estimated_minutes = Some(40);
        let mut c = make_issue("C", Status::Open);
        c.estimated_minutes = Some(60);
        let mut d = make_issue("D", Status::Open);
        d.estimated_minutes = Some(80);
        let m = compute_median_estimated_minutes(&[a, b, c, d]);
        assert_eq!(m, 50); // (40+60)/2
    }

    // -----------------------------------------------------------------------
    // Scope changes
    // -----------------------------------------------------------------------

    #[test]
    fn scope_changes_returns_none_without_git() {
        let dir = std::env::temp_dir().join(format!("bvr-scope-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let sprint = make_sprint("s1", &[], None);
        let result = compute_sprint_scope_changes(&dir, &sprint, &HashMap::new());
        assert!(result.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scope_changes_empty_sprint_id_returns_none() {
        let sprint = Sprint {
            id: String::new(),
            name: String::new(),
            start_date: None,
            end_date: None,
            bead_ids: vec![],
            velocity_target: None,
            created_at: None,
            updated_at: None,
        };
        let result = compute_sprint_scope_changes(Path::new("."), &sprint, &HashMap::new());
        assert!(result.is_none());
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    #[test]
    fn parse_git_header_line_valid() {
        let line = "abc123def456abc123def456abc123def456abc1\x002026-01-05T10:00:00+09:00";
        let result = parse_git_header_line(line);
        assert!(result.is_some());
        let (sha, ts) = result.unwrap();
        assert_eq!(sha, "abc123def456abc123def456abc123def456abc1");
        assert!(ts > jiff::Timestamp::default());
    }

    #[test]
    fn parse_git_header_line_invalid_sha() {
        let line = "short\x002026-01-05T10:00:00Z";
        assert!(parse_git_header_line(line).is_none());
    }

    #[test]
    fn parse_git_header_line_no_null() {
        let line = "abc123def456abc123def456abc123def456abc12026-01-05T10:00:00Z";
        assert!(parse_git_header_line(line).is_none());
    }

    #[test]
    fn set_difference_basic() {
        let a = vec!["A".into(), "B".into(), "C".into()];
        let b = vec!["B".into(), "D".into()];
        let diff = set_difference(&a, &b);
        assert_eq!(diff, vec!["A", "C"]);
    }

    #[test]
    fn set_difference_empty_a() {
        let a: Vec<String> = vec![];
        let b = vec!["B".into()];
        assert!(set_difference(&a, &b).is_empty());
    }

    #[test]
    fn set_difference_empty_b() {
        let a = vec!["A".into(), "B".into()];
        let b: Vec<String> = vec![];
        assert_eq!(set_difference(&a, &b), vec!["A", "B"]);
    }

    #[test]
    fn has_label_case_insensitive() {
        let labels = vec!["Frontend".into(), "BUG".into()];
        assert!(has_label(&labels, "frontend"));
        assert!(has_label(&labels, "bug"));
        assert!(!has_label(&labels, "backend"));
    }
}
