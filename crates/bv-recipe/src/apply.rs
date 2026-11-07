//! The recipe engine — port of Go `pkg/recipe/apply.go` at commit `18afafa`.
//!
//! [`apply`] filters issues with the recipe's [`crate::FilterConfig`], orders
//! them by the recipe's sort chain (reading caller-supplied metrics for the
//! graph fields) and keeps at most `view.max_items`. The TUI and the robot path
//! share it so both honour `--recipe` identically.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use bv_core::model::{Issue, Status};
use bv_correlation::readiness::DependencyState;

use crate::readiness::Readiness;
use crate::types::{
    canonical_sort_field, parse_relative_time, Recipe, SORT_FIELD_BETWEENNESS, SORT_FIELD_CREATED,
    SORT_FIELD_ID, SORT_FIELD_IMPACT, SORT_FIELD_PAGERANK, SORT_FIELD_PRIORITY, SORT_FIELD_STATUS,
    SORT_FIELD_TITLE, SORT_FIELD_TRIAGE, SORT_FIELD_UPDATED,
};

/// Per-issue graph scores for the metric sort fields.
///
/// Go's `*analysis.GraphStats` satisfies it. This crate deliberately takes a
/// trait rather than importing `bv-analysis`, so the TUI and the robot path can
/// each hand in whatever stats they already computed.
pub trait GraphMetrics {
    fn page_rank_score(&self, id: &str) -> f64;
    fn betweenness_score(&self, id: &str) -> f64;
    /// Critical-path score; the `impact` sort field.
    fn critical_path_score(&self, id: &str) -> f64;
}

/// The lookups a recipe sort may need.
///
/// A `None` [`Metrics::graph`] makes pagerank/betweenness/impact read as 0 and a
/// `None` [`Metrics::triage`] makes triage read as 0, so ties fall through to
/// the secondary sort and the ID tie-break. Callers that want a meaningful
/// metric order must supply the corresponding source;
/// [`Recipe::needs_graph_metrics`] and [`Recipe::needs_triage_scores`] say which
/// are required.
#[derive(Default)]
pub struct Metrics<'a> {
    pub graph: Option<&'a dyn GraphMetrics>,
    /// Issue ID -> triage score.
    pub triage: Option<&'a BTreeMap<String, f64>>,
    /// Full dependency authority before display scoping.
    pub readiness: Option<&'a Readiness>,
}

impl<'a> Metrics<'a> {
    /// Go `Metrics.score` — an absent source reads 0, never panics.
    fn score(&self, field: &str, id: &str) -> f64 {
        match field {
            SORT_FIELD_PAGERANK => self.graph.map(|g| g.page_rank_score(id)).unwrap_or(0.0),
            SORT_FIELD_BETWEENNESS => self.graph.map(|g| g.betweenness_score(id)).unwrap_or(0.0),
            SORT_FIELD_IMPACT => self.graph.map(|g| g.critical_path_score(id)).unwrap_or(0.0),
            SORT_FIELD_TRIAGE => self.triage.and_then(|t| t.get(id).copied()).unwrap_or(0.0),
            _ => 0.0,
        }
    }
}

/// Go `Apply` — filters, sorts and truncates. The input slice is never mutated;
/// a malformed time filter is reported rather than skipped.
///
/// Go treats a zero `time.Time` as "use the wall clock". This port takes the
/// reference instant explicitly, so `None` means the same thing: call
/// [`now_or_wall_clock`] first, or pass `jiff::Timestamp::now()`.
pub fn apply(
    issues: &[Issue],
    metrics: &Metrics<'_>,
    recipe: &Recipe,
    now: Option<jiff::Timestamp>,
) -> Result<Vec<Issue>, String> {
    let mut filtered = filter_with(issues, recipe, now, metrics.readiness)?;
    sort_issues(&mut filtered, metrics, recipe);
    let max = recipe.view.max_items;
    if max > 0 && filtered.len() as i64 > max {
        filtered.truncate(max as usize);
    }
    Ok(filtered)
}

/// Go `Filter` — the issues matching every filter, in input order, as a new
/// slice. Blocker checks look at the full readiness authority, so a blocker
/// excluded by the status filter still counts as blocking. Issues without a
/// created/updated timestamp are not excluded by the date filters.
pub fn filter(
    issues: &[Issue],
    recipe: &Recipe,
    now: Option<jiff::Timestamp>,
) -> Result<Vec<Issue>, String> {
    filter_with(issues, recipe, now, None)
}

/// Resolve the reference instant: Go substitutes `time.Now()` for the zero time.
pub fn now_or_wall_clock(now: Option<jiff::Timestamp>) -> jiff::Timestamp {
    now.unwrap_or_else(jiff::Timestamp::now)
}

