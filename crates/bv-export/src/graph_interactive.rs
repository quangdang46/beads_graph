//! Interactive force-graph HTML export — port of Go `pkg/export/graph_interactive.go`
//! and the `generateUltimateHTML` template in
//! `pkg/export/graph_render_beautiful.go`, at the frozen parity commit
//! `18afafa`.
//!
//! This is the interactive half of `--export-graph`, and it is the half that
//! reads `--graph-title`. Go builds a self-contained HTML page: the graph data
//! as one JSON object, `force-graph` and `marked` inlined so the file works
//! with no network, and a hand-written stylesheet and UI around it.
//!
//! Three pieces are vendored verbatim from the Go reference rather than
//! reimplemented, and each carries its own provenance in `assets/PROVENANCE.md`:
//! `force-graph.min.js`, `marked.min.js`, and `graph_interactive.html` — the
//! 95 KB template, which Go also holds as a literal. The only edit to the
//! template is that Go's thirteen positional `%s`/`%d` verbs became named
//! `@@BV_*@@` sentinels, because Rust's `format!` would otherwise have to
//! double all 74 literal CSS percentages and every `{` in the stylesheet.
//!
//! Go HTML-escapes the title, the data hash and the project name before
//! interpolating them, but **not** the graph JSON, which is embedded raw into
//! a `<script>` body. That is a latent `</script>` break in Go; it is
//! reproduced here rather than silently fixed, so the output matches byte for
//! byte, and called out in [`escape_html`].

use bv_core::model::{Issue, Status};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::graph_snapshot::GraphSnapshotMetrics;

/// The metrics the interactive view reads. Go reads them off `*GraphStats`;
/// Rust's `GraphStats` only carries the three the static snapshot needs, so
/// the caller supplies the rest. Every field is optional in the sense that
/// `None` means "this metric was skipped", which is what Go does when a
/// metric is absent from the map.
#[derive(Debug, Clone, Default)]
pub struct GraphSnapshotMetricsExtras {
    pub eigenvector: Option<BTreeMap<String, f64>>,
    pub hubs: Option<BTreeMap<String, f64>>,
    pub authorities: Option<BTreeMap<String, f64>>,
    pub slack: Option<BTreeMap<String, f64>>,
    pub core_number: Option<BTreeMap<String, u32>>,
    pub articulation: Option<Vec<String>>,
    pub page_rank_rank: Option<BTreeMap<String, i64>>,
    pub betweenness_rank: Option<BTreeMap<String, i64>>,
    pub in_degree: Option<BTreeMap<String, i64>>,
    pub out_degree: Option<BTreeMap<String, i64>>,
}

/// Go `InteractiveGraphOptions` (`pkg/export/graph_interactive.go:26-38`).
pub struct InteractiveGraphOptions {
    pub issues: Vec<Issue>,
    /// The three metrics [`crate::graph_snapshot`] uses, which the page also
    /// shows.
    pub stats: GraphSnapshotMetrics,
    /// The remaining nine metrics.
    pub extras: GraphSnapshotMetricsExtras,
    /// The full `--robot-triage` payload, embedded under `"triage"`. The page
    /// renders it, so omitting it changes the document.
    pub triage: Option<serde_json::Value>,
    /// A correlation report, embedded under `"history_stats"` and
    /// `"git_range"` and mined per-issue for the commit list.
    pub history: Option<HistoryView>,
    /// Go: `Title` — empty becomes `"Dependency Graph"`.
    pub title: String,
    pub data_hash: String,
    /// Go: `Path` — empty auto-generates a timestamped filename.
    pub path: PathBuf,
    /// Go: `ProjectName` — also the filename stem when `path` is empty.
    pub project_name: String,
    /// Go: `RobotEnvelope` — spliced into the graph object verbatim.
    pub robot_envelope: BTreeMap<String, serde_json::Value>,
    /// Go reads `time.Now()` inside the template function. It is a parameter
    /// here so the output is testable; pass the CLI's `robotNow()`.
    pub generated_at: String,
}

