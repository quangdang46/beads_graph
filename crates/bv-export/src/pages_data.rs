//! The `--export-pages` runtime data files: `data/meta.json`,
//! `data/triage.json`, `data/project_health.json` and `data/graph_layout.json`.
//!
//! Every file here is written by Go's `(*SQLiteExporter).writeRobotOutputs` and
//! `writeGraphLayout` (pkg/export/sqlite_export.go), both of which funnel
//! through `writeRobotJSON` and `writeJSON`. Those two helpers own three things
//! this module has to reproduce exactly:
//!
//! 1. **The envelope overlay.** `writeRobotJSON` marshals the payload, re-reads
//!    it as `map[string]json.RawMessage`, and overwrites every key the
//!    `RobotEnvelope` carries — unconditionally, except for `generated_at`,
//!    which the payload keeps if it already has one. That is why `meta.json`
//!    shows `"version": "v0.25.0"` although `ExportMeta.Version` is `"1.0.0"`,
//!    and why `project_health.json` inherits the envelope's second-precision
//!    stamp while `meta.json` keeps its own nanosecond one.
//! 2. **Top-level key sorting.** `writeJSON` receives a Go *map*, and
//!    `encoding/json` sorts map keys. Nested values are `json.RawMessage`,
//!    which the encoder re-emits verbatim — so nested objects keep their own
//!    order (Go struct declaration order, or sorted for a nested Go map) and
//!    only the top level is alphabetised. This crate's `serde_json` builds with
//!    `preserve_order`, so `serde_json::Map` is an `IndexMap` and would keep
//!    insertion order; [`write_robot_json`] sorts the top level itself.
//! 3. **Go number and string rendering.** `encoding/json` writes a whole
//!    `float64` without a trailing `.0` (`[200, -40]`, not `[200.0, -40.0]`)
//!    and escapes `<`, `>` and `&` as the `\u003c` / `\u003e` / `\u0026`
//!    sequences (Go's `SetEscapeHTML` defaults to on).
//!    `serde_json` does neither, so neither does a plain
//!    `serde_json::to_string_pretty`.
//!
//! `beads.sqlite3.config.json` is NOT here either: it hashes the published
//! database, so it belongs to the SQLite exporter's `chunkIfNeeded` port in
//! [`crate::sqlite_export`], and lives there.
//!
//! `data/history.json` is likewise not here. Go writes that one with
//! `json.MarshalIndent` + `os.WriteFile` (main.go:3186), so it has no trailing
//! newline, and the CLI layer already emits it byte-exactly.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Map, Value};

// ---------------------------------------------------------------------------
// Go JSON rendering
// ---------------------------------------------------------------------------

/// Go `encoding/json` string escaping with `SetEscapeHTML` on (the default for
/// `json.NewEncoder`): `<`, `>` and `&` become `\u003c`, `\u003e` and
/// `\u0026`.
///
/// Matches `go_escape_string` in `crates/bv/src/main.rs`, which is the copy the
/// robot-output paths were verified against; it is repeated here rather than
/// lifted into a shared crate because the CLI's copy is private and this crate
/// must not depend on the binary.
fn go_escape_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Go `strconv.AppendFloat(f, 'f'/'e', -1, 64)`: the shortest decimal that
/// round-trips, in `'f'` form normally and `'e'` form when the magnitude is
/// below `1e-6` or at/above `1e21` — with Go's two-digit exponent padding
/// trimmed (`e-09` → `e-9`).
fn go_format_f64(f: f64) -> String {
    if f.is_nan() || f.is_infinite() {
        // Go's encoder rejects these outright; no export path produces them.
        return "null".into();
    }
    let abs = f.abs();
    if abs != 0.0 && !(1e-6..1e21).contains(&abs) {
        let s = format!("{:e}", f); // "1.5e21" | "1e-7" | "-2.5e-8"
        if let Some(pos) = s.find('e') {
            let (mantissa, exp) = s.split_at(pos);
            let exp = &exp[1..];
            let (sign, digits) = match exp.strip_prefix('-') {
                Some(rest) => ("-", rest),
                None => ("+", exp),
            };
            let digits = if digits.len() == 2 && digits.starts_with('0') {
                &digits[1..]
            } else {
                digits
            };
            return format!("{mantissa}e{sign}{digits}");
        }
        return s;
    }
    format!("{f}")
}