/// Go's unexported `filter` — the one implementation both entry points share.
fn filter_with(
    issues: &[Issue],
    recipe: &Recipe,
    now: Option<jiff::Timestamp>,
    readiness: Option<&Readiness>,
) -> Result<Vec<Issue>, String> {
    let now = now_or_wall_clock(now);
    let f = &recipe.filters;

    let thresholds = TimeThresholds::parse(f, now)?;
    // Go builds the index from the full input when the caller supplies none.
    // The caller-supplied variant is the authority, and is how a recipe applied
    // to a narrowed scope still sees blockers outside it.
    let owned;
    let readiness = match readiness {
        Some(r) => r,
        None => {
            owned = Readiness::new(issues);
            &owned
        }
    };

    let mut result = Vec::with_capacity(issues.len());
    for issue in issues {
        if !matches_status(issue, &f.status)
            || !matches_priority(issue, &f.priority)
            || !has_all_tags(issue, &f.tags)
            || has_any_tag(issue, &f.exclude_tags)
            || !thresholds.matches(issue)
        {
            continue;
        }
        if let Some(has_blockers) = f.has_blockers {
            let blocked = readiness.dependency_state(&issue.id) != DependencyState::Satisfied;
            if has_blockers != blocked {
                continue;
            }
        }
        if let Some(actionable) = f.actionable {
            if actionable != readiness.ready(&issue.id, now) {
                continue;
            }
        }
        if !f.title_contains.is_empty()
            && !issue
                .title
                .to_lowercase()
                .contains(&f.title_contains.to_lowercase())
        {
            continue;
        }
        if !f.id_prefix.is_empty() && !issue.id.starts_with(&f.id_prefix) {
            continue;
        }
        result.push(issue.clone());
    }
    Ok(result)
}

/// Go `timeThresholds`. Each field is `None` when the recipe leaves it unset,
/// which is Go's zero `time.Time` — the date filters skip unset thresholds.
#[derive(Default)]
struct TimeThresholds {
    created_after: Option<jiff::Timestamp>,
    created_before: Option<jiff::Timestamp>,
    updated_after: Option<jiff::Timestamp>,
    updated_before: Option<jiff::Timestamp>,
}

impl TimeThresholds {
    /// Go `parseTimeFilters` — a malformed value is an error naming its field.
    fn parse(f: &crate::types::FilterConfig, now: jiff::Timestamp) -> Result<Self, String> {
        fn one(
            key: &str,
            value: &str,
            now: jiff::Timestamp,
        ) -> Result<Option<jiff::Timestamp>, String> {
            if value.is_empty() {
                return Ok(None);
            }
            parse_relative_time(value, now).map_err(|e| format!("filters.{key}: {e}"))
        }
        Ok(Self {
            created_after: one("created_after", &f.created_after, now)?,
            created_before: one("created_before", &f.created_before, now)?,
            updated_after: one("updated_after", &f.updated_after, now)?,
            updated_before: one("updated_before", &f.updated_before, now)?,
        })
    }

    /// Go `timeThresholds.matches` — an issue with no timestamp is never
    /// excluded by a date filter.
    fn matches(&self, issue: &Issue) -> bool {
        // `excluded` mirrors Go's `threshold != zero && issue != zero &&
        // issue.Before(threshold)` guard exactly: an unset threshold or an
        // issue with no timestamp keeps the issue.
        let created = issue.created_at.as_deref().and_then(parse_ts);
        let updated = issue.updated_at.as_deref().and_then(parse_ts);
        let excluded = |threshold: Option<jiff::Timestamp>,
                        at: Option<jiff::Timestamp>,
                        cmp: fn(jiff::Timestamp, jiff::Timestamp) -> bool| {
            match (threshold, at) {
                (Some(t), Some(a)) => cmp(a, t),
                _ => false,
            }
        };
        // created_after / created_before keep issues inside the window.
        !excluded(self.created_after, created, |a, t| a < t)
            && !excluded(self.created_before, created, |a, t| a > t)
            && !excluded(self.updated_after, updated, |a, t| a < t)
            && !excluded(self.updated_before, updated, |a, t| a > t)
    }
}

/// Parse an issue timestamp. Go's model types these as `time.Time`, so an
/// unparseable string has no counterpart there; it reads as unset, matching the
/// "no timestamp" branch of every filter and sort.
fn parse_ts(raw: &str) -> Option<jiff::Timestamp> {
    raw.parse().ok()
}

/// Go `matchesStatus`. `Status` is a closed enum in this port, so the
/// case-insensitive comparison is against its canonical lowercase spelling —
/// the same result Go's `strings.EqualFold` gives for every status it can hold.
fn matches_status(issue: &Issue, statuses: &[String]) -> bool {
    if statuses.is_empty() {
        return true;
    }
    statuses
        .iter()
        .any(|s| issue.status.as_str().eq_ignore_ascii_case(s.trim()))
}

/// Go `matchesPriority` — exact match; 0 is the highest priority.
fn matches_priority(issue: &Issue, priorities: &[i64]) -> bool {
    priorities.is_empty() || priorities.contains(&issue.priority)
}