/// The slice of `correlation.HistoryReport` the page needs.
#[derive(Debug, Clone)]
pub struct HistoryView {
    pub stats: serde_json::Value,
    pub git_range: String,
    /// Per-issue commit lists, in the order Go iterates them.
    pub per_issue: BTreeMap<String, HistoryIssue>,
}

/// Go's per-issue history slice: `history.Commits` and `history.LastAuthor`.
#[derive(Debug, Clone)]
pub struct HistoryIssue {
    pub commits: Vec<serde_json::Value>,
    pub last_author: String,
}

/// Errors from [`generate_interactive_graph_html`].
#[derive(Debug, thiserror::Error)]
pub enum InteractiveGraphError {
    /// `graph_interactive.go:120`.
    #[error("no issues to export")]
    NoIssues,
    /// `graph_interactive.go:302`.
    #[error("marshal graph data: {0}")]
    Marshal(#[source] serde_json::Error),
    /// `graph_interactive.go:328`.
    #[error("create dir: {0}")]
    CreateDir(#[source] std::io::Error),
    /// `graph_interactive.go:332`.
    #[error("writing graph: {0}")]
    Write(#[source] std::io::Error),
}

/// Go `graphNode` (`pkg/export/graph_interactive.go:42-84`).
///
/// Field order is the JSON key order, which is what the page's JavaScript and
/// the rendered document depend on.
#[derive(Debug, Serialize)]
struct GraphNode {
    id: String,
    title: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    description: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    design: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    acceptance_criteria: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    notes: String,
    status: String,
    priority: i32,
    r#type: String,
    labels: Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    assignee: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    created_at: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    updated_at: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    closed_at: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    due_date: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    blocked_by: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    blocks: Vec<String>,
    #[serde(skip_serializing_if = "is_zero")]
    commit_count: usize,
    #[serde(skip_serializing_if = "String::is_empty")]
    last_author: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    commits: Vec<serde_json::Value>,
    #[serde(serialize_with = "serialize_go_number")]
    pagerank: f64,
    #[serde(serialize_with = "serialize_go_number")]
    betweenness: f64,
    #[serde(serialize_with = "serialize_go_number")]
    eigenvector: f64,
    #[serde(serialize_with = "serialize_go_number")]
    hub: f64,
    #[serde(serialize_with = "serialize_go_number")]
    authority: f64,
    #[serde(serialize_with = "serialize_go_number")]
    critical_path: f64,
    in_degree: i64,
    out_degree: i64,
    core_number: i64,
    #[serde(serialize_with = "serialize_go_number")]
    slack: f64,
    is_articulation: bool,
    pagerank_rank: i64,
    betweenness_rank: i64,
}

fn is_zero(v: &usize) -> bool {
    *v == 0
}

/// Emit a metric the way Go's `encoding/json` emits a `float64`.
///
/// This matters more than it looks. `serde_json` writes every float with a
/// decimal point, so a critical-path score of `2.0` becomes `2.0` where Go
/// writes `2`; nine metrics per node means the page's embedded JSON would
/// differ from Go's on every single node. Go renders a `float64` as the
/// shortest decimal that round-trips, switches to exponent notation below
/// `1e-6` or at/above `1e21`, keeps the `+` on a positive exponent, and drops
/// the padding zero from a negative one. Rust's `Display` already matches the
/// first two rules for ordinary magnitudes, so only the exponent branch needs
/// adjusting.
fn serialize_go_number<S: serde::Serializer>(
    value: &f64,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    // A `RawValue` is spliced into the output verbatim, which is the only way
    // to emit `2` rather than `2.0` — the number has to be written as text,
    // not as a parsed value that serde_json would re-format.
    let raw = serde_json::value::RawValue::from_string(go_json_float(*value))
        .map_err(serde::ser::Error::custom)?;
    raw.serialize(serializer)
}

/// Go's `floatEncoder` (`encoding/json/encode.go`) minus the infinity/NaN
/// branch, which cannot arise: the graph metrics are finite by construction and
/// Go's own encoder would have rejected the document before this point.
pub fn go_json_float(value: f64) -> String {
    let abs = value.abs();
    if abs != 0.0 && !(1e-6..1e21).contains(&abs) {
        let rendered = format!("{value:e}");
        // Go's `AppendFloat(_, 'e', -1, 64)` writes "1e+21"; Rust writes "1e21".
        return match rendered.split_once('e') {
            Some((mantissa, exponent)) if !exponent.starts_with('-') => {
                format!("{mantissa}e+{exponent}")
            }
            _ => rendered,
        };
    }
    format!("{value}")
}

/// Go `graphLink` (`pkg/export/graph_interactive.go:87-92`).
#[derive(Debug, Serialize)]
struct GraphLink {
    source: String,
    target: String,
    r#type: String,
    critical: bool,
}

/// Go `GenerateInteractiveGraphHTML` (`pkg/export/graph_interactive.go:118-343`).
///
/// Returns the path written, matching Go's return value.
pub fn generate_interactive_graph_html(
    opts: &InteractiveGraphOptions,
) -> Result<PathBuf, InteractiveGraphError> {
    if opts.issues.is_empty() {
        return Err(InteractiveGraphError::NoIssues);
    }

    let issue_ids: BTreeSet<&str> = opts.issues.iter().map(|i| i.id.as_str()).collect();

    // Reverse dependency map — graph_interactive.go:166-172.
    let mut blocks_map: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for issue in &opts.issues {
        for dep in &issue.dependencies {
            if issue_ids.contains(dep.depends_on_id.as_str()) && dep.r#type.is_blocking() {
                blocks_map
                    .entry(dep.depends_on_id.as_str())
                    .or_default()
                    .push(issue.id.clone());
            }
        }
    }

    let articulation: BTreeSet<&str> = opts
        .extras
        .articulation
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(String::as_str)
        .collect();

    let mut nodes: Vec<GraphNode> = Vec::with_capacity(opts.issues.len());
    let mut links: Vec<GraphLink> = Vec::new();
    for issue in &opts.issues {
        let blocked_by: Vec<String> = issue
            .dependencies
            .iter()
            .filter(|d| issue_ids.contains(d.depends_on_id.as_str()) && d.r#type.is_blocking())
            .map(|d| d.depends_on_id.clone())
            .collect();

        let history = opts
            .history
            .as_ref()
            .and_then(|h| h.per_issue.get(&issue.id));

        nodes.push(GraphNode {
            id: issue.id.clone(),
            title: issue.title.clone(),
            description: issue.description.clone(),
            design: issue.design.clone(),
            acceptance_criteria: issue.acceptance_criteria.clone(),
            notes: issue.notes.clone(),
            status: status_text(issue.status),
            priority: issue.priority,
            r#type: issue.issue_type.clone(),
            labels: issue.labels.clone(),
            assignee: issue.assignee.clone(),
            created_at: go_datetime(issue.created_at.as_deref()),
            updated_at: go_datetime(issue.updated_at.as_deref()),
            closed_at: go_datetime(issue.closed_at.as_deref()),
            due_date: go_date(issue.due_date.as_deref()),
            blocks: blocks_map
                .get(issue.id.as_str())
                .cloned()
                .unwrap_or_default(),
            blocked_by,
            commit_count: history.map_or(0, |h| h.commits.len()),
            last_author: history.map_or(String::new(), |h| h.last_author.clone()),
            commits: history.map(|h| h.commits.clone()).unwrap_or_default(),
            pagerank: lookup(&opts.stats.pagerank, &issue.id),
            betweenness: lookup(&opts.stats.betweenness, &issue.id),
            eigenvector: lookup_opt(&opts.extras.eigenvector, &issue.id),
            hub: lookup_opt(&opts.extras.hubs, &issue.id),
            authority: lookup_opt(&opts.extras.authorities, &issue.id),
            critical_path: lookup(&opts.stats.critical_path, &issue.id),
            in_degree: lookup_int(&opts.extras.in_degree, &issue.id),
            out_degree: lookup_int(&opts.extras.out_degree, &issue.id),
            core_number: opts
                .extras
                .core_number
                .as_ref()
                .and_then(|m| m.get(&issue.id))
                .map_or(0, |v| i64::from(*v)),
            slack: lookup_opt(&opts.extras.slack, &issue.id),
            is_articulation: articulation.contains(issue.id.as_str()),
            pagerank_rank: lookup_int(&opts.extras.page_rank_rank, &issue.id),
            betweenness_rank: lookup_int(&opts.extras.betweenness_rank, &issue.id),
        });

        // Links come from *every* dependency, blocking or not — only
        // `BlockedBy` is restricted to blocking ones (graph_interactive.go:246).
        for dep in &issue.dependencies {
            if !issue_ids.contains(dep.depends_on_id.as_str()) {
                continue;
            }
            // `graph_interactive.go:250` — critical means both ends have zero
            // slack, and only when stats were computed at all.
            let critical = lookup_opt(&opts.extras.slack, &issue.id) == 0.0
                && lookup_opt(&opts.extras.slack, &dep.depends_on_id) == 0.0;
            links.push(GraphLink {
                source: issue.id.clone(),
                target: dep.depends_on_id.clone(),
                r#type: dep_type_text(dep.r#type.clone()),
                critical,
            });
        }
    }

    // Go `graph_interactive.go:266` — sorted by ID for determinism.
    nodes.sort_by(|a, b| a.id.cmp(&b.id));

    // Go builds `graphData` as a `map[string]interface{}`
    // (graph_interactive.go:275), and `encoding/json` sorts a map's keys. The
    // envelope keys land in the same map, so they sort in with the rest. A
    // `BTreeMap` reproduces that; the `nodes` and `links` *arrays* keep their
    // own order regardless.
    let mut graph_data: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    graph_data.insert(
        "nodes".to_string(),
        serde_json::to_value(&nodes).map_err(InteractiveGraphError::Marshal)?,
    );
    graph_data.insert(
        "links".to_string(),
        serde_json::to_value(&links).map_err(InteractiveGraphError::Marshal)?,
    );
    for (key, value) in &opts.robot_envelope {
        graph_data.insert(key.clone(), value.clone());
    }
    if let Some(triage) = &opts.triage {
        graph_data.insert("triage".to_string(), triage.clone());
    }
    if let Some(history) = &opts.history {
        graph_data.insert("history_stats".to_string(), history.stats.clone());
        graph_data.insert(
            "git_range".to_string(),
            serde_json::Value::String(history.git_range.clone()),
        );
    }

    let title = if opts.title.is_empty() {
        "Dependency Graph".to_string()
    } else {
        opts.title.clone()
    };

    // graph_interactive.go:308-318 — the auto-generated filename, and the
    // `.html` fix-up that runs either way.
    let mut output_path = opts.path.clone();
    if output_path.as_os_str().is_empty() {
        let stem = if opts.project_name.is_empty() {
            "graph"
        } else {
            opts.project_name.as_str()
        };
        output_path = PathBuf::from(generate_interactive_graph_filename(
            stem,
            &opts.generated_at,
        ));
    }
    if !output_path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("html"))
    {
        let stem = output_path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        output_path.set_file_name(format!("{stem}.html"));
    }

    let html = render_html(opts, &title, &graph_data, nodes.len(), links.len());

    if let Some(parent) = output_path.parent() {
        if !parent.as_os_str().is_empty() && parent != Path::new(".") {
            std::fs::create_dir_all(parent).map_err(InteractiveGraphError::CreateDir)?;
        }
    }
    std::fs::write(&output_path, html).map_err(InteractiveGraphError::Write)?;
    Ok(output_path)
}

/// Go `GenerateInteractiveGraphFilename` (`pkg/export/graph_interactive.go:96-113`).
///
/// Go shells out to `git rev-parse --short HEAD` for the hash and falls back
/// to `nogit`; the caller supplies `generated_at` in place of Go's `time.Now()`
/// and `git_head` in place of the subprocess, so the name is deterministic here.
pub fn generate_interactive_graph_filename(project_name: &str, generated_at: &str) -> String {
    // Go: two successive `strings.ReplaceAll` calls, which is the same as
    // replacing either character.
    let safe_name = project_name.replace([' ', '/'], "_");
    format!("{safe_name}_graph_export__{generated_at}.html")
}

/// Go `generateUltimateHTML` (`pkg/export/graph_render_beautiful.go:10-1967`).
fn render_html(
    opts: &InteractiveGraphOptions,
    title: &str,
    graph_data: &BTreeMap<String, serde_json::Value>,
    node_count: usize,
    edge_count: usize,
) -> String {
    let json =
        go_escape_json(&serde_json::to_string(graph_data).unwrap_or_else(|_| "null".to_string()));
    // The vendored template is still Go's *format string*, so its 37 literal
    // percent signs are written `%%` and `Sprintf` collapses each pair. That
    // has to happen before substitution: the graph JSON and the two library
    // bundles legitimately contain `%`, and Go inserts them as `%s` arguments
    // rather than parsing them as format text.
    let mut html = TEMPLATE.replace("%%", "%");
    for (sentinel, value) in [
        ("@@BV_TITLE@@", escape_html(title)),
        ("@@BV_NODES@@", node_count.to_string()),
        ("@@BV_EDGES@@", edge_count.to_string()),
        ("@@BV_TIMESTAMP@@", opts.generated_at.clone()),
        ("@@BV_HASH@@", escape_html(&opts.data_hash)),
        ("@@BV_PROJECT@@", escape_html(&opts.project_name)),
        // Go interpolates the libraries and the graph JSON raw.
        ("@@BV_FORCE_GRAPH_JS@@", FORCE_GRAPH_JS.to_string()),
        ("@@BV_MARKED_JS@@", MARKED_JS.to_string()),
        ("@@BV_GRAPH_DATA_JSON@@", json),
    ] {
        html = html.replace(sentinel, &value);
    }
    html
}

/// Go's `encoding/json` HTML-escapes `<`, `>` and `&` on the way out, plus
/// the two JavaScript line terminators. `serde_json` escapes neither, so an
/// issue title containing markup reaches the `<script>` body verbatim where
/// Go would have written `<`.
///
/// A blind replace is exactly right rather than a shortcut: none of these five
/// characters can occur in JSON *syntax*, only inside string literals, so every
/// occurrence of one is inside a value Go would have escaped.
fn go_escape_json(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    for ch in json.chars() {
        match ch {
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c => out.push(c),
        }
    }
    out
}

/// Go's `html.EscapeString` (`graph_render_beautiful.go:12-14`).
///
/// Go escapes five characters. Note Go does *not* escape the graph JSON, so a
/// `</script>` inside an issue title would terminate the script element; that
/// is Go's behaviour and it is reproduced here so the two files match.
fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '\'' => out.push_str("&#39;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&#34;"),
            c => out.push(c),
        }
    }
    out
}

