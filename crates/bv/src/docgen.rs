//! Port of Go `internal/docgen` (beads_viewer/internal/docgen/docgen.go).
//!
//! `bv --generate-docs` renders seven Markdown tables from the live
//! registries, writes each under `docs/generated/`, writes `constants.json`,
//! and rewrites the marker blocks in README.md and AGENTS.md. Every function
//! here mirrors one Go function; the Go line is cited so the two can be diffed.
//!
//! The previous `--generate-docs` handler wrote two invented files instead
//! (`bvr-docs.md`, `bvr-docs.json`), so the two binaries produced disjoint
//! output trees under the same directory.

use crate::flags;

/// Go `DefaultFlagSections` (docgen.go:49-132). Each entry mirrors a section
/// of `rootHelpSections` in cmd/bv/main.go; the first match wins, and a flag
/// matching none lands in "Other".
/// Go `rootHelpSections` (cmd/bv/main.go:64-...), transcribed. The Go matcher
/// is `isOneOf(...) || hasAnyPrefix(name, "...") || strings.HasPrefix(name,
/// "robot-")`, so each entry carries an exact list plus a prefix list; the
/// first section that matches wins and an unmatched flag lands in "Other".
///
/// This table is what `bv --generate-docs` groups by, and it is NOT the same
/// list as `docgen.DefaultFlagSections` — that one is docgen's own copy and has
/// drifted from the help. Go's `RenderFlagsTable` receives `opts.Sections`,
/// which `cmd/bv` fills from `rootHelpSections`, so the help grouping is the
/// one that lands in flags.md.
struct FlagSection {
    title: &'static str,
    exact: &'static [&'static str],
    prefixes: &'static [&'static str],
}

const FLAG_SECTIONS: &[FlagSection] = &[
    FlagSection {
        title: "General Flags",
        exact: &[
            "background-mode",
            "check-update",
            "cpu-profile",
            "db",
            "force-full-analysis",
            "format",
            "help",
            "no-background-mode",
            "no-cache",
            "profile-json",
            "profile-startup",
            "rollback",
            "stats",
            "theme",
            "update",
            "update-dry-run",
            "version",
            "yes",
        ],
        prefixes: &[],
    },
    FlagSection {
        title: "Search & Filters",
        exact: &[
            "alert-label",
            "alert-type",
            "label",
            "recipe",
            "repo",
            "robot-by-assignee",
            "robot-by-label",
            "robot-max-results",
            "robot-min-confidence",
            "search",
            "search-",
            "severity",
            "workspace",
        ],
        prefixes: &["search-"],
    },
    FlagSection {
        title: "Robot & Planning Flags",
        exact: &[
            "agents",
            "attention-limit",
            "brief",
            "capacity-label",
            "correlation-",
            "file-beads-limit",
            "forecast-agents",
            "forecast-label",
            "forecast-sprint",
            "graph-depth",
            "graph-format",
            "graph-root",
            "hotspots-limit",
            "orphans-min-score",
            "related-include-closed",
            "related-max-results",
            "related-min-relevance",
            "relations-limit",
            "relations-threshold",
            "robot-",
            "schema-command",
            "suggest-bead",
            "suggest-confidence",
            "suggest-type",
        ],
        // Go's matcher here is `strings.HasPrefix(name, "robot-") ||
        // isOneOf(...)`, and the `robot-` prefix was missing from this
        // transcription. Every `--robot-*` flag therefore fell through to
        // "Other" and 43 rows carried the wrong group column.
        prefixes: &["robot-", "correlation-"],
    },
    FlagSection {
        title: "History & Drift",
        exact: &[
            "as-of",
            "baseline-info",
            "bead-history",
            "check-drift",
            "diff-since",
            "history-limit",
            "history-since",
            "min-confidence",
            "save-baseline",
        ],
        prefixes: &[],
    },
    FlagSection {
        title: "Export & Reporting",
        exact: &[
            "agent-brief",
            "debug-height",
            "debug-render",
            "debug-width",
            "emit-script",
            "export",
            "export-format",
            "export-graph",
            "export-include-graph",
            "export-md",
            "export-pages",
            "export-template",
            "graph-preset",
            "graph-title",
            "no-hooks",
            "no-live-reload",
            "pages",
            "pages-include-closed",
            "pages-include-history",
            "pages-title",
            "preview-pages",
            "priority-brief",
            "script-format",
            "script-limit",
            "watch-export",
        ],
        prefixes: &[],
    },
    FlagSection {
        title: "Agent File Management",
        // Go's matcher is `strings.HasPrefix(name, "agents-")`, so this entry
        // belongs in `prefixes`. It sat in `exact`, where it matched only a flag
        // literally named `agents-`, and the six real ones (add, check,
        // dry-run, force, remove, update) fell through to "Other".
        exact: &[],
        prefixes: &["agents-"],
    },
    // No "Debug Flags" section, deliberately. `--generate-docs` does not use
    // `docgen.DefaultFlagSections` (docgen.go:48), which does define one; it
    // builds its section list from `rootHelpSections` (main.go:1747-1752), and
    // that list has six entries, none of them "Debug Flags". Go's
    // `isDebug = s.Title == "Debug Flags"` (docgen.go:157) therefore never
    // fires for this caller, and the debug-last sort arm never engages.
    //
    // An empty section here matched nothing but invited the belief that
    // `--generate-docs` and friends landed in a debug group — they do not.
    // In `rootHelpSections` they are split by role: `cpu-profile`,
    // `profile-startup` and `profile-json` are "General Flags", and
    // `debug-render`, `debug-width` and `debug-height` are "Export &
    // Reporting" (main.go:73-74, 186-188). `generate-docs` itself is in no
    // section at all and renders as "Other", which is what Go emits.
];