fn write_number(n: &serde_json::Number, out: &mut String) {
    // A float that happens to be whole still has to render without the trailing
    // `.0` `serde_json` would add: Go's `float64` encoder uses the same
    // shortest-round-trip rule as `strconv`, which prints `21` for 21.0.
    match (n.as_i64(), n.as_u64()) {
        (Some(_), _) | (_, Some(_)) => out.push_str(&n.to_string()),
        (None, None) => out.push_str(&go_format_f64(n.as_f64().unwrap_or(0.0))),
    }
}

/// `json.Encoder` with `SetIndent("", "  ")`.
///
/// `sort_top_level` reproduces Go sorting a `map[string]json.RawMessage`; nested
/// objects are left in the order the Rust serializer produced, because Go's
/// `RawMessage` payload is re-emitted byte for byte and only Go *maps* sort.
fn encode_pretty(v: &Value, out: &mut String, indent: usize, sort_top_level: bool) {
    let pad = |out: &mut String, n: usize| {
        for _ in 0..n {
            out.push(' ');
        }
    };
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => write_number(n, out),
        Value::String(s) => out.push_str(&go_escape_string(s)),
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(",\n");
                }
                pad(out, indent + 2);
                encode_pretty(item, out, indent + 2, false);
            }
            out.push('\n');
            pad(out, indent);
            out.push(']');
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            let mut keys: Vec<&String> = map.keys().collect();
            if sort_top_level {
                keys.sort_unstable();
            }
            out.push_str("{\n");
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push_str(",\n");
                }
                pad(out, indent + 2);
                out.push_str(&go_escape_string(k));
                out.push_str(": ");
                encode_pretty(&map[*k], out, indent + 2, false);
            }
            out.push('\n');
            pad(out, indent);
            out.push('}');
        }
    }
}

/// Go `(*SQLiteExporter).writeRobotJSON` (pkg/export/sqlite_export.go:646-668)
/// followed by `writeJSON` (sqlite_export.go:845-858).
///
/// `envelope` is the already-serialised `RobotEnvelope`; every one of its keys
/// replaces the payload's, except `generated_at`, which a payload that already
/// carries one keeps — a payload may record a nanosecond-precision instant that
/// the shared analysis timestamp would flatten.
pub fn write_robot_json(
    path: &Path,
    payload: &Value,
    envelope: &Map<String, Value>,
) -> std::io::Result<()> {
    let mut fields: Map<String, Value> = match payload {
        Value::Object(m) => m.clone(),
        other => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("export payload is not a JSON object: {other}"),
            ))
        }
    };
    for (key, value) in envelope {
        if key == "generated_at" && fields.contains_key(key) {
            continue;
        }
        fields.insert(key.clone(), value.clone());
    }
    let mut out = String::new();
    encode_pretty(&Value::Object(fields), &mut out, 0, true);
    // `json.Encoder.Encode` terminates every value with a newline.
    out.push('\n');
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, out)
}

// ---------------------------------------------------------------------------
// data/meta.json
// ---------------------------------------------------------------------------

