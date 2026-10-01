//! Flag registry — port of Go `cmd/bv` flag definitions (main.go:1417-1663)
//! with category grouping, robot-primary classification, and the validation
//! tables Go drives its three post-parse checks from: the ordered
//! modifier-requires rules (main.go:1786-1843), the modifier-requires recovery
//! examples (main.go:257-316), and the enum-value rules (main.go:1845-1848).

/// A single CLI flag definition.
#[derive(Debug, Clone)]
pub struct FlagDef {
    /// Long name without leading dashes.
    pub name: &'static str,
    /// Value kind — used for clap value parsing and validation.
    pub kind: FlagKind,
    /// True when this flag alone is a "primary command" (exclusive group).
    pub primary: bool,
    /// Exclusive group id when primary (Go: 37 groups).
    pub group: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlagKind {
    Bool,
    Str,
    Int,
    Float,
    /// pflag's `float64` (vendor/.../float64.go:20). Distinct from `Float`
    /// only in the string Go prints in `--generate-docs`; parsing is the same.
    Float64,
    /// Go's `percentOrFraction` (cmd/bv/flag_types.go:60).
    PercentOrFraction,
    /// pflag's `stringArray` (vendor/.../string_array.go) — repeatable, and
    /// pflag renders its nil default as the literal `[]`.
    StringArray,
}

const fn b(name: &'static str) -> FlagDef {
    FlagDef {
        name,
        kind: FlagKind::Bool,
        primary: false,
        group: None,
    }
}
const fn s(name: &'static str) -> FlagDef {
    FlagDef {
        name,
        kind: FlagKind::Str,
        primary: false,
        group: None,
    }
}
const fn i(name: &'static str) -> FlagDef {
    FlagDef {
        name,
        kind: FlagKind::Int,
        primary: false,
        group: None,
    }
}

// ---------------------------------------------------------------------------
// Root `--help` rendering (Go `printRootHelp`, main.go:1380-1427)
// ---------------------------------------------------------------------------

/// Program name printed in the `--help` banner and its footer.
///
/// This is `bvr`, the name the binary actually installs under (`install.sh`).
/// Go printed `bv` here, but `bv` is not a binary this project ships — a user
/// who ran the installer and then typed the `bv --robot-help` line this footer
/// ends with got `command not found`. The differential gate never diffed this
/// page (no golden captures the help output), so naming the real binary costs
/// no parity coverage.
pub const HELP_PROGRAM: &str = "bvr";

/// Program name printed by `--version`.
///
/// This one deliberately stays `bv`. `--version` reports the Go-compatibility
/// identity that the envelope and all 65 frozen goldens carry (`bv v0.25.0`),
/// so it is a parity surface rather than a usage hint, and
/// `cli_behavior::version_exits_zero` pins it. The help page names the binary
/// you can actually run; this names the thing it emulates.
pub const VERSION_PROGRAM: &str = "bv";

/// Column budget Go passes to `pflag`'s `FlagUsagesWrapped` (main.go:1425).
const HELP_COLS: usize = 100;

/// Section headers, in the order Go emits them. `rootHelpSections`
/// (main.go:59-206) declares the first six; pflag's leftover sweep produces
/// the seventh (main.go:1392).
pub const HELP_SECTIONS: &[&str] = &[
    "General Flags",
    "Search & Filters",
    "Robot & Planning Flags",
    "History & Drift",
    "Export & Reporting",
    "Agent File Management",
    "Other Flags",
];

/// One row of the two-column `--help` listing.
///
/// A side table rather than new `FlagDef` fields: the help order is Go's
/// *registration* order (main.go:1461-1663), which matches neither
/// `ROBOT_PRIMARIES` nor `MODIFIER_FLAGS`, and `FlagDef` is const-evaluable
/// and consumed as such. Look a registry entry up with [`FlagDef::help`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HelpFlag {
    /// Long name without leading dashes.
    pub name: &'static str,
    /// Single-letter alias, when Go registered one (`flag.StringP`).
    pub short: Option<char>,
    /// A `HELP_SECTIONS` title.
    pub section: &'static str,
    /// pflag's `Value.Type()` as displayed after the flag name; empty for
    /// bool flags, which pflag prints with no type word.
    pub type_word: &'static str,
    /// Go's usage string plus the `(default …)` suffix pflag appends for a
    /// non-zero default (flag.go:755-761). Precomputed here so the renderer
    /// only has to lay out text.
    pub help: &'static str,
}

/// Every flag Go advertises, in registration order — the order `--help`
/// prints them in, because `flag.CommandLine.SortFlags` is `false`
/// (main.go:1457) and the per-section FlagSets inherit that order.
pub const HELP_FLAGS: &[HelpFlag] = &[
    HelpFlag {
        name: "cpu-profile",
        short: None,
        section: "General Flags",
        type_word: "string",
        help: "Write CPU profile to file",
    },
    HelpFlag {
        name: "generate-docs",
        short: None,
        section: "Other Flags",
        type_word: "",
        help: "Generate documentation markdown and JSON artifacts",
    },
    HelpFlag {
        name: "db",
        short: None,
        section: "General Flags",
        type_word: "string",
        help: "Path to beads database file or .beads directory (overrides BEADS_DB and BEADS_DIR env vars)",
    },
    HelpFlag {
        name: "version",
        short: None,
        section: "General Flags",
        type_word: "",
        help: "Show version",
    },
    HelpFlag {
        name: "update",
        short: None,
        section: "General Flags",
        type_word: "",
        help: "Update bvr to the latest version",
    },
    HelpFlag {
        name: "check-update",
        short: None,
        section: "General Flags",
        type_word: "",
        help: "Check if a new version is available",
    },
    HelpFlag {
        name: "rollback",
        short: None,
        section: "General Flags",
        type_word: "",
        help: "Rollback to the previous version (from backup)",
    },
    HelpFlag {
        name: "update-dry-run",
        short: None,
        section: "General Flags",
        type_word: "",
        help: "Show what an update would do without installing (use via 'bvr upgrade --dry-run')",
    },
    HelpFlag {
        name: "yes",
        short: None,
        section: "General Flags",
        type_word: "",
        help: "Skip confirmation prompts (use with --update)",
    },
    HelpFlag {
        name: "export-md",
        short: None,
        section: "Export & Reporting",
        type_word: "string",
        help: "Export issues to a Markdown file (e.g., report.md)",
    },
    HelpFlag {
        name: "export",
        short: None,
        section: "Export & Reporting",
        type_word: "string",
        help: "Export a report using recipe defaults or explicit export options",
    },
    HelpFlag {
        name: "export-format",
        short: None,
        section: "Export & Reporting",
        type_word: "string",
        help: "Report format: markdown, json, csv or mermaid",
    },
    HelpFlag {
        name: "export-include-graph",
        short: None,
        section: "Export & Reporting",
        type_word: "",
        help: "Include dependency context in the report (explicit false overrides recipe) (default true)",
    },
    HelpFlag {
        name: "export-template",
        short: None,
        section: "Export & Reporting",
        type_word: "string",
        help: "Markdown template path; explicit empty disables a recipe template",
    },
    HelpFlag {
        name: "robot-help",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "",
        help: "Show AI agent help",
    },
    HelpFlag {
        name: "robot-capabilities",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "",
        help: "Output machine-readable command capabilities for AI agents",
    },
    HelpFlag {
        name: "robot-docs",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "string",
        help: "Machine-readable JSON docs for AI agents. Topics: guide, commands, examples, env, exit-codes, all",
    },
    HelpFlag {
        name: "format",
        short: Some('f'),
        section: "General Flags",
        type_word: "string",
        help: "Structured output format for --robot-* commands: json or toon (env: BV_OUTPUT_FORMAT, TOON_DEFAULT_FORMAT)",
    },
    HelpFlag {
        name: "stats",
        short: None,
        section: "General Flags",
        type_word: "",
        help: "Show JSON vs TOON token estimates on stderr (env: TOON_STATS=1)",
    },
    HelpFlag {
        name: "robot-insights",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "",
        help: "Output graph analysis and insights as JSON for AI agents",
    },
    HelpFlag {
        name: "robot-plan",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "",
        help: "Output dependency-respecting execution plan as JSON for AI agents",
    },
    HelpFlag {
        name: "robot-priority",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "",
        help: "Output priority recommendations as JSON for AI agents",
    },
    HelpFlag {
        name: "robot-triage",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "",
        help: "Output unified triage as JSON (the mega-command for AI agents)",
    },
    HelpFlag {
        name: "robot-triage-by-track",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "",
        help: "Group triage recommendations by execution track (bv-87)",
    },
    HelpFlag {
        name: "robot-triage-by-label",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "",
        help: "Group triage recommendations by label (bv-87)",
    },
    HelpFlag {
        name: "robot-next",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "",
        help: "Output only the top pick recommendation as JSON (minimal triage)",
    },
    HelpFlag {
        name: "brief",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "",
        help: "Compact --robot-triage output: only decision-relevant fields (id, title, status, assignee, blockers, unblocks) (#183)",
    },
    HelpFlag {
        name: "robot-not-ready-labels",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "string",
        help: "Comma-separated labels marking a bead not-ready: excluded from claimable --robot-next/--robot-triage top picks (env: BV_ROBOT_NOT_READY_LABELS; #173)",
    },
    HelpFlag {
        name: "robot-diff",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "",
        help: "Output diff as JSON (use with --diff-since)",
    },
    HelpFlag {
        name: "robot-recipes",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "",
        help: "Output available recipes as JSON for AI agents",
    },
    HelpFlag {
        name: "robot-label-health",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "",
        help: "Output label health metrics as JSON for AI agents",
    },
    HelpFlag {
        name: "robot-label-flow",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "",
        help: "Output cross-label dependency flow as JSON for AI agents",
    },
    HelpFlag {
        name: "robot-label-attention",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "",
        help: "Output attention-ranked labels as JSON for AI agents",
    },
    HelpFlag {
        name: "attention-limit",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "int",
        help: "Limit number of labels in --robot-label-attention output (default 5)",
    },
    HelpFlag {
        name: "robot-alerts",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "",
        help: "Output alerts (drift + proactive) as JSON for AI agents",
    },
    HelpFlag {
        name: "robot-metrics",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "",
        help: "Output performance metrics (timing, cache, memory) as JSON",
    },
    HelpFlag {
        name: "robot-schema",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "",
        help: "Output JSON Schema definitions for all robot commands",
    },
    HelpFlag {
        name: "schema-command",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "string",
        help: "Output schema for specific command only (e.g., robot-triage)",
    },
    HelpFlag {
        name: "robot-suggest",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "",
        help: "Output smart suggestions (duplicates, dependencies, labels, cycles) as JSON",
    },
    HelpFlag {
        name: "suggest-type",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "string",
        help: "Filter suggestions by type: duplicate, dependency, label, cycle",
    },
    HelpFlag {
        name: "suggest-confidence",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "float",
        help: "Minimum confidence for suggestions (0.0-1.0)",
    },
    HelpFlag {
        name: "suggest-bead",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "string",
        help: "Filter suggestions for specific bead ID",
    },
    HelpFlag {
        name: "robot-graph",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "",
        help: "Output dependency graph as JSON/DOT/Mermaid for AI agents",
    },
    HelpFlag {
        name: "graph-format",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "string",
        help: "Graph output format: json, dot, mermaid (default \"json\")",
    },
    HelpFlag {
        name: "graph-root",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "string",
        help: "Subgraph from specific root issue ID",
    },
    HelpFlag {
        name: "graph-depth",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "int",
        help: "Max depth for subgraph (0 = unlimited)",
    },
    HelpFlag {
        name: "export-graph",
        short: None,
        section: "Export & Reporting",
        type_word: "string",
        help: "Export graph: .html for interactive, .png/.svg for static (auto-names if empty)",
    },
    HelpFlag {
        name: "graph-preset",
        short: None,
        section: "Export & Reporting",
        type_word: "string",
        help: "Graph layout preset: compact (default) or roomy (default \"compact\")",
    },
    HelpFlag {
        name: "graph-title",
        short: None,
        section: "Export & Reporting",
        type_word: "string",
        help: "Title for graph export (default: project name)",
    },
    HelpFlag {
        name: "robot-min-confidence",
        short: None,
        section: "Search & Filters",
        type_word: "float",
        help: "Filter robot outputs by minimum confidence (0.0-1.0)",
    },
    HelpFlag {
        name: "robot-max-results",
        short: None,
        section: "Search & Filters",
        type_word: "int",
        help: "Limit robot output count (0 = use defaults)",
    },
    HelpFlag {
        name: "robot-by-label",
        short: None,
        section: "Search & Filters",
        type_word: "string",
        help: "Filter robot outputs by label (exact match)",
    },
    HelpFlag {
        name: "robot-by-assignee",
        short: None,
        section: "Search & Filters",
        type_word: "string",
        help: "Filter robot outputs by assignee (exact match)",
    },
    HelpFlag {
        name: "label",
        short: Some('l'),
        section: "Search & Filters",
        type_word: "string",
        help: "Scope analysis to label's subgraph (applies to every --robot-* command that loads issues, e.g. --robot-insights, --robot-plan, --robot-priority, --robot-orphans)",
    },
    HelpFlag {
        name: "severity",
        short: None,
        section: "Search & Filters",
        type_word: "string",
        help: "Filter robot alerts by severity (info|warning|critical)",
    },
    HelpFlag {
        name: "alert-type",
        short: None,
        section: "Search & Filters",
        type_word: "string",
        help: "Filter robot alerts by alert type (e.g., stale_issue)",
    },
    HelpFlag {
        name: "alert-label",
        short: None,
        section: "Search & Filters",
        type_word: "string",
        help: "Filter robot alerts by label match",
    },
    HelpFlag {
        name: "recipe",
        short: Some('r'),
        section: "Search & Filters",
        type_word: "string",
        help: "Apply a recipe by name (e.g., triage, actionable, high-impact) or by .yaml/.yml file path (e.g., .beads/recipes/sprint.yaml)",
    },
    HelpFlag {
        name: "search",
        short: None,
        section: "Search & Filters",
        type_word: "string",
        help: "Hashed keyword search query (builds/updates index on first run)",
    },
    HelpFlag {
        name: "robot-search",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "",
        help: "Output keyword or hybrid search results as JSON for AI agents (use with --search)",
    },
    HelpFlag {
        name: "search-limit",
        short: None,
        section: "Search & Filters",
        type_word: "int",
        help: "Max results for --search/--robot-search (default 10)",
    },
    HelpFlag {
        name: "search-min-score",
        short: None,
        section: "Search & Filters",
        type_word: "string",
        help: "Minimum text similarity before hybrid ranking (-1..1); exact IDs also obey this threshold",
    },
    HelpFlag {
        name: "search-mode",
        short: None,
        section: "Search & Filters",
        type_word: "string",
        help: "Search ranking mode: text or hybrid (default: BV_SEARCH_MODE or text)",
    },
    HelpFlag {
        name: "search-preset",
        short: None,
        section: "Search & Filters",
        type_word: "string",
        help: "Hybrid preset name (default: BV_SEARCH_PRESET or default)",
    },
    HelpFlag {
        name: "search-weights",
        short: None,
        section: "Search & Filters",
        type_word: "string",
        help: "Hybrid weights JSON (overrides preset; keys: text,pagerank,status,impact,priority,recency)",
    },
    HelpFlag {
        name: "diff-since",
        short: None,
        section: "History & Drift",
        type_word: "string",
        help: "Show changes since historical point (commit SHA, branch, tag, or date)",
    },
    HelpFlag {
        name: "as-of",
        short: None,
        section: "History & Drift",
        type_word: "string",
        help: "View state at point in time (commit SHA, branch, tag, or date)",
    },
    HelpFlag {
        name: "no-cache",
        short: None,
        section: "General Flags",
        type_word: "",
        help: "Bypass disk cache for robot triage (also: BV_NO_CACHE=1)",
    },
    HelpFlag {
        name: "force-full-analysis",
        short: None,
        section: "General Flags",
        type_word: "",
        help: "Compute all metrics regardless of graph size (may be slow for large graphs)",
    },
    HelpFlag {
        name: "profile-startup",
        short: None,
        section: "General Flags",
        type_word: "",
        help: "Output detailed startup timing profile for diagnostics",
    },
    HelpFlag {
        name: "profile-json",
        short: None,
        section: "General Flags",
        type_word: "",
        help: "Output profile in JSON format (use with --profile-startup)",
    },
    HelpFlag {
        name: "no-hooks",
        short: None,
        section: "Export & Reporting",
        type_word: "",
        help: "Skip running hooks during export",
    },
    HelpFlag {
        name: "workspace",
        short: None,
        section: "Search & Filters",
        type_word: "string",
        help: "Load issues from workspace config file (.bv/workspace.yaml)",
    },
    HelpFlag {
        name: "repo",
        short: None,
        section: "Search & Filters",
        type_word: "string",
        help: "Filter issues by repository prefix (e.g., 'api-' or 'api')",
    },
    HelpFlag {
        name: "save-baseline",
        short: None,
        section: "History & Drift",
        type_word: "string",
        help: "Save current metrics as baseline with optional description",
    },
    HelpFlag {
        name: "baseline-info",
        short: None,
        section: "History & Drift",
        type_word: "",
        help: "Show information about the current baseline",
    },
    HelpFlag {
        name: "check-drift",
        short: None,
        section: "History & Drift",
        type_word: "",
        help: "Check for drift from baseline (exit codes: 0=OK, 1=critical, 2=warning)",
    },
    HelpFlag {
        name: "robot-drift",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "",
        help: "Output drift check as JSON (use with --check-drift)",
    },
    HelpFlag {
        name: "robot-history",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "",
        help: "Output bead-to-commit correlations as JSON",
    },
    HelpFlag {
        name: "bead-history",
        short: None,
        section: "History & Drift",
        type_word: "string",
        help: "Show history for specific bead ID",
    },
    HelpFlag {
        name: "history-since",
        short: None,
        section: "History & Drift",
        type_word: "string",
        help: "Limit history to commits after this date/ref (e.g., '30 days ago', '2024-01-01')",
    },
    HelpFlag {
        name: "history-limit",
        short: None,
        section: "History & Drift",
        type_word: "int",
        help: "Max commits to analyze (0 = unlimited) (default 500)",
    },
    HelpFlag {
        name: "robot-history-timeout-ms",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "int",
        help: "Budget in ms for the git-history prologue of robot triage (0 = unbounded; default 10000, env BV_ROBOT_HISTORY_TIMEOUT_MS) (default -1)",
    },
    HelpFlag {
        name: "min-confidence",
        short: None,
        section: "History & Drift",
        type_word: "float",
        help: "Filter correlations by minimum confidence (0.0-1.0)",
    },
    HelpFlag {
        name: "id-pattern",
        short: None,
        section: "Other Flags",
        type_word: "stringArray",
        help: "Custom bead ID regex for commit-message matching, e.g. 'bh-[a-z0-9]{5}' (repeatable; capture group 1 is the ID, else the whole match) (#188)",
    },
    HelpFlag {
        name: "robot-explain-correlation",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "string",
        help: "Explain why a commit is linked to a bead (format: SHA:beadID)",
    },
    HelpFlag {
        name: "robot-confirm-correlation",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "string",
        help: "Confirm a correlation is correct (format: SHA:beadID)",
    },
    HelpFlag {
        name: "robot-reject-correlation",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "string",
        help: "Reject an incorrect correlation (format: SHA:beadID)",
    },
    HelpFlag {
        name: "correlation-by",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "string",
        help: "Agent/user identifier for correlation feedback",
    },
    HelpFlag {
        name: "correlation-reason",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "string",
        help: "Reason for correlation feedback",
    },
    HelpFlag {
        name: "robot-correlation-stats",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "",
        help: "Output correlation feedback statistics as JSON",
    },
    HelpFlag {
        name: "robot-orphans",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "",
        help: "Output orphan commit candidates (commits that should be linked but aren't) as JSON",
    },
    HelpFlag {
        name: "orphans-min-score",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "int",
        help: "Minimum suspicion score for orphan candidates (0-100) (default 30)",
    },
    HelpFlag {
        name: "robot-file-beads",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "string",
        help: "Output beads that touched a file path as JSON",
    },
    HelpFlag {
        name: "file-beads-limit",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "int",
        help: "Max closed beads to show (use with --robot-file-beads) (default 20)",
    },
    HelpFlag {
        name: "robot-file-hotspots",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "",
        help: "Output files touched by most beads as JSON",
    },
    HelpFlag {
        name: "hotspots-limit",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "int",
        help: "Max hotspots to show (use with --robot-file-hotspots) (default 10)",
    },
    HelpFlag {
        name: "robot-impact",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "string",
        help: "Analyze impact of modifying files (comma-separated paths)",
    },
    HelpFlag {
        name: "robot-file-relations",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "string",
        help: "Output files that frequently co-change with the given file path",
    },
    HelpFlag {
        name: "relations-threshold",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "float",
        help: "Minimum correlation threshold (0.0-1.0) for related files (default 0.5)",
    },
    HelpFlag {
        name: "relations-limit",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "int",
        help: "Max related files to show (default 10)",
    },
    HelpFlag {
        name: "robot-related",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "string",
        help: "Output beads related to a specific bead ID as JSON",
    },
    HelpFlag {
        name: "related-min-relevance",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "percent_or_fraction",
        help: "Minimum relevance score for related work (int 0-100 percent OR float 0.0-1.0 fraction) (default 20)",
    },
    HelpFlag {
        name: "related-max-results",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "int",
        help: "Max results per category for related work (default 10)",
    },
    HelpFlag {
        name: "related-include-closed",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "",
        help: "Include closed beads in related work results",
    },
    HelpFlag {
        name: "robot-blocker-chain",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "string",
        help: "Output full blocker chain analysis for issue ID as JSON",
    },
    HelpFlag {
        name: "robot-impact-network",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "string",
        help: "Output bead impact network as JSON (empty for full, or bead ID for subnetwork)",
    },
    HelpFlag {
        name: "network-depth",
        short: None,
        section: "Other Flags",
        type_word: "int",
        help: "Depth of subnetwork when querying specific bead (1-3) (default 2)",
    },
    HelpFlag {
        name: "robot-causality",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "string",
        help: "Output causal chain analysis for bead ID as JSON",
    },
    HelpFlag {
        name: "robot-sprint-list",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "",
        help: "Output sprints as JSON",
    },
    HelpFlag {
        name: "robot-sprint-show",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "string",
        help: "Output specific sprint details as JSON",
    },
    HelpFlag {
        name: "robot-forecast",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "string",
        help: "Output ETA forecast for bead ID, or 'all' for all open issues",
    },
    HelpFlag {
        name: "forecast-label",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "string",
        help: "Filter forecast by label",
    },
    HelpFlag {
        name: "forecast-sprint",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "string",
        help: "Filter forecast by sprint ID",
    },
    HelpFlag {
        name: "forecast-agents",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "int",
        help: "Number of parallel agents for capacity calculation (default 1)",
    },
    HelpFlag {
        name: "robot-capacity",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "",
        help: "Output capacity simulation and completion projection as JSON",
    },
    HelpFlag {
        name: "agents",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "int",
        help: "Number of parallel agents for capacity simulation (default 1)",
    },
    HelpFlag {
        name: "capacity-label",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "string",
        help: "Filter capacity simulation by label",
    },
    HelpFlag {
        name: "robot-burndown",
        short: None,
        section: "Robot & Planning Flags",
        type_word: "string",
        help: "Output burndown data for sprint ID, or 'current' for active sprint",
    },
    HelpFlag {
        name: "emit-script",
        short: None,
        section: "Export & Reporting",
        type_word: "",
        help: "Emit shell script for top-N recommendations (agent workflows)",
    },
    HelpFlag {
        name: "script-limit",
        short: None,
        section: "Export & Reporting",
        type_word: "int",
        help: "Limit number of items in emitted script (use with --emit-script) (default 5)",
    },
    HelpFlag {
        name: "script-format",
        short: None,
        section: "Export & Reporting",
        type_word: "string",
        help: "Script format: bash, fish, or zsh (use with --emit-script) (default \"bash\")",
    },
    HelpFlag {
        name: "feedback-accept",
        short: None,
        section: "Other Flags",
        type_word: "string",
        help: "Record accept feedback for issue ID (tunes recommendation weights)",
    },
    HelpFlag {
        name: "feedback-ignore",
        short: None,
        section: "Other Flags",
        type_word: "string",
        help: "Record ignore feedback for issue ID (tunes recommendation weights)",
    },
    HelpFlag {
        name: "feedback-reset",
        short: None,
        section: "Other Flags",
        type_word: "",
        help: "Reset all feedback data to defaults",
    },
    HelpFlag {
        name: "feedback-show",
        short: None,
        section: "Other Flags",
        type_word: "",
        help: "Show current feedback status and weight adjustments",
    },
    HelpFlag {
        name: "priority-brief",
        short: None,
        section: "Export & Reporting",
        type_word: "string",
        help: "Export priority brief to Markdown file (e.g., brief.md)",
    },
    HelpFlag {
        name: "agent-brief",
        short: None,
        section: "Export & Reporting",
        type_word: "string",
        help: "Export agent brief bundle to directory (includes triage.json, insights.json, brief.md, helpers.md)",
    },
    HelpFlag {
        name: "export-pages",
        short: None,
        section: "Export & Reporting",
        type_word: "string",
        help: "Export static site to directory (e.g., ./bv-pages)",
    },
    HelpFlag {
        name: "pages-title",
        short: None,
        section: "Export & Reporting",
        type_word: "string",
        help: "Custom title for static site",
    },
    HelpFlag {
        name: "pages-include-closed",
        short: None,
        section: "Export & Reporting",
        type_word: "",
        help: "Include closed issues in export (default: true) (default true)",
    },
    HelpFlag {
        name: "pages-include-history",
        short: None,
        section: "Export & Reporting",
        type_word: "",
        help: "Include git history for time-travel (default: true) (default true)",
    },
    HelpFlag {
        name: "preview-pages",
        short: None,
        section: "Export & Reporting",
        type_word: "string",
        help: "Preview existing static site bundle",
    },
    HelpFlag {
        name: "no-live-reload",
        short: None,
        section: "Export & Reporting",
        type_word: "",
        help: "Disable live-reload in preview mode",
    },
    HelpFlag {
        name: "watch-export",
        short: None,
        section: "Export & Reporting",
        type_word: "",
        help: "Watch for beads changes and auto-regenerate export (use with --export-pages)",
    },
    HelpFlag {
        name: "pages",
        short: None,
        section: "Export & Reporting",
        type_word: "",
        help: "Launch interactive Pages deployment wizard",
    },
    HelpFlag {
        name: "debug-render",
        short: None,
        section: "Export & Reporting",
        type_word: "string",
        help: "Render a view and output to file (views: insights, board)",
    },
    HelpFlag {
        name: "debug-width",
        short: None,
        section: "Export & Reporting",
        type_word: "int",
        help: "Width for debug render (default 180)",
    },
    HelpFlag {
        name: "debug-height",
        short: None,
        section: "Export & Reporting",
        type_word: "int",
        help: "Height for debug render (default 50)",
    },
    HelpFlag {
        name: "theme",
        short: None,
        section: "General Flags",
        type_word: "string",
        help: "Color theme: light, dark, or auto (default: detect terminal background)",
    },
    HelpFlag {
        name: "background-mode",
        short: None,
        section: "General Flags",
        type_word: "",
        help: "Enable experimental background snapshot loading (TUI only)",
    },
    HelpFlag {
        name: "no-background-mode",
        short: None,
        section: "General Flags",
        type_word: "",
        help: "Disable experimental background snapshot loading (TUI only)",
    },
    HelpFlag {
        name: "agents-add",
        short: None,
        section: "Agent File Management",
        type_word: "",
        help: "Add beads workflow instructions to AGENTS.md (creates file if needed)",
    },
    HelpFlag {
        name: "agents-remove",
        short: None,
        section: "Agent File Management",
        type_word: "",
        help: "Remove beads workflow instructions from AGENTS.md",
    },
    HelpFlag {
        name: "agents-update",
        short: None,
        section: "Agent File Management",
        type_word: "",
        help: "Update beads workflow instructions to latest version",
    },
    HelpFlag {
        name: "agents-check",
        short: None,
        section: "Agent File Management",
        type_word: "",
        help: "Check AGENTS.md blurb status (default if no --agents-* action)",
    },
    HelpFlag {
        name: "agents-dry-run",
        short: None,
        section: "Agent File Management",
        type_word: "",
        help: "Show what would happen without executing (use with --agents-*)",
    },
    HelpFlag {
        name: "agents-force",
        short: None,
        section: "Agent File Management",
        type_word: "",
        help: "Skip confirmation prompts (use with --agents-*)",
    },
    HelpFlag {
        name: "help",
        short: Some('h'),
        section: "General Flags",
        type_word: "",
        help: "help for bvr",
    },
];