fn section_for(name: &str) -> &'static str {
    for s in FLAG_SECTIONS {
        if s.exact.contains(&name) {
            return s.title;
        }
        if s.prefixes.iter().any(|p| name.starts_with(p)) {
            return s.title;
        }
    }
    "Other"
}

/// Go's flag type string — `pflag.Value.Type()` verbatim, which for the
/// project's own flags is `float64` and `percent_or_fraction`, not the
/// normalised `float`/`string` the other variants use.
fn flag_type_name(kind: flags::FlagKind) -> &'static str {
    match kind {
        flags::FlagKind::Bool => "bool",
        flags::FlagKind::Int => "int",
        flags::FlagKind::Float => "float",
        flags::FlagKind::Float64 => "float64",
        flags::FlagKind::PercentOrFraction => "percent_or_fraction",
        // Both repeatable spellings render as pflag's `stringArray`.
        flags::FlagKind::StringArray => "stringArray",
        flags::FlagKind::Str => "string",
    }
}

/// Go `RenderFlagsTable` (docgen.go:133-207).
///
/// Sort order is load-bearing for a byte comparison: debug flags last, then
/// group, then name (docgen.go:181-190).
pub fn render_flags_table() -> String {
    struct Row {
        name: &'static str,
        type_str: &'static str,
        default: String,
        description: String,
        group: &'static str,
        is_debug: bool,
    }

    let mut rows: Vec<Row> = Vec::new();
    for f in flags::all_flags() {
        let group = section_for(f.name);
        // Go's `DefValue` is `pflag`'s own rendering, and an empty default
        // becomes the literal `(empty)` (docgen.go:159-167). Every non-empty
        // default is wrapped in backticks.
        let default = if f.default.is_empty() {
            "(empty)".to_string()
        } else {
            format!("`{}`", f.default)
        };
        rows.push(Row {
            name: f.name,
            type_str: flag_type_name(f.kind),
            default,
            description: f.description.replace('\n', " "),
            group,
            is_debug: group == "Debug Flags",
        });
    }
    rows.sort_by(|a, b| {
        a.is_debug
            .cmp(&b.is_debug)
            .then_with(|| a.group.cmp(b.group))
            .then_with(|| a.name.cmp(b.name))
    });

    let mut out = String::from("| Flag | Type | Default | Description | Group |\n");
    out.push_str("| :--- | :--- | :--- | :--- | :--- |\n");
    for r in &rows {
        out.push_str(&format!(
            "| `--{}` | {} | {} | {} | {} |\n",
            r.name, r.type_str, r.default, r.description, r.group
        ));
    }
    out.trim_end().to_string()
}

