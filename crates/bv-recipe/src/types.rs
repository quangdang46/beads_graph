//! Recipe configuration types and validation — port of Go
//! `pkg/recipe/types.go` at commit `18afafa` (v0.25.0).
//!
//! A recipe is a reusable view configuration: [`Recipe::filters`] decide
//! membership, [`Recipe::sort`] decides ordering and [`Recipe::view`] supplies
//! presentation defaults. [`crate::apply`] is the one engine the TUI and the
//! robot path share; this module only describes and validates the shape.

use serde::{Deserialize, Serialize};

/// Recipe fields, validated by [`Recipe::validate`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    #[serde(default, rename = "name")]
    pub name: String,
    #[serde(default, rename = "description")]
    pub description: String,
    #[serde(default, rename = "filters")]
    pub filters: FilterConfig,
    #[serde(default, rename = "sort")]
    pub sort: SortConfig,
    #[serde(default, rename = "view")]
    pub view: ViewConfig,
    #[serde(default, rename = "export")]
    pub export: ExportConfig,
    /// Analysis metrics a view should surface. Setting this also enables metric
    /// display in the TUI.
    #[serde(default, rename = "metrics")]
    pub metrics: Vec<String>,
}

/// Which issues to include. Every field is honoured by [`crate::apply::filter`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilterConfig {
    /// Keep issues whose status equals one of these (case-insensitive).
    #[serde(default, rename = "status")]
    pub status: Vec<String>,
    /// Keep issues whose priority is one of these (0 = highest).
    #[serde(default, rename = "priority")]
    pub priority: Vec<i64>,
    /// Keep issues carrying ALL of these labels (case-insensitive).
    #[serde(default, rename = "tags")]
    pub tags: Vec<String>,
    /// Drop issues carrying ANY of these labels (case-insensitive).
    #[serde(default, rename = "exclude_tags")]
    pub exclude_tags: Vec<String>,
    /// Keep issues created at or after this; relative ("14d", "2w", "1m", "1y")
    /// or ISO date.
    #[serde(default, rename = "created_after")]
    pub created_after: String,
    /// Keep issues created at or before this; relative or ISO date.
    #[serde(default, rename = "created_before")]
    pub created_before: String,
    /// Keep issues updated at or after this; relative or ISO date.
    #[serde(default, rename = "updated_after")]
    pub updated_after: String,
    /// Keep issues updated at or before this; relative or ISO date.
    #[serde(default, rename = "updated_before")]
    pub updated_before: String,
    /// `Some(true)` = has an open blocking dependency, `Some(false)` = has none.
    #[serde(default, rename = "has_blockers")]
    pub has_blockers: Option<bool>,
    /// `Some(true)` = no open blockers and not deferred, `Some(false)` = the
    /// complement.
    #[serde(default, rename = "actionable")]
    pub actionable: Option<bool>,
    /// Case-insensitive substring match on the title.
    #[serde(default, rename = "title_contains")]
    pub title_contains: String,
    /// e.g. "bv-" for project filtering.
    #[serde(default, rename = "id_prefix")]
    pub id_prefix: String,
}

/// How to order issues.
///
/// `field` is one of the [`SORT_FIELD_*`] constants (`created_at`/`updated_at`
/// are accepted as aliases of `created`/`updated`). `direction` is `"asc"` or
/// `"desc"`; when empty the field's natural direction applies: priority, title,
/// id and status ascend, while created, updated and the graph metrics
/// (pagerank, betweenness, impact, triage) descend. [`Recipe::sort_chain`]
/// breaks ties and issue ID (natural order) breaks any remaining tie.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SortConfig {
    #[serde(default, rename = "field")]
    pub field: String,
    #[serde(default, rename = "direction")]
    pub direction: String,
    #[serde(default, rename = "secondary")]
    pub secondary: Option<Box<SortConfig>>,
}