/// Byte-for-byte port of pflag `wrapN` (vendor/github.com/spf13/pflag/flag.go:639).
/// Byte-indexed like the original, which is why [`HELP_FLAGS`] is ASCII-only
/// (enforced by `help_table_is_ascii`).
fn wrap_n(i: usize, slop: usize, s: &str) -> (&str, &str) {
    if i + slop > s.len() {
        return (s, "");
    }
    let head = &s.as_bytes()[..i];
    let last_of = |needle: u8| {
        head.iter()
            .rposition(|&c| c == needle)
            .map_or(-1i64, |p| p as i64)
    };
    let w = [last_of(b' '), last_of(b'\t'), last_of(b'\n')]
        .into_iter()
        .max()
        .unwrap_or(-1);
    if w <= 0 {
        return (s, "");
    }
    let w = w as usize;
    let nl = last_of(b'\n');
    if nl > 0 && (nl as usize) < w {
        return (&s[..nl as usize], &s[nl as usize + 1..]);
    }
    (&s[..w], &s[w + 1..])
}

/// Byte-for-byte port of pflag `wrap` (flag.go:658). The first line is
/// assumed to be indented by the caller; continuations get `indent` spaces.
fn wrap(indent: usize, cols: usize, s: &str) -> String {
    let pad = " ".repeat(indent);
    if cols == 0 {
        return s.replace('\n', &format!("\n{pad}"));
    }
    let mut indent = indent as i64;
    let mut width = cols as i64 - indent;
    let mut r = String::new();
    if width < 24 {
        indent = 16;
        width = cols as i64 - indent;
        r.push('\n');
        r.push_str(&" ".repeat(indent as usize));
    }
    if width < 24 {
        return s.replace('\n', &r);
    }
    let slop = 5i64;
    width -= slop;
    let (first, mut rest) = wrap_n(width as usize, slop as usize, s);
    r.push_str(&first.replace('\n', &format!("\n{pad}")));
    while !rest.is_empty() {
        let (t, next) = wrap_n(width as usize, slop as usize, rest);
        r.push('\n');
        r.push_str(&pad);
        r.push_str(&t.replace('\n', &format!("\n{pad}")));
        rest = next;
    }
    r
}

/// Every long flag name the registry knows about, robot primaries first.
/// Used by `--generate-docs` to record the accepted surface as an artifact.
/// Whether `name` is registered as a value-taking string flag.
///
/// Go's `isFlagActive` only applies the TrimSpace test to `stringValue` flags
/// (main.go:199-207); every other kind is "active" on presence. The
/// modifier-requires rules need this to tell `--diff-since ""` (present, but
/// not active) from `--diff-since HEAD~5`.
pub fn flag_is_string(name: &str) -> bool {
    let target = name.trim_start_matches('-');
    let is_str =
        |f: &&FlagDef| f.name == target && matches!(f.kind, FlagKind::Str | FlagKind::StringArray);
    ROBOT_PRIMARIES.iter().find(is_str).is_some() || MODIFIER_FLAGS.iter().find(is_str).is_some()
}

/// The declared kind of a registered flag, or `None` when the name is not in
/// the registry. Go's `flag` package decides whether a flag consumes the next
/// argv element from its own registration (`flag.BoolFlag` set or not), so the
/// missing-argument check has to read the same table rather than guess from the
/// spelling.
pub fn flag_kind(name: &str) -> Option<FlagKind> {
    let target = name.trim_start_matches('-');
    ROBOT_PRIMARIES
        .iter()
        .chain(MODIFIER_FLAGS.iter())
        .find(|f| f.name == target)
        .map(|f| f.kind)
}

pub fn flag_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = ROBOT_PRIMARIES.iter().map(|f| f.name).collect();
    names.extend(MODIFIER_FLAGS.iter().map(|f| f.name));
    names
}

/// Whether `name` (a bare long name, no leading dashes) is a registered flag —
/// the test behind Go's `flags.Lookup(name) == nil` in
/// `rewriteSingleDashLongFlags` (main.go:548).
///
/// The registry is exactly `flag.CommandLine` as `main` builds it before
/// `newRootCommand` merges it into `cmd.Flags()` (main.go:518). It does *not*
/// contain cobra's auto `--help` flag: that is added by `InitDefaultHelpFlag`
/// during `Execute`, after `rewriteSingleDashLongFlags` has already run
/// (main.go:4546), which is why Go answers `bv -help` with pflag's
/// `unknown shorthand flag: 'e' in -elp` rather than printing help.
pub fn flag_lookup(name: &str) -> bool {
    let known = |f: &FlagDef| f.name == name;
    ROBOT_PRIMARIES.iter().any(known) || MODIFIER_FLAGS.iter().any(known)
}

/// Port of pflag `FlagUsagesWrapped` (flag.go:707-778) for one section: the
/// widest left column sets the description column for every row in it.
fn flag_usages(rows: &[&HelpFlag]) -> String {
    let mut lines: Vec<String> = Vec::with_capacity(rows.len());
    let mut maxlen = 0usize;
    for f in rows {
        let mut line = match f.short {
            Some(c) => format!("  -{c}, --{}", f.name),
            None => format!("      --{}", f.name),
        };
        if !f.type_word.is_empty() {
            line.push(' ');
            line.push_str(f.type_word);
        }
        // Sentinel pflag swaps for padding once the column width is known.
        line.push('\u{0}');
        maxlen = maxlen.max(line.len());
        line.push_str(f.help);
        lines.push(line);
    }

    let mut buf = String::new();
    for line in &lines {
        let sidx = line.find('\u{0}').expect("sentinel present");
        // `Fprintln(prefix, spacing, wrapped)` inserts a space between each
        // argument, landing the description at maxlen + 2.
        buf.push_str(&line[..sidx]);
        buf.push(' ');
        buf.push_str(&" ".repeat(maxlen - sidx));
        buf.push(' ');
        buf.push_str(&wrap(maxlen + 2, HELP_COLS, &line[sidx + 1..]));
        buf.push('\n');
    }
    buf
}

/// Render the full root `--help` page, byte-identical to Go bv v0.25.0.
pub fn render_help(prog: &str) -> String {
    let mut out = format!("Usage: {prog} [flags]\n\nA TUI viewer for beads issue tracker.\n\n");
    for title in HELP_SECTIONS {
        let rows: Vec<&HelpFlag> = HELP_FLAGS.iter().filter(|f| f.section == *title).collect();
        if rows.is_empty() {
            continue;
        }
        out.push_str(title);
        out.push_str(":\n");
        out.push_str(&flag_usages(&rows));
        out.push('\n');
    }
    out.push_str(&format!(
        "Run `{prog} --robot-help` for detailed AI/robot command documentation.\n"
    ));
    out
}