/// Go `RenderEnvTable` (docgen.go:209-224). Backticks around the name, raw
/// default (so `(none)` stays as written).
pub fn render_env_table() -> String {
    let mut out = String::from("| Variable | Description | Default |\n");
    out.push_str("|:---|:---|:---|\n");
    for v in flags::ENV_VARS {
        out.push_str(&format!(
            "| `{}` | {} | {} |\n",
            v.name, v.description, v.default
        ));
    }
    out.trim_end().to_string()
}

/// Go `RenderAlertsTables` (docgen.go:226-357). Two tables: the proactive
/// checks that need no baseline, then the drift checks that compare against
/// one. The threshold text is interpolated from the live config, so it tracks
/// any change to the defaults.
pub fn render_alerts_tables() -> String {
    let cfg = bv_analysis::drift::DriftConfig::default();
    let mut out = String::new();
    out.push_str("**Proactive checks** (run on the current graph, no baseline needed):\n\n");
    out.push_str("| Type | Trigger | Severity | `.bv/drift.yaml` keys (default) |\n");
    out.push_str("|------|---------|----------|----------------------------------|\n");

    // (type, trigger, severity, keys) — docgen.go:239-277.
    let proactive: [(&str, &str, &str, String); 7] = [
        (
            "stale_issue",
            "No activity for `stale_warning_days` (warning) or `stale_critical_days` (critical); thresholds are multiplied by `in_progress_stale_multiplier` for `in_progress` issues; `label_overrides` can tighten or loosen per label",
            "Warning / Critical",
            format!(
                "`stale_warning_days` ({}), `stale_critical_days` ({}), `in_progress_stale_multiplier` ({:.1})",
                cfg.stale_warning_days, cfg.stale_critical_days, cfg.in_progress_stale_multiplier
            ),
        ),
        (
            "blocking_cascade",
            "Actionable issue unblocks N+ others",
            "Info / Warning",
            format!(
                "`blocking_cascade_info_threshold` ({}), `blocking_cascade_warning_threshold` ({})",
                cfg.blocking_cascade_info_threshold, cfg.blocking_cascade_warning_threshold
            ),
        ),
        (
            "high_impact_unblock",
            "Actionable issue unblocks N+ others of which at least one is P0/P1 (two or more urgent items escalate to warning)",
            "Info / Warning",
            format!(
                "`high_impact_unblock_min` ({}), `high_impact_priority_max` ({})",
                cfg.high_impact_unblock_min, cfg.high_impact_priority_max
            ),
        ),
        (
            "abandoned_claim",
            "An `in_progress` issue with an assignee idle longer than `stale_warning_days` x `in_progress_stale_multiplier` x `abandoned_claim_multiplier` (14 days by default)",
            "Warning",
            format!(
                "`abandoned_claim_multiplier` ({})",
                fmt_g(cfg.abandoned_claim_multiplier)
            ),
        ),
        (
            "potential_duplicate",
            "Two open issues whose title/description keyword Jaccard similarity reaches the threshold (same detector as `--robot-suggest`); closed issues are never paired",
            "Info",
            format!(
                "`duplicate_jaccard_threshold` ({:.1}), `duplicate_max_alerts` ({})",
                cfg.duplicate_jaccard_threshold, cfg.duplicate_max_alerts
            ),
        ),
        (
            "priority_mismatch",
            "`--robot-priority` recommends a *higher* priority with confidence at or above the floor (downgrade suggestions stay in `--robot-priority`)",
            "Warning",
            format!(
                "`priority_mismatch_min_confidence` ({:.1})",
                cfg.priority_mismatch_min_confidence
            ),
        ),
        (
            "velocity_drop",
            "Closes in the last window fell by the percentage or more versus the previous window, which must contain at least the baseline count of closes",
            "Warning",
            format!(
                "`velocity_drop_pct` ({}), `velocity_window_days` ({}), `velocity_min_baseline` ({})",
                fmt_g(cfg.velocity_drop_pct), cfg.velocity_window_days, cfg.velocity_min_baseline
            ),
        ),
    ];
    for (t, trig, sev, keys) in &proactive {
        out.push_str(&format!("| `{t}` | {trig} | {sev} | {keys} |\n"));
    }

    out.push_str("\n**Drift checks** (compare the current graph with the baseline saved by `bvr --save-baseline`):\n\n");
    out.push_str("| Type | Trigger | Severity | `.bv/drift.yaml` keys (default) |\n");
    out.push_str("|------|---------|----------|----------------------------------|\n");

    // (type, trigger, severity, keys) — docgen.go:285-345.
    let checks: [(&str, &str, &str, String); 8] = [
        (
            "new_cycle",
            "A cycle exists that the baseline did not have",
            "Critical",
            "(always on unless disabled)".to_string(),
        ),
        (
            "density_growth",
            "Graph density up by the info or warning percentage",
            "Info / Warning",
            format!(
                "`density_info_pct` ({}), `density_warning_pct` ({})",
                fmt_g(cfg.density_info_pct),
                fmt_g(cfg.density_warning_pct)
            ),
        ),
        (
            "node_count_change",
            "Node count changed by the percentage or more",
            "Info",
            format!(
                "`node_growth_info_pct` ({})",
                fmt_g(cfg.node_growth_info_pct)
            ),
        ),
        (
            "edge_count_change",
            "Edge count changed by the percentage or more",
            "Info",
            format!(
                "`edge_growth_info_pct` ({})",
                fmt_g(cfg.edge_growth_info_pct)
            ),
        ),
        (
            "scope_creep",
            "Open-issue count grew by the percentage or more since the baseline",
            "Info",
            format!("`scope_creep_pct` ({})", fmt_g(cfg.scope_creep_pct)),
        ),
        (
            "blocked_increase",
            "N or more additional blocked issues",
            "Warning",
            format!(
                "`blocked_increase_threshold` ({})",
                cfg.blocked_increase_threshold
            ),
        ),
        (
            "actionable_change",
            "Actionable count down by the warning percentage, or changed by the info percentage",
            "Info / Warning",
            format!(
                "`actionable_decrease_warning_pct` ({}), `actionable_increase_info_pct` ({})",
                fmt_g(cfg.actionable_decrease_warning_pct),
                fmt_g(cfg.actionable_increase_info_pct)
            ),
        ),
        (
            "pagerank_change",
            "A top-metric issue's PageRank moved by the percentage or more",
            "Warning",
            format!(
                "`pagerank_change_warning_pct` ({})",
                fmt_g(cfg.pagerank_change_warning_pct)
            ),
        ),
    ];
    // Go lists exactly eight drift checks; the array above is sized to the
    // rows actually emitted, and a shorter list would silently drop a check.
    debug_assert_eq!(checks.len(), 8, "docgen drift table lost a row");
    for (t, trig, sev, keys) in &checks {
        out.push_str(&format!("| `{t}` | {trig} | {sev} | {keys} |\n"));
    }

    out.trim_end().to_string()
}