/// Go `hasLabel`.
fn has_label(issue: &Issue, tag: &str) -> bool {
    issue.labels.iter().any(|l| l.eq_ignore_ascii_case(tag))
}

/// Go `hasAllTags` — every requested tag must be present.
fn has_all_tags(issue: &Issue, tags: &[String]) -> bool {
    tags.iter().all(|t| has_label(issue, t))
}

/// Go `hasAnyTag` — one excluded tag is enough to drop the issue.
fn has_any_tag(issue: &Issue, tags: &[String]) -> bool {
    tags.iter().any(|t| has_label(issue, t))
}

/// Go `SortIssues` — orders `issues` in place by the recipe's sort chain. Each
/// level compares its field in its (possibly defaulted) direction and falls
/// through to the next on a tie; natural issue-ID order breaks any remaining
/// tie, so the result never depends on input order. An empty chain leaves the
/// slice untouched.
///
/// Go's `sort.SliceStable` here is equivalent to a stable sort with a total
/// comparator, because the ID tie-break makes the chain decisive.
pub fn sort_issues(issues: &mut [Issue], metrics: &Metrics<'_>, recipe: &Recipe) {
    let chain = recipe.sort_chain();
    if chain.is_empty() {
        return;
    }
    let levels: Vec<SortLevel> = chain
        .iter()
        .map(|s| {
            let (field, _) = canonical_sort_field(&s.field);
            let field = field.into_owned();
            let desc = sort_descending(&field, &s.direction);
            SortLevel { field, desc }
        })
        .collect();
    issues.sort_by(|a, b| {
        for lvl in &levels {
            let cmp = compare_issues(a, b, &lvl.field, metrics);
            if cmp == Ordering::Equal {
                continue;
            }
            return if lvl.desc { cmp.reverse() } else { cmp };
        }
        natural_cmp(&a.id, &b.id)
    });
}

struct SortLevel {
    field: String,
    desc: bool,
}

/// Go `sortDescending` — an explicit direction wins; otherwise dates and the
/// graph metrics default to descending and everything else to ascending.
fn sort_descending(field: &str, direction: &str) -> bool {
    match direction.trim().to_lowercase().as_str() {
        "desc" => return true,
        "asc" => return false,
        _ => {}
    }
    matches!(
        field,
        SORT_FIELD_CREATED
            | SORT_FIELD_UPDATED
            | SORT_FIELD_PAGERANK
            | SORT_FIELD_BETWEENNESS
            | SORT_FIELD_IMPACT
            | SORT_FIELD_TRIAGE
    )
}

/// Go `compareIssues` — orders `a` before `b` in ascending order of `field`.
/// An unknown field compares equal (`validate` rejects it at load time).
fn compare_issues(a: &Issue, b: &Issue, field: &str, metrics: &Metrics<'_>) -> Ordering {
    match field {
        SORT_FIELD_PRIORITY => a.priority.cmp(&b.priority),
        // An unset timestamp sorts before every real one, which is Go's zero
        // `time.Time` in year 1.
        SORT_FIELD_CREATED => cmp_ts(a.created_at.as_deref(), b.created_at.as_deref()),
        SORT_FIELD_UPDATED => cmp_ts(a.updated_at.as_deref(), b.updated_at.as_deref()),
        SORT_FIELD_TITLE => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
        SORT_FIELD_ID => natural_cmp(&a.id, &b.id),
        SORT_FIELD_STATUS => a.status.as_str().cmp(b.status.as_str()),
        SORT_FIELD_PAGERANK | SORT_FIELD_BETWEENNESS | SORT_FIELD_IMPACT | SORT_FIELD_TRIAGE => {
            // Go's `compareFloats` treats NaN as equal, so it cannot reorder a
            // slice; `partial_cmp` would panic-free but differently, so spell
            // the comparison out.
            let (x, y) = (metrics.score(field, &a.id), metrics.score(field, &b.id));
            if x < y {
                Ordering::Less
            } else if x > y {
                Ordering::Greater
            } else {
                Ordering::Equal
            }
        }
        _ => Ordering::Equal,
    }
}

/// Go `compareTimes` over the unparsed stamp, with `None` standing in for the
/// zero time.
fn cmp_ts(a: Option<&str>, b: Option<&str>) -> Ordering {
    match (a.and_then(parse_ts), b.and_then(parse_ts)) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(x), Some(y)) => x.cmp(&y),
    }
}

/// Go `naturalLess` — orders IDs sharing a prefix by their trailing number
/// ("bv-2" < "bv-10") and everything else lexically.
pub fn natural_less(s1: &str, s2: &str) -> bool {
    natural_cmp(s1, s2) == Ordering::Less
}