/// Go `ExportMeta` (pkg/export/sqlite_types.go:63-71) as it reaches
/// `writeRobotOutputs` (sqlite_export.go:631-639).
///
/// `DataHash` is declared `omitempty` and is never assigned on the export path,
/// so the file's `data_hash` is entirely the envelope's. `Version` is likewise
/// always overwritten by the envelope, so this payload's own `"1.0.0"` never
/// reaches the wire.
pub fn export_meta_payload(
    generated_at: &str,
    issue_count: usize,
    dependency_count: usize,
    git_commit: &str,
    title: &str,
) -> Value {
    let mut m = Map::new();
    m.insert("version".into(), Value::from("1.0.0"));
    m.insert("generated_at".into(), Value::from(generated_at));
    if !git_commit.is_empty() {
        m.insert("git_commit".into(), Value::from(git_commit));
    }
    m.insert("issue_count".into(), Value::from(issue_count));
    m.insert("dependency_count".into(), Value::from(dependency_count));
    if !title.is_empty() {
        m.insert("title".into(), Value::from(title));
    }
    Value::Object(m)
}

// ---------------------------------------------------------------------------
// data/graph_layout.json
// ---------------------------------------------------------------------------

/// One blocking dependency edge, in the order the exporter collected it
/// (main.go:3125-3138 walks the issues, then each issue's dependency list).
#[derive(Debug, Clone)]
pub struct BlockingDep {
    pub issue_id: String,
    pub depends_on_id: String,
    pub dep_type: bv_core::model::DependencyType,
}

/// Everything `writeGraphLayout` (sqlite_export.go:1059-1194) reads off
/// `e.Stats`. The layout is NOT a physics simulation: it is a deterministic
/// layered DAG embed computed at export time, so the browser can draw the graph
/// before force-graph finishes.
pub struct GraphLayoutInput<'a> {
    /// The exported issues, in loader order.
    pub issue_ids: &'a [String],
    pub deps: &'a [BlockingDep],
    /// `e.Stats.TopologicalOrder` — dependencies first, `None` on a cyclic
    /// graph, which is what selects the BFS fallback below.
    pub topological_order: Option<&'a [String]>,
    pub page_rank: Option<&'a BTreeMap<String, f64>>,
    pub betweenness: Option<&'a BTreeMap<String, f64>>,
    pub cycles: Option<&'a Vec<Vec<String>>>,
    /// `time.Now().UTC().Format(time.RFC3339)` — second precision, because Go
    /// formats this one by hand rather than letting `encoding/json` render a
    /// `time.Time`.
    pub generated_at: String,
}