/// Go's `%g` verb, which is what the drift thresholds print with. Rust's `{}`
/// for f64 differs for values like 20.0 (Go prints `20`, Rust `20` too) but
/// for 0.25 both print `0.25`; the cases that matter here are all short
/// decimals, so a plain `{}` matches. Kept as a named helper so the choice is
/// visible rather than incidental.
fn fmt_g(v: f64) -> String {
    format!("{v}")
}

/// Go `RenderRecipesTable` (docgen.go:359-397). Recipes are listed in a fixed
/// order rather than sorted, and any recipe outside that list is appended
/// alphabetically so a new one is never dropped.
pub fn render_recipes_table() -> String {
    const ORDER: &[&str] = &[
        "default",
        "actionable",
        "recent",
        "blocked",
        "high-impact",
        "stale",
        "triage",
        "closed",
        "release-cut",
        "quick-wins",
        "bottlenecks",
    ];
    let known: Vec<&str> = flags::RECIPES
        .iter()
        .map(|r| r.name)
        .filter(|n| ORDER.contains(n))
        .collect();
    let mut names: Vec<&str> = ORDER
        .iter()
        .copied()
        .filter(|n| known.contains(n))
        .collect();
    let mut extra: Vec<&str> = flags::RECIPES
        .iter()
        .map(|r| r.name)
        .filter(|n| !ORDER.contains(n))
        .collect();
    extra.sort_unstable();
    names.extend(extra);

    let mut out = String::from("| Recipe | Purpose |\n|:---|:---|\n");
    for name in names {
        let desc = flags::RECIPES
            .iter()
            .find(|r| r.name == name)
            .map(|r| r.description)
            .unwrap_or_default();
        out.push_str(&format!("| `{name}` | {desc} |\n"));
    }
    out.trim_end().to_string()
}