/// Display options. `max_items` is applied by [`crate::apply::apply`]; the rest
/// configure the TUI list, detail and graph views.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewConfig {
    /// Ordered list columns; empty keeps the normal adaptive row.
    #[serde(default, rename = "columns")]
    pub columns: Vec<String>,
    /// Start in the dependency graph.
    #[serde(default, rename = "show_graph")]
    pub show_graph: bool,
    /// Show selected metric values on list rows and in issue details.
    #[serde(default, rename = "show_metrics")]
    pub show_metrics: bool,
    /// List groups: status, priority, tag (first sorted label), none.
    #[serde(default, rename = "group_by")]
    pub group_by: String,
    /// Start list groups collapsed; Enter toggles a group.
    #[serde(default, rename = "collapsed")]
    pub collapsed: bool,
    /// Keep only the first N issues after sorting (0 = unlimited).
    #[serde(default, rename = "max_items")]
    pub max_items: i64,
    /// Maximum title display cells; 0 uses available width.
    #[serde(default, rename = "truncate_title")]
    pub truncate_title: i64,
}

/// Export defaults, applied only when an export is explicitly requested.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportConfig {
    /// markdown, json, csv, mermaid.
    #[serde(default, rename = "format")]
    pub format: String,
    /// `None` uses the format default.
    #[serde(default, rename = "include_graph")]
    pub include_graph: Option<bool>,
    /// Markdown template path, relative to the working directory.
    #[serde(default, rename = "template")]
    pub template: String,
}

/// Column names accepted by [`ViewConfig::columns`] (Go `ViewColumns`).
pub const VIEW_COLUMNS: [&str; 8] = [
    "id", "title", "status", "priority", "created", "updated", "tags", "blockers",
];

/// Metric names accepted by [`Recipe::metrics`] (Go `ViewMetrics`).
pub const VIEW_METRICS: [&str; 9] = [
    "pagerank",
    "betweenness",
    "impact",
    "triage",
    "hub",
    "authority",
    "eigenvector",
    "kcore",
    "slack",
];

/// Sort fields accepted by [`SortConfig::field`].
pub const SORT_FIELD_PRIORITY: &str = "priority";
pub const SORT_FIELD_CREATED: &str = "created";
pub const SORT_FIELD_UPDATED: &str = "updated";
pub const SORT_FIELD_TITLE: &str = "title";
pub const SORT_FIELD_ID: &str = "id";
pub const SORT_FIELD_STATUS: &str = "status";
/// Reads [`crate::apply::GraphMetrics::page_rank_score`].
pub const SORT_FIELD_PAGERANK: &str = "pagerank";
/// Reads [`crate::apply::GraphMetrics::betweenness_score`].
pub const SORT_FIELD_BETWEENNESS: &str = "betweenness";
/// Reads [`crate::apply::GraphMetrics::critical_path_score`].
pub const SORT_FIELD_IMPACT: &str = "impact";
/// Reads the caller-supplied triage score map.
pub const SORT_FIELD_TRIAGE: &str = "triage";

/// Go `knownSortFields` — the order `validate` lists in its error message.
const KNOWN_SORT_FIELDS: [&str; 10] = [
    SORT_FIELD_PRIORITY,
    SORT_FIELD_CREATED,
    SORT_FIELD_UPDATED,
    SORT_FIELD_TITLE,
    SORT_FIELD_ID,
    SORT_FIELD_STATUS,
    SORT_FIELD_PAGERANK,
    SORT_FIELD_BETWEENNESS,
    SORT_FIELD_IMPACT,
    SORT_FIELD_TRIAGE,
];

/// Go `canonicalSortField` — lowercases, trims and resolves the
/// `created_at`/`updated_at` aliases. The bool is false for unknown fields, in
/// which case the returned value is the lowercased input, exactly as Go returns
/// `f` alongside `false`.
pub fn canonical_sort_field(field: &str) -> (std::borrow::Cow<'_, str>, bool) {
    let lowered = field.trim().to_lowercase();
    match KNOWN_SORT_FIELDS.iter().find(|k| **k == lowered) {
        Some(known) => (std::borrow::Cow::Borrowed(*known), true),
        None => match lowered.as_str() {
            "created_at" => (std::borrow::Cow::Borrowed(SORT_FIELD_CREATED), true),
            "updated_at" => (std::borrow::Cow::Borrowed(SORT_FIELD_UPDATED), true),
            _ => (std::borrow::Cow::Owned(lowered), false),
        },
    }
}