/// Go `writeGraphLayout` (pkg/export/sqlite_export.go:1059-1194).
pub fn build_graph_layout(input: &GraphLayoutInput<'_>) -> Value {
    let GraphLayoutInput {
        issue_ids,
        deps,
        topological_order,
        page_rank,
        betweenness,
        cycles,
        generated_at,
    } = input;
    let issue_ids: &[String] = issue_ids;
    let deps: &[BlockingDep] = deps;
    let topological_order: Option<&[String]> = *topological_order;
    let cycles: Option<&Vec<Vec<String>>> = *cycles;

    let mut blocked_by: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut blocks: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for dep in deps {
        blocked_by
            .entry(dep.issue_id.as_str())
            .or_default()
            .push(dep.depends_on_id.as_str());
        blocks
            .entry(dep.depends_on_id.as_str())
            .or_default()
            .push(dep.issue_id.as_str());
    }
    let len_of = |m: &BTreeMap<&str, Vec<&str>>, k: &str| m.get(k).map_or(0, Vec::len);

    // --- depth ---
    let mut depth: BTreeMap<&str, i64> = BTreeMap::new();
    match topological_order {
        // Longest-path along the topological order (dependencies first). The
        // relaxation is order-independent for a valid topological order, so the
        // traversal order does not change the result.
        Some(order) if !order.is_empty() => {
            for id in order {
                let d = *depth.get(id.as_str()).unwrap_or(&0);
                depth.entry(id.as_str()).or_insert(0);
                for child in blocks.get(id.as_str()).into_iter().flatten() {
                    if depth.get(child).copied().unwrap_or(0) < d + 1 {
                        depth.insert(child, d + 1);
                    }
                }
            }
        }
        // No usable order (Go takes this branch when the graph has cycles):
        // shortest distance from the in-degree-0 roots. Revising a visited node
        // would make the answer depend on edge order, so it is not revisited —
        // Go's comment, and its behaviour.
        _ => {
            let mut queue: Vec<&str> = Vec::new();
            for id in issue_ids {
                if len_of(&blocked_by, id) == 0 {
                    depth.insert(id.as_str(), 0);
                    queue.push(id.as_str());
                }
            }
            let mut head = 0;
            while head < queue.len() {
                let current = queue[head];
                head += 1;
                let d = depth.get(current).copied().unwrap_or(0);
                for child in blocks.get(current).into_iter().flatten() {
                    if !depth.contains_key(child) {
                        depth.insert(child, d + 1);
                        queue.push(child);
                    }
                }
            }
        }
    }
    for id in issue_ids {
        depth.entry(id.as_str()).or_insert(0);
    }

    // --- positions ---
    // Only real exported issues are drawable; an issue that exists solely as a
    // dependency endpoint is kept as graph context but gets no coordinates.
    let mut depth_groups: BTreeMap<i64, Vec<&str>> = BTreeMap::new();
    let mut max_depth = 0i64;
    for id in issue_ids {
        let d = *depth.get(id.as_str()).unwrap_or(&0);
        depth_groups.entry(d).or_default().push(id.as_str());
        if d > max_depth {
            max_depth = d;
        }
    }

    const X_SPACING: f64 = 200.0;
    const Y_SPACING: f64 = 80.0;
    // `positions` and `metrics` are Go maps (`map[string][2]float64` /
    // `map[string][5]float64`, sqlite_export.go:983-984), and `encoding/json`
    // sorts map keys. They are therefore keyed by ISSUE ID, not by the depth
    // order they are computed in — collecting into a `BTreeMap` before the
    // final `Map` is what reproduces that, and this crate's `serde_json` builds
    // with `preserve_order` so an insertion-ordered `Map` would keep the wrong
    // order.
    let mut positions: BTreeMap<String, Value> = BTreeMap::new();
    for d in 0..=max_depth {
        let Some(mut nodes_at_depth) = depth_groups.get(&d).cloned() else {
            continue;
        };
        nodes_at_depth.sort_unstable();
        let count = nodes_at_depth.len();
        // Go: -(count-1) * ySpacing / 2, so the column is centred on y = 0.
        // `count - 1` underflows at count == 0, but an empty group never gets
        // here and the branch above already skipped it.
        let start_y = -((count as f64 - 1.0) * Y_SPACING / 2.0);
        for (i, id) in nodes_at_depth.iter().enumerate() {
            positions.insert(
                (*id).to_string(),
                Value::from(vec![d as f64 * X_SPACING, start_y + i as f64 * Y_SPACING]),
            );
        }
    }
    let positions = sorted_object(positions);

    // --- metrics ---
    let mut cycle_nodes: BTreeMap<&str, bool> = BTreeMap::new();
    for cycle in cycles.iter().flat_map(|c| c.iter()) {
        for id in cycle {
            cycle_nodes.insert(id.as_str(), true);
        }
    }
    let mut metrics: BTreeMap<String, Value> = BTreeMap::new();
    for id in issue_ids {
        let pr = page_rank
            .and_then(|m| m.get(id.as_str()))
            .copied()
            .unwrap_or(0.0);
        let bt = betweenness
            .and_then(|m| m.get(id.as_str()))
            .copied()
            .unwrap_or(0.0);
        let in_cycle = if cycle_nodes.get(id.as_str()).copied().unwrap_or(false) {
            1.0
        } else {
            0.0
        };
        metrics.insert(
            id.clone(),
            // [pagerank, betweenness, blocked_by, blocks, in_cycle]
            Value::from(vec![
                pr,
                bt,
                len_of(&blocked_by, id) as f64,
                len_of(&blocks, id) as f64,
                in_cycle,
            ]),
        );
    }

    let metrics = sorted_object(metrics);

    // --- links ---
    // [DependsOnID, IssueID] — the direction the force layout draws.
    let links: Vec<Value> = deps
        .iter()
        .map(|d| Value::from(vec![d.depends_on_id.clone(), d.issue_id.clone()]))
        .collect();

    // Go `GraphLayout` (sqlite_export.go:981-994). `Version` is overwritten by
    // the envelope; `Cycles` is `omitempty` and an acyclic graph must not emit
    // an empty array for it.
    let mut layout = Map::new();
    layout.insert("positions".into(), positions);
    layout.insert("metrics".into(), metrics);
    layout.insert("links".into(), Value::Array(links.clone()));
    if let Some(cs) = cycles {
        if !cs.is_empty() {
            layout.insert(
                "cycles".into(),
                serde_json::to_value(cs).unwrap_or(Value::Null),
            );
        }
    }
    layout.insert("version".into(), Value::from("1.0.0"));
    layout.insert("generated_at".into(), Value::from(generated_at.clone()));
    layout.insert("node_count".into(), Value::from(issue_ids.len()));
    layout.insert("edge_count".into(), Value::from(links.len()));
    Value::Object(layout)
}