/// Go `RenderPresetsTable` (docgen.go:398-425). The description map is
/// fixed text; the weight columns come from the live preset table.
pub fn render_presets_table() -> String {
    let mut out = String::from(
        "| Preset | Text | PageRank | Status | Impact | Priority | Recency | Description |\n",
    );
    out.push_str("|:---|:---:|:---:|:---:|:---:|:---:|:---:|:---|\n");
    for p in flags::SEARCH_PRESETS {
        out.push_str(&format!(
            "| `{}` | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {} |\n",
            p.name, p.text, p.pagerank, p.status, p.impact, p.priority, p.recency, p.description
        ));
    }
    out.trim_end().to_string()
}

/// Go `RenderKeysTable` (docgen.go:426-457). Go prints the context cell only
/// on the first row of each context group, so the table reads as sections.
pub fn render_keys_table() -> String {
    let mut out = String::from("| Context | Key | Action |\n");
    out.push_str("| :--- | :---: | :--- |\n");
    let mut last_context = "";
    for b in flags::key_bindings() {
        let context_display = if b.context != last_context {
            last_context = b.context;
            format!("**{}**", b.context)
        } else {
            String::new()
        };
        out.push_str(&format!(
            "| {context_display} | `{}` | {} |\n",
            b.key, b.action
        ));
    }
    out.trim_end().to_string()
}

/// Go `RenderSortModesTable` (docgen.go:458-468). Entirely literal.
pub fn render_sort_modes_table() -> String {
    let mut out = String::from("| Mode | Key Display | Ordering Logic | Use Case |\n");
    out.push_str("|:---|:---:|:---|:---|\n");
    for (mode, display, logic, use_case) in [
        (
            "**Default**",
            "Default",
            "Priority (asc) → Created (desc)",
            "Standard priority-driven workflow",
        ),
        (
            "**Created ↑**",
            "Created ↑",
            "Creation date ascending (oldest first)",
            "Audit: find long-standing issues",
        ),
        (
            "**Created ↓**",
            "Created ↓",
            "Creation date descending (newest first)",
            "Review: see recently created work",
        ),
        (
            "**Priority**",
            "Priority",
            "Priority only (P0 → P4)",
            "Pure priority triage",
        ),
        (
            "**Updated**",
            "Updated",
            "Last update descending (newest first)",
            "Activity tracking: see active issues",
        ),
    ] {
        out.push_str(&format!(
            "| {mode} | `{display}` | {logic} | {use_case} |\n"
        ));
    }
    out.trim_end().to_string()
}