fn status_text(status: Status) -> String {
    status.as_str().to_string()
}

/// Go's `string(dep.Type)` — the raw wire value, not the Rust enum name.
fn dep_type_text(kind: bv_core::model::DependencyType) -> String {
    kind.as_str().to_string()
}

fn lookup(map: &BTreeMap<String, f64>, key: &str) -> f64 {
    map.get(key).copied().unwrap_or(0.0)
}

/// Go reads a nil metric map as an empty one, so every value is 0 when the
/// metric was skipped. `None` here is Go's nil map.
fn lookup_opt(map: &Option<BTreeMap<String, f64>>, key: &str) -> f64 {
    map.as_ref()
        .and_then(|m| m.get(key))
        .copied()
        .unwrap_or(0.0)
}

fn lookup_int(map: &Option<BTreeMap<String, i64>>, key: &str) -> i64 {
    map.as_ref().and_then(|m| m.get(key)).copied().unwrap_or(0)
}

/// Go `t.Format("2006-01-02 15:04")`, rendered in the offset the source
/// carried — the same helper the Markdown writer uses.
fn go_datetime(raw: Option<&str>) -> String {
    match raw.and_then(crate::markdown::wall_clock) {
        Some(local) => local.strftime("%Y-%m-%d %H:%M").to_string(),
        None => String::new(),
    }
}