/// Robot primary commands (~41). Group = exclusive-command family.
pub const ROBOT_PRIMARIES: &[FlagDef] = &[
    FlagDef {
        name: "robot-help",
        kind: FlagKind::Bool,
        primary: true,
        group: Some("meta"),
    },
    FlagDef {
        name: "robot-capabilities",
        kind: FlagKind::Bool,
        primary: true,
        group: Some("meta"),
    },
    FlagDef {
        name: "robot-docs",
        kind: FlagKind::Str,
        primary: true,
        group: Some("meta"),
    },
    FlagDef {
        name: "robot-schema",
        kind: FlagKind::Bool,
        primary: true,
        group: Some("meta"),
    },
    FlagDef {
        name: "robot-recipes",
        kind: FlagKind::Bool,
        primary: true,
        group: Some("meta"),
    },
    FlagDef {
        name: "robot-metrics",
        kind: FlagKind::Bool,
        primary: true,
        group: Some("meta"),
    },
    // triage family shares one group
    FlagDef {
        name: "robot-triage",
        kind: FlagKind::Bool,
        primary: true,
        group: Some("triage"),
    },
    FlagDef {
        name: "robot-next",
        kind: FlagKind::Bool,
        primary: true,
        group: Some("triage"),
    },
    FlagDef {
        name: "robot-triage-by-track",
        kind: FlagKind::Bool,
        primary: true,
        group: Some("triage"),
    },
    FlagDef {
        name: "robot-triage-by-label",
        kind: FlagKind::Bool,
        primary: true,
        group: Some("triage"),
    },
    FlagDef {
        name: "robot-insights",
        kind: FlagKind::Bool,
        primary: true,
        group: Some("insights"),
    },
    FlagDef {
        name: "robot-plan",
        kind: FlagKind::Bool,
        primary: true,
        group: Some("plan"),
    },
    FlagDef {
        name: "robot-priority",
        kind: FlagKind::Bool,
        primary: true,
        group: Some("priority"),
    },
    FlagDef {
        name: "robot-alerts",
        kind: FlagKind::Bool,
        primary: true,
        group: Some("alerts"),
    },
    FlagDef {
        name: "robot-suggest",
        kind: FlagKind::Bool,
        primary: true,
        group: Some("suggest"),
    },
    FlagDef {
        name: "robot-graph",
        kind: FlagKind::Bool,
        primary: true,
        group: Some("graph"),
    },
    FlagDef {
        name: "robot-search",
        kind: FlagKind::Bool,
        primary: true,
        group: Some("search"),
    },
    FlagDef {
        name: "robot-diff",
        kind: FlagKind::Bool,
        primary: true,
        group: Some("diff"),
    },
    FlagDef {
        name: "robot-drift",
        kind: FlagKind::Bool,
        primary: true,
        group: Some("drift"),
    },
    FlagDef {
        name: "robot-history",
        kind: FlagKind::Bool,
        primary: true,
        group: Some("history"),
    },
    FlagDef {
        name: "robot-explain-correlation",
        kind: FlagKind::Str,
        primary: true,
        group: Some("corr-feedback"),
    },
    FlagDef {
        name: "robot-confirm-correlation",
        kind: FlagKind::Str,
        primary: true,
        group: Some("corr-feedback"),
    },
    FlagDef {
        name: "robot-reject-correlation",
        kind: FlagKind::Str,
        primary: true,
        group: Some("corr-feedback"),
    },
    FlagDef {
        name: "robot-correlation-stats",
        kind: FlagKind::Bool,
        primary: true,
        group: Some("corr-stats"),
    },
    FlagDef {
        name: "robot-orphans",
        kind: FlagKind::Bool,
        primary: true,
        group: Some("orphans"),
    },
    FlagDef {
        name: "robot-file-beads",
        kind: FlagKind::Str,
        primary: true,
        group: Some("file-beads"),
    },
    FlagDef {
        name: "robot-file-hotspots",
        kind: FlagKind::Bool,
        primary: true,
        group: Some("hotspots"),
    },
    FlagDef {
        name: "robot-impact",
        kind: FlagKind::Str,
        primary: true,
        group: Some("impact"),
    },
    FlagDef {
        name: "robot-file-relations",
        kind: FlagKind::Str,
        primary: true,
        group: Some("file-relations"),
    },
    FlagDef {
        name: "robot-related",
        kind: FlagKind::Str,
        primary: true,
        group: Some("related"),
    },
    FlagDef {
        name: "robot-blocker-chain",
        kind: FlagKind::Str,
        primary: true,
        group: Some("blocker-chain"),
    },
    FlagDef {
        name: "robot-impact-network",
        kind: FlagKind::Str,
        primary: true,
        group: Some("impact-network"),
    },
    FlagDef {
        name: "robot-causality",
        kind: FlagKind::Str,
        primary: true,
        group: Some("causality"),
    },
    FlagDef {
        name: "robot-sprint-list",
        kind: FlagKind::Bool,
        primary: true,
        group: Some("sprints"),
    },
    FlagDef {
        name: "robot-sprint-show",
        kind: FlagKind::Str,
        primary: true,
        group: Some("sprints"),
    },
    FlagDef {
        name: "robot-forecast",
        kind: FlagKind::Str,
        primary: true,
        group: Some("forecast"),
    },
    FlagDef {
        name: "robot-capacity",
        kind: FlagKind::Bool,
        primary: true,
        group: Some("capacity"),
    },
    FlagDef {
        name: "robot-burndown",
        kind: FlagKind::Str,
        primary: true,
        group: Some("burndown"),
    },
    FlagDef {
        name: "robot-label-health",
        kind: FlagKind::Bool,
        primary: true,
        group: Some("label-health"),
    },
    FlagDef {
        name: "robot-label-flow",
        kind: FlagKind::Bool,
        primary: true,
        group: Some("label-flow"),
    },
    FlagDef {
        name: "robot-label-attention",
        kind: FlagKind::Bool,
        primary: true,
        group: Some("label-attention"),
    },
];

/// Non-primary flags (general/scoping/modifiers). Complete Go inventory.
pub const MODIFIER_FLAGS: &[FlagDef] = &[
    // General
    b("version"),
    s("db"),
    b("update"),
    b("check-update"),
    b("rollback"),
    b("update-dry-run"),
    b("yes"),
    s("format"),
    b("stats"),
    b("profile-startup"),
    b("profile-json"),
    b("no-cache"),
    b("force-full-analysis"),
    s("theme"),
    b("background-mode"),
    b("no-background-mode"),
    s("cpu-profile"),
    // Doc generation (Go main.go:1461) — emits markdown + JSON artifacts.
    b("generate-docs"),
    // Triage modifiers
    b("brief"),
    i("attention-limit"),
    s("robot-not-ready-labels"),
    i("robot-history-timeout-ms"),
    // Graph export
    s("graph-format"),
    s("graph-root"),
    i("graph-depth"),
    s("graph-preset"),
    s("graph-title"),
    s("export-graph"),
    // Report export (Go main.go:1471-1474) — recipe defaults or explicit options.
    s("export"),
    s("export-format"),
    b("export-include-graph"),
    s("export-template"),
    // Alerts/suggest filters
    s("severity"),
    s("alert-type"),
    s("alert-label"),
    s("suggest-type"),
    s("suggest-confidence"),
    s("suggest-bead"),
    // Scoping
    s("label"),
    s("recipe"),
    s("workspace"),
    s("repo"),
    // History/correlation
    s("as-of"),
    s("diff-since"),
    s("save-baseline"),
    b("baseline-info"),
    b("check-drift"),
    s("bead-history"),
    s("history-since"),
    i("history-limit"),
    s("min-confidence"),
    s("id-pattern"), // repeatable
    s("correlation-by"),
    s("correlation-reason"),
    i("orphans-min-score"),
    i("file-beads-limit"),
    i("hotspots-limit"),
    s("relations-threshold"),
    i("relations-limit"),
    s("related-min-relevance"),
    i("related-max-results"),
    b("related-include-closed"),
    i("network-depth"),
    // Search
    s("search"),
    i("search-limit"),
    s("search-min-score"),
    s("search-mode"),
    s("search-preset"),
    s("search-weights"),
    // Forecast/capacity/burndown
    s("forecast-label"),
    s("forecast-sprint"),
    i("forecast-agents"),
    i("agents"),
    s("capacity-label"),
    // Priority filters
    s("robot-by-label"),
    s("robot-by-assignee"),
    f2("robot-min-confidence"),
    i("robot-max-results"),
    // Script emission / feedback
    b("emit-script"),
    i("script-limit"),
    s("script-format"),
    s("feedback-accept"),
    s("feedback-ignore"),
    b("feedback-reset"),
    b("feedback-show"),
    // Exports
    s("export-md"),
    b("no-hooks"),
    s("priority-brief"),
    s("agent-brief"),
    s("export-pages"),
    s("pages-title"),
    b("pages-include-closed"),
    b("pages-include-history"),
    s("preview-pages"),
    b("no-live-reload"),
    b("watch-export"),
    b("pages"),
    // Schema
    s("schema-command"),
    // Debug/render
    s("debug-render"),
    i("debug-width"),
    i("debug-height"),
    // Agents-file mgmt
    b("agents-add"),
    b("agents-remove"),
    b("agents-update"),
    b("agents-check"),
    b("agents-dry-run"),
    b("agents-force"),
];

const fn f2(name: &'static str) -> FlagDef {
    FlagDef {
        name,
        kind: FlagKind::Float,
        primary: false,
        group: None,
    }
}

/// Modifier-requires table — Go `modifierRules` (main.go:1786-1843), in Go's
/// order. All 58 rows.
///
/// The ORDER IS SEMANTICS, not presentation. Go's `validateModifierFlags`
/// walks this slice and returns the first rule whose modifier was supplied
/// without a satisfying co-flag, so `bv --graph-preset roomy --severity info`
/// reports `--graph-preset requires --export-graph` (row 1804), not
/// `--severity requires --robot-alerts` (row 1806). Consumers must therefore
/// iterate this slice front-to-back and take the first hit; a sorted or
/// re-derived order makes a two-problem command report the wrong error.
pub const MODIFIER_REQUIRES: &[(&str, &[&str])] = &[
    // main.go:1786-1788 — report-export options ride with a report command.
    ("export-format", &["export", "export-md"]),
    ("export-include-graph", &["export", "export-md"]),
    ("export-template", &["export", "export-md"]),
    // main.go:1789-1795
    ("robot-diff", &["diff-since"]),
    ("robot-search", &["search"]),
    ("search-limit", &["search"]),
    ("search-min-score", &["search"]),
    ("search-mode", &["search"]),
    ("search-preset", &["search"]),
    ("search-weights", &["search"]),
    // main.go:1796-1800
    ("attention-limit", &["robot-label-attention"]),
    ("schema-command", &["robot-schema"]),
    ("suggest-type", &["robot-suggest"]),
    ("suggest-confidence", &["robot-suggest"]),
    ("suggest-bead", &["robot-suggest"]),
    // main.go:1801-1805 — graph output vs. graph *export* are different commands.
    ("graph-format", &["robot-graph"]),
    (
        "graph-root",
        &[
            "robot-graph",
            "robot-triage",
            "robot-triage-by-track",
            "robot-triage-by-label",
            "robot-next",
        ],
    ),
    ("graph-depth", &["robot-graph"]),
    ("graph-preset", &["export-graph"]),
    ("graph-title", &["export-graph"]),
    // main.go:1806-1810
    ("severity", &["robot-alerts"]),
    ("alert-type", &["robot-alerts"]),
    ("alert-label", &["robot-alerts"]),
    ("profile-json", &["profile-startup"]),
    ("robot-drift", &["check-drift"]),
    // main.go:1811-1816
    (
        "history-since",
        &["robot-history", "bead-history", "robot-causality"],
    ),
    (
        "history-limit",
        &["robot-history", "bead-history", "robot-causality"],
    ),
    (
        "brief",
        &[
            "robot-triage",
            "robot-triage-by-track",
            "robot-triage-by-label",
        ],
    ),
    (
        "robot-history-timeout-ms",
        &[
            "robot-triage",
            "robot-triage-by-track",
            "robot-triage-by-label",
            "robot-next",
        ],
    ),
    (
        "robot-not-ready-labels",
        &[
            "robot-triage",
            "robot-triage-by-track",
            "robot-triage-by-label",
            "robot-next",
        ],
    ),
    ("min-confidence", &["robot-history", "bead-history"]),
    // main.go:1817-1826
    (
        "correlation-by",
        &["robot-confirm-correlation", "robot-reject-correlation"],
    ),
    (
        "correlation-reason",
        &["robot-confirm-correlation", "robot-reject-correlation"],
    ),
    ("orphans-min-score", &["robot-orphans"]),
    ("file-beads-limit", &["robot-file-beads"]),
    ("hotspots-limit", &["robot-file-hotspots"]),
    ("relations-threshold", &["robot-file-relations"]),
    ("relations-limit", &["robot-file-relations"]),
    ("related-min-relevance", &["robot-related"]),
    ("related-max-results", &["robot-related"]),
    ("related-include-closed", &["robot-related"]),
    // main.go:1827-1836
    ("network-depth", &["robot-impact-network"]),
    ("forecast-label", &["robot-forecast"]),
    ("forecast-sprint", &["robot-forecast"]),
    ("forecast-agents", &["robot-forecast"]),
    ("agents", &["robot-capacity"]),
    ("capacity-label", &["robot-capacity"]),
    ("robot-by-label", &["robot-priority"]),
    ("robot-by-assignee", &["robot-priority"]),
    ("script-limit", &["emit-script"]),
    ("script-format", &["emit-script"]),
    // main.go:1837-1843
    ("pages-title", &["export-pages"]),
    ("pages-include-closed", &["export-pages"]),
    ("pages-include-history", &["export-pages"]),
    ("no-live-reload", &["preview-pages"]),
    ("watch-export", &["export-pages"]),
    ("debug-width", &["debug-render"]),
    ("debug-height", &["debug-render"]),
];

// ---------------------------------------------------------------------------
// Modifier-requires recovery examples (Go `modifierRecoveryExamples`,
// main.go:257-316, formatted by `formatModifierRecoveryExamples`,
// main.go:238-255)
// ---------------------------------------------------------------------------