/// Go `GenerateConstantsJSON` (docgen.go:471-581). The values come from the
/// live registries, so this documents the build rather than a stale copy.
pub fn generate_constants_json() -> String {
    let w = bv_analysis::scoring::default_weights();
    let drift = bv_analysis::drift::DriftConfig::default();
    use bv_correlation::scorer::Method;
    let corr_range = |m: Method, description: &str| -> serde_json::Value {
        let (min, max) = m.range();
        serde_json::json!({ "min": min, "max": max, "description": description })
    };
    let json = serde_json::json!({
        "impact_weights": {
            "pagerank": w.pagerank,
            "betweenness": w.betweenness,
            "blocker_ratio": w.blocker_ratio,
            "staleness": w.staleness,
            "priority_boost": w.priority_boost,
            "time_to_impact": w.time_to_impact,
            "urgency": w.urgency,
            "risk": w.risk,
        },
        "label_health": {
            // Go docgen.go:487-492 — the four label-health factors are
            // equally weighted; the previous zeros documented a constant that
            // bv-analysis has not exported.
            "weights": {
                "velocity": 0.25,
                "freshness": 0.25,
                "flow": 0.25,
                "criticality": 0.25,
            },
            "thresholds": {
                "healthy": bv_analysis::label_health::HEALTHY_THRESHOLD,
                "warning": bv_analysis::label_health::WARNING_THRESHOLD,
                "stale_days": bv_analysis::label_health::DEFAULT_STALE_THRESHOLD_DAYS,
            },
        },
        // Go `MethodRanges` (pkg/correlation/scorer.go) — read from the live
        // table rather than copied, so a range change shows up here.
        "correlation_ranges": {
            "co_committed": corr_range(Method::CoCommitted,
                "Changed in same commit as bead update (direct causation)"),
            "explicit_id": corr_range(Method::ExplicitId,
                "Commit message explicitly references bead ID (developer intent)"),
            "temporal_author": corr_range(Method::TemporalAuthor,
                "By same author during bead's active window (temporal correlation)"),
        },
        "staleness_thresholds": {
            "drift_stale_warning_days": drift.stale_warning_days,
            "drift_stale_critical_days": drift.stale_critical_days,
            "drift_in_progress_stale_multiplier": drift.in_progress_stale_multiplier,
            "label_health_stale_days": bv_analysis::label_health::DEFAULT_STALE_THRESHOLD_DAYS,
            "priority_staleness_days": 30,
        },
        "timeout_tiers": {
            "small":  { "max_nodes": 99,  "timeout_ms": 2000, "mode": "exact" },
            "medium": { "min_nodes": 100, "max_nodes": 499,  "timeout_ms": 500,  "mode": "exact" },
            "large":  { "min_nodes": 500, "max_nodes": 1999, "timeout_ms": 300,  "mode": "approximate",
                        "approx_betweenness_timeout_ms": 500 },
            "xl":     { "min_nodes": 2000, "timeout_ms": 200,  "mode": "approximate",
                        "approx_betweenness_timeout_ms": 500 },
        },
    });
    // Go builds this from `map[string]any` (docgen.go:474), and Go's
    // encoding/json sorts map keys. Rust's `serde_json` is built with
    // `preserve_order`, so the object literal would serialize in insertion
    // order and every key would be misplaced. Sorting here matches the
    // oracle. The robot envelope is unaffected: it is a Go *struct*, so
    // there the declaration order is the contract.
    // Rust's  on a Map is shallow, and Go's marshaller sorts at
    // every level, so the walk has to recurse.
    fn sort_json_deep(v: serde_json::Value) -> serde_json::Value {
        match v {
            serde_json::Value::Object(mut m) => {
                m.sort_keys();
                let sorted: serde_json::Map<String, serde_json::Value> = m
                    .into_iter()
                    .map(|(k, val)| (k, sort_json_deep(val)))
                    .collect();
                serde_json::Value::Object(sorted)
            }
            serde_json::Value::Array(a) => {
                serde_json::Value::Array(a.into_iter().map(sort_json_deep).collect())
            }
            other => other,
        }
    }
    // Go's `enc.SetIndent("", "  ")` (docgen.go:573) — two-space indent, and
    // `Encoder.Encode` appends the trailing newline that `go_json_string` does
    // not emit.
    format!("{}\n", crate::go_json_string_pretty(&sort_json_deep(json)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flag_lands_in_exactly_one_section() {
        assert_eq!(section_for("db"), "General Flags");
        assert_eq!(section_for("search-limit"), "Search & Filters");
        // Prefix match, per Go's robot section.
        assert_eq!(section_for("robot-triage"), "Robot & Planning Flags");
        assert_eq!(section_for("export-pages"), "Export & Reporting");
        // `generate-docs` is in no `rootHelpSections` entry, so Go leaves it
        // "Other" (main.go:151-158). It is not a debug-group flag.
        assert_eq!(section_for("generate-docs"), "Other");
        // The flags whose *names* start with `debug-` are not debug-group
        // flags either — Go files them under Export & Reporting (main.go:186-188).
        assert_eq!(section_for("debug-render"), "Export & Reporting");
        // Nor are the `profile-*` / `cpu-profile` flags, which are General.
        assert_eq!(section_for("profile-json"), "General Flags");
        assert_eq!(section_for("something-unlisted"), "Other");
    }

    #[test]
    fn no_flag_lands_in_a_debug_group() {
        // Go's `isDebug` arm of the comparator (docgen.go:158, 186-188) never
        // fires for the section list `--generate-docs` actually passes, so the
        // table sorts by group then name alone. This test pins that the port
        // reaches the same conclusion rather than sorting on a group that the
        // oracle never assigns.
        let table = render_flags_table();
        assert!(
            !table.contains("| Debug Flags |"),
            "no row may be grouped as Debug Flags: {table}"
        );
    }

    #[test]
    fn rows_sort_by_group_then_name() {
        // The comparator's first live key is the group, then the name; the
        // debug arm is dead (see `no_flag_lands_in_a_debug_group`). Group
        // ordering is plain string order, so "Other" sits between
        // "History & Drift" and "Robot & Planning Flags".
        let table = render_flags_table();
        let pos = |needle: &str| {
            table
                .find(needle)
                .unwrap_or_else(|| panic!("{needle} missing from table"))
        };
        assert!(pos("| General Flags |") < pos("| History & Drift |"));
        assert!(pos("| History & Drift |") < pos("| Other |"));
        assert!(pos("| Other |") < pos("| Robot & Planning Flags |"));
    }

    #[test]
    fn an_empty_default_renders_as_the_go_placeholder() {
        // Go substitutes the bare literal `(empty)` for an empty DefValue
        // (docgen.go:166-167) and wraps only non-empty defaults in backticks
        // (docgen.go:168-173) — so the placeholder is *not* backticked.
        let table = render_flags_table();
        assert!(table.contains("| (empty) |"), "{table}");
        assert!(!table.contains("| `(empty)` |"), "{table}");
    }

    #[test]
    fn every_table_has_its_go_header() {
        assert!(render_flags_table().starts_with("| Flag | Type | Default | Description | Group |"));
        assert!(render_env_table().starts_with("| Variable | Description | Default |"));
        assert!(render_alerts_tables().starts_with("**Proactive checks**"));
        assert!(render_recipes_table().starts_with("| Recipe | Purpose |"));
        assert!(render_presets_table().starts_with("| Preset | Text | PageRank |"));
        assert!(render_keys_table().starts_with("| Context | Key | Action |"));
        assert!(render_sort_modes_table().starts_with("| Mode | Key Display |"));
    }

    #[test]
    fn the_alerts_table_keeps_both_halves() {
        let t = render_alerts_tables();
        assert!(t.contains("**Proactive checks**"), "proactive half missing");
        assert!(t.contains("**Drift checks**"), "drift half missing");
        // The eight drift checks Go lists (docgen.go:285-345).
        for key in [
            "new_cycle",
            "density_growth",
            "node_count_change",
            "edge_count_change",
            "scope_creep",
            "blocked_increase",
            "actionable_change",
            "pagerank_change",
        ] {
            assert!(t.contains(&format!("`{key}`")), "drift check {key} missing");
        }
    }

    #[test]
    fn constants_json_carries_every_registry() {
        let v: serde_json::Value =
            serde_json::from_str(&generate_constants_json()).expect("valid JSON");
        assert!(v["impact_weights"]["pagerank"].is_number());
        assert!(v["label_health"]["weights"]["velocity"].is_number());
        assert!(v["timeout_tiers"]["xl"]["timeout_ms"].is_number());
    }
}