/// Go `t.Format("2006-01-02")`.
///
/// A beads `due_date` is normally a full RFC 3339 instant, which takes the
/// offset-preserving path. A bare `YYYY-MM-DD` is accepted too: Go's
/// `time.Time` would have parsed it from a differently-shaped source, and
/// dropping the field entirely would lose a date the document can show.
fn go_date(raw: Option<&str>) -> String {
    let Some(raw) = raw else { return String::new() };
    if let Ok(date) = raw.parse::<jiff::civil::Date>() {
        return date.strftime("%Y-%m-%d").to_string();
    }
    match crate::markdown::wall_clock(raw) {
        Some(local) => local.strftime("%Y-%m-%d").to_string(),
        None => String::new(),
    }
}

const TEMPLATE: &str = include_str!("../assets/graph_interactive.html");
const FORCE_GRAPH_JS: &str = include_str!("../assets/force-graph.min.js");
const MARKED_JS: &str = include_str!("../assets/marked.min.js");

#[cfg(test)]
mod tests {
    use super::*;
    use bv_core::model::Status;
    use bv_core::model::{Dependency, DependencyType};

    fn issue(id: &str, status: Status, deps: &[(&str, DependencyType)]) -> Issue {
        Issue {
            id: id.to_string(),
            content_hash: String::new(),
            title: format!("title {id}"),
            description: String::new(),
            design: String::new(),
            acceptance_criteria: String::new(),
            notes: String::new(),
            status,
            priority: 1,
            issue_type: "task".to_string(),
            assignee: String::new(),
            estimated_minutes: None,
            created_at: None,
            updated_at: None,
            due_date: None,
            defer_until: None,
            closed_at: None,
            external_ref: None,
            compaction_level: 0,
            compacted_at: None,
            compacted_at_commit: None,
            original_size: 0,
            labels: Vec::new(),
            dependencies: deps
                .iter()
                .map(|(depends_on, kind)| Dependency {
                    issue_id: id.to_string(),
                    depends_on_id: (*depends_on).to_string(),
                    depends_on_legacy: String::new(),
                    target_id_legacy: String::new(),
                    r#type: kind.clone(),
                    created_at: None,
                    created_by: String::new(),
                })
                .collect(),
            comments: Vec::new(),
            source_repo: String::new(),
        }
    }