/// Concrete invocations Go appends to a modifier-requires error so the caller
/// can retry without reading the docs. Row order mirrors Go's `switch`, and
/// the eleven modifiers Go leaves undecorated simply have no row here — Go's
/// `default` arm returns `nil`, which formats to the empty string, so an
/// undecorated error must stay byte-identical to the undecorated form.
///
/// Strings are copied verbatim from Go, quotes and all. `search-min-score`
/// rides with the other five search modifiers on Go's 257-263 row.
pub const MODIFIER_RECOVERY_EXAMPLES: &[(&str, &[&str])] = &[
    // main.go:259-264
    (
        "robot-search",
        &[
            r#"bvr robot-search "login oauth" --json"#,
            r#"bvr --search "login oauth" --robot-search --format json"#,
        ],
    ),
    (
        "search-limit",
        &[
            r#"bvr robot-search "login oauth" --json"#,
            r#"bvr --search "login oauth" --robot-search --format json"#,
        ],
    ),
    (
        "search-min-score",
        &[
            r#"bvr robot-search "login oauth" --json"#,
            r#"bvr --search "login oauth" --robot-search --format json"#,
        ],
    ),
    (
        "search-mode",
        &[
            r#"bvr robot-search "login oauth" --json"#,
            r#"bvr --search "login oauth" --robot-search --format json"#,
        ],
    ),
    (
        "search-preset",
        &[
            r#"bvr robot-search "login oauth" --json"#,
            r#"bvr --search "login oauth" --robot-search --format json"#,
        ],
    ),
    (
        "search-weights",
        &[
            r#"bvr robot-search "login oauth" --json"#,
            r#"bvr --search "login oauth" --robot-search --format json"#,
        ],
    ),
    // main.go:265-269
    (
        "robot-diff",
        &[
            "bvr robot-diff HEAD~1 --json",
            "bvr --robot-diff --diff-since HEAD~1 --format json",
        ],
    ),
    // main.go:270-278
    ("schema-command", &["bvr robot-schema triage --json"]),
    ("graph-format", &["bvr robot-graph mermaid --json"]),
    ("graph-depth", &["bvr robot-graph mermaid --json"]),
    (
        "graph-root",
        &["bvr robot-graph json --graph-root A --json"],
    ),
    // main.go:279-288
    ("severity", &["bvr robot-alerts --severity critical --json"]),
    (
        "alert-type",
        &["bvr robot-alerts --severity critical --json"],
    ),
    (
        "alert-label",
        &["bvr robot-alerts --severity critical --json"],
    ),
    (
        "robot-drift",
        &["bvr --check-drift --robot-drift --format json"],
    ),
    (
        "history-since",
        &[r#"bvr robot-history --history-since "30 days ago" --json"#],
    ),
    // main.go:289-297
    (
        "history-limit",
        &[r#"bvr robot-history --history-since "30 days ago" --json"#],
    ),
    (
        "min-confidence",
        &[r#"bvr robot-history --history-since "30 days ago" --json"#],
    ),
    (
        "robot-history-timeout-ms",
        &["bvr robot-triage --robot-history-timeout-ms 10000 --json"],
    ),
    // main.go:298-305
    ("brief", &["bvr robot-triage --brief --json"]),
    (
        "correlation-by",
        &["bvr robot-confirm-correlation deadbeef:A --correlation-by agent --json"],
    ),
    (
        "correlation-reason",
        &["bvr robot-confirm-correlation deadbeef:A --correlation-by agent --json"],
    ),
    // main.go:306-310
    (
        "orphans-min-score",
        &["bvr robot-orphans --orphans-min-score 30 --json"],
    ),
    (
        "file-beads-limit",
        &["bvr robot-file-beads README.md --file-beads-limit 10 --json"],
    ),
    (
        "hotspots-limit",
        &["bvr robot-file-hotspots --hotspots-limit 10 --json"],
    ),
    (
        "relations-threshold",
        &["bvr robot-file-relations README.md --relations-limit 10 --json"],
    ),
    // main.go:311-320
    (
        "relations-limit",
        &["bvr robot-file-relations README.md --relations-limit 10 --json"],
    ),
    (
        "related-min-relevance",
        &["bvr robot-related A --related-max-results 5 --json"],
    ),
    (
        "related-max-results",
        &["bvr robot-related A --related-max-results 5 --json"],
    ),
    (
        "related-include-closed",
        &["bvr robot-related A --related-max-results 5 --json"],
    ),
    // main.go:321-327
    (
        "network-depth",
        &["bvr robot-impact-network A --network-depth 2 --json"],
    ),
    (
        "forecast-label",
        &["bvr robot-forecast all --forecast-agents 3 --json"],
    ),
    (
        "forecast-sprint",
        &["bvr robot-forecast all --forecast-agents 3 --json"],
    ),
    // main.go:328-338
    (
        "forecast-agents",
        &["bvr robot-forecast all --forecast-agents 3 --json"],
    ),
    ("agents", &["bvr robot-capacity --agents 3 --json"]),
    ("capacity-label", &["bvr robot-capacity --agents 3 --json"]),
    (
        "robot-by-label",
        &["bvr robot-priority --robot-by-label backend --json"],
    ),
    // main.go:339-346
    (
        "robot-by-assignee",
        &["bvr robot-priority --robot-by-label backend --json"],
    ),
    ("script-limit", &["bvr --emit-script --script-limit 5"]),
    ("script-format", &["bvr --emit-script --script-limit 5"]),
    // main.go:347-358
    (
        "pages-title",
        &[r#"bvr --export-pages ./bv-pages --pages-title "Nightly Build""#],
    ),
    (
        "pages-include-closed",
        &[r#"bvr --export-pages ./bv-pages --pages-title "Nightly Build""#],
    ),
    (
        "pages-include-history",
        &[r#"bvr --export-pages ./bv-pages --pages-title "Nightly Build""#],
    ),
    (
        "no-live-reload",
        &["bvr --preview-pages ./bv-pages --no-live-reload"],
    ),
    (
        "watch-export",
        &["bvr --export-pages ./bv-pages --watch-export"],
    ),
    // main.go:359-361
    (
        "debug-width",
        &["bvr --debug-render triage --debug-width 120 --debug-height 40"],
    ),
    (
        "debug-height",
        &["bvr --debug-render triage --debug-width 120 --debug-height 40"],
    ),
];

/// Go `modifierRecoveryExamples` (main.go:257). A modifier with no row gets
/// Go's `default: return nil`, i.e. an empty list.
pub fn modifier_recovery_examples(modifier: &str) -> &'static [&'static str] {
    MODIFIER_RECOVERY_EXAMPLES
        .iter()
        .find(|(name, _)| *name == modifier)
        .map_or(&[][..], |(_, examples)| *examples)
}

/// Go `formatModifierRecoveryExamples` (main.go:238), the suffix
/// `validateModifierFlags` splices onto `--X requires Y` (main.go:232).
///
/// Three shapes, all returned verbatim (leading `\n` included):
///
/// * unmapped modifier → `""`
/// * exactly one example → `"\nTry: `<invocation>`."`
/// * several → `"\nTry one of:"` then `"\n  `<invocation>`"` per example
pub fn format_modifier_recovery_examples(modifier: &str) -> String {
    let examples = modifier_recovery_examples(modifier);
    match examples {
        [] => String::new(),
        [only] => format!("\nTry: `{only}`."),
        many => {
            let mut out = String::from("\nTry one of:");
            for example in many {
                out.push_str("\n  `");
                out.push_str(example);
                out.push('`');
            }
            out
        }
    }
}

// ---------------------------------------------------------------------------
// Enum flags (Go `validateEnumFlags`, main.go:363-395; rules at 1845-1848)
// ---------------------------------------------------------------------------

/// One `enumFlagRule` (main.go:203). `allowed` order is Go's slice order and
/// is load-bearing: it is both the "expected one of" list and the candidate
/// order `suggestClosest` walks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnumFlagRule {
    pub name: &'static str,
    pub allowed: &'static [&'static str],
}

/// Go `enumRules` (main.go:1845-1848) — every string flag Go constrains to a
/// closed set, checked after the modifier-requires pass and before the
/// exclusive-primary pass.
pub const ENUM_RULES: &[EnumFlagRule] = &[
    EnumFlagRule {
        name: "graph-format",
        allowed: &["json", "dot", "mermaid"],
    },
    EnumFlagRule {
        name: "script-format",
        allowed: &["bash", "fish", "zsh"],
    },
];

/// A rejected enum value, carrying the parts of Go's error text so a caller
/// can render it without re-deriving anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumFlagError {
    /// The `--`-less flag name (Go's `rule.name`).
    pub name: &'static str,
    /// The value exactly as supplied — Go quotes the raw value, not the
    /// lowercased/trimmed one.
    pub value: String,
    /// Go's `rule.allowed`, in declaration order.
    pub allowed: &'static [&'static str],
    /// Go's `formatDidYouMean` suffix: `"; did you mean \"zsh\"?"` or `""`.
    /// Always `""` for the empty-value error, which Go emits without a
    /// suggestion.
    pub did_you_mean: String,
}

impl EnumFlagError {
    /// Go's `validateEnumFlags` error text, byte-for-byte (main.go:378, 384).
    ///
    /// The two branches are distinct in Go and stay distinct here: a value that
    /// normalizes to empty gets no `did you mean`, because there is no typo to
    /// correct — the flag was simply given no value.
    pub fn message(&self) -> String {
        let expected = self.allowed.join(", ");
        if self.value_is_blank_after_normalizing() {
            format!(
                "invalid --{} {} (expected one of {expected})",
                self.name,
                go_quote(&self.value)
            )
        } else {
            format!(
                "invalid --{} {} (expected one of {expected}){}",
                self.name,
                go_quote(&self.value),
                self.did_you_mean
            )
        }
    }

    /// Go normalizes with `strings.ToLower(strings.TrimSpace(value))` and
    /// branches on the result (main.go:374). The message has to branch the same
    /// way, so re-derive the predicate instead of storing a second flag.
    fn value_is_blank_after_normalizing(&self) -> bool {
        self.value.trim().to_lowercase().is_empty()
    }
}

/// Go `validateEnumFlags` (main.go:363), reduced to its pure decision: the
/// first rule whose flag was supplied with a value outside `allowed` wins,
/// because Go `return`s immediately.
///
/// `supplied` pairs a flag name with its effective value — one entry per
/// changed string flag. Repeated flags are last-wins, matching pflag's
/// `FlagSet.GetString`, so pass the value the parser settled on.
#[allow(dead_code)] // consumed by the validation layer, which this file does not own
pub fn validate_enum_flags(supplied: &[(&str, &str)]) -> Option<EnumFlagError> {
    for rule in ENUM_RULES {
        let Some((_, value)) = supplied.iter().rev().find(|(name, _)| *name == rule.name) else {
            continue;
        };
        let normalized = value.trim().to_lowercase();
        if normalized.is_empty() {
            return Some(EnumFlagError {
                name: rule.name,
                value: (*value).to_string(),
                allowed: rule.allowed,
                did_you_mean: String::new(),
            });
        }
        if !rule.allowed.contains(&normalized.as_str()) {
            return Some(EnumFlagError {
                name: rule.name,
                value: (*value).to_string(),
                allowed: rule.allowed,
                did_you_mean: format_did_you_mean(&normalized, rule.allowed),
            });
        }
    }
    None
}

/// Go `formatDidYouMean` (main.go:409).
#[allow(dead_code)] // exercised through validate_enum_flags' error text
fn format_did_you_mean(value: &str, allowed: &[&'static str]) -> String {
    let suggestion = suggest_closest(value, allowed);
    if suggestion.is_empty() {
        return String::new();
    }
    format!("; did you mean {}?", go_quote(suggestion))
}

/// Go `suggestClosest` (main.go:413). The tie-break is transcribed literally:
/// a candidate at distance exactly `bestDist` only displaces the incumbent
/// when it sorts before it, and the walk keeps the FIRST row on a full tie.
#[allow(dead_code)] // exercised through validate_enum_flags' error text
fn suggest_closest(value: &str, allowed: &[&'static str]) -> &'static str {
    let value = normalize_enum_value(value);
    if value.is_empty() || allowed.is_empty() {
        return "";
    }
    let mut best: Option<&'static str> = None;
    let mut best_dist = max_suggestion_distance(&value);
    for candidate in allowed {
        let normalized = normalize_enum_value(candidate);
        if normalized.is_empty() {
            continue;
        }
        let dist = levenshtein_distance(&value, &normalized);
        // Go mutates `bestDist` from the threshold, so the very first
        // candidate is accepted whenever it lands within it.
        if dist <= best_dist
            && best.is_none_or(|incumbent| {
                dist < best_dist || normalized < normalize_enum_value(incumbent)
            })
        {
            best = Some(candidate);
            best_dist = dist;
        }
    }
    best.unwrap_or("")
}

/// Go's `strings.ToLower(strings.TrimSpace(...))`, the normalization applied
/// before every enum comparison and suggestion.
fn normalize_enum_value(value: &str) -> String {
    value.trim().to_lowercase()
}

/// Go `maxSuggestionDistance` (main.go:451) — a length-tiered ceiling on how
/// far a typo may be from an accepted value. `len` is a BYTE count, matching
/// Go, so a multibyte value gets the wider budget a longer string would.
fn max_suggestion_distance(value: &str) -> usize {
    match value.len() {
        0..=4 => 2,
        5..=10 => 3,
        _ => 4,
    }
}

/// Go `levenshteinDistance` (main.go:422), byte-for-byte. Go indexes
/// `a[i-1]`/`b[j-1]` directly, so the edit distance it computes is over BYTES,
/// not chars — a multibyte rune costs 2-4 edits. Porting over `char_indices`
/// would silently make Rust more forgiving than Go on non-ASCII input.
#[allow(dead_code)] // exercised through validate_enum_flags' error text
fn levenshtein_distance(a: &str, b: &str) -> usize {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a == b {
        return 0;
    }
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (cur[j - 1] + 1).min(prev[j] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Go's `%q` (i.e. `strconv.Quote`) for the value and suggestion in enum error
/// text. Rust's `{:?}` is not a substitute: it spells non-printables as
/// `\u{7f}` where Go writes `\x7f`, and it escapes `'` where Go does not.
///
/// The ASCII half is an exact transcription of `strconv`'s cases. For the rest
/// it reuses `char::escape_debug` — the only std predicate tracking Unicode
/// printability — and reshapes its `\u{…}` spelling into Go's `\uXXXX` /
/// `\UXXXXXXXX`. Flag values are shell text, so the ASCII half is the one that
/// decides the goldens; the non-ASCII half exists so the two never drift into
/// emitting invalid UTF-8.
#[allow(dead_code)] // exercised through validate_enum_flags' error text
fn go_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        let code = ch as u32;
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            // Go's `strconv.Quote` is `QuoteToASCII`-free and never escapes the
            // apostrophe; Rust's `escape_debug` always does, so it is split out
            // here rather than left to fall through.
            '\'' => out.push('\''),
            '\u{7}' => out.push_str("\\a"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{b}' => out.push_str("\\v"),
            // Go's `strconv` escape set for non-printables below U+0100,
            // plus DEL. These are exactly the runes with a named short form
            // or an ASCII-range fallback in `strconv.quoteWith`.
            _ if code < 0x20 || code == 0x7f => out.push_str(&format!("\\x{code:02x}")),
            _ => {
                let escaped = ch.escape_debug().to_string();
                if escaped == ch.to_string() {
                    out.push(ch);
                } else if let Some(hex) = escaped
                    .strip_prefix("\\u{")
                    .and_then(|h| h.strip_suffix('}'))
                {
                    // Go pads to 4 hex digits in the BMP and 8 above it.
                    let width = if u32::from_str_radix(hex, 16).is_ok_and(|c| c <= 0xFFFF) {
                        4
                    } else {
                        8
                    };
                    let prefix = if width == 4 { "\\u" } else { "\\U" };
                    out.push_str(prefix);
                    for _ in hex.len()..width {
                        out.push('0');
                    }
                    out.push_str(hex);
                } else {
                    out.push_str(&escaped);
                }
            }
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `flag_lookup` is the whole basis of
    /// `argv::normalize_flag_spelling`: a name the registry does not know is
    /// left alone, which is what keeps a negative value (`-1e400`) from being
    /// rewritten into a flag. A flag missing from `ROBOT_PRIMARIES` /
    /// `MODIFIER_FLAGS` would therefore stop being recognized under its
    /// single-dash spelling, so the registry is pinned against the exact set
    /// Go's `main` builds into `flag.CommandLine` before `newRootCommand`
    /// merges it into `cmd.Flags()` (main.go:1458-1583, merged at :518).
    #[test]
    fn registry_covers_every_go_command_line_flag() {
        const GO_FLAGS: &[&str] = &[
            "agent-brief",
            "agents",
            "agents-add",
            "agents-check",
            "agents-dry-run",
            "agents-force",
            "agents-remove",
            "agents-update",
            "alert-label",
            "alert-type",
            "as-of",
            "attention-limit",
            "background-mode",
            "baseline-info",
            "bead-history",
            "brief",
            "capacity-label",
            "check-drift",
            "check-update",
            "correlation-by",
            "correlation-reason",
            "cpu-profile",
            "db",
            "debug-height",
            "debug-render",
            "debug-width",
            "diff-since",
            "emit-script",
            "export",
            "export-format",
            "export-graph",
            "export-include-graph",
            "export-md",
            "export-pages",
            "export-template",
            "feedback-accept",
            "feedback-ignore",
            "feedback-reset",
            "feedback-show",
            "file-beads-limit",
            "force-full-analysis",
            "forecast-agents",
            "forecast-label",
            "forecast-sprint",
            "format",
            "generate-docs",
            "graph-depth",
            "graph-format",
            "graph-preset",
            "graph-root",
            "graph-title",
            "history-limit",
            "history-since",
            "hotspots-limit",
            "id-pattern",
            "label",
            "min-confidence",
            "network-depth",
            "no-background-mode",
            "no-cache",
            "no-hooks",
            "no-live-reload",
            "orphans-min-score",
            "pages",
            "pages-include-closed",
            "pages-include-history",
            "pages-title",
            "preview-pages",
            "priority-brief",
            "profile-json",
            "profile-startup",
            "recipe",
            "related-include-closed",
            "related-max-results",
            "related-min-relevance",
            "relations-limit",
            "relations-threshold",
            "repo",
            "robot-alerts",
            "robot-blocker-chain",
            "robot-burndown",
            "robot-by-assignee",
            "robot-by-label",
            "robot-capabilities",
            "robot-capacity",
            "robot-causality",
            "robot-confirm-correlation",
            "robot-correlation-stats",
            "robot-diff",
            "robot-docs",
            "robot-drift",
            "robot-explain-correlation",
            "robot-file-beads",
            "robot-file-hotspots",
            "robot-file-relations",
            "robot-forecast",
            "robot-graph",
            "robot-help",
            "robot-history",
            "robot-history-timeout-ms",
            "robot-impact",
            "robot-impact-network",
            "robot-insights",
            "robot-label-attention",
            "robot-label-flow",
            "robot-label-health",
            "robot-max-results",
            "robot-metrics",
            "robot-min-confidence",
            "robot-next",
            "robot-not-ready-labels",
            "robot-orphans",
            "robot-plan",
            "robot-priority",
            "robot-recipes",
            "robot-reject-correlation",
            "robot-related",
            "robot-schema",
            "robot-search",
            "robot-sprint-list",
            "robot-sprint-show",
            "robot-suggest",
            "robot-triage",
            "robot-triage-by-label",
            "robot-triage-by-track",
            "rollback",
            "save-baseline",
            "schema-command",
            "script-format",
            "script-limit",
            "search",
            "search-limit",
            "search-min-score",
            "search-mode",
            "search-preset",
            "search-weights",
            "severity",
            "stats",
            "suggest-bead",
            "suggest-confidence",
            "suggest-type",
            "theme",
            "update",
            "update-dry-run",
            "version",
            "watch-export",
            "workspace",
            "yes",
        ];
        for name in GO_FLAGS {
            assert!(
                flag_lookup(name),
                "{name} is in Go's flag.CommandLine but not the registry"
            );
        }
    }

    /// cobra's auto `--help` is the one name that is deliberately *absent*:
    /// it is added by `InitDefaultHelpFlag` during `Execute`, after
    /// `rewriteSingleDashLongFlags` has already run (main.go:4546).
    #[test]
    fn cobra_help_is_not_in_the_rewrite_registry() {
        assert!(!flag_lookup("help"));
    }

    /// Independent re-statement of Go's `rootHelpSections` matchers
    /// (main.go:59-206), so `HELP_FLAGS.section` is checked against the Go
    /// source rather than trusted.
    fn section_of(name: &str) -> &'static str {
        const GENERAL: &[&str] = &[
            "help",
            "version",
            "cpu-profile",
            "db",
            "update",
            "check-update",
            "update-dry-run",
            "rollback",
            "yes",
            "format",
            "stats",
            "profile-startup",
            "profile-json",
            "no-cache",
            "force-full-analysis",
            "theme",
            "background-mode",
            "no-background-mode",
        ];
        const SEARCH: &[&str] = &[
            "recipe",
            "search",
            "label",
            "severity",
            "alert-type",
            "alert-label",
            "workspace",
            "repo",
            "robot-min-confidence",
            "robot-max-results",
            "robot-by-label",
            "robot-by-assignee",
        ];
        const ROBOT: &[&str] = &[
            "attention-limit",
            "brief",
            "schema-command",
            "suggest-type",
            "suggest-confidence",
            "suggest-bead",
            "graph-format",
            "graph-root",
            "graph-depth",
            "orphans-min-score",
            "file-beads-limit",
            "hotspots-limit",
            "relations-threshold",
            "relations-limit",
            "related-min-relevance",
            "related-max-results",
            "related-include-closed",
            "forecast-label",
            "forecast-sprint",
            "forecast-agents",
            "agents",
            "capacity-label",
        ];
        const HISTORY: &[&str] = &[
            "diff-since",
            "as-of",
            "save-baseline",
            "baseline-info",
            "check-drift",
            "bead-history",
            "history-since",
            "history-limit",
            "min-confidence",
        ];
        const EXPORT: &[&str] = &[
            "export",
            "export-format",
            "export-include-graph",
            "export-template",
            "export-md",
            "no-hooks",
            "export-graph",
            "graph-preset",
            "graph-title",
            "emit-script",
            "script-limit",
            "script-format",
            "priority-brief",
            "agent-brief",
            "export-pages",
            "pages-title",
            "pages-include-closed",
            "pages-include-history",
            "preview-pages",
            "no-live-reload",
            "watch-export",
            "pages",
            "debug-render",
            "debug-width",
            "debug-height",
        ];
        if GENERAL.contains(&name) {
            "General Flags"
        } else if SEARCH.contains(&name) || name.starts_with("search-") {
            "Search & Filters"
        } else if name.starts_with("robot-")
            || ROBOT.contains(&name)
            || name.starts_with("correlation-")
        {
            "Robot & Planning Flags"
        } else if HISTORY.contains(&name) {
            "History & Drift"
        } else if EXPORT.contains(&name) {
            "Export & Reporting"
        } else if name.starts_with("agents-") {
            "Agent File Management"
        } else {
            "Other Flags"
        }
    }

    #[test]
    fn every_help_row_sits_in_its_go_section() {
        for h in HELP_FLAGS {
            assert_eq!(
                section_of(h.name),
                h.section,
                "--{} is filed under {:?}",
                h.name,
                h.section
            );
        }
    }

    /// Every flag the registry accepts must be advertised by `--help`, or a
    /// user has no way to discover it (issue #5).
    #[test]
    fn help_lists_every_registered_flag() {
        let rendered = render_help(HELP_PROGRAM);
        for f in ROBOT_PRIMARIES.iter().chain(MODIFIER_FLAGS) {
            let row = format!("--{}", f.name);
            assert!(
                rendered.contains(&row),
                "flag {row} is registered but missing from `--help`"
            );
            // Look the row up directly rather than through a `FlagDef`
            // accessor: the test is about the two tables agreeing, and the
            // accessor had no production caller.
            assert!(
                HELP_FLAGS.iter().any(|h| h.name == f.name),
                "flag --{} has no HELP_FLAGS row",
                f.name
            );
        }
    }

    #[test]
    fn help_shows_every_section_header() {
        let rendered = render_help(HELP_PROGRAM);
        for title in HELP_SECTIONS {
            assert!(
                rendered.contains(&format!("{title}:\n")),
                "section header {title:?} missing from `--help`"
            );
        }
    }

    /// pflag wraps on byte offsets, so the layout is only reproducible while
    /// every help string stays ASCII.
    #[test]
    fn help_table_is_ascii() {
        for h in HELP_FLAGS {
            for field in [h.name, h.section, h.type_word, h.help] {
                assert!(field.is_ascii(), "non-ASCII help field: {field:?}");
            }
        }
    }

    #[test]
    fn help_table_names_are_unique() {
        let mut names: Vec<&str> = HELP_FLAGS.iter().map(|h| h.name).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "duplicate flag name in HELP_FLAGS");
    }

    /// Go registers one short alias per `flag.StringP` call (main.go:1476,
    /// 1521, 1523) plus cobra's `-h`.
    #[test]
    fn help_short_aliases_match_go() {
        let with_short: Vec<(&str, char)> = HELP_FLAGS
            .iter()
            .filter_map(|h| h.short.map(|c| (h.name, c)))
            .collect();
        assert_eq!(
            with_short,
            vec![
                ("format", 'f'),
                ("label", 'l'),
                ("recipe", 'r'),
                ("help", 'h'),
            ]
        );
        for (name, alias) in with_short {
            assert_eq!(
                HELP_FLAGS.iter().find(|h| h.name == name).unwrap().short,
                Some(alias)
            );
        }
    }

    /// Section membership is a property of the name (Go `rootHelpSections`),
    /// so a flag cannot drift out of its group without this firing.
    #[test]
    fn help_sections_match_go_matchers() {
        let general = [
            "help",
            "version",
            "cpu-profile",
            "db",
            "update",
            "check-update",
            "update-dry-run",
            "rollback",
            "yes",
            "format",
            "stats",
            "profile-startup",
            "profile-json",
            "no-cache",
            "force-full-analysis",
            "theme",
            "background-mode",
            "no-background-mode",
        ];
        for name in general {
            assert_eq!(section_of(name), "General Flags", "--{name}");
        }
        for name in [
            "label",
            "recipe",
            "search",
            "search-mode",
            "severity",
            "workspace",
            "repo",
            "robot-min-confidence",
            "robot-by-assignee",
        ] {
            assert_eq!(section_of(name), "Search & Filters", "--{name}");
        }
        for name in [
            "robot-help",
            "robot-triage",
            "robot-capabilities",
            "schema-command",
            "graph-format",
            "correlation-by",
            "brief",
            "agents",
            "forecast-label",
        ] {
            assert_eq!(section_of(name), "Robot & Planning Flags", "--{name}");
        }
        for name in [
            "diff-since",
            "as-of",
            "save-baseline",
            "check-drift",
            "bead-history",
            "history-limit",
            "min-confidence",
        ] {
            assert_eq!(section_of(name), "History & Drift", "--{name}");
        }
        for name in [
            "export",
            "export-md",
            "export-pages",
            "no-hooks",
            "pages",
            "debug-render",
            "watch-export",
            "graph-preset",
        ] {
            assert_eq!(section_of(name), "Export & Reporting", "--{name}");
        }
        for name in ["agents-add", "agents-remove", "agents-force"] {
            assert_eq!(section_of(name), "Agent File Management", "--{name}");
        }
        for name in [
            "generate-docs",
            "id-pattern",
            "network-depth",
            "feedback-show",
        ] {
            assert_eq!(section_of(name), "Other Flags", "--{name}");
        }
    }

    /// Column layout is pflag's: two-space short column, six-space long
    /// column, and every description starting at the section's widest left
    /// column + 2. Spot-check the three shapes — plain, wrapped continuation,
    /// and a `(default …)` suffix — in two different sections so the column
    /// width is visibly derived per section rather than global.
    #[test]
    fn help_layout_matches_pflag() {
        let out = render_help(HELP_PROGRAM);
        let expected = [
            // General Flags' widest left column is `      --force-full-analysis`
            // (27) → descriptions start at column 29 (0-indexed).
            "      --force-full-analysis   Compute all metrics regardless of graph size (may be slow for\n",
            "                              large graphs)\n",
            // short alias: `  -f, --format string` (22) padded to column 29.
            "  -f, --format string         Structured output format for --robot-* commands: json or toon\n",
            "                              (env: BV_OUTPUT_FORMAT, TOON_DEFAULT_FORMAT)\n",
            // Export & Reporting's widest left column is
            // `      --pages-include-closed` (29) → descriptions start at 31.
            "      --graph-preset string      Graph layout preset: compact (default) or roomy (default\n",
            "                                 \"compact\")\n",
            // string defaults are rendered %q-quoted, numeric ones bare.
            "      --relations-threshold float                   Minimum correlation threshold (0.0-1.0)\n",
            "                                                    for related files (default 0.5)\n",
            "      --related-min-relevance percent_or_fraction   Minimum relevance score for related work\n",
            "                                                    (int 0-100 percent OR float 0.0-1.0\n",
            "                                                    fraction) (default 20)\n",
        ];
        for line in expected {
            assert!(out.contains(line), "missing help line: {line:?}");
        }
    }

    /// The banner and footer carry the program name, so a caller can render
    /// the Rust name without touching the flag data.
    #[test]
    fn help_banner_uses_program_name() {
        let out = render_help("bvr");
        assert!(out.starts_with("Usage: bvr [flags]\n\n"));
        assert!(
            out.ends_with("Run `bvr --robot-help` for detailed AI/robot command documentation.\n")
        );
    }

    /// The help page may only ever point at a command that exists. It used to
    /// print Go's `bv`, which this project does not ship — `install.sh`
    /// installs `bvr` and nothing else — so the footer's "Run `bv --robot-help`"
    /// was a dead end for every user who did not already have the Go oracle on
    /// PATH.
    #[test]
    fn help_never_points_at_a_command_we_do_not_ship() {
        let out = render_help(HELP_PROGRAM);
        assert!(
            !out.contains("`bv "),
            "help still suggests running `bv`: {:?}",
            out.lines()
                .filter(|l| l.contains("`bv "))
                .collect::<Vec<_>>()
        );
        // The strings that only *look* like a program reference must survive a
        // blanket rename: `.bv/workspace.yaml` is a directory, `(bv-87)` is an
        // issue ID, and `./bv-pages` is an example export path.
        for kept in [".bv/workspace.yaml", "(bv-87)", "./bv-pages"] {
            assert!(out.contains(kept), "unrelated `bv` string lost: {kept}");
        }
    }

    /// `--version` reports the emulated tool rather than this binary, so it
    /// keeps Go's name even though `--help` now prints the runnable one.
    #[test]
    fn version_keeps_the_go_name_that_help_dropped() {
        assert_eq!(VERSION_PROGRAM, "bv");
        assert_eq!(HELP_PROGRAM, "bvr");
    }

    #[test]
    fn robot_primaries_count_matches_go() {
        assert_eq!(ROBOT_PRIMARIES.len(), 41);
    }

    #[test]
    fn triage_family_is_one_exclusive_group() {
        let group: Vec<_> = ROBOT_PRIMARIES
            .iter()
            .filter(|f| f.group == Some("triage"))
            .collect();
        assert_eq!(group.len(), 4);
    }

    /// Issue #5: the six long flags Go advertises in `bv --help` must be
    /// registered or the CLI rejects a flag upstream accepts.
    #[test]
    fn issue_5_missing_flags_are_registered() {
        let registered: Vec<&str> = MODIFIER_FLAGS.iter().map(|f| f.name).collect();
        for flag in [
            "export",
            "export-format",
            "export-include-graph",
            "export-template",
            "generate-docs",
            "search-min-score",
        ] {
            assert!(
                registered.contains(&flag),
                "flag --{flag} is in Go --help but missing from MODIFIER_FLAGS"
            );
        }
    }

    /// Go main.go:1786-1792 — export options are rejected without a report command.
    #[test]
    fn issue_5_export_options_require_report_command() {
        for modifier in ["export-format", "export-include-graph", "export-template"] {
            let row = MODIFIER_REQUIRES
                .iter()
                .find(|(name, _)| *name == modifier)
                .unwrap_or_else(|| panic!("{modifier} missing from MODIFIER_REQUIRES"));
            assert_eq!(row.1, &["export", "export-md"]);
        }
    }

    #[test]
    fn search_min_score_requires_search() {
        let row = MODIFIER_REQUIRES
            .iter()
            .find(|(name, _)| *name == "search-min-score")
            .expect("search-min-score missing from MODIFIER_REQUIRES");
        assert_eq!(row.1, &["search"]);
    }

    // -----------------------------------------------------------------------
    // A. Evaluation order
    // -----------------------------------------------------------------------

    /// Go's `modifierRules` in source order. Transcribed independently of
    /// [`MODIFIER_REQUIRES`] so the table cannot quietly drift: a reordering
    /// that keeps every rule present still fails here, which is the whole bug
    /// this ordering guards against.
    const GO_MODIFIER_RULE_ORDER: &[&str] = &[
        "export-format",
        "export-include-graph",
        "export-template",
        "robot-diff",
        "robot-search",
        "search-limit",
        "search-min-score",
        "search-mode",
        "search-preset",
        "search-weights",
        "attention-limit",
        "schema-command",
        "suggest-type",
        "suggest-confidence",
        "suggest-bead",
        "graph-format",
        "graph-root",
        "graph-depth",
        "graph-preset",
        "graph-title",
        "severity",
        "alert-type",
        "alert-label",
        "profile-json",
        "robot-drift",
        "history-since",
        "history-limit",
        "brief",
        "robot-history-timeout-ms",
        "robot-not-ready-labels",
        "min-confidence",
        "correlation-by",
        "correlation-reason",
        "orphans-min-score",
        "file-beads-limit",
        "hotspots-limit",
        "relations-threshold",
        "relations-limit",
        "related-min-relevance",
        "related-max-results",
        "related-include-closed",
        "network-depth",
        "forecast-label",
        "forecast-sprint",
        "forecast-agents",
        "agents",
        "capacity-label",
        "robot-by-label",
        "robot-by-assignee",
        "script-limit",
        "script-format",
        "pages-title",
        "pages-include-closed",
        "pages-include-history",
        "no-live-reload",
        "watch-export",
        "debug-width",
        "debug-height",
    ];

    #[test]
    fn modifier_rules_are_in_go_evaluation_order() {
        let actual: Vec<&str> = MODIFIER_REQUIRES.iter().map(|(name, _)| *name).collect();
        assert_eq!(actual, GO_MODIFIER_RULE_ORDER);
        assert_eq!(actual.len(), 58, "Go registers 58 modifier rules");
    }

    /// A two-problem invocation must report the FIRST rule Go hits, and a
    /// set-based or name-sorted implementation cannot distinguish that. These
    /// three pairs are the ones the old table got wrong: `graph-preset` and
    /// `graph-title` sat after the alert/severity rows instead of before them,
    /// and `robot-not-ready-labels` sat after all of them.
    #[test]
    fn first_violation_follows_table_order() {
        // --graph-preset is row 19, --severity is row 21 (Go main.go:1804, 1806).
        let hits: Vec<&str> = MODIFIER_REQUIRES
            .iter()
            .filter(|(modifier, _)| matches!(*modifier, "graph-preset" | "severity"))
            .map(|(modifier, _)| *modifier)
            .collect();
        assert_eq!(hits, ["graph-preset", "severity"]);

        // --robot-history-timeout-ms is row 28, --min-confidence is row 30
        // (Go main.go:1814, 1816) — the old table had them the other way round.
        let hits: Vec<&str> = MODIFIER_REQUIRES
            .iter()
            .filter(|(modifier, _)| {
                matches!(*modifier, "robot-history-timeout-ms" | "min-confidence")
            })
            .map(|(modifier, _)| *modifier)
            .collect();
        assert_eq!(hits, ["robot-history-timeout-ms", "min-confidence"]);

        // --pages-title is row 52, --no-live-reload is row 55
        // (Go main.go:1837, 1840) — the old table ran --no-live-reload first.
        let hits: Vec<&str> = MODIFIER_REQUIRES
            .iter()
            .filter(|(modifier, _)| matches!(*modifier, "pages-title" | "no-live-reload"))
            .map(|(modifier, _)| *modifier)
            .collect();
        assert_eq!(hits, ["pages-title", "no-live-reload"]);
    }

    #[test]
    fn every_modifier_rule_has_a_distinct_name() {
        let mut names: Vec<&str> = MODIFIER_REQUIRES.iter().map(|(name, _)| *name).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(
            names.len(),
            total,
            "duplicate modifier in MODIFIER_REQUIRES"
        );
        for (_, required) in MODIFIER_REQUIRES {
            assert!(
                !required.is_empty(),
                "a rule with no co-flag can never pass"
            );
        }
    }

    // -----------------------------------------------------------------------
    // B. Recovery examples
    // -----------------------------------------------------------------------

    /// Go's `modifierRecoveryExamples` switch arms, as the set of modifiers
    /// that carry a hint. The eleven members of `MODIFIER_REQUIRES` *not*
    /// listed here are the ones Go's `default: return nil` leaves bare, and
    /// they must keep formatting to the empty string.
    const GO_HINTED_MODIFIERS: &[&str] = &[
        "robot-search",
        "search-limit",
        "search-min-score",
        "search-mode",
        "search-preset",
        "search-weights",
        "robot-diff",
        "schema-command",
        "graph-format",
        "graph-depth",
        "graph-root",
        "severity",
        "alert-type",
        "alert-label",
        "robot-drift",
        "history-since",
        "history-limit",
        "min-confidence",
        "robot-history-timeout-ms",
        "brief",
        "correlation-by",
        "correlation-reason",
        "orphans-min-score",
        "file-beads-limit",
        "hotspots-limit",
        "relations-threshold",
        "relations-limit",
        "related-min-relevance",
        "related-max-results",
        "related-include-closed",
        "network-depth",
        "forecast-label",
        "forecast-sprint",
        "forecast-agents",
        "agents",
        "capacity-label",
        "robot-by-label",
        "robot-by-assignee",
        "script-limit",
        "script-format",
        "pages-title",
        "pages-include-closed",
        "pages-include-history",
        "no-live-reload",
        "watch-export",
        "debug-width",
        "debug-height",
    ];

    /// Every rule gets a hint except the eleven Go leaves bare, and the hint
    /// table invents no rules of its own.
    #[test]
    fn recovery_hints_cover_exactly_gos_switch_arms() {
        let covered: Vec<&str> = MODIFIER_RECOVERY_EXAMPLES
            .iter()
            .map(|(name, _)| *name)
            .collect();
        let mut expected: Vec<&str> = GO_HINTED_MODIFIERS.to_vec();
        expected.sort_unstable();
        let mut actual = covered.clone();
        actual.sort_unstable();
        assert_eq!(actual, expected);
        assert_eq!(covered.len(), 47, "Go's switch arms cover 47 modifiers");

        // The complement — Go's `default: return nil` — in rule-table order.
        let unhinted: Vec<&str> = MODIFIER_REQUIRES
            .iter()
            .map(|(name, _)| *name)
            .filter(|name| !covered.contains(name))
            .collect();
        assert_eq!(
            unhinted,
            [
                "export-format",
                "export-include-graph",
                "export-template",
                "attention-limit",
                "suggest-type",
                "suggest-confidence",
                "suggest-bead",
                "graph-preset",
                "graph-title",
                "profile-json",
                "robot-not-ready-labels",
            ],
            "the unhinted set is Go's default arm, nothing more and nothing less"
        );
    }

    #[test]
    fn recovery_hint_text_matches_go() {
        // Go main.go:265-269 — two examples get the "Try one of:" block, and
        // each invocation keeps its own backtick fence.
        assert_eq!(
            format_modifier_recovery_examples("search-min-score"),
            "\nTry one of:\n  `bvr robot-search \"login oauth\" --json`\n  `bvr --search \"login oauth\" --robot-search --format json`"
        );
        assert_eq!(
            format_modifier_recovery_examples("robot-diff"),
            "\nTry one of:\n  `bvr robot-diff HEAD~1 --json`\n  `bvr --robot-diff --diff-since HEAD~1 --format json`"
        );
        // Go main.go:270 — a single example gets the short "Try: `x`." form,
        // with a trailing period, and no "one of".
        assert_eq!(
            format_modifier_recovery_examples("schema-command"),
            "\nTry: `bvr robot-schema triage --json`."
        );
        assert_eq!(
            format_modifier_recovery_examples("brief"),
            "\nTry: `bvr robot-triage --brief --json`."
        );
        // Go main.go:296-297 — escaped quotes in a double-quoted Go literal.
        assert_eq!(
            format_modifier_recovery_examples("history-limit"),
            "\nTry: `bvr robot-history --history-since \"30 days ago\" --json`."
        );
        // Go main.go:347-349 — a raw string keeps its double quotes literal.
        assert_eq!(
            format_modifier_recovery_examples("pages-title"),
            "\nTry: `bvr --export-pages ./bv-pages --pages-title \"Nightly Build\"`."
        );
    }

    #[test]
    fn unmapped_modifiers_get_no_recovery_hint() {
        // Go's `default: return nil` formats to "", so these errors stay as
        // bare as they were before the hint existed.
        for modifier in [
            "export-format",
            "export-include-graph",
            "export-template",
            "attention-limit",
            "suggest-type",
            "suggest-confidence",
            "suggest-bead",
            "graph-preset",
            "graph-title",
            "profile-json",
            "robot-not-ready-labels",
        ] {
            assert!(
                modifier_recovery_examples(modifier).is_empty(),
                "{modifier} must carry no recovery examples"
            );
            assert_eq!(
                format_modifier_recovery_examples(modifier),
                "",
                "{modifier}"
            );
        }
        // A flag nobody has a rule for is equally undecorated.
        assert_eq!(format_modifier_recovery_examples("not-a-flag"), "");
    }

    #[test]
    fn recovery_example_rows_are_shared_across_each_go_case() {
        // Go returns ONE slice per `case`, so every modifier in a multi-name arm
        // must resolve to the same examples — a per-modifier typo in the table
        // is otherwise invisible.
        for group in [
            &["graph-format", "graph-depth"][..],
            &["severity", "alert-type", "alert-label"][..],
            &["history-since", "history-limit", "min-confidence"][..],
            &["relations-threshold", "relations-limit"][..],
            &[
                "related-min-relevance",
                "related-max-results",
                "related-include-closed",
            ][..],
            &["forecast-label", "forecast-sprint", "forecast-agents"][..],
            &["agents", "capacity-label"][..],
            &["robot-by-label", "robot-by-assignee"][..],
            &["script-limit", "script-format"][..],
            &[
                "pages-title",
                "pages-include-closed",
                "pages-include-history",
            ][..],
            &["debug-width", "debug-height"][..],
        ] {
            let first = modifier_recovery_examples(group[0]);
            for name in &group[1..] {
                assert_eq!(modifier_recovery_examples(name), first, "{name}");
            }
        }
        // All six search modifiers share the 257-263 pair, search-min-score
        // included — it is the row Go's newer flag joined.
        for name in [
            "robot-search",
            "search-limit",
            "search-min-score",
            "search-mode",
            "search-preset",
            "search-weights",
        ] {
            assert_eq!(modifier_recovery_examples(name).len(), 2, "{name}");
        }
    }

    // -----------------------------------------------------------------------
    // C. Enum validation
    // -----------------------------------------------------------------------

    #[test]
    fn enum_rules_match_go() {
        // Go main.go:1845-1848 — graph-format AND script-format, in that order.
        assert_eq!(ENUM_RULES.len(), 2);
        assert_eq!(ENUM_RULES[0].name, "graph-format");
        assert_eq!(ENUM_RULES[0].allowed, &["json", "dot", "mermaid"]);
        assert_eq!(ENUM_RULES[1].name, "script-format");
        assert_eq!(ENUM_RULES[1].allowed, &["bash", "fish", "zsh"]);
    }

    #[test]
    fn enum_accepts_every_allowed_value_case_insensitively() {
        for rule in ENUM_RULES {
            let flag = rule.name;
            for value in rule.allowed {
                assert_eq!(
                    validate_enum_flags(&[(flag, value)]),
                    None,
                    "--{flag} {value} must be accepted"
                );
                // Go normalizes with TrimSpace + ToLower before comparing.
                let shouty = value.to_uppercase();
                assert_eq!(validate_enum_flags(&[(flag, &shouty)]), None, "--{flag}");
                let padded = format!("  {value}\t");
                assert_eq!(validate_enum_flags(&[(flag, &padded)]), None, "--{flag}");
            }
        }
    }

    #[test]
    fn script_format_rejects_an_unknown_shell() {
        // Go's exact stderr for `--emit-script --script-format sh`:
        //   invalid --script-format "sh" (expected one of bash, fish, zsh); did you mean "zsh"?
        let err = validate_enum_flags(&[("script-format", "sh")])
            .expect("sh is not one of bash|fish|zsh");
        assert_eq!(err.name, "script-format");
        assert_eq!(err.value, "sh");
        assert_eq!(
            err.message(),
            "invalid --script-format \"sh\" (expected one of bash, fish, zsh); did you mean \"zsh\"?"
        );
    }

    #[test]
    fn enum_suggestion_follows_gos_distance_ceiling() {
        // "x" is 1 byte so the ceiling is 2, and it is 3-4 edits from every
        // allowed value — Go reports the value with no hint at all.
        let far = validate_enum_flags(&[("script-format", "x")]).expect("x is invalid");
        assert_eq!(far.did_you_mean, "");
        assert_eq!(
            far.message(),
            "invalid --script-format \"x\" (expected one of bash, fish, zsh)"
        );

        // "gh" is also 1 edit short of nothing, but 2 edits from "zsh" and 3
        // from the other two, so the same ceiling admits exactly one candidate.
        let edge = validate_enum_flags(&[("script-format", "gh")]).expect("gh is invalid");
        assert_eq!(
            edge.message(),
            "invalid --script-format \"gh\" (expected one of bash, fish, zsh); did you mean \"zsh\"?"
        );

        // "bashh" is 1 edit from "bash" and inside every ceiling. The two
        // 3-edit candidates never get a look once "bash" tightens `bestDist`.
        let near = validate_enum_flags(&[("script-format", "bashh")]).expect("bashh is invalid");
        assert_eq!(
            near.message(),
            "invalid --script-format \"bashh\" (expected one of bash, fish, zsh); did you mean \"bash\"?"
        );
    }

    /// Go emits a *different* message for a value that normalizes to empty
    /// (main.go:376): no `did you mean`, because nothing was typed. Keeping the
    /// two branches apart is the only way `--script-format ""` reads like Go.
    #[test]
    fn empty_enum_value_gets_no_did_you_mean() {
        for blank in ["", "   ", "\t\n"] {
            let err = validate_enum_flags(&[("script-format", blank)]).expect("blank is invalid");
            assert_eq!(err.did_you_mean, "", "{blank:?}");
            assert_eq!(
                err.message(),
                format!(
                    "invalid --script-format {} (expected one of bash, fish, zsh)",
                    go_quote(blank)
                )
            );
        }
    }

    #[test]
    fn graph_format_is_validated_too() {
        let err = validate_enum_flags(&[("graph-format", "jsonn")]).expect("jsonn is invalid");
        assert_eq!(
            err.message(),
            "invalid --graph-format \"jsonn\" (expected one of json, dot, mermaid); did you mean \"json\"?"
        );
    }

    /// Go `return`s on the first failing rule, so a command bad in both ways
    /// reports `graph-format` (main.go:1846) and never reaches `script-format`.
    #[test]
    fn first_failing_enum_rule_wins() {
        let err = validate_enum_flags(&[("script-format", "sh"), ("graph-format", "svg")])
            .expect("both are invalid");
        assert_eq!(err.name, "graph-format");
        assert_eq!(
            validate_enum_flags(&[("script-format", "sh")]).map(|e| e.name),
            Some("script-format")
        );
    }

    #[test]
    fn unsupplied_enum_flags_are_not_checked() {
        // Go skips a rule unless the flag was `Changed`.
        assert_eq!(validate_enum_flags(&[]), None);
        assert_eq!(validate_enum_flags(&[("severity", "nonsense")]), None);
    }

    #[test]
    fn repeated_enum_flags_use_the_last_value() {
        // pflag's GetString is last-wins, so the parser's settled value decides.
        assert_eq!(
            validate_enum_flags(&[("script-format", "sh"), ("script-format", "zsh")]),
            None
        );
        let err = validate_enum_flags(&[("script-format", "zsh"), ("script-format", "sh")])
            .expect("last value is invalid");
        assert_eq!(err.value, "sh");
    }

    #[test]
    fn levenshtein_is_byte_wise_like_go() {
        assert_eq!(levenshtein_distance("sh", "zsh"), 1);
        assert_eq!(levenshtein_distance("sh", "bash"), 2);
        assert_eq!(levenshtein_distance("sh", "fish"), 2);
        assert_eq!(levenshtein_distance("gh", "bash"), 3);
        assert_eq!(levenshtein_distance("", "fish"), 4);
        assert_eq!(levenshtein_distance("kitten", "sitting"), 3);
        // Go indexes `a[i-1]`, so it scores a 2-byte rune as 2 edits, not the
        // 1 a char-wise port would report. Same first byte, so "é"→"è" is 1.
        assert_eq!(levenshtein_distance("e", "é"), 2);
        assert_eq!(levenshtein_distance("é", "è"), 1);
    }

    #[test]
    fn suggestion_ceiling_is_length_tiered_by_bytes() {
        // Go main.go:451-457 — <=4 bytes gives 2, <=10 gives 3, else 4.
        assert_eq!(max_suggestion_distance(""), 2);
        assert_eq!(max_suggestion_distance("sh"), 2);
        assert_eq!(max_suggestion_distance("merm"), 2);
        assert_eq!(max_suggestion_distance("bashh"), 3);
        assert_eq!(max_suggestion_distance("mermai"), 3);
        assert_eq!(max_suggestion_distance("jsonnnn"), 3);
        assert_eq!(max_suggestion_distance("a-very-long-value"), 4);
        // A 2-byte rune pushes a 5-char string out of the top tier.
        assert_eq!(max_suggestion_distance("ééé"), 3);
    }

    #[test]
    fn go_quote_matches_strconv_quote() {
        assert_eq!(go_quote("sh"), "\"sh\"");
        assert_eq!(go_quote(""), "\"\"");
        assert_eq!(go_quote("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(go_quote("back\\slash"), "\"back\\\\slash\"");
        assert_eq!(go_quote("line\nfeed"), "\"line\\nfeed\"");
        assert_eq!(go_quote("tab\there"), "\"tab\\there\"");
        assert_eq!(
            go_quote("\u{1}\u{7}\u{8}\u{b}\u{c}\u{7f}"),
            "\"\\x01\\a\\b\\v\\f\\x7f\""
        );
        // Go leaves the apostrophe alone inside a string; Rust's Debug would
        // escape it, which is exactly why this is not `{:?}`.
        assert_eq!(go_quote("it's"), "\"it's\"");
        assert_eq!(go_quote("naïve café"), "\"naïve café\"");
    }
}

// ---------------------------------------------------------------------------
// Go-side registries, used by --generate-docs.
//
// `FlagDef` above is the dispatch registry: it answers "is this flag
// registered and what kind of value does it take", which is all the parser
// and the validators need. `docgen` needs three more fields — the default
// and the one-line usage string — so this table carries them. The values are
// transcribed from cmd/bv/main.go's flag declarations, not hand-invented;
// a flag missing here simply does not appear in the generated docs, which is
// the same failure Go's own `fs.VisitAll` would produce for an unregistered
// name.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct GoFlag {
    pub name: &'static str,
    pub kind: FlagKind,
    /// Go's `DefValue`, as `pflag` would render it. Empty means the doc
    /// generator writes `(empty)` — see `docgen::render_flags_table`.
    pub default: &'static str,
    pub description: &'static str,
}
pub const GO_FLAGS: &[GoFlag] = &[
    GoFlag { name: "agent-brief", kind: FlagKind::Str, default: "", description: "Export agent brief bundle to directory (includes triage.json, insights.json, brief.md, helpers.md)" },
    GoFlag { name: "agents", kind: FlagKind::Int, default: "1", description: "Number of parallel agents for capacity simulation" },
    GoFlag { name: "agents-add", kind: FlagKind::Bool, default: "false", description: "Add beads workflow instructions to AGENTS.md (creates file if needed)" },
    GoFlag { name: "agents-check", kind: FlagKind::Bool, default: "false", description: "Check AGENTS.md blurb status (default if no --agents-* action)" },
    GoFlag { name: "agents-dry-run", kind: FlagKind::Bool, default: "false", description: "Show what would happen without executing (use with --agents-*)" },
    GoFlag { name: "agents-force", kind: FlagKind::Bool, default: "false", description: "Skip confirmation prompts (use with --agents-*)" },
    GoFlag { name: "agents-remove", kind: FlagKind::Bool, default: "false", description: "Remove beads workflow instructions from AGENTS.md" },
    GoFlag { name: "agents-update", kind: FlagKind::Bool, default: "false", description: "Update beads workflow instructions to latest version" },
    GoFlag { name: "alert-label", kind: FlagKind::Str, default: "", description: "Filter robot alerts by label match" },
    GoFlag { name: "alert-type", kind: FlagKind::Str, default: "", description: "Filter robot alerts by alert type (e.g., stale_issue)" },
    GoFlag { name: "as-of", kind: FlagKind::Str, default: "", description: "View state at point in time (commit SHA, branch, tag, or date)" },
    GoFlag { name: "attention-limit", kind: FlagKind::Int, default: "5", description: "Limit number of labels in --robot-label-attention output" },
    GoFlag { name: "background-mode", kind: FlagKind::Bool, default: "false", description: "Enable experimental background snapshot loading (TUI only)" },
    GoFlag { name: "baseline-info", kind: FlagKind::Bool, default: "false", description: "Show information about the current baseline" },
    GoFlag { name: "bead-history", kind: FlagKind::Str, default: "", description: "Show history for specific bead ID" },
    GoFlag { name: "brief", kind: FlagKind::Bool, default: "false", description: "Compact --robot-triage output: only decision-relevant fields (id, title, status, assignee, blockers, unblocks) (#183)" },
    GoFlag { name: "capacity-label", kind: FlagKind::Str, default: "", description: "Filter capacity simulation by label" },
    GoFlag { name: "check-drift", kind: FlagKind::Bool, default: "false", description: "Check for drift from baseline (exit codes: 0=OK, 1=critical, 2=warning)" },
    GoFlag { name: "check-update", kind: FlagKind::Bool, default: "false", description: "Check if a new version is available" },
    GoFlag { name: "correlation-by", kind: FlagKind::Str, default: "", description: "Agent/user identifier for correlation feedback" },
    GoFlag { name: "correlation-reason", kind: FlagKind::Str, default: "", description: "Reason for correlation feedback" },
    GoFlag { name: "cpu-profile", kind: FlagKind::Str, default: "", description: "Write CPU profile to file" },
    GoFlag { name: "db", kind: FlagKind::Str, default: "", description: "Path to beads database file or .beads directory (overrides BEADS_DB and BEADS_DIR env vars)" },
    GoFlag { name: "debug-height", kind: FlagKind::Int, default: "50", description: "Height for debug render" },
    GoFlag { name: "debug-render", kind: FlagKind::Str, default: "", description: "Render a view and output to file (views: insights, board)" },
    GoFlag { name: "debug-width", kind: FlagKind::Int, default: "180", description: "Width for debug render" },
    GoFlag { name: "diff-since", kind: FlagKind::Str, default: "", description: "Show changes since historical point (commit SHA, branch, tag, or date)" },
    GoFlag { name: "emit-script", kind: FlagKind::Bool, default: "false", description: "Emit shell script for top-N recommendations (agent workflows)" },
    GoFlag { name: "export", kind: FlagKind::Str, default: "", description: "Export a report using recipe defaults or explicit export options" },
    GoFlag { name: "export-format", kind: FlagKind::Str, default: "", description: "Report format: markdown, json, csv or mermaid" },
    GoFlag { name: "export-graph", kind: FlagKind::Str, default: "", description: "Export graph: .html for interactive, .png/.svg for static (auto-names if empty)" },
    GoFlag { name: "export-include-graph", kind: FlagKind::Bool, default: "true", description: "Include dependency context in the report (explicit false overrides recipe)" },
    GoFlag { name: "export-md", kind: FlagKind::Str, default: "", description: "Export issues to a Markdown file (e.g., report.md)" },
    GoFlag { name: "export-pages", kind: FlagKind::Str, default: "", description: "Export static site to directory (e.g., ./bv-pages)" },
    GoFlag { name: "export-template", kind: FlagKind::Str, default: "", description: "Markdown template path; explicit empty disables a recipe template" },
    GoFlag { name: "feedback-accept", kind: FlagKind::Str, default: "", description: "Record accept feedback for issue ID (tunes recommendation weights)" },
    GoFlag { name: "feedback-ignore", kind: FlagKind::Str, default: "", description: "Record ignore feedback for issue ID (tunes recommendation weights)" },
    GoFlag { name: "feedback-reset", kind: FlagKind::Bool, default: "false", description: "Reset all feedback data to defaults" },
    GoFlag { name: "feedback-show", kind: FlagKind::Bool, default: "false", description: "Show current feedback status and weight adjustments" },
    GoFlag { name: "file-beads-limit", kind: FlagKind::Int, default: "20", description: "Max closed beads to show (use with --robot-file-beads)" },
    GoFlag { name: "force-full-analysis", kind: FlagKind::Bool, default: "false", description: "Compute all metrics regardless of graph size (may be slow for large graphs)" },
    GoFlag { name: "forecast-agents", kind: FlagKind::Int, default: "1", description: "Number of parallel agents for capacity calculation" },
    GoFlag { name: "forecast-label", kind: FlagKind::Str, default: "", description: "Filter forecast by label" },
    GoFlag { name: "forecast-sprint", kind: FlagKind::Str, default: "", description: "Filter forecast by sprint ID" },
    GoFlag { name: "format", kind: FlagKind::Str, default: "", description: "Structured output format for --robot-* commands: json or toon (env: BV_OUTPUT_FORMAT, TOON_DEFAULT_FORMAT)" },
    GoFlag { name: "generate-docs", kind: FlagKind::Bool, default: "false", description: "Generate documentation markdown and JSON artifacts" },
    GoFlag { name: "graph-depth", kind: FlagKind::Int, default: "0", description: "Max depth for subgraph (0 = unlimited)" },
    GoFlag { name: "graph-format", kind: FlagKind::Str, default: "json", description: "Graph output format: json, dot, mermaid" },
    GoFlag { name: "graph-preset", kind: FlagKind::Str, default: "compact", description: "Graph layout preset: compact (default) or roomy" },
    GoFlag { name: "graph-root", kind: FlagKind::Str, default: "", description: "Subgraph from specific root issue ID" },
    GoFlag { name: "graph-title", kind: FlagKind::Str, default: "", description: "Title for graph export (default: project name)" },
    GoFlag { name: "history-limit", kind: FlagKind::Int, default: "500", description: "Max commits to analyze (0 = unlimited)" },
    GoFlag { name: "history-since", kind: FlagKind::Str, default: "", description: "Limit history to commits after this date/ref (e.g., '30 days ago', '2024-01-01')" },
    GoFlag { name: "hotspots-limit", kind: FlagKind::Int, default: "10", description: "Max hotspots to show (use with --robot-file-hotspots)" },
    GoFlag { name: "id-pattern", kind: FlagKind::StringArray, default: "[]", description: "Custom bead ID regex for commit-message matching, e.g. 'bh-[a-z0-9]{5}' (repeatable; capture group 1 is the ID, else the whole match) (#188)" },
    GoFlag { name: "label", kind: FlagKind::Str, default: "", description: "Scope analysis to label's subgraph (applies to every --robot-* command that loads issues, e.g. --robot-insights, --robot-plan, --robot-priority, --robot-orphans)" },
    GoFlag { name: "min-confidence", kind: FlagKind::Float64, default: "0", description: "Filter correlations by minimum confidence (0.0-1.0)" },
    GoFlag { name: "network-depth", kind: FlagKind::Int, default: "2", description: "Depth of subnetwork when querying specific bead (1-3)" },
    GoFlag { name: "no-background-mode", kind: FlagKind::Bool, default: "false", description: "Disable experimental background snapshot loading (TUI only)" },
    GoFlag { name: "no-cache", kind: FlagKind::Bool, default: "false", description: "Bypass disk cache for robot triage (also: BV_NO_CACHE=1)" },
    GoFlag { name: "no-hooks", kind: FlagKind::Bool, default: "false", description: "Skip running hooks during export" },
    GoFlag { name: "no-live-reload", kind: FlagKind::Bool, default: "false", description: "Disable live-reload in preview mode" },
    GoFlag { name: "orphans-min-score", kind: FlagKind::Int, default: "30", description: "Minimum suspicion score for orphan candidates (0-100)" },
    GoFlag { name: "pages", kind: FlagKind::Bool, default: "false", description: "Launch interactive Pages deployment wizard" },
    GoFlag { name: "pages-include-closed", kind: FlagKind::Bool, default: "true", description: "Include closed issues in export (default: true)" },
    GoFlag { name: "pages-include-history", kind: FlagKind::Bool, default: "true", description: "Include git history for time-travel (default: true)" },
    GoFlag { name: "pages-title", kind: FlagKind::Str, default: "", description: "Custom title for static site" },
    GoFlag { name: "preview-pages", kind: FlagKind::Str, default: "", description: "Preview existing static site bundle" },
    GoFlag { name: "priority-brief", kind: FlagKind::Str, default: "", description: "Export priority brief to Markdown file (e.g., brief.md)" },
    GoFlag { name: "profile-json", kind: FlagKind::Bool, default: "false", description: "Output profile in JSON format (use with --profile-startup)" },
    GoFlag { name: "profile-startup", kind: FlagKind::Bool, default: "false", description: "Output detailed startup timing profile for diagnostics" },
    GoFlag { name: "recipe", kind: FlagKind::Str, default: "", description: "Apply a recipe by name (e.g., triage, actionable, high-impact) or by .yaml/.yml file path (e.g., .beads/recipes/sprint.yaml)" },
    GoFlag { name: "related-include-closed", kind: FlagKind::Bool, default: "false", description: "Include closed beads in related work results" },
    GoFlag { name: "related-min-relevance", kind: FlagKind::PercentOrFraction, default: "20", description: "Minimum relevance score for related work (int 0-100 percent OR float 0.0-1.0 fraction)" },
    GoFlag { name: "related-max-results", kind: FlagKind::Int, default: "10", description: "Max results per category for related work" },
    GoFlag { name: "relations-limit", kind: FlagKind::Int, default: "10", description: "Max related files to show" },
    GoFlag { name: "relations-threshold", kind: FlagKind::Float64, default: "0.5", description: "Minimum correlation threshold (0.0-1.0) for related files" },
    GoFlag { name: "repo", kind: FlagKind::Str, default: "", description: "Filter issues by repository prefix (e.g., 'api-' or 'api')" },
    GoFlag { name: "robot-alerts", kind: FlagKind::Bool, default: "false", description: "Output alerts (drift + proactive) as JSON for AI agents" },
    GoFlag { name: "robot-blocker-chain", kind: FlagKind::Str, default: "", description: "Output full blocker chain analysis for issue ID as JSON" },
    GoFlag { name: "robot-burndown", kind: FlagKind::Str, default: "", description: "Output burndown data for sprint ID, or 'current' for active sprint" },
    GoFlag { name: "robot-by-assignee", kind: FlagKind::Str, default: "", description: "Filter robot outputs by assignee (exact match)" },
    GoFlag { name: "robot-by-label", kind: FlagKind::Str, default: "", description: "Filter robot outputs by label (exact match)" },
    GoFlag { name: "robot-capabilities", kind: FlagKind::Bool, default: "false", description: "Output machine-readable command capabilities for AI agents" },
    GoFlag { name: "robot-capacity", kind: FlagKind::Bool, default: "false", description: "Output capacity simulation and completion projection as JSON" },
    GoFlag { name: "robot-causality", kind: FlagKind::Str, default: "", description: "Output causal chain analysis for bead ID as JSON" },
    GoFlag { name: "robot-confirm-correlation", kind: FlagKind::Str, default: "", description: "Confirm a correlation is correct (format: SHA:beadID)" },
    GoFlag { name: "robot-correlation-stats", kind: FlagKind::Bool, default: "false", description: "Output correlation feedback statistics as JSON" },
    GoFlag { name: "robot-diff", kind: FlagKind::Bool, default: "false", description: "Output diff as JSON (use with --diff-since)" },
    GoFlag { name: "robot-docs", kind: FlagKind::Str, default: "", description: "Machine-readable JSON docs for AI agents. Topics: guide, commands, examples, env, exit-codes, all" },
    GoFlag { name: "robot-drift", kind: FlagKind::Bool, default: "false", description: "Output drift check as JSON (use with --check-drift)" },
    GoFlag { name: "robot-explain-correlation", kind: FlagKind::Str, default: "", description: "Explain why a commit is linked to a bead (format: SHA:beadID)" },
    GoFlag { name: "robot-file-beads", kind: FlagKind::Str, default: "", description: "Output beads that touched a file path as JSON" },
    GoFlag { name: "robot-file-hotspots", kind: FlagKind::Bool, default: "false", description: "Output files touched by most beads as JSON" },
    GoFlag { name: "robot-file-relations", kind: FlagKind::Str, default: "", description: "Output files that frequently co-change with the given file path" },
    GoFlag { name: "robot-forecast", kind: FlagKind::Str, default: "", description: "Output ETA forecast for bead ID, or 'all' for all open issues" },
    GoFlag { name: "robot-graph", kind: FlagKind::Bool, default: "false", description: "Output dependency graph as JSON/DOT/Mermaid for AI agents" },
    GoFlag { name: "robot-help", kind: FlagKind::Bool, default: "false", description: "Show AI agent help" },
    GoFlag { name: "robot-history", kind: FlagKind::Bool, default: "false", description: "Output bead-to-commit correlations as JSON" },
    GoFlag { name: "robot-history-timeout-ms", kind: FlagKind::Int, default: "-1", description: "Budget in ms for the git-history prologue of robot triage (0 = unbounded; default 10000, env BV_ROBOT_HISTORY_TIMEOUT_MS)" },
    GoFlag { name: "robot-impact", kind: FlagKind::Str, default: "", description: "Analyze impact of modifying files (comma-separated paths)" },
    GoFlag { name: "robot-impact-network", kind: FlagKind::Str, default: "", description: "Output bead impact network as JSON (empty for full, or bead ID for subnetwork)" },
    GoFlag { name: "robot-insights", kind: FlagKind::Bool, default: "false", description: "Output graph analysis and insights as JSON for AI agents" },
    GoFlag { name: "robot-label-attention", kind: FlagKind::Bool, default: "false", description: "Output attention-ranked labels as JSON for AI agents" },
    GoFlag { name: "robot-label-flow", kind: FlagKind::Bool, default: "false", description: "Output cross-label dependency flow as JSON for AI agents" },
    GoFlag { name: "robot-label-health", kind: FlagKind::Bool, default: "false", description: "Output label health metrics as JSON for AI agents" },
    GoFlag { name: "robot-max-results", kind: FlagKind::Int, default: "0", description: "Limit robot output count (0 = use defaults)" },
    GoFlag { name: "robot-metrics", kind: FlagKind::Bool, default: "false", description: "Output performance metrics (timing, cache, memory) as JSON" },
    GoFlag { name: "robot-min-confidence", kind: FlagKind::Float64, default: "0", description: "Filter robot outputs by minimum confidence (0.0-1.0)" },
    GoFlag { name: "robot-next", kind: FlagKind::Bool, default: "false", description: "Output only the top pick recommendation as JSON (minimal triage)" },
    GoFlag { name: "robot-not-ready-labels", kind: FlagKind::Str, default: "", description: "Comma-separated labels marking a bead not-ready: excluded from claimable --robot-next/--robot-triage top picks (env: BV_ROBOT_NOT_READY_LABELS; #173)" },
    GoFlag { name: "robot-orphans", kind: FlagKind::Bool, default: "false", description: "Output orphan commit candidates (commits that should be linked but aren't) as JSON" },
    GoFlag { name: "robot-plan", kind: FlagKind::Bool, default: "false", description: "Output dependency-respecting execution plan as JSON for AI agents" },
    GoFlag { name: "robot-priority", kind: FlagKind::Bool, default: "false", description: "Output priority recommendations as JSON for AI agents" },
    GoFlag { name: "robot-recipes", kind: FlagKind::Bool, default: "false", description: "Output available recipes as JSON for AI agents" },
    GoFlag { name: "robot-reject-correlation", kind: FlagKind::Str, default: "", description: "Reject an incorrect correlation (format: SHA:beadID)" },
    GoFlag { name: "robot-related", kind: FlagKind::Str, default: "", description: "Output beads related to a specific bead ID as JSON" },
    GoFlag { name: "robot-schema", kind: FlagKind::Bool, default: "false", description: "Output JSON Schema definitions for all robot commands" },
    GoFlag { name: "robot-search", kind: FlagKind::Bool, default: "false", description: "Output keyword or hybrid search results as JSON for AI agents (use with --search)" },
    GoFlag { name: "robot-sprint-list", kind: FlagKind::Bool, default: "false", description: "Output sprints as JSON" },
    GoFlag { name: "robot-sprint-show", kind: FlagKind::Str, default: "", description: "Output specific sprint details as JSON" },
    GoFlag { name: "robot-suggest", kind: FlagKind::Bool, default: "false", description: "Output smart suggestions (duplicates, dependencies, labels, cycles) as JSON" },
    GoFlag { name: "robot-triage", kind: FlagKind::Bool, default: "false", description: "Output unified triage as JSON (the mega-command for AI agents)" },
    GoFlag { name: "robot-triage-by-label", kind: FlagKind::Bool, default: "false", description: "Group triage recommendations by label (bv-87)" },
    GoFlag { name: "robot-triage-by-track", kind: FlagKind::Bool, default: "false", description: "Group triage recommendations by execution track (bv-87)" },
    GoFlag { name: "rollback", kind: FlagKind::Bool, default: "false", description: "Rollback to the previous version (from backup)" },
    GoFlag { name: "save-baseline", kind: FlagKind::Str, default: "", description: "Save current metrics as baseline with optional description" },
    GoFlag { name: "schema-command", kind: FlagKind::Str, default: "", description: "Output schema for specific command only (e.g., robot-triage)" },
    GoFlag { name: "script-format", kind: FlagKind::Str, default: "bash", description: "Script format: bash, fish, or zsh (use with --emit-script)" },
    GoFlag { name: "script-limit", kind: FlagKind::Int, default: "5", description: "Limit number of items in emitted script (use with --emit-script)" },
    GoFlag { name: "search", kind: FlagKind::Str, default: "", description: "Hashed keyword search query (builds/updates index on first run)" },
    GoFlag { name: "search-limit", kind: FlagKind::Int, default: "10", description: "Max results for --search/--robot-search" },
    GoFlag { name: "search-min-score", kind: FlagKind::Str, default: "", description: "Minimum text similarity before hybrid ranking (-1..1); exact IDs also obey this threshold" },
    GoFlag { name: "search-mode", kind: FlagKind::Str, default: "", description: "Search ranking mode: text or hybrid (default: BV_SEARCH_MODE or text)" },
    GoFlag { name: "search-preset", kind: FlagKind::Str, default: "", description: "Hybrid preset name (default: BV_SEARCH_PRESET or default)" },
    GoFlag { name: "search-weights", kind: FlagKind::Str, default: "", description: "Hybrid weights JSON (overrides preset; keys: text,pagerank,status,impact,priority,recency)" },
    GoFlag { name: "severity", kind: FlagKind::Str, default: "", description: "Filter robot alerts by severity (info|warning|critical)" },
    GoFlag { name: "stats", kind: FlagKind::Bool, default: "false", description: "Show JSON vs TOON token estimates on stderr (env: TOON_STATS=1)" },
    GoFlag { name: "suggest-bead", kind: FlagKind::Str, default: "", description: "Filter suggestions for specific bead ID" },
    GoFlag { name: "suggest-confidence", kind: FlagKind::Float64, default: "0", description: "Minimum confidence for suggestions (0.0-1.0)" },
    GoFlag { name: "suggest-type", kind: FlagKind::Str, default: "", description: "Filter suggestions by type: duplicate, dependency, label, cycle" },
    GoFlag { name: "theme", kind: FlagKind::Str, default: "", description: "Color theme: light, dark, or auto (default: detect terminal background)" },
    GoFlag { name: "update", kind: FlagKind::Bool, default: "false", description: "Update bv to the latest version" },
    GoFlag { name: "update-dry-run", kind: FlagKind::Bool, default: "false", description: "Show what an update would do without installing (use via 'bv upgrade --dry-run')" },
    GoFlag { name: "version", kind: FlagKind::Bool, default: "false", description: "Show version" },
    GoFlag { name: "watch-export", kind: FlagKind::Bool, default: "false", description: "Watch for beads changes and auto-regenerate export (use with --export-pages)" },
    GoFlag { name: "workspace", kind: FlagKind::Str, default: "", description: "Load issues from workspace config file (.bv/workspace.yaml)" },
    GoFlag { name: "yes", kind: FlagKind::Bool, default: "false", description: "Skip confirmation prompts (use with --update)" },
];
pub const ENV_VARS: &[EnvVar] = &[
    EnvVar { name: "BEADS_DB", description: "Path to a beads database file or `.beads` directory. Overrides `BEADS_DIR`; overridden by `--db`.", default: "(unset)" },
    EnvVar { name: "BEADS_DIR", description: "Custom beads directory path. When set, overrides the default `.beads` directory lookup.", default: "`.beads` in cwd" },
    EnvVar { name: "BV_BACKGROUND_MODE", description: "Startup default for the background snapshot worker (`1` on, `0` off). At runtime the TUI promotes itself to the worker after any synchronous reload that takes 1 s or longer; `0` pins synchronous reload and disables that promotion.", default: "sync at startup, auto-promote after a slow reload" },
    EnvVar { name: "BV_BUILD_HYBRID_WASM", description: "Set to `1` to build the hybrid search WASM scorer during `--export-pages` (requires wasm-pack).", default: "(skip)" },
    EnvVar { name: "BV_CACHE_DIR", description: "Base directory for the disk caches (`analysis_cache/` and correlation caches live under it).", default: "`<user cache dir>/bv`" },
    EnvVar { name: "BV_DEBOUNCE_MS", description: "Debounce window (milliseconds) for live reload events in background mode.", default: "`200`" },
    EnvVar { name: "BV_DEBUG", description: "Any value: write `[BV_DEBUG]` diagnostics to stderr.", default: "(off)" },
    EnvVar { name: "BV_FORCE_POLL", description: "Alias for `BV_FORCE_POLLING`.", default: "(auto)" },
    EnvVar { name: "BV_FORCE_POLLING", description: "Force polling-based live reload (useful on NFS/SMB/SSHFS/FUSE or any setup where filesystem events are unreliable) (`1`/`0`).", default: "(auto)" },
    EnvVar { name: "BV_FRESHNESS_STALE_S", description: "Snapshot staleness critical threshold (seconds).", default: "`120`" },
    EnvVar { name: "BV_FRESHNESS_WARN_S", description: "Snapshot staleness warning threshold (seconds).", default: "`30`" },
    EnvVar { name: "BV_HEARTBEAT_INTERVAL_S", description: "Background worker heartbeat interval (seconds).", default: "`5`" },
    EnvVar { name: "BV_INSIGHTS_MAP_LIMIT", description: "Positive entry limit for each `--robot-insights` metric map; zero or invalid values use the default.", default: "`200`" },
    EnvVar { name: "BV_MAX_LINE_SIZE_MB", description: "Max JSONL line size in MB (lines larger than this are skipped with a warning). Applies to the TUI, the background worker, and robot loads.", default: "`10`" },
    EnvVar { name: "BV_METRICS", description: "Set to `0` to disable internal timing metrics collection (`--robot-metrics`).", default: "(enabled)" },
    EnvVar { name: "BV_NO_BG_QUERY", description: "Any non-empty value skips bv's Windows Terminal background-color query; the existing terminal-color fallback remains in use. No effect on other platforms.", default: "(unset)" },
    EnvVar { name: "BV_NO_BROWSER", description: "Any value: never open a browser after exports or deployments.", default: "(unset)" },
    EnvVar { name: "BV_NO_CACHE", description: "Set to `1` to bypass the robot analysis and correlation disk caches (`--no-cache` sets it).", default: "(cache on)" },
    EnvVar { name: "BV_NO_GITIGNORE", description: "Disable automatic ignore-file management for `.bv/` entirely (any non-empty value). See [Automatic `.bv/` ignore handling](#automatic-bv-ignore-handling).", default: "(enabled)" },
    EnvVar { name: "BV_NO_SAVED_CONFIG", description: "Any value: the `--pages` wizard ignores the saved deployment configuration.", default: "(unset)" },
    EnvVar { name: "BV_NO_UPDATE_CHECK", description: "Set to `1` to skip the TUI's startup release check (`updates: {check: false}` in `~/.config/bv/config.yaml` does the same); explicit `--check-update` / `--update` still work.", default: "(check on)" },
    EnvVar { name: "BV_OUTPUT_FORMAT", description: "Default robot output format: `json` or `toon` (overridden by `--format`).", default: "`json`" },
    EnvVar { name: "BV_PHASE2_TIMEOUT_S", description: "Override per-metric Phase 2 timeouts (seconds).", default: "(size-based)" },
    EnvVar { name: "BV_PRETTY_JSON", description: "Set to `1` for indented JSON output.", default: "(compact)" },
    EnvVar { name: "BV_ROBOT", description: "Set to `1` to force robot mode (clean stdout, JSON logs, disk cache on). Every `--robot-*` flag sets it.", default: "(unset)" },
    EnvVar { name: "BV_ROBOT_HISTORY_TIMEOUT_MS", description: "Bound on the git-history prologue of `--robot-triage` in milliseconds; `0` = unbounded.", default: "`10000`" },
    EnvVar { name: "BV_ROBOT_NOT_READY_LABELS", description: "Comma-separated labels marking a bead not-ready; excluded from claimable `--robot-next`/`--robot-triage` top picks (`--robot-not-ready-labels` overrides).", default: "(none)" },
    EnvVar { name: "BV_SEARCH_MODE", description: "Default search mode: `text` or `hybrid` (`--search-mode` overrides).", default: "`text`" },
    EnvVar { name: "BV_SEARCH_PRESET", description: "Default hybrid preset: `default`, `bug-hunting`, `sprint-planning`, `impact-first`, `text-only`; setting one implies hybrid mode.", default: "`default`" },
    EnvVar { name: "BV_SEARCH_WEIGHTS", description: "JSON weight map for hybrid search; overrides the preset.", default: "(preset)" },
    EnvVar { name: "BV_SEMANTIC_DIM", description: "Embedding dimension for the hashed search index.", default: "`384`" },
    EnvVar { name: "BV_SEMANTIC_EMBEDDER", description: "Embedding provider for `bvr --search` and TUI search. Only `hash` (FNV-1a keyword feature hashing) is implemented; `python-sentence-transformers` and `openai` are reserved names that fail with \"not implemented\".", default: "`hash`" },
    EnvVar { name: "BV_SEMANTIC_MODEL", description: "Model name for a future non-hash provider; ignored by `hash`.", default: "(empty)" },
    EnvVar { name: "BV_SKIP_PHASE2", description: "Skip Phase 2 graph metrics (centrality, cycles, critical path) (`1`/`0`).", default: "(disabled)" },
    EnvVar { name: "BV_TEST_MODE", description: "Any value: test harness mode; suppresses browser opening, terminal capability queries, and the background worker's idle GC tuning.", default: "(unset)" },
    EnvVar { name: "BV_THEME", description: "Pin the TUI palette: `light` or `dark` (overridden by `--theme`).", default: "(auto-detect)" },
    EnvVar { name: "BV_TUI_AUTOCLOSE_MS", description: "Quit the TUI automatically after this many milliseconds (for automated tests).", default: "(unset)" },
    EnvVar { name: "BV_UPDATE_USE_TOKEN", description: "Set to `1` to let the update check and `--update` send the ambient `GITHUB_TOKEN` / `GH_TOKEN` to api.github.com (`updates: {use_token: true}` in config.yaml does the same).", default: "(never sent)" },
    EnvVar { name: "BV_WATCHDOG_INTERVAL_S", description: "Background worker watchdog interval (seconds).", default: "`10`" },
    EnvVar { name: "BV_WORKER_LOG_LEVEL", description: "Log level for the background snapshot worker.", default: "(default)" },
    EnvVar { name: "BV_WORKER_METRICS", description: "Truthy value: the background worker records its own metrics.", default: "(off)" },
    EnvVar { name: "BV_WORKER_TRACE", description: "Path to a trace file the background worker appends to.", default: "(off)" },
];
// env vars: 42

#[derive(Debug, Clone, Copy)]
pub struct EnvVar {
    pub name: &'static str,
    pub description: &'static str,
    pub default: &'static str,
}

/// Every flag, in one list, for the doc generator. Go's `fs.VisitAll` walks
/// the live `pflag.FlagSet`; this is the transcribed equivalent.
pub fn all_flags() -> impl Iterator<Item = &'static GoFlag> {
    GO_FLAGS.iter()
}

#[derive(Debug, Clone, Copy)]
pub struct Recipe {
    pub name: &'static str,
    pub description: &'static str,
}

/// Go `pkg/recipe/defaults/recipes.yaml`, in the declaration order
/// `docgen.RenderRecipesTable` iterates.
pub const RECIPES: &[Recipe] = &[
    Recipe {
        name: "default",
        description: "Default view showing all open issues sorted by priority",
    },
    Recipe {
        name: "actionable",
        description: "Issues ready to work on (no open blockers)",
    },
    Recipe {
        name: "recent",
        description: "Issues updated in the last 7 days",
    },
    Recipe {
        name: "blocked",
        description: "Issues waiting on dependencies",
    },
    Recipe {
        name: "high-impact",
        description: "Issues with highest blocking impact (PageRank)",
    },
    Recipe {
        name: "stale",
        description: "Open issues not updated in 30+ days",
    },
    Recipe {
        name: "triage",
        description: "Issues sorted by computed triage score (high impact + unblocking potential)",
    },
    Recipe {
        name: "closed",
        description: "Recently closed issues",
    },
    Recipe {
        name: "release-cut",
        description: "Recently closed items for changelog generation",
    },
    Recipe {
        name: "quick-wins",
        description: "Easy items with no blockers - good for quick progress",
    },
    Recipe {
        name: "bottlenecks",
        description: "High betweenness nodes - potential project bottlenecks",
    },
];

#[derive(Debug, Clone, Copy)]
pub struct SearchPreset {
    pub name: &'static str,
    pub text: f64,
    pub pagerank: f64,
    pub status: f64,
    pub impact: f64,
    pub priority: f64,
    pub recency: f64,
    pub description: &'static str,
}

/// Go `pkg/search/presets.go` — the `presets` map, in `ListPresets` order,
/// with the descriptions `docgen.RenderPresetsTable` attaches.
pub const SEARCH_PRESETS: &[SearchPreset] = &[
    SearchPreset {
        name: "default",
        text: 0.40,
        pagerank: 0.20,
        status: 0.15,
        impact: 0.10,
        priority: 0.10,
        recency: 0.05,
        description: "Balanced general-purpose search (text-led with graph context)",
    },
    SearchPreset {
        name: "bug-hunting",
        text: 0.30,
        pagerank: 0.15,
        status: 0.15,
        impact: 0.15,
        priority: 0.20,
        recency: 0.05,
        description: "Prioritizes open issues with high impact and recency",
    },
    SearchPreset {
        name: "sprint-planning",
        text: 0.30,
        pagerank: 0.20,
        status: 0.25,
        impact: 0.15,
        priority: 0.05,
        recency: 0.05,
        description: "Heavily weights PageRank and blocker impact for sprint grooming",
    },
    SearchPreset {
        name: "impact-first",
        text: 0.25,
        pagerank: 0.30,
        status: 0.10,
        impact: 0.20,
        priority: 0.10,
        recency: 0.05,
        description: "Centrality-first: PageRank and graph impact dominate text matches",
    },
    SearchPreset {
        name: "text-only",
        text: 1.00,
        pagerank: 0.00,
        status: 0.00,
        impact: 0.00,
        priority: 0.00,
        recency: 0.00,
        description: "Hashed keyword similarity with zero graph metric weighting",
    },
];

#[derive(Debug, Clone, Copy)]
pub struct KeyBinding {
    pub context: &'static str,
    pub key: &'static str,
    pub action: &'static str,
}

/// Go `ui.GetKeyBindingDocs()`. Go prints the context cell only on the first
/// row of each run, so this list is already grouped in display order.
pub fn key_bindings() -> &'static [KeyBinding] {
    KEY_BINDINGS
}

pub const KEY_BINDINGS: &[KeyBinding] = &[
    KeyBinding {
        context: "all",
        key: "j",
        action: "Move down",
    },
    KeyBinding {
        context: "all",
        key: "k",
        action: "Move up",
    },
    KeyBinding {
        context: "all",
        key: "G",
        action: "Go to end",
    },
    KeyBinding {
        context: "list",
        key: "home",
        action: "Go to start",
    },
    KeyBinding {
        context: "board,tree",
        key: "gg",
        action: "Go to start",
    },
    KeyBinding {
        context: "all",
        key: "ctrl+d",
        action: "Page down",
    },
    KeyBinding {
        context: "all",
        key: "ctrl+u",
        action: "Page up",
    },
    KeyBinding {
        context: "all",
        key: "enter",
        action: "Open details",
    },
    KeyBinding {
        context: "all",
        key: "esc",
        action: "Back/close (list: clear filters)",
    },
    KeyBinding {
        context: "all",
        key: "q",
        action: "Quit",
    },
    KeyBinding {
        context: "list,detail",
        key: "a",
        action: "Actionable view",
    },
    KeyBinding {
        context: "list,detail",
        key: "b",
        action: "Board view",
    },
    KeyBinding {
        context: "list,detail",
        key: "g",
        action: "Graph view",
    },
    KeyBinding {
        context: "list,detail",
        key: "h",
        action: "History view",
    },
    KeyBinding {
        context: "list,detail",
        key: "i",
        action: "Insights panel",
    },
    KeyBinding {
        context: "list,detail",
        key: "E",
        action: "Tree view (parent-child hierarchy)",
    },
    KeyBinding {
        context: "list,detail",
        key: "f",
        action: "Flow matrix (cross-label dependencies)",
    },
    KeyBinding {
        context: "list,detail",
        key: "P",
        action: "Sprint dashboard",
    },
    KeyBinding {
        context: "list,detail",
        key: "[",
        action: "Label dashboard",
    },
    KeyBinding {
        context: "list,detail",
        key: "]",
        action: "Attention view",
    },
    KeyBinding {
        context: "list",
        key: "!",
        action: "Alerts panel",
    },
    KeyBinding {
        context: "list",
        key: "w",
        action: "Repo picker (workspace mode)",
    },
    KeyBinding {
        context: "all",
        key: "?",
        action: "Help overlay",
    },
    KeyBinding {
        context: "all",
        key: ";",
        action: "Shortcuts sidebar",
    },
    KeyBinding {
        context: "list,detail",
        key: "p",
        action: "Priority hints",
    },
    KeyBinding {
        context: "list",
        key: "o",
        action: "Open issues only",
    },
    KeyBinding {
        context: "list",
        key: "c",
        action: "Closed issues only",
    },
    KeyBinding {
        context: "list",
        key: "r",
        action: "Ready (unblocked)",
    },
    KeyBinding {
        context: "list",
        key: "l",
        action: "Label picker",
    },
    KeyBinding {
        context: "list",
        key: "/",
        action: "Search/filter",
    },
    KeyBinding {
        context: "list",
        key: "s",
        action: "Cycle sort mode",
    },
    KeyBinding {
        context: "list",
        key: "S",
        action: "Sort by triage score (triage recipe)",
    },
    KeyBinding {
        context: "list,detail",
        key: "t",
        action: "Time travel (custom revision)",
    },
    KeyBinding {
        context: "list,detail",
        key: "T",
        action: "Time travel (HEAD~5)",
    },
    KeyBinding {
        context: "list",
        key: "n",
        action: "Next changed issue (time travel)",
    },
    KeyBinding {
        context: "list",
        key: "N",
        action: "Previous changed issue (time travel)",
    },
    KeyBinding {
        context: "list,detail",
        key: "x",
        action: "Export to markdown",
    },
    KeyBinding {
        context: "all",
        key: "y",
        action: "Copy issue ID",
    },
    KeyBinding {
        context: "detail",
        key: "C",
        action: "Copy full issue",
    },
    KeyBinding {
        context: "detail",
        key: "O",
        action: "Open in $EDITOR",
    },
    KeyBinding {
        context: "list",
        key: "'",
        action: "Recipe picker",
    },
    KeyBinding {
        context: "all",
        key: "U",
        action: "Self-update check",
    },
    KeyBinding {
        context: "list",
        key: "V",
        action: "Cass sessions",
    },
    KeyBinding {
        context: "graph",
        key: "hjkl",
        action: "Navigate graph",
    },
    KeyBinding {
        context: "graph",
        key: "H",
        action: "Scroll left",
    },
    KeyBinding {
        context: "graph",
        key: "L",
        action: "Scroll right",
    },
    KeyBinding {
        context: "graph",
        key: "J/K",
        action: "Scroll graph vertically",
    },
    KeyBinding {
        context: "graph",
        key: "space",
        action: "Expand/collapse dependency paths",
    },
    KeyBinding {
        context: "graph",
        key: "PgUp",
        action: "Previous 10 nodes",
    },
    KeyBinding {
        context: "graph",
        key: "PgDn",
        action: "Next 10 nodes",
    },
    KeyBinding {
        context: "board",
        key: "h",
        action: "Previous column",
    },
    KeyBinding {
        context: "board",
        key: "l",
        action: "Next column",
    },
    KeyBinding {
        context: "board",
        key: "H",
        action: "First column",
    },
    KeyBinding {
        context: "board",
        key: "L",
        action: "Last column",
    },
    KeyBinding {
        context: "board",
        key: "s",
        action: "Cycle swimlane mode",
    },
    KeyBinding {
        context: "board",
        key: "tab",
        action: "Toggle detail",
    },
    KeyBinding {
        context: "tree",
        key: "E",
        action: "Exit tree view",
    },
    KeyBinding {
        context: "board",
        key: "ctrl+j",
        action: "Scroll detail down",
    },
    KeyBinding {
        context: "board",
        key: "ctrl+k",
        action: "Scroll detail up",
    },
    KeyBinding {
        context: "insights",
        key: "h",
        action: "Previous panel",
    },
    KeyBinding {
        context: "insights",
        key: "l",
        action: "Next panel",
    },
    KeyBinding {
        context: "insights",
        key: "tab",
        action: "Next panel",
    },
    KeyBinding {
        context: "insights",
        key: "shift+tab",
        action: "Previous panel",
    },
    KeyBinding {
        context: "insights",
        key: "e",
        action: "Toggle explanations",
    },
    KeyBinding {
        context: "insights",
        key: "x",
        action: "Calculation proof",
    },
    KeyBinding {
        context: "insights",
        key: "m",
        action: "Heatmap toggle",
    },
    KeyBinding {
        context: "history",
        key: "v",
        action: "Toggle git/bead mode",
    },
    KeyBinding {
        context: "history",
        key: "tab",
        action: "Toggle focus",
    },
    KeyBinding {
        context: "history",
        key: "t",
        action: "Toggle timeline pane",
    },
    KeyBinding {
        context: "history",
        key: "f",
        action: "Toggle file tree",
    },
    KeyBinding {
        context: "history",
        key: "J",
        action: "Detail scroll down",
    },
    KeyBinding {
        context: "history",
        key: "K",
        action: "Detail scroll up",
    },
    KeyBinding {
        context: "history",
        key: "o",
        action: "Open in browser",
    },
    KeyBinding {
        context: "attention",
        key: "g",
        action: "Go to top",
    },
    KeyBinding {
        context: "attention",
        key: "enter",
        action: "Label drilldown",
    },
    KeyBinding {
        context: "attention",
        key: "1-9",
        action: "Filter list by rank",
    },
    KeyBinding {
        context: "attention",
        key: "]",
        action: "Close attention view",
    },
    KeyBinding {
        context: "sprint",
        key: "P",
        action: "Close sprint dashboard",
    },
    KeyBinding {
        context: "sprint",
        key: "j",
        action: "Next sprint",
    },
    KeyBinding {
        context: "sprint",
        key: "k",
        action: "Previous sprint",
    },
];