/// `natural_less` as a total order, which is what the sort tie-break needs.
fn natural_cmp(s1: &str, s2: &str) -> Ordering {
    // Go `split`: everything up to the trailing digit run, the number, and
    // whether a digit run was found at all.
    fn split(s: &str) -> (String, Option<i64>, bool) {
        let bytes = s.as_bytes();
        let mut last_digit: isize = -1;
        let mut i = bytes.len();
        while i > 0 {
            let b = bytes[i - 1];
            if !b.is_ascii_digit() {
                break;
            }
            i -= 1;
            last_digit = i as isize;
        }
        if last_digit == -1 {
            return (s.to_string(), None, false);
        }
        let start = last_digit as usize;
        match s[start..].parse::<i64>() {
            // Go ignores the parse error, but an all-digit run that long can
            // only overflow, in which case it falls back to a lexical compare.
            Ok(n) => (s[..start].to_string(), Some(n), true),
            Err(_) => (s.to_string(), None, false),
        }
    }
    let (p1, n1, ok1) = split(s1);
    let (p2, n2, ok2) = split(s2);
    if ok1 && ok2 && p1 == p2 {
        if let (Some(n1), Some(n2)) = (n1, n2) {
            return n1.cmp(&n2);
        }
    }
    s1.cmp(s2)
}

/// Go `Status.IsValid` re-exported for callers filtering on custom states.
/// The Rust enum is closed, so this only documents the rule: nonblank is valid.
pub fn status_is_valid(status: &str) -> bool {
    !status.trim().is_empty()
}