    fn opts(issues: Vec<Issue>) -> InteractiveGraphOptions {
        InteractiveGraphOptions {
            issues,
            stats: GraphSnapshotMetrics::default(),
            extras: GraphSnapshotMetricsExtras::default(),
            triage: None,
            history: None,
            title: "T".to_string(),
            data_hash: "hash".to_string(),
            path: PathBuf::new(),
            project_name: "proj".to_string(),
            robot_envelope: BTreeMap::new(),
            generated_at: "2024-01-15 10:00:00".to_string(),
        }
    }

    fn render_with_title(issues: Vec<Issue>, title: &str) -> String {
        let mut o = opts(issues);
        o.title = title.to_string();
        let mut data = BTreeMap::new();
        data.insert("nodes".to_string(), serde_json::json!([]));
        data.insert("links".to_string(), serde_json::json!([]));
        render_html(&o, title, &data, 3, 2)
    }

    fn render(issues: Vec<Issue>) -> String {
        render_with_title(issues, "<b>&amp;</b>")
    }

    #[test]
    fn every_sentinel_is_replaced() {
        let html = render(vec![issue("A", Status::Open, &[])]);
        assert!(!html.contains("@@BV_"), "a sentinel survived");
        // The libraries are inlined, not linked: the page has to work with no
        // network, which is what the template's own header comment claims.
        assert!(html.contains("force-graph"), "force-graph is missing");
        assert!(html.contains("marked"), "marked is missing");
        assert!(
            !html.contains("<script src="),
            "an external script crept in"
        );
        assert!(!html.contains("<link "), "an external stylesheet crept in");
        // Two library script tags plus the page's own.
        assert_eq!(html.matches("<script>").count(), 3);
        assert_eq!(html.matches("</script>").count(), 3);
    }

