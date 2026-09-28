//! `--export-pages` data-file contract: `data/meta.json`, `data/triage.json`,
//! `data/project_health.json` and `data/graph_layout.json`.
//!
//! All four are written by Go's `(*SQLiteExporter).writeRobotOutputs` /
//! `writeGraphLayout` (pkg/export/sqlite_export.go) through `writeRobotJSON` and
//! `writeJSON`, and all four carry three properties that a plain
//! `serde_json::to_string_pretty` gets wrong and that nothing else in the crate
//! would catch:
//!
//!   1. **The envelope wins.** `writeRobotJSON` re-reads the payload as a
//!      `map[string]json.RawMessage` and overwrites every envelope key — so
//!      `version` is `"v0.25.0"` even though `ExportMeta.Version` and
//!      `GraphLayout.Version` are both the literal `"1.0.0"`, and `meta.json`
//!      has a `data_hash` its own struct never assigns. `triage.json`'s NESTED
//!      `meta.version` is a different field and does keep `"1.0.0"`.
//!   2. **`generated_at` is payload-owned when the payload has one.**
//!      `ExportMeta.GeneratedAt` is a `time.Time`, so it is RFC3339Nano, while
//!      `GraphLayout.GeneratedAt` is a hand-formatted second-precision string.
//!      `project_health.json` has neither, so it inherits the envelope's.
//!   3. **The top level is sorted, the nested levels are not.** `writeJSON`
//!      receives a Go *map* and `encoding/json` sorts map keys — but a nested
//!      `json.RawMessage` is re-emitted verbatim, so `centrality`, `status`,
//!      `counts` and `graph` keep Go's struct declaration order. Only the nested
//!      Go MAPS (`positions`, `metrics`, `pagerank`, `betweenness`) sort.
//!
//! Plus the two encoding details: `json.Encoder.Encode` emits a trailing
//! newline, and Go writes a whole `float64` without a trailing `.0`.
//!
//! Each case drives the real binary against a scratch workspace seeded from the
//! repo's own JSONL. `--pages-include-history=false` keeps the git-history
//! prologue out of the way: the scratch directory is not a repository.

use std::path::{Path, PathBuf};
use std::process::Command;

const REPO_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