/// Go `isGraphMetricField` — whether the field reads graph metrics.
fn is_graph_metric_field(field: &str) -> bool {
    matches!(
        field,
        SORT_FIELD_PAGERANK | SORT_FIELD_BETWEENNESS | SORT_FIELD_IMPACT
    )
}

impl Recipe {
    /// Go `SortChain` — the sort configs in tie-break order: primary,
    /// secondary, secondary's secondary, and so on. Entries with an empty field
    /// are skipped.
    pub fn sort_chain(&self) -> Vec<&SortConfig> {
        let mut chain = Vec::new();
        let mut cursor = Some(&self.sort);
        while let Some(cfg) = cursor {
            if !cfg.field.trim().is_empty() {
                chain.push(cfg);
            }
            cursor = cfg.secondary.as_deref();
        }
        chain
    }

    /// Go `NeedsGraphMetrics` — whether the recipe sorts by pagerank,
    /// betweenness or impact, so callers can decide whether to run graph
    /// analysis first.
    pub fn needs_graph_metrics(&self) -> bool {
        self.sort_chain().into_iter().any(|s| {
            let (field, ok) = canonical_sort_field(&s.field);
            ok && is_graph_metric_field(&field)
        })
    }

    /// Go `NeedsTriageScores` — whether the recipe sorts by triage score.
    pub fn needs_triage_scores(&self) -> bool {
        self.sort_chain().into_iter().any(|s| {
            let (field, ok) = canonical_sort_field(&s.field);
            ok && field == SORT_FIELD_TRIAGE
        })
    }

    /// Go `Validate` — rejects recipes `apply` cannot honour: unknown sort
    /// fields or directions, malformed time filters, blank status values, a
    /// negative `max_items`/`truncate_title`, plus unsupported presentation
    /// fields. Status is deliberately an open vocabulary (Go
    /// `model.Status.IsValid` is only a nonblank test), so custom workflow
    /// states such as "qa-review" validate; the Rust `Status` enum is narrower,
    /// so a custom status can never appear on an issue to be matched against.
    ///
    /// Every problem found is reported, joined with `"; "`, in Go's order.
    pub fn validate(&self) -> Result<(), String> {
        let mut problems: Vec<String> = Vec::new();

        for s in self.sort_chain() {
            if !canonical_sort_field(&s.field).1 {
                problems.push(format!(
                    "sort.field {:?} is not one of {}",
                    s.field,
                    KNOWN_SORT_FIELDS.join(", ")
                ));
            }
            match s.direction.trim().to_lowercase().as_str() {
                "" | "asc" | "desc" => {}
                _ => problems.push(format!(
                    "sort.direction {:?} must be asc or desc",
                    s.direction
                )),
            }
        }

        // Go validates the time filters against `time.Now()`; only the parse
        // matters, and every field parses the same way, so one reference
        // instant is enough.
        let now = jiff::Timestamp::now();
        for (key, value) in [
            ("filters.created_after", &self.filters.created_after),
            ("filters.created_before", &self.filters.created_before),
            ("filters.updated_after", &self.filters.updated_after),
            ("filters.updated_before", &self.filters.updated_before),
        ] {
            if value.is_empty() {
                continue;
            }
            if let Err(err) = parse_relative_time(value, now) {
                problems.push(format!("{key}: {err}"));
            }
        }

        for s in &self.filters.status {
            // Go `model.Status(...).IsValid()` — any nonblank value is accepted.
            if s.trim().is_empty() {
                problems.push(format!("filters.status {s:?} must not be blank"));
            }
        }

        if self.view.max_items < 0 {
            problems.push(format!(
                "view.max_items {} must not be negative",
                self.view.max_items
            ));
        }
        if self.view.truncate_title < 0 {
            problems.push("view.truncate_title must not be negative".to_string());
        }

        for (name, values, known) in [
            ("view.columns", &self.view.columns, &VIEW_COLUMNS[..]),
            ("metrics", &self.metrics, &VIEW_METRICS[..]),
        ] {
            let mut seen: Vec<&str> = Vec::new();
            for value in values.iter() {
                if !known.contains(&value.as_str()) {
                    problems.push(format!(
                        "{name} {value:?} must be one of {}",
                        known.join(", ")
                    ));
                }
                if seen.contains(&value.as_str()) {
                    problems.push(format!("{name} repeats {value:?}"));
                }
                seen.push(value);
            }
        }

        match self.view.group_by.as_str() {
            "" | "none" | "status" | "priority" | "tag" => {}
            other => problems.push(format!(
                "view.group_by {other:?} must be status, priority, tag or none"
            )),
        }
        if self.view.collapsed && (self.view.group_by.is_empty() || self.view.group_by == "none") {
            problems.push("view.collapsed requires view.group_by".to_string());
        }

        match self.export.format.as_str() {
            "" | "markdown" | "json" | "csv" | "mermaid" => {}
            other => problems.push(format!(
                "export.format {other:?} must be markdown, json, csv or mermaid"
            )),
        }

        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems.join("; "))
        }
    }
}