/// Whether `status` names one of the states this port's [`Status`] can hold.
/// Go's model accepts any string, so a custom workflow state filters here but
/// never matches an issue.
pub fn is_known_status(status: &Status) -> bool {
    status_is_valid(status.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{issue, ts, IssueExt};
    use crate::types::{FilterConfig, SortConfig, ViewConfig};
    use bv_core::model::Status;

    /// Go's `stubGraph`.
    struct StubGraph {
        pagerank: BTreeMap<String, f64>,
        betweenness: BTreeMap<String, f64>,
        impact: BTreeMap<String, f64>,
    }

    impl StubGraph {
        fn new() -> Self {
            Self {
                pagerank: BTreeMap::new(),
                betweenness: BTreeMap::new(),
                impact: BTreeMap::new(),
            }
        }
        fn page_rank(mut self, pairs: &[(&str, f64)]) -> Self {
            self.pagerank
                .extend(pairs.iter().map(|(k, v)| (k.to_string(), *v)));
            self
        }
        fn betweenness(mut self, pairs: &[(&str, f64)]) -> Self {
            self.betweenness
                .extend(pairs.iter().map(|(k, v)| (k.to_string(), *v)));
            self
        }
        fn impact(mut self, pairs: &[(&str, f64)]) -> Self {
            self.impact
                .extend(pairs.iter().map(|(k, v)| (k.to_string(), *v)));
            self
        }
    }

    impl GraphMetrics for StubGraph {
        fn page_rank_score(&self, id: &str) -> f64 {
            self.pagerank.get(id).copied().unwrap_or(0.0)
        }
        fn betweenness_score(&self, id: &str) -> f64 {
            self.betweenness.get(id).copied().unwrap_or(0.0)
        }
        fn critical_path_score(&self, id: &str) -> f64 {
            self.impact.get(id).copied().unwrap_or(0.0)
        }
    }

    fn ids(issues: &[Issue]) -> Vec<String> {
        issues.iter().map(|i| i.id.clone()).collect()
    }

    fn require_ids(got: &[Issue], want: &[&str]) {
        assert_eq!(ids(got), want);
    }

    /// Deliberately shuffled input; C and D tie on pagerank so the secondary
    /// (priority asc) decides, and E/F tie on both so natural ID order decides.
    fn metric_corpus() -> Vec<Issue> {
        vec![
            issue("bv-3").priority(3),
            issue("bv-1").priority(2),
            issue("bv-10").priority(1),
            issue("bv-4").priority(0),
            issue("bv-2").status(Status::InProgress).priority(1),
            issue("bv-9").priority(1),
            issue("bv-5").status(Status::Closed).priority(0),
        ]
    }

    fn metric_graph() -> StubGraph {
        StubGraph::new()
            .page_rank(&[
                ("bv-1", 0.9),
                ("bv-2", 0.7),
                ("bv-3", 0.5),
                ("bv-4", 0.5),
                ("bv-9", 0.1),
                ("bv-10", 0.1),
                ("bv-5", 1.0),
            ])
            .betweenness(&[("bv-1", 1.0), ("bv-2", 5.0), ("bv-3", 3.0)])
            .impact(&[("bv-1", 0.2), ("bv-2", 0.8)])
    }

    /// Go's `high-impact` builtin, restated here so the engine tests do not
    /// depend on the loader (covered separately).
    fn high_impact() -> Recipe {
        Recipe {
            filters: FilterConfig {
                status: vec!["open".into(), "in_progress".into()],
                ..Default::default()
            },
            sort: SortConfig {
                field: SORT_FIELD_PAGERANK.into(),
                direction: "desc".into(),
                secondary: Some(Box::new(SortConfig {
                    field: SORT_FIELD_PRIORITY.into(),
                    direction: "asc".into(),
                    ..Default::default()
                })),
            },
            view: ViewConfig {
                max_items: 20,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn metric_sorts_and_secondary_tie_breaks() {
        let issues = metric_corpus();
        let graph = metric_graph();
        let metrics = Metrics {
            graph: Some(&graph),
            ..Default::default()
        };
        let r = high_impact();
        assert!(r.needs_graph_metrics() && !r.needs_triage_scores());

        // bv-5 is closed and filtered out despite the top pagerank; bv-3/bv-4
        // tie on pagerank and fall through to priority asc; bv-9/bv-10 tie on
        // both and fall through to natural ID order.
        let got = apply(&issues, &metrics, &r, None).unwrap();
        require_ids(&got, &["bv-1", "bv-2", "bv-4", "bv-3", "bv-9", "bv-10"]);

        // Without a graph source every score reads as 0 and the secondary sort
        // carries the order: priority asc, then natural ID.
        let got = apply(&issues, &Metrics::default(), &r, None).unwrap();
        require_ids(&got, &["bv-4", "bv-2", "bv-9", "bv-10", "bv-1", "bv-3"]);

        // Betweenness sort, explicit ascending; a missing score reads as 0.
        let r = Recipe {
            sort: SortConfig {
                field: SORT_FIELD_BETWEENNESS.into(),
                direction: "asc".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        let got = apply(&issues[..3], &metrics, &r, None).unwrap();
        require_ids(&got, &["bv-10", "bv-1", "bv-3"]);

        // Impact (critical path) defaults to descending.
        let r = Recipe {
            sort: SortConfig {
                field: SORT_FIELD_IMPACT.into(),
                ..Default::default()
            },
            ..Default::default()
        };
        let got = apply(&issues[..5], &metrics, &r, None).unwrap();
        require_ids(&got, &["bv-2", "bv-1", "bv-3", "bv-4", "bv-10"]);
    }

    #[test]
    fn triage_scores_come_from_metrics_not_the_graph() {
        let issues = metric_corpus();
        let triage = BTreeMap::from([
            ("bv-3".to_string(), 0.9),
            ("bv-1".to_string(), 0.4),
            ("bv-2".to_string(), 0.4),
        ]);
        let r = Recipe {
            filters: FilterConfig {
                status: vec!["open".into(), "in_progress".into()],
                actionable: Some(true),
                ..Default::default()
            },
            sort: SortConfig {
                field: SORT_FIELD_TRIAGE.into(),
                direction: "desc".into(),
                secondary: Some(Box::new(SortConfig {
                    field: SORT_FIELD_PRIORITY.into(),
                    direction: "asc".into(),
                    ..Default::default()
                })),
            },
            view: ViewConfig {
                max_items: 20,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(r.needs_triage_scores() && !r.needs_graph_metrics());
        let metrics = Metrics {
            triage: Some(&triage),
            ..Default::default()
        };
        let got = apply(&issues, &metrics, &r, None).unwrap();
        require_ids(&got, &["bv-3", "bv-2", "bv-1", "bv-4", "bv-9", "bv-10"]);
    }

    #[test]
    fn a_tertiary_sort_level_is_honoured() {
        let r = Recipe {
            sort: SortConfig {
                field: SORT_FIELD_PAGERANK.into(),
                secondary: Some(Box::new(SortConfig {
                    field: SORT_FIELD_STATUS.into(),
                    secondary: Some(Box::new(SortConfig {
                        field: SORT_FIELD_TITLE.into(),
                        direction: "desc".into(),
                        ..Default::default()
                    })),
                    ..Default::default()
                })),
                ..Default::default()
            },
            ..Default::default()
        };
        let tied = vec![
            issue("t-1").title("alpha"),
            issue("t-2").title("beta"),
            issue("t-3").status(Status::Blocked).title("gamma"),
        ];
        let got = apply(&tied, &Metrics::default(), &r, None).unwrap();
        require_ids(&got, &["t-3", "t-2", "t-1"]);
    }

    #[test]
    fn max_items_truncates_after_sorting_and_never_mutates_input() {
        let issues: Vec<Issue> = (1..=30)
            .map(|i| issue(&format!("q-{i:02}")).priority(i % 4))
            .collect();
        let before = ids(&issues);
        let quick_wins = Recipe {
            filters: FilterConfig {
                status: vec!["open".into()],
                priority: vec![2, 3],
                actionable: Some(true),
                ..Default::default()
            },
            sort: SortConfig {
                field: SORT_FIELD_PRIORITY.into(),
                direction: "asc".into(),
                ..Default::default()
            },
            view: ViewConfig {
                max_items: 15,
                ..Default::default()
            },
            ..Default::default()
        };
        let got = apply(&issues, &Metrics::default(), &quick_wins, None).unwrap();
        assert_eq!(got.len(), 15);
        for i in &got {
            assert!(i.priority == 2 || i.priority == 3, "kept {}", i.id);
        }
        for w in got.windows(2) {
            assert!(w[0].priority <= w[1].priority, "{:?}", ids(&got));
        }

        // max_items larger than the result set is a no-op; zero means unlimited.
        for max in [100, 0] {
            let r = Recipe {
                view: ViewConfig {
                    max_items: max,
                    ..Default::default()
                },
                ..Default::default()
            };
            assert_eq!(
                apply(&issues, &Metrics::default(), &r, None).unwrap().len(),
                30
            );
        }

        apply(
            &issues,
            &Metrics::default(),
            &Recipe {
                sort: SortConfig {
                    field: SORT_FIELD_PRIORITY.into(),
                    ..Default::default()
                },
                view: ViewConfig {
                    max_items: 3,
                    ..Default::default()
                },
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert_eq!(ids(&issues), before, "apply mutated its input");
    }

    #[test]
    fn status_priority_tags_title_and_prefix() {
        let issues = vec![
            issue("UI-1")
                .title("Add login button")
                .priority(1)
                .labels(&["Frontend", "p0"]),
            issue("API-2")
                .title("Login endpoint")
                .status(Status::InProgress)
                .priority(2)
                .labels(&["backend"]),
            issue("API-3")
                .title("Health check")
                .status(Status::Closed)
                .priority(2)
                .labels(&["backend", "ops"]),
            issue("API-4").status(Status::Tombstone).priority(0),
        ];
        let now = Some(jiff::Timestamp::now());

        let r = Recipe {
            filters: FilterConfig {
                status: vec!["OPEN".into(), "in_progress".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        require_ids(&filter(&issues, &r, now).unwrap(), &["UI-1", "API-2"]);

        // "closed" matches exactly: a tombstone is not a closed issue.
        let r = Recipe {
            filters: FilterConfig {
                status: vec!["closed".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        require_ids(&filter(&issues, &r, now).unwrap(), &["API-3"]);

        let r = Recipe {
            filters: FilterConfig {
                priority: vec![2],
                ..Default::default()
            },
            ..Default::default()
        };
        require_ids(&filter(&issues, &r, now).unwrap(), &["API-2", "API-3"]);

        // tags: ALL required, case-insensitive; exclude_tags: ANY drops.
        let r = Recipe {
            filters: FilterConfig {
                tags: vec!["backend".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        require_ids(&filter(&issues, &r, now).unwrap(), &["API-2", "API-3"]);
        let r = Recipe {
            filters: FilterConfig {
                tags: vec!["backend".into(), "OPS".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        require_ids(&filter(&issues, &r, now).unwrap(), &["API-3"]);
        let r = Recipe {
            filters: FilterConfig {
                tags: vec!["frontend".into()],
                exclude_tags: vec!["P0".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(filter(&issues, &r, now).unwrap().is_empty());

        let r = Recipe {
            filters: FilterConfig {
                title_contains: "LOGIN".into(),
                id_prefix: "API".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        require_ids(&filter(&issues, &r, now).unwrap(), &["API-2"]);
    }

    #[test]
    fn date_windows_keep_undated_issues() {
        let now = ts("2026-09-01T12:00:00Z");
        // `jiff::Timestamp` only does elapsed time, and every instant here is
        // UTC, so a day is exactly 24 hours.
        let days_ago = |n: i64| now.checked_sub(jiff::Span::new().hours(n * 24)).unwrap();
        let dated = |id: &str, t: jiff::Timestamp| issue(id).created_at(t).updated_at(t);
        let issues = vec![
            dated("fresh", days_ago(1)),
            dated("week", days_ago(8)),
            dated("old", days_ago(40)),
            issue("undated"),
        ];

        let by = |f: FilterConfig| Recipe {
            filters: f,
            ..Default::default()
        };

        require_ids(
            &filter(
                &issues,
                &by(FilterConfig {
                    created_after: "7d".into(),
                    ..Default::default()
                }),
                Some(now),
            )
            .unwrap(),
            &["fresh", "undated"],
        );
        require_ids(
            &filter(
                &issues,
                &by(FilterConfig {
                    updated_after: "2w".into(),
                    ..Default::default()
                }),
                Some(now),
            )
            .unwrap(),
            &["fresh", "week", "undated"],
        );
        require_ids(
            &filter(
                &issues,
                &by(FilterConfig {
                    created_before: "7d".into(),
                    ..Default::default()
                }),
                Some(now),
            )
            .unwrap(),
            &["week", "old", "undated"],
        );
        require_ids(
            &filter(
                &issues,
                &by(FilterConfig {
                    updated_before: "30d".into(),
                    ..Default::default()
                }),
                Some(now),
            )
            .unwrap(),
            &["old", "undated"],
        );
        // A window combines both bounds; ISO dates work too.
        require_ids(
            &filter(
                &issues,
                &by(FilterConfig {
                    updated_after: "2026-07-01".into(),
                    updated_before: "3d".into(),
                    ..Default::default()
                }),
                Some(now),
            )
            .unwrap(),
            &["week", "old", "undated"],
        );
    }

    #[test]
    fn a_malformed_time_filter_is_an_error_naming_its_field() {
        let issues = vec![issue("a")];
        let now = ts("2026-09-01T12:00:00Z");
        let r = Recipe {
            filters: FilterConfig {
                updated_after: "fortnight".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        let err = filter(&issues, &r, Some(now)).unwrap_err();
        assert!(err.contains("filters.updated_after"), "{err}");

        let r = Recipe {
            filters: FilterConfig {
                created_before: "soon".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        let err = apply(&issues, &Metrics::default(), &r, Some(now)).unwrap_err();
        assert!(err.contains("filters.created_before"), "{err}");
    }

    #[test]
    fn blockers_actionable_and_deferral() {
        let now = ts("2026-09-01T12:00:00Z");
        let issues = vec![
            issue("root"),
            issue("closed-root").status(Status::Closed),
            issue("gone-root").status(Status::Tombstone),
            issue("blocked").depends_on("root", "blocks"),
            issue("unblocked").depends_on("closed-root", "blocks"),
            issue("unblocked-tombstone").depends_on("gone-root", "blocks"),
            issue("dangling").depends_on("missing", "blocks"),
            issue("related").depends_on("root", "related"),
            issue("deferred").defer_until("2026-11-30T00:00:00Z"),
            issue("undeferred").defer_until("2026-08-30T00:00:00Z"),
        ];

        let by = |f: FilterConfig| Recipe {
            filters: f,
            ..Default::default()
        };

        require_ids(
            &filter(
                &issues,
                &by(FilterConfig {
                    has_blockers: Some(true),
                    ..Default::default()
                }),
                Some(now),
            )
            .unwrap(),
            &["blocked", "dangling"],
        );
        require_ids(
            &filter(
                &issues,
                &by(FilterConfig {
                    has_blockers: Some(false),
                    ..Default::default()
                }),
                Some(now),
            )
            .unwrap(),
            &[
                "root",
                "closed-root",
                "gone-root",
                "unblocked",
                "unblocked-tombstone",
                "related",
                "deferred",
                "undeferred",
            ],
        );

        // actionable: open/ongoing, proven dependency-ready, and not deferred.
        require_ids(
            &filter(
                &issues,
                &by(FilterConfig {
                    actionable: Some(true),
                    ..Default::default()
                }),
                Some(now),
            )
            .unwrap(),
            &[
                "root",
                "unblocked",
                "unblocked-tombstone",
                "related",
                "undeferred",
            ],
        );
        // actionable: false is the complement, not a no-op.
        require_ids(
            &filter(
                &issues,
                &by(FilterConfig {
                    actionable: Some(false),
                    ..Default::default()
                }),
                Some(now),
            )
            .unwrap(),
            &[
                "closed-root",
                "gone-root",
                "blocked",
                "dangling",
                "deferred",
            ],
        );

        // A blocker hidden by the status filter still blocks.
        require_ids(
            &filter(
                &issues,
                &by(FilterConfig {
                    status: vec!["open".into()],
                    actionable: Some(true),
                    id_prefix: "block".into(),
                    ..Default::default()
                }),
                Some(now),
            )
            .unwrap(),
            &[],
        );
    }

    #[test]
    fn actionable_retains_full_dependency_authority() {
        let now = ts("2026-09-04T12:00:00Z");
        let visible = vec![
            issue("api-chain").depends_on("web", "blocks"),
            issue("api-closed").depends_on("done", "blocks"),
            issue("api-tombstone").depends_on("gone", "blocks"),
            issue("api-child").depends_on("parent", "parent-child"),
            issue("api-missing").depends_on("missing", "blocks"),
            issue("api-ongoing").status(Status::InProgress),
            issue("api-parked").status(Status::Blocked),
        ];
        let full: Vec<Issue> = visible
            .iter()
            .cloned()
            .chain([
                issue("web").depends_on("ops", "blocks"),
                issue("ops"),
                issue("done").status(Status::Closed),
                issue("gone").status(Status::Tombstone),
                issue("parent").depends_on("ops", "blocks"),
            ])
            .collect();
        let r = Recipe {
            filters: FilterConfig {
                actionable: Some(true),
                ..Default::default()
            },
            sort: SortConfig {
                field: SORT_FIELD_ID.into(),
                direction: "asc".into(),
                ..Default::default()
            },
            ..Default::default()
        };

        // The full-authority index can prove closed predecessors satisfied even
        // when they are hidden from the filtered slice.
        let authority = Readiness::new(&full);
        let metrics = Metrics {
            readiness: Some(&authority),
            ..Default::default()
        };
        require_ids(
            &apply(&visible, &metrics, &r, Some(now)).unwrap(),
            &["api-closed", "api-ongoing", "api-tombstone"],
        );

        // Without that authority they remain unknown, not ready.
        require_ids(
            &apply(&visible, &Metrics::default(), &r, Some(now)).unwrap(),
            &["api-ongoing"],
        );

        // Closing the inherited external blocker releases the child. The direct
        // web predecessor stays open, so api-chain is still withheld.
        let mut full2 = full.clone();
        for i in &mut full2 {
            if i.id == "ops" {
                i.status = Status::Closed;
            }
        }
        let authority2 = Readiness::new(&full2);
        let metrics2 = Metrics {
            readiness: Some(&authority2),
            ..Default::default()
        };
        require_ids(
            &apply(&visible, &metrics2, &r, Some(now)).unwrap(),
            &["api-child", "api-closed", "api-ongoing", "api-tombstone"],
        );

        // No selected candidates means no output, even with useful context.
        assert!(apply(&[], &metrics2, &r, Some(now)).unwrap().is_empty());
    }

    #[test]
    fn sort_defaults_and_every_field() {
        let now = jiff::Timestamp::now();
        let issues = vec![
            issue("A")
                .title("zzz")
                .priority(2)
                .created_at(now.checked_sub(jiff::Span::new().hours(1)).unwrap())
                .updated_at(now.checked_sub(jiff::Span::new().minutes(30)).unwrap()),
            issue("B")
                .title("aaa")
                .status(Status::Blocked)
                .created_at(now)
                .updated_at(now),
        ];

        let sorted = |field: &str, direction: &str| {
            let mut cp = issues.clone();
            sort_issues(
                &mut cp,
                &Metrics::default(),
                &Recipe {
                    sort: SortConfig {
                        field: field.into(),
                        direction: direction.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            );
            ids(&cp)
        };

        assert_eq!(sorted(SORT_FIELD_PRIORITY, ""), ["B", "A"]); // P0 first
        assert_eq!(sorted(SORT_FIELD_PRIORITY, "desc"), ["A", "B"]);
        assert_eq!(sorted(SORT_FIELD_CREATED, ""), ["B", "A"]); // newest first
        assert_eq!(sorted("created_at", "asc"), ["A", "B"]);
        assert_eq!(sorted(SORT_FIELD_UPDATED, ""), ["B", "A"]);
        assert_eq!(sorted("updated_at", "ASC"), ["A", "B"]);
        assert_eq!(sorted(SORT_FIELD_TITLE, ""), ["B", "A"]);
        assert_eq!(sorted(SORT_FIELD_TITLE, "desc"), ["A", "B"]);
        assert_eq!(sorted(SORT_FIELD_STATUS, ""), ["B", "A"]); // "blocked" < "open"

        // ID sorts naturally: bv-2 before bv-10.
        let mut id_issues = vec![issue("bv-10"), issue("bv-2"), issue("bv-1"), issue("x")];
        sort_issues(
            &mut id_issues,
            &Metrics::default(),
            &Recipe {
                sort: SortConfig {
                    field: SORT_FIELD_ID.into(),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        require_ids(&id_issues, &["bv-1", "bv-2", "bv-10", "x"]);

        // An unknown field compares equal and the ID tie-break decides.
        assert_eq!(sorted("unknown", ""), ["A", "B"]);

        // No sort field leaves input order alone.
        let mut cp = vec![issue("z"), issue("a")];
        sort_issues(&mut cp, &Metrics::default(), &Recipe::default());
        require_ids(&cp, &["z", "a"]);
    }

    #[test]
    fn natural_id_ordering() {
        assert!(natural_less("bv-2", "bv-10"));
        assert!(!natural_less("bv-10", "bv-2"));
        assert!(natural_less("bv-1", "bv-2"));
        // Different prefixes, or no digit run, fall back to lexical.
        assert!(natural_less("a", "bv-1"));
        assert!(natural_less("bv-1", "x"));
        assert!(!natural_less("bv-1", "bv-1"));
    }

    #[test]
    fn custom_workflow_statuses_validate_but_cannot_match_a_closed_enum() {
        // Go's Status is an open string, so "qa-review" both validates and can
        // match an issue carrying it. bv-core's Status is a closed 10-value
        // enum, so a recipe may only name the ten.
        let r = Recipe {
            filters: FilterConfig {
                status: vec!["QA-REVIEW".into(), "done".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(r.validate(), Ok(()));
        let issues = vec![issue("open"), issue("review").status(Status::Review)];
        assert!(apply(&issues, &Metrics::default(), &r, None)
            .unwrap()
            .is_empty());

        // A status this port does know is selectable...
        let by_status = Recipe {
            filters: FilterConfig {
                status: vec!["review".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        require_ids(
            &apply(&issues, &Metrics::default(), &by_status, None).unwrap(),
            &["review"],
        );

        // ...but is never ready to claim: `Ready` admits open and in_progress
        // only. Go drops a custom workflow status the same way, which is the
        // point of the original test.
        let actionable = Recipe {
            filters: FilterConfig {
                status: vec!["review".into()],
                actionable: Some(true),
                ..Default::default()
            },
            ..Default::default()
        };
        require_ids(
            &apply(&issues, &Metrics::default(), &actionable, None).unwrap(),
            &[],
        );
    }
}