/// `bvr --export-pages <dir>` over a scratch copy of the repo's own data.
fn export(tag: &str, extra: &[&str]) -> PathBuf {
    let base = std::env::temp_dir().join(format!("bvr-pages-data-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let beads = base.join(".beads");
    std::fs::create_dir_all(&beads).expect("create .beads");
    std::fs::copy(
        Path::new(REPO_ROOT).join(".beads/issues.jsonl"),
        beads.join("issues.jsonl"),
    )
    .expect("seed issues.jsonl");
    let out = base.join("site");
    let status = Command::new(env!("CARGO_BIN_EXE_bvr"))
        .current_dir(&base)
        .args(["--export-pages", out.to_str().expect("utf-8 out dir")])
        .args(["--db", ".beads/issues.jsonl"])
        .arg("--pages-include-history=false")
        .args(extra)
        .output()
        .expect("bvr runs");
    assert!(
        status.status.success(),
        "export failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    out
}

fn read_json(out: &Path, name: &str) -> (String, serde_json::Value) {
    let path = out.join("data").join(name);
    let raw = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let text = String::from_utf8(raw).expect("data files are UTF-8");
    let value = serde_json::from_str(&text).expect("data files are valid JSON");
    (text, value)
}

/// Every `data/*.json` the SQLite exporter writes ends with `0x0A`: Go writes
/// them with `json.Encoder.Encode`, which terminates each value with a newline.
/// `history.json` is the exception and is not in this list — Go writes that one
/// with `json.MarshalIndent` + `os.WriteFile`.
#[test]
fn every_written_file_ends_with_a_newline() {
    let out = export("newline", &[]);
    for name in [
        "meta.json",
        "triage.json",
        "project_health.json",
        "graph_layout.json",
    ] {
        let path = out.join("data").join(name);
        let bytes = std::fs::read(&path).expect("file exists");
        assert_eq!(
            bytes.last().copied(),
            Some(b'\n'),
            "{name} must end with the newline json.Encoder.Encode writes"
        );
    }
}

/// Only the top level is sorted. A nested Go struct keeps its declaration
/// order, so a blanket "sort everything" encoder would move `phase2_ready`
/// ahead of `cycle_count` and break the file.
#[test]
fn only_the_top_level_is_alphabetical() {
    let out = export("order", &[]);
    for name in [
        "meta.json",
        "triage.json",
        "project_health.json",
        "graph_layout.json",
    ] {
        let (_, value) = read_json(&out, name);
        let obj = value.as_object().expect("object");
        let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(keys, sorted, "{name} top-level keys are not sorted");
    }

    // `GraphHealth` (pkg/analysis/triage.go:220-227) declares node_count,
    // edge_count, density, has_cycles, cycle_count, phase2_ready — which is
    // neither sorted nor the order `cycle_count` would land in if it were simply
    // appended.
    let (_, health) = read_json(&out, "project_health.json");
    let graph_keys: Vec<&str> = health["graph"]
        .as_object()
        .expect("graph object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        graph_keys,
        vec![
            "node_count",
            "edge_count",
            "density",
            "has_cycles",
            "phase2_ready"
        ]
    );

    // `MetricStatus` is a struct too: PageRank/Betweenness/Eigenvector/HITS/
    // Critical/Cycles/KCore/Articulation/Slack (bv-analysis analyzer.rs).
    let (_, triage) = read_json(&out, "triage.json");
    let status_keys: Vec<&str> = triage["status"]
        .as_object()
        .expect("status object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        status_keys,
        vec![
            "PageRank",
            "Betweenness",
            "Eigenvector",
            "HITS",
            "Critical",
            "Cycles",
            "KCore",
            "Articulation",
            "Slack"
        ]
    );
}

/// The three envelope traps. Each one is a way to publish a well-formed file
/// that no consumer can use.
#[test]
fn the_envelope_overwrites_the_payload() {
    let out = export("envelope", &[]);

    // `version`: the payload structs say "1.0.0", the envelope says "v0.25.0".
    let (_, meta) = read_json(&out, "meta.json");
    assert_eq!(meta["version"], "v0.25.0");
    let (_, layout) = read_json(&out, "graph_layout.json");
    assert_eq!(layout["version"], "v0.25.0");
    let (_, triage) = read_json(&out, "triage.json");
    assert_eq!(triage["version"], "v0.25.0");
    // ...but triage.json's NESTED `meta.version` is a different field entirely
    // and does keep the struct's own value.
    assert_eq!(triage["meta"]["version"], "1.0.0");

    // `data_hash` is `omitempty` on `ExportMeta` and never assigned on the
    // export path, so meta.json's can only have come from the envelope.
    assert!(
        meta["data_hash"].as_str().is_some_and(|h| h.len() == 64),
        "meta.json's data_hash must be the envelope's"
    );

    // `generated_at`: meta.json keeps its own nanosecond-precision instant,
    // graph_layout.json its own second-precision one, and project_health.json
    // — which has neither — inherits the envelope's.
    assert_ne!(meta["generated_at"], triage["meta"]["generated_at"]);
    let (_, health) = read_json(&out, "project_health.json");
    assert_eq!(health["generated_at"], triage["generated_at"]);

    // Every file carries the full envelope key set, not a subset.
    for name in ["meta.json", "project_health.json", "graph_layout.json"] {
        let (_, value) = read_json(&out, name);
        for key in [
            "authority_hash",
            "data_hash",
            "output_format",
            "scope_hash",
            "source_authority",
            "source_kind",
            "source_path",
            "version",
        ] {
            assert!(
                value.get(key).is_some(),
                "{name} is missing the envelope key {key}"
            );
        }
    }
}

/// `graph_layout.json` is computed at export time: a deterministic layered DAG
/// embed (longest-path depth along the topological order, `x = depth*200`,
/// `y` centred on zero at 80 per row) plus a SEPARATE restricted re-analysis for
/// the `centrality` block. Neither is derivable in the browser, and a reader
/// that got only the layout would have no scores at all.
#[test]
fn graph_layout_is_a_complete_precomputed_embed() {
    let out = export("layout", &[]);
    let (_, layout) = read_json(&out, "graph_layout.json");

    let n = layout["node_count"].as_u64().expect("node_count") as usize;
    assert!(n > 0, "an export with issues has nodes");
    assert_eq!(
        layout["edge_count"].as_u64().expect("edge_count") as usize,
        layout["links"].as_array().expect("links").len()
    );

    for key in ["positions", "metrics"] {
        let map = layout[key].as_object().expect(key);
        assert_eq!(map.len(), n, "{key} must cover every node exactly once");
        let keys: Vec<&str> = map.keys().map(String::as_str).collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(keys, sorted, "{key} is a Go map, so its keys sort");
    }

    // `positions` is a layered embed: x is the depth times 200, and each column
    // of nodes is centred on y = 0 with 80 between consecutive rows. A column
    // starts at `-(count-1)*80/2`, so an odd count lands the first node on a
    // half-step — the spacing and the centring are the invariants, not "y is a
    // multiple of 80".
    let mut columns: std::collections::BTreeMap<i64, Vec<f64>> = Default::default();
    for (_id, xy) in layout["positions"].as_object().expect("positions") {
        let pair = xy.as_array().expect("a [x, y] pair");
        let x = pair[0].as_f64().expect("x");
        let y = pair[1].as_f64().expect("y");
        assert!(
            (x - (x / 200.0).round() * 200.0).abs() < 1e-9,
            "{x} is not a multiple of 200"
        );
        assert!(x >= 0.0, "{x} is a depth, so it cannot be negative");
        columns.entry(x.round() as i64).or_default().push(y);
    }
    assert!(!columns.is_empty(), "no columns at all");
    for (x, mut ys) in columns {
        ys.sort_by(|a, b| a.partial_cmp(b).expect("y is finite"));
        assert!(
            (ys[0] + ys[ys.len() - 1]).abs() < 1e-9,
            "the column at x={x} is not centred on y = 0 ({ys:?})"
        );
        for pair in ys.windows(2) {
            assert!(
                (pair[1] - pair[0] - 80.0).abs() < 1e-9,
                "the column at x={x} is not spaced 80 apart ({ys:?})"
            );
        }
    }

    // `metrics` is [pagerank, betweenness, blocked_by, blocks, in_cycle].
    for (_id, m) in layout["metrics"].as_object().expect("metrics") {
        assert_eq!(m.as_array().expect("a 5-tuple").len(), 5);
    }

    // The `centrality` block is a separate analysis: the export turns HITS,
    // eigenvector, critical path, cycles, k-core, articulation and slack off
    // (sqlite_export.go:1005-1056), so those seven report a bare "skipped"
    // while page rank and betweenness are computed.
    let centrality = layout["centrality"].as_object().expect("centrality");
    assert_eq!(centrality["version"], 1);
    let status = centrality["status"].as_object().expect("status");
    for metric in [
        "Eigenvector",
        "HITS",
        "Critical",
        "Cycles",
        "KCore",
        "Articulation",
        "Slack",
    ] {
        assert_eq!(status[metric]["state"], "skipped", "{metric} is disabled");
        assert!(
            status[metric].get("reason").is_none(),
            "{metric} is a bare skipped, with no reason"
        );
    }
    assert_eq!(status["PageRank"]["state"], "computed");
    assert_eq!(status["Betweenness"]["state"], "computed");
    // Exact Brandes omits zero scores, so Go back-fills them — the map has to
    // cover every node or the browser draws a node with no centrality at all.
    for key in ["pagerank", "betweenness"] {
        assert_eq!(
            centrality[key].as_object().expect(key).len(),
            n,
            "{key} must be back-filled for every node"
        );
    }

    // The repo's own graph is acyclic, and `Cycles` is `omitempty`: an empty
    // array would claim "we looked and found none" under a key Go omits.
    assert!(
        layout.get("cycles").is_none(),
        "an acyclic graph must not emit a cycles key"
    );
}

/// Go's encoder writes a whole `float64` with no trailing `.0`, so the layout's
/// `[200, -40]` must not come out as `[200.0, -40.0]`.
#[test]
fn whole_floats_render_without_a_trailing_zero() {
    let out = export("floats", &[]);
    let (text, _) = read_json(&out, "graph_layout.json");
    assert!(
        !text.contains(".0,") && !text.contains(".0\n") && !text.contains(".0]"),
        "a whole float was rendered with a trailing .0"
    );
    // The specific pair a two-node-wide column produces.
    assert!(
        text.contains("[0, -") || text.contains("\n      0,\n"),
        "no zero-valued coordinate found; the assertion above needs a real case"
    );
}

/// `--pages-include-closed` is a pflag bool defaulting TRUE (main.go:1623).
/// Reading it as "present means on" made the default invocation drop every
/// closed bead — which on a finished workspace is all of them, leaving an empty
/// graph that still printed a success line.
#[test]
fn closed_issues_are_included_by_default() {
    let with_closed = export("include-closed", &[]);
    let (_, all) = read_json(&with_closed, "graph_layout.json");
    let without = export("exclude-closed", &["--pages-include-closed=false"]);
    let (_, open_only) = read_json(&without, "graph_layout.json");

    let all_nodes = all["node_count"].as_u64().expect("node_count");
    let open_nodes = open_only["node_count"].as_u64().expect("node_count");
    assert!(
        all_nodes > open_nodes,
        "the default must ship more issues than --pages-include-closed=false \
         (default {all_nodes}, filtered {open_nodes})"
    );
    assert_eq!(
        all_nodes,
        all["positions"].as_object().expect("positions").len() as u64
    );
}