/// `model.Issue{ID: id, Status: model.StatusOpen}` with every other field at
/// its zero value. Built through the loader's own deserializer because
/// `model.Issue` has no `Default`: this is the only way to get one without
/// listing all 25 fields, and going through serde guarantees the defaults match
/// the ones a real record would get.
fn synthetic_open_issue(id: &str) -> bv_core::model::Issue {
    // `id` and `title` are the only fields `model.Issue` declares without a
    // `#[serde(default)]`; Go's literal leaves every other field at its zero
    // value, which is what the omitted keys below reproduce.
    serde_json::from_value(serde_json::json!({ "id": id, "title": "", "status": "open" }))
        .expect("a minimal Issue always deserializes")
}

/// Go `(*SQLiteExporter).graphCentrality` (pkg/export/sqlite_export.go:1005-1056),
/// wrapped with the `centrality` key.
///
/// The per-node `metrics` above come from `e.Stats` — the full analysis of the
/// exported set. This block comes from a SECOND, deliberately restricted
/// re-analysis: it rebuilds a synthetic all-OPEN issue set from the exported
/// issues plus every dependency endpoint missing from the export, then turns off
/// every metric whose browser-side implementation has its own sampling. Using
/// one analysis for both would be right today and wrong the moment a threshold
/// trips.
pub fn build_graph_centrality(issue_ids: &[String], deps: &[BlockingDep]) -> Value {
    // Rebuild the synthetic all-open issue set. Go writes
    // `&model.Issue{ID: id, Status: model.StatusOpen}` — nothing else is read
    // off these, but the graph builder needs a real slice to walk.
    let mut by_id: BTreeMap<String, bv_core::model::Issue> = BTreeMap::new();
    for id in issue_ids {
        by_id.insert(id.clone(), synthetic_open_issue(id));
    }
    for dep in deps {
        for id in [&dep.issue_id, &dep.depends_on_id] {
            by_id
                .entry(id.clone())
                .or_insert_with(|| synthetic_open_issue(id));
        }
        if let Some(issue) = by_id.get_mut(&dep.issue_id) {
            issue.dependencies.push(bv_core::model::Dependency {
                issue_id: dep.issue_id.clone(),
                depends_on_id: dep.depends_on_id.clone(),
                r#type: dep.dep_type.clone(),
                depends_on_legacy: String::new(),
                target_id_legacy: String::new(),
                created_at: None,
                created_by: String::new(),
            });
        }
    }
    let issues: Vec<bv_core::model::Issue> = by_id.into_values().collect();

    let node_count = issues.len();
    let density = if node_count > 1 {
        deps.len() as f64 / (node_count as f64 * (node_count as f64 - 1.0))
    } else {
        0.0
    };
    let mut cfg = bv_analysis::analyzer::config_for_size(node_count, deps.len(), density);
    // Browser sampling has different pivots and thresholds, so it cannot reuse
    // native approximations; and the browser runs its own copies of these, so
    // paying export time for them produces scores nothing reads.
    if cfg.betweenness_mode == "approximate" {
        cfg.compute_betweenness = false;
    }
    cfg.compute_hits = false;
    cfg.compute_eigenvector = false;
    cfg.compute_critical_path = false;
    cfg.compute_cycles = false;
    cfg.compute_kcore = false;
    cfg.compute_articulation = false;
    cfg.compute_slack = false;

    let g = std::sync::Arc::new(bv_analysis::analyzer::build_graph(&issues));
    let (analysis, _profile) = bv_analysis::analyzer::analyze_with_profile(g, &cfg);

    let mut out = Map::new();
    out.insert("version".into(), Value::from(1));
    // Both maps are `omitempty`, and Go's `omitempty` drops a NIL map and an
    // EMPTY map alike — so an empty graph, whose analyzer reports every enabled
    // metric as "computed" while publishing no scores, emits neither key rather
    // than two empty objects.
    if analysis.status.page_rank.state == "computed" {
        if let Some(pr) = float_map_to_json(analysis.page_rank.as_ref()) {
            out.insert("pagerank".into(), pr);
        }
    }
    if analysis.status.betweenness.state == "computed" {
        let mut betweenness = analysis.betweenness.clone().unwrap_or_default();
        // Exact Brandes stores only nonzero scores. A *completed* run makes its
        // omitted zero entries known — unlike a pending or timed-out one, whose
        // absent keys mean "not computed" and must stay absent.
        for id in by_id_keys(issue_ids, deps) {
            betweenness.entry(id).or_insert(0.0);
        }
        if let Some(bt) = float_map_to_json(Some(&betweenness)) {
            out.insert("betweenness".into(), bt);
        }
    }
    // `Centrality` is a non-nil pointer in Go, so `omitempty` never drops it and
    // the status block is always present.
    out.insert("status".into(), analysis.status.to_json_map());
    Value::Object(out)
}