/// Go `TimeParseError` — the only parse failure [`parse_relative_time`] reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeParseError {
    pub input: String,
}

impl std::fmt::Display for TimeParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "invalid time format: {} (expected relative like '14d', '2w', '1m' or ISO date)",
            self.input
        )
    }
}

impl std::error::Error for TimeParseError {}

/// Go `ParseRelativeTime` — turns a relative time string into an absolute
/// instant. Supports `Nd` (days), `Nw` (weeks), `Nm` (months), `Ny` (years)
/// case-insensitively; anything else is tried as RFC 3339, then as a naive
/// `YYYY-MM-DDTHH:MM:SS`, then as a date-only `YYYY-MM-DD`.
///
/// Go reads the last two forms with `time.ParseInLocation(format, s,
/// now.Location())`. Every production caller passes `robotNow()`, which is UTC
/// (`cmd/bv/main.go:1165-1172`), so this port reads them as UTC. An empty
/// string yields `None` — Go's zero `time.Time`, whose `IsZero()` the date
/// filters test.
pub fn parse_relative_time(
    s: &str,
    now: jiff::Timestamp,
) -> Result<Option<jiff::Timestamp>, TimeParseError> {
    if s.is_empty() {
        return Ok(None);
    }
    let s = s.trim();

    // Relative form: Go's `^(\d+)([dwmy])$` — a digit run followed by one
    // unit letter. Splitting on `chars` (not `split_at(len - 1)`) keeps a
    // multi-byte final character from panicking on a byte boundary, and an
    // unknown unit simply fails the regex and falls through to the absolute
    // formats below, exactly as Go does.
    let lowered = s.to_lowercase();
    if let Some(unit) = lowered.chars().next_back() {
        let digits = &lowered[..lowered.len() - unit.len_utf8()];
        if !digits.is_empty()
            && digits.bytes().all(|b| b.is_ascii_digit())
            && matches!(unit, 'd' | 'w' | 'm' | 'y')
        {
            // Go ignores `strconv.Atoi`'s error, so an overflowing count
            // becomes 0 and the threshold lands exactly on `now`.
            let n: i64 = digits.parse().unwrap_or(0);
            let span = match unit {
                'd' => jiff::Span::new().days(-n),
                'w' => jiff::Span::new().days(-n * 7),
                'm' => jiff::Span::new().months(-n),
                'y' => jiff::Span::new().years(-n),
                _ => unreachable!("unit already checked against dwmy"),
            };
            // `jiff::Timestamp` refuses calendar units (days, months, years);
            // it only models elapsed time. Go's `AddDate` is calendar
            // arithmetic, so re-anchor on the calendar before adding, exactly
            // as `bv`'s own `parse_relative_time` does.
            let zoned = now.to_zoned(jiff::tz::TimeZone::UTC);
            if let Ok(shifted) = zoned.checked_add(span) {
                return Ok(Some(shifted.timestamp()));
            }
        }
    }

    if let Ok(ts) = s.parse::<jiff::Timestamp>() {
        return Ok(Some(ts));
    }
    if let Ok(dt) = jiff::civil::DateTime::strptime("%Y-%m-%dT%H:%M:%S", s) {
        if let Ok(zoned) = dt.to_zoned(jiff::tz::TimeZone::UTC) {
            return Ok(Some(zoned.timestamp()));
        }
    }
    if let Ok(date) = jiff::civil::Date::strptime("%Y-%m-%d", s) {
        let dt = date.to_datetime(jiff::civil::Time::midnight());
        if let Ok(zoned) = dt.to_zoned(jiff::tz::TimeZone::UTC) {
            return Ok(Some(zoned.timestamp()));
        }
    }

    Err(TimeParseError {
        input: s.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> jiff::Timestamp {
        text.parse().expect("valid RFC3339 timestamp")
    }

    #[test]
    fn relative_days_weeks_months_years() {
        let now = at("2025-01-15T12:00:00Z");
        assert_eq!(
            parse_relative_time("14d", now).unwrap(),
            Some(at("2025-01-01T12:00:00Z"))
        );
        assert_eq!(
            parse_relative_time("2w", now).unwrap(),
            Some(at("2025-01-01T12:00:00Z"))
        );
        assert_eq!(
            parse_relative_time("7D", now).unwrap(),
            Some(at("2025-01-08T12:00:00Z"))
        );
        let mar = at("2025-03-15T12:00:00Z");
        assert_eq!(
            parse_relative_time("1m", mar).unwrap(),
            Some(at("2025-02-15T12:00:00Z"))
        );
        assert_eq!(
            parse_relative_time("1y", mar).unwrap(),
            Some(at("2024-03-15T12:00:00Z"))
        );
        // 0 of any unit is a no-op, which is how Go's ignored Atoi error
        // behaves for an overflowing count.
        assert_eq!(parse_relative_time("0d", now).unwrap(), Some(now));
    }

    #[test]
    fn a_month_back_from_a_short_month_is_constrained() {
        // Go's `AddDate` normalises 31 March minus one month to 3 March. jiff
        // constrains it to the last day of February instead. Both the Rust
        // recipe filters and `bv`'s own `parse_relative_time` take the jiff
        // reading, so the two stay consistent; recorded here because it is the
        // one place the calendar arithmetic differs from Go.
        let end_of_march = at("2025-03-31T00:00:00Z");
        assert_eq!(
            parse_relative_time("1m", end_of_march).unwrap(),
            Some(at("2025-02-28T00:00:00Z"))
        );
    }

    #[test]
    fn iso_forms_and_empty() {
        let now = jiff::Timestamp::now();
        // Date-only is midnight UTC, matching Go's ParseInLocation with the UTC
        // `robotNow()` reference.
        assert_eq!(
            parse_relative_time("2024-06-15", now).unwrap(),
            Some(at("2024-06-15T00:00:00Z"))
        );
        assert_eq!(
            parse_relative_time("2024-06-15T10:30:00Z", now).unwrap(),
            Some(at("2024-06-15T10:30:00Z"))
        );
        // A non-UTC offset resolves to the same instant.
        assert_eq!(
            parse_relative_time("2024-06-15T12:30:00+02:00", now).unwrap(),
            Some(at("2024-06-15T10:30:00Z"))
        );
        // The naive form Go reads with ParseInLocation is UTC here.
        assert_eq!(
            parse_relative_time("2024-06-15T10:30:00", now).unwrap(),
            Some(at("2024-06-15T10:30:00Z"))
        );
        assert_eq!(parse_relative_time("", now).unwrap(), None);
    }

    #[test]
    fn invalid_time_reports_the_go_message() {
        let err = parse_relative_time("fortnight", jiff::Timestamp::now()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid time format: fortnight (expected relative like '14d', '2w', '1m' or ISO date)"
        );
    }

    #[test]
    fn relative_time_requires_a_dwmy_unit() {
        // Go's `^(\d+)([dwmy])$` rejects every other suffix; these must not be
        // accepted as "now" and must not panic on a non-ASCII final character.
        let now = jiff::Timestamp::now();
        for bad in ["14x", "5!", "123", "14", "d", "1.5d", "12dd", "14\u{65e5}"] {
            assert!(
                parse_relative_time(bad, now).is_err(),
                "{bad:?} should be rejected"
            );
        }
        // A valid unit still parses, and absolute dates still fall through.
        assert!(parse_relative_time("14d", now).is_ok());
        assert_eq!(
            parse_relative_time("2024-06-15", now).unwrap(),
            Some(at("2024-06-15T00:00:00Z"))
        );
    }

    #[test]
    fn canonical_sort_field_resolves_aliases_and_case() {
        let c = |f: &str| {
            let (v, ok) = canonical_sort_field(f);
            (v.into_owned(), ok)
        };
        assert_eq!(c("created_at"), ("created".into(), true));
        assert_eq!(c(" UPDATED_AT "), ("updated".into(), true));
        assert_eq!(c("PageRank"), ("pagerank".into(), true));
        assert_eq!(c("karma"), ("karma".into(), false));
    }

    #[test]
    fn sort_chain_skips_empty_levels_and_walks_the_whole_chain() {
        let r = Recipe {
            sort: SortConfig {
                field: "pagerank".into(),
                secondary: Some(Box::new(SortConfig {
                    field: String::new(),
                    secondary: Some(Box::new(SortConfig {
                        field: "priority".into(),
                        ..Default::default()
                    })),
                    ..Default::default()
                })),
                ..Default::default()
            },
            ..Default::default()
        };
        let chain = r.sort_chain();
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].field, "pagerank");
        assert_eq!(chain[1].field, "priority");
        assert!(r.needs_graph_metrics());
        assert!(!r.needs_triage_scores());
    }

    #[test]
    fn presentation_validation_rejects_every_unsupported_value() {
        for (r, want) in [
            (
                Recipe {
                    view: ViewConfig {
                        columns: vec!["secret".into()],
                        ..Default::default()
                    },
                    ..Default::default()
                },
                "view.columns",
            ),
            (
                Recipe {
                    view: ViewConfig {
                        columns: vec!["id".into(), "id".into()],
                        ..Default::default()
                    },
                    ..Default::default()
                },
                "repeats",
            ),
            (
                Recipe {
                    view: ViewConfig {
                        group_by: "assignee".into(),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                "view.group_by",
            ),
            (
                Recipe {
                    view: ViewConfig {
                        collapsed: true,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                "view.collapsed",
            ),
            (
                Recipe {
                    view: ViewConfig {
                        truncate_title: -1,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                "view.truncate_title",
            ),
            (
                Recipe {
                    metrics: vec!["invented".into()],
                    ..Default::default()
                },
                "metrics",
            ),
        ] {
            let err = r.validate().unwrap_err();
            assert!(err.contains(want), "{err:?} should contain {want:?}");
        }
    }

    #[test]
    fn every_presentation_group_by_value_validates() {
        for group in ["", "none", "status", "priority", "tag"] {
            let r = Recipe {
                view: ViewConfig {
                    columns: VIEW_COLUMNS.iter().map(|s| s.to_string()).collect(),
                    group_by: group.into(),
                    truncate_title: 1,
                    ..Default::default()
                },
                metrics: VIEW_METRICS.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            };
            assert_eq!(r.validate(), Ok(()), "group_by {group:?}");
        }
    }

    #[test]
    fn validate_rejects_unusable_recipes_with_go_wording() {
        let cases: Vec<(Recipe, &str)> = vec![
            (
                Recipe {
                    sort: SortConfig {
                        field: "karma".into(),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                "sort.field \"karma\" is not one of priority, created, updated, title, id, status, pagerank, betweenness, impact, triage",
            ),
            (
                Recipe {
                    sort: SortConfig {
                        field: "priority".into(),
                        secondary: Some(Box::new(SortConfig {
                            field: "vibes".into(),
                            ..Default::default()
                        })),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                "sort.field \"vibes\"",
            ),
            (
                Recipe {
                    sort: SortConfig {
                        field: "priority".into(),
                        direction: "down".into(),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                "sort.direction \"down\" must be asc or desc",
            ),
            (
                Recipe {
                    filters: FilterConfig {
                        created_after: "yesterday".into(),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                "filters.created_after: invalid time format: yesterday",
            ),
            (
                Recipe {
                    filters: FilterConfig {
                        updated_before: "1 month".into(),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                "filters.updated_before: invalid time format: 1 month",
            ),
            (
                Recipe {
                    filters: FilterConfig {
                        status: vec!["open".into(), "   ".into()],
                        ..Default::default()
                    },
                    ..Default::default()
                },
                "filters.status \"   \" must not be blank",
            ),
            (
                Recipe {
                    filters: FilterConfig {
                        status: vec!["open".into(), String::new()],
                        ..Default::default()
                    },
                    ..Default::default()
                },
                "filters.status \"\" must not be blank",
            ),
            (
                Recipe {
                    view: ViewConfig {
                        max_items: -1,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                "view.max_items -1 must not be negative",
            ),
            (
                Recipe {
                    export: ExportConfig {
                        format: "shell".into(),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                "export.format \"shell\" must be markdown, json, csv or mermaid",
            ),
        ];
        for (r, want) in cases {
            let err = r.validate().unwrap_err();
            assert!(err.contains(want), "{err:?} should contain {want:?}");
        }
    }

    #[test]
    fn a_complete_recipe_validates() {
        let r = Recipe {
            name: "x".into(),
            description: "d".into(),
            filters: FilterConfig {
                status: vec!["OPEN".into(), "in_progress".into()],
                updated_after: "14d".into(),
                created_before: "2026-01-01".into(),
                ..Default::default()
            },
            sort: SortConfig {
                field: "updated_at".into(),
                direction: "DESC".into(),
                secondary: Some(Box::new(SortConfig {
                    field: "pagerank".into(),
                    ..Default::default()
                })),
            },
            view: ViewConfig {
                columns: vec!["id".into()],
                show_graph: true,
                show_metrics: true,
                group_by: "status".into(),
                collapsed: true,
                max_items: 5,
                truncate_title: 40,
            },
            export: ExportConfig {
                format: "markdown".into(),
                include_graph: Some(true),
                template: "t.tmpl".into(),
            },
            metrics: vec!["pagerank".into()],
        };
        assert_eq!(r.validate(), Ok(()));
    }

    #[test]
    fn needs_flags_read_the_whole_chain() {
        let triage = Recipe {
            sort: SortConfig {
                field: "priority".into(),
                secondary: Some(Box::new(SortConfig {
                    field: "triage".into(),
                    ..Default::default()
                })),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(triage.needs_triage_scores());
        assert!(!triage.needs_graph_metrics());
    }

    #[test]
    fn unknown_yaml_keys_are_rejected_by_name() {
        // Go's `decodeStrict` sets `KnownFields(true)`, so a misspelt filter is
        // reported instead of silently dropped.
        let err = serde_yaml_ng::from_str::<Recipe>("filters:\n  statuses: [open]\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("statuses"), "{err}");
    }
}
