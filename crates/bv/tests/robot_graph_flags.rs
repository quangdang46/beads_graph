//! `--robot-graph` flag and empty-result parity with Go v0.25.0, plus the
//! pflag parse-error contract for `--orphans-min-score` / `--history-limit`.
//!
//! Go accepts `--graph-root <missing>` and `--graph-depth -1` silently
//! (`pkg/export/graph_export.go:112-117,187-240`): the subgraph BFS simply
//! visits nothing / treats any `maxDepth <= 0` as unlimited. What Go DOES
//! pin down is the empty-result payload shape — `ExportGraph` returns early
//! (graph_export.go:80-91) with the empty-graph explanation, no
//! `filters_applied` (the label scope is added later, only when set), and no
//! `graph`/`adjacency` keys — and the per-format fields for dot/mermaid
//! (`edges`, `filters_applied`, and the format-specific `explanation`,
//! graph_export.go:107-144), which an earlier port dropped.
//!
//! For the numeric flags Go registers plain `flag.Int`/`Float64` with no
//! range validation (main.go:1494-1576): out-of-range values are used as-is
//! or floored downstream (`GetRelatedFiles` maps `threshold <= 0` to 0.5,
//! `attention-limit <= 0` to 5, `robot-max-results <= 0` to 10). Only the
//! *parse* failure is pinned: pflag reports
//! `invalid argument "v" for "--flag" flag: strconv.ParseInt: ...` at exit 1
//! (main.go:4548).

use std::process::Command;

const REPO_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

fn run(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_bvr"))
        .current_dir(REPO_ROOT)
        .args(args)
        .output()
        .expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn payload(stdout: &str) -> serde_json::Value {
    serde_json::from_str(stdout)
        .unwrap_or_else(|e| panic!("stdout is one JSON object: {e}\n{stdout}"))
}

/// Go `ExportGraph`'s early return on an empty filtered set: nodes/edges 0,
/// the empty-graph explanation, and — because `GraphExportResult`'s
/// `FiltersApplied`/`Adjacency` stay nil — no `filters_applied`/`adjacency`
/// keys at all (the root that produced the empty set is NOT reported).
#[test]
fn graph_root_that_matches_nothing_is_an_empty_graph_not_an_error() {
    let (code, stdout, stderr) = run(&["--robot-graph", "--graph-root", "NOPE", "--json"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let p = payload(&stdout);
    assert_eq!(p["nodes"], 0);
    assert_eq!(p["edges"], 0);
    assert_eq!(p["format"], "json");
    assert_eq!(
        p["explanation"]["what"],
        "Empty graph - no issues match the filter criteria"
    );
    assert_eq!(
        p["explanation"]["when_to_use"],
        "Adjust filter parameters to include more issues"
    );
    assert!(p.get("filters_applied").is_none(), "{p}");
    assert!(p.get("adjacency").is_none(), "{p}");
    assert!(p.get("graph").is_none(), "{p}");
}

/// `--graph-depth -1` is accepted silently: `maxDepth > 0` gates the depth
/// check in extractSubgraph, so a negative behaves exactly like 0
/// (unlimited), and `config.Depth > 0` keeps it out of `filters_applied`.
#[test]
fn graph_depth_negative_is_unlimited_and_unreported() {
    let (code, stdout, stderr) = run(&["--robot-graph", "--graph-depth", "-1", "--json"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let p = payload(&stdout);
    assert!(p["nodes"].as_u64().unwrap_or(0) > 0, "{p}");
    assert!(p.get("filters_applied").is_none(), "{p}");
}

/// The dot/mermaid branches carry the same envelope fields as json: `edges`,
/// `filters_applied` (sorted keys) and the per-format `explanation` — Go's
/// `GraphExplanation` differs per format and the json one has no
/// `how_to_render`.
#[test]
fn graph_dot_and_mermaid_emit_the_full_result_shape() {
    for (format, what, how) in [
        (
            "dot",
            "Dependency graph in Graphviz DOT format",
            "Save to file.dot, run: dot -Tpng file.dot -o graph.png",
        ),
        (
            "mermaid",
            "Dependency graph in Mermaid diagram format",
            "Paste into any Markdown renderer that supports Mermaid, or use mermaid.live",
        ),
    ] {
        let (code, stdout, stderr) = run(&[
            "--robot-graph",
            "--graph-format",
            format,
            "--graph-depth",
            "1",
            "--json",
        ]);
        assert_eq!(code, 0, "{format}: stderr: {stderr}");
        let p = payload(&stdout);
        assert_eq!(p["format"], format);
        assert!(p["graph"].as_str().is_some_and(|s| !s.is_empty()), "{p}");
        assert!(p.get("nodes").is_some(), "{p}");
        assert!(p.get("edges").is_some(), "{p}");
        assert_eq!(p["filters_applied"]["depth"], "1", "{p}");
        assert_eq!(p["explanation"]["what"], what, "{p}");
        assert_eq!(p["explanation"]["how_to_render"], how, "{p}");
        assert!(p.get("adjacency").is_none(), "{p}");
    }
}

/// pflag int parse failures are exit 1 with the strconv detail — not the
/// exit 2 + bespoke message the port used to emit.
#[test]
fn orphans_min_score_parse_error_matches_pflag() {
    let (code, _stdout, stderr) = run(&["--robot-orphans", "--orphans-min-score", "abc", "--json"]);
    assert_eq!(code, 1, "stderr: {stderr}");
    assert_eq!(
        stderr.trim(),
        "invalid argument \"abc\" for \"--orphans-min-score\" flag: \
         strconv.ParseInt: parsing \"abc\": invalid syntax"
    );
}

#[test]
fn history_limit_parse_error_matches_pflag() {
    let (code, _stdout, stderr) = run(&["--robot-history", "--history-limit", "abc", "--json"]);
    assert_eq!(code, 1, "stderr: {stderr}");
    assert_eq!(
        stderr.trim(),
        "invalid argument \"abc\" for \"--history-limit\" flag: \
         strconv.ParseInt: parsing \"abc\": invalid syntax"
    );
}