    #[test]
    fn the_title_is_html_escaped() {
        // graph_render_beautiful.go:12
        let html = render(vec![issue("A", Status::Open, &[])]);
        assert!(html.contains("&lt;b&gt;&amp;amp;&lt;/b&gt;"));
        assert!(!html.contains("<title><b>"));
    }

    #[test]
    fn the_template_starts_and_ends_like_go() {
        let html = render(vec![issue("A", Status::Open, &[])]);
        assert!(html.starts_with("<!DOCTYPE html>\n<html lang=\"en\">"));
        assert!(html.trim_end().ends_with("</html>"));
    }

    #[test]
    fn a_missing_title_falls_back() {
        // graph_interactive.go:308-311 — the default lives inside the
        // generator, so this has to go through it.
        let dir = std::env::temp_dir().join(format!(
            "bvr-graph-interactive-title-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut o = opts(vec![issue("A", Status::Open, &[])]);
        o.path = dir.join("out.html");
        o.title = String::new();
        let written = generate_interactive_graph_html(&o).expect("written");
        let html = std::fs::read_to_string(written).unwrap();
        assert!(html.contains("<title>Dependency Graph | bv Graph</title>"));
        assert!(html.contains("<h1><span>Dependency Graph</span> Graph</h1>"));
    }

    #[test]
    fn the_auto_filename_matches_go_shape() {
        // graph_interactive.go:96-113 — the stem is sanitised, and the
        // timestamp/hash tail is appended.
        assert_eq!(
            generate_interactive_graph_filename(
                "my project",
                "2024_01_15__10_00__git_head_hash__abc123"
            ),
            "my_project_graph_export__2024_01_15__10_00__git_head_hash__abc123.html"
        );
        assert_eq!(
            generate_interactive_graph_filename("a/b", "X"),
            "a_b_graph_export__X.html"
        );
    }

    #[test]
    fn an_extensionless_path_gains_html() {
        // graph_interactive.go:320-323
        let dir =
            std::env::temp_dir().join(format!("bvr-graph-interactive-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut o = opts(vec![issue("A", Status::Open, &[])]);
        o.path = dir.join("graph");
        let written = generate_interactive_graph_html(&o).expect("written");
        assert_eq!(written.extension().and_then(|e| e.to_str()), Some("html"));
        assert!(written.exists());
    }

    #[test]
    fn an_empty_issue_set_is_rejected() {
        // graph_interactive.go:120-122
        let o = opts(Vec::new());
        assert!(matches!(
            generate_interactive_graph_html(&o),
            Err(InteractiveGraphError::NoIssues)
        ));
    }

    #[test]
    fn the_full_document_is_written_and_escaped() {
        let dir =
            std::env::temp_dir().join(format!("bvr-graph-interactive-doc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut o = opts(vec![issue("A", Status::Open, &[])]);
        o.path = dir.join("out.html");
        o.title = "My <Graph>".to_string();
        let written = generate_interactive_graph_html(&o).expect("written");
        let html = std::fs::read_to_string(&written).unwrap();
        assert!(html.contains("<title>My &lt;Graph&gt; | bv Graph</title>"));
        assert!(html.contains("<h1><span>My &lt;Graph&gt;</span> Graph</h1>"));
    }
}