/// `data/graph_layout.json`: the layout from [`build_graph_layout`] plus the
/// `centrality` block from [`build_graph_centrality`], assembled as Go's
/// `GraphLayout` struct. The two analyses are kept separate above because they
/// are genuinely separate; the document is where they meet.
pub fn build_graph_layout_document(input: &GraphLayoutInput<'_>, centrality: Value) -> Value {
    let layout = match build_graph_layout(input) {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    // Go declares `Centrality` first (sqlite_export.go:982). The top level is
    // sorted on the way out, so this only keeps the in-memory document readable.
    let mut ordered = Map::new();
    ordered.insert("centrality".into(), centrality);
    ordered.extend(layout);
    Value::Object(ordered)
}

fn by_id_keys(issue_ids: &[String], deps: &[BlockingDep]) -> Vec<String> {
    let mut keys: Vec<String> = issue_ids.to_vec();
    for dep in deps {
        for id in [&dep.issue_id, &dep.depends_on_id] {
            if !keys.iter().any(|k| k == id) {
                keys.push(id.clone());
            }
        }
    }
    keys.sort_unstable();
    keys.dedup();
    keys
}

/// Go `map[string]float64` as `encoding/json` writes it: key-sorted, and
/// absent entirely when the map is nil or empty (`omitempty` drops both).
fn float_map_to_json(m: Option<&BTreeMap<String, f64>>) -> Option<Value> {
    let m = m?;
    if m.is_empty() {
        return None;
    }
    let mut out = BTreeMap::new();
    for (k, v) in m {
        out.insert(k.clone(), Value::from(*v));
    }
    Some(sorted_object(out))
}

/// A `serde_json::Map` whose keys are in Go map order (sorted), which is what
/// `encoding/json` emits for every Go `map` field. Nested values keep whatever
/// order they already had — Go re-emits a `json.RawMessage` verbatim.
fn sorted_object(map: BTreeMap<String, Value>) -> Value {
    let mut out = Map::with_capacity(map.len());
    for (k, v) in map {
        out.insert(k, v);
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Map<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), Value::from(*v)))
            .collect()
    }

    #[test]
    fn whole_floats_render_without_trailing_zero() {
        // Go writes a `float64` with strconv's shortest-round-trip rule, so the
        // layout's `[200, -40]` must not become `[200.0, -40.0]`. `SetIndent`
        // expands the array across lines; the numbers inside are the point.
        let mut out = String::new();
        encode_pretty(
            &serde_json::json!([200.0, -40.0, 21.0, 0.0]),
            &mut out,
            2,
            false,
        );
        assert_eq!(out, "[\n    200,\n    -40,\n    21,\n    0\n  ]");
    }

    #[test]
    fn strings_are_html_escaped_like_go() {
        let mut out = String::new();
        encode_pretty(&serde_json::json!("a<b>&c"), &mut out, 0, false);
        // `\u003c`, `\u003e` and `\u0026` are six literal characters
        // each; a Rust unicode escape would decode them instead.
        let expected = r#""a\u003cb\u003e\u0026c""#;
        assert_eq!(out, expected);
    }

    #[test]
    fn only_the_top_level_is_sorted() {
        // Go sorts the top-level `map[string]json.RawMessage`; the nested
        // `json.RawMessage` is re-emitted verbatim, keeping struct order.
        let payload = serde_json::json!({"zebra": 1, "alpha": {"zz": 1, "aa": 2}});
        let mut out = String::new();
        encode_pretty(&payload, &mut out, 0, true);
        assert_eq!(
            out,
            "{\n  \"alpha\": {\n    \"zz\": 1,\n    \"aa\": 2\n  },\n  \"zebra\": 1\n}"
        );
    }

    #[test]
    fn envelope_wins_except_for_an_existing_generated_at() {
        let dir = std::env::temp_dir().join(format!("bvr-pages-data-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("meta.json");
        write_robot_json(
            &path,
            &serde_json::json!({"version": "1.0.0", "generated_at": "2026-01-01T00:00:00.123456Z"}),
            &env(&[
                ("version", "v0.25.0"),
                ("generated_at", "2026-01-01T00:00:00Z"),
                ("data_hash", "abc"),
            ]),
        )
        .unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.ends_with('\n'), "writeJSON emits a trailing newline");
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["version"], "v0.25.0");
        assert_eq!(parsed["generated_at"], "2026-01-01T00:00:00.123456Z");
        assert_eq!(parsed["data_hash"], "abc");
    }

    #[test]
    fn missing_generated_at_is_inherited_from_the_envelope() {
        let dir =
            std::env::temp_dir().join(format!("bvr-pages-data-inherit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("project_health.json");
        write_robot_json(
            &path,
            &serde_json::json!({"counts": {"total": 3}}),
            &env(&[("generated_at", "2026-01-01T00:00:00Z")]),
        )
        .unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["generated_at"], "2026-01-01T00:00:00Z");
    }

    #[test]
    fn layout_layers_a_chain_with_the_go_spacings() {
        // root blocks mid blocks leaf: longest-path depth 0, 1, 2.
        let ids = vec!["root".to_string(), "mid".to_string(), "leaf".to_string()];
        let deps = vec![
            BlockingDep {
                issue_id: "mid".into(),
                depends_on_id: "root".into(),
                dep_type: bv_core::model::DependencyType::Blocks,
            },
            BlockingDep {
                issue_id: "leaf".into(),
                depends_on_id: "mid".into(),
                dep_type: bv_core::model::DependencyType::Blocks,
            },
        ];
        let order = vec!["root".to_string(), "mid".to_string(), "leaf".to_string()];
        let layout = build_graph_layout(&GraphLayoutInput {
            issue_ids: &ids,
            deps: &deps,
            topological_order: Some(&order),
            page_rank: None,
            betweenness: None,
            cycles: None,
            generated_at: "2026-01-01T00:00:00Z".into(),
        });
        // One node per depth, so each column's single entry sits at y = 0.
        assert_eq!(layout["positions"]["root"], serde_json::json!([0.0, 0.0]));
        assert_eq!(layout["positions"]["mid"], serde_json::json!([200.0, 0.0]));
        assert_eq!(layout["positions"]["leaf"], serde_json::json!([400.0, 0.0]));
        // links point DependsOnID -> IssueID.
        assert_eq!(
            layout["links"],
            serde_json::json!([["root", "mid"], ["mid", "leaf"]])
        );
        // [pagerank, betweenness, blocked_by, blocks, in_cycle]
        assert_eq!(
            layout["metrics"]["mid"],
            serde_json::json!([0.0, 0.0, 1.0, 1.0, 0.0])
        );
        assert_eq!(layout["node_count"], 3);
        assert_eq!(layout["edge_count"], 2);
        // An acyclic graph must not emit a `cycles` key at all.
        assert!(layout.get("cycles").is_none());
    }

    #[test]
    fn layout_without_a_topological_order_uses_bfs_depths() {
        // root -> mid only; leaf is unreachable, so it stays at depth 0.
        let ids = vec!["root".to_string(), "mid".to_string(), "leaf".to_string()];
        let deps = vec![BlockingDep {
            issue_id: "mid".into(),
            depends_on_id: "root".into(),
            dep_type: bv_core::model::DependencyType::Blocks,
        }];
        let layout = build_graph_layout(&GraphLayoutInput {
            issue_ids: &ids,
            deps: &deps,
            topological_order: None,
            page_rank: None,
            betweenness: None,
            cycles: None,
            generated_at: "2026-01-01T00:00:00Z".into(),
        });
        assert_eq!(layout["positions"]["root"][0], 0.0);
        assert_eq!(layout["positions"]["mid"][0], 200.0);
        assert_eq!(layout["positions"]["leaf"][0], 0.0);
    }

    #[test]
    fn layout_cycles_are_emitted_only_when_present() {
        let ids = vec!["a".to_string()];
        let deps: Vec<BlockingDep> = Vec::new();
        let cycles = vec![vec!["a".to_string(), "a".to_string()]];
        let layout = build_graph_layout(&GraphLayoutInput {
            issue_ids: &ids,
            deps: &deps,
            topological_order: Some(&["a".to_string()]),
            page_rank: None,
            betweenness: None,
            cycles: Some(&cycles),
            generated_at: "2026-01-01T00:00:00Z".into(),
        });
        assert_eq!(layout["cycles"], serde_json::json!([["a", "a"]]));
        assert_eq!(layout["metrics"]["a"][4], 1.0);
    }

    #[test]
    fn centrality_backfills_zero_for_every_synthetic_node() {
        let ids = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let deps = vec![BlockingDep {
            issue_id: "b".into(),
            depends_on_id: "a".into(),
            dep_type: bv_core::model::DependencyType::Blocks,
        }];
        let centrality = build_graph_centrality(&ids, &deps);
        let bt = &centrality["betweenness"];
        for id in ["a", "b", "c"] {
            assert!(
                bt.get(id).is_some(),
                "exact betweenness must be backfilled for {id}"
            );
        }
        assert_eq!(centrality["version"], 1);
        // Every metric the exporter turns off reports a bare "skipped".
        assert_eq!(centrality["status"]["Eigenvector"]["state"], "skipped");
        assert!(centrality["status"]["Eigenvector"].get("reason").is_none());
    }
}
