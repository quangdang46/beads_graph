//! `--profile-startup` / `--profile-json` parity with Go bv v0.25.0.
//!
//! Go dispatches `--profile-startup` on presence at `cmd/bv/main.go:3566-3570`,
//! passing the already-loaded `issues`, the `loadDuration` measured around
//! `datasource.LoadIssues` (main.go:2609-2755) and two booleans, then exits 0.
//! `runProfileStartup` (main.go:4969-5030) emits one of two documents:
//!
//! * with `--profile-json`, an anonymous struct encoded through
//!   `newRobotEncoder` — keys in Go's declaration order:
//!   `generated_at`, `data_path`, `load_jsonl`, `profile`, `total_with_load`,
//!   `recommendations`. The `profile` value is `*analysis.StartupProfile`
//!   verbatim, so its 24 keys and their order are a contract too.
//! * otherwise `printProfileReport` (main.go:5035-5073), where every literal —
//!   the `===============` rule, the `Phase 1 (blocking):` / `Phase 2 (async in
//!   normal mode, sync for profiling):` headings, the 14-column metric labels
//!   `printMetricLine` emits, `Total startup:`, `Size tier:`, `Skipped metrics:`
//!   / `All metrics computed` and `Recommendations:` — is compared against the
//!   oracle rather than eyeballed.
//!
//! Until this landed the flag was registered in `flags.rs` and carried a live
//! modifier-requires rule (flags.rs:1750), so `bvr --profile-json` correctly
//! refused — but `bvr --profile-startup` matched no dispatch, fell through to
//! the interactive TUI launcher and hung. No golden covers it either: every
//! value in the document is a wall-clock measurement.
//!
//! The shape assertions below are therefore differential-by-construction: the
//! non-timing parts (key order, node/edge/density counts, the whole `config`
//! object, the skipped-metric line, the size tier, the cycle suffix) are
//! reproducible, and the timing parts are masked. To re-derive the oracle side:
//!
//! ```sh
//! ./beads_viewer/.bv-go --profile-startup --db <fixture>   # human
//! ./beads_viewer/.bv-go --profile-startup --profile-json --db <fixture>
//! ```

use serde_json::Value;
use std::path::Path;
use std::process::Command;

/// Every run pins the source with `--db`, so the loader cannot pick `beads.db`
/// on one side and `issues.jsonl` on the other — the oracle prefers whichever
/// is fresher, bvr always prefers the JSONL. The source is a parameter rather
/// than a baked-in prefix because `--db` resolves to the FIRST occurrence, so
/// a default plus a per-test override would silently measure the default.
fn run(dir: &str, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_bvr"))
        .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .args(["--db", dir])
        .args(args)
        .output()
        .expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// The repository's own `.beads/issues.jsonl` — 46 issues, the Small tier.
const SELF: &str = ".beads/issues.jsonl";

fn bvr(args: &[&str]) -> (i32, String, String) {
    run(SELF, args)
}

fn json_in(dir: &str, args: &[&str]) -> Value {
    let (code, stdout, stderr) = run(dir, args);
    assert_eq!(code, 0, "stderr: {stderr}");
    serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("stdout is JSON ({e}): {stdout}"))
}

/// Present a path with `/` separators whatever the platform produced.
///
/// Paths in the payload come from `Path::join` and Go's `filepath.Join`, so
/// they carry `\` on Windows. A literal like `.beads/issues.jsonl` is then
/// false there — not because the port resolved anything differently, but
/// because the assertion hardcoded a separator. Comparing normalized copies
/// keeps the assertion about resolution rather than about the host.
fn to_slashes(path: &str) -> String {
    path.replace('\\', "/")
}

fn json(args: &[&str]) -> Value {
    json_in(SELF, args)
}

/// The synthetic graphs under `tests/fixtures/`, each of which lands in a
/// different size tier — the tier boundaries at Go `config.go:109-121`
/// (`<100` / `<500` / `<2000` / rest) are what select the config, and
/// `xl_2500` is the only one whose `ConfigForSize` skips cycles.
const FIXTURES: &[(&str, &str)] = &[
    ("small_chain", "tests/fixtures/small_chain/.beads"),
    ("medium_tree", "tests/fixtures/medium_tree/.beads"),
    ("large_cyclic_600", "tests/fixtures/large_cyclic_600/.beads"),
    ("xl_2500", "tests/fixtures/xl_2500/.beads"),
];

/// Go's `StartupProfile` field order (pkg/analysis/graph.go:27-61), which
/// `encoding/json` preserves because the value is a struct.
const PROFILE_KEYS: &[&str] = &[
    "node_count",
    "edge_count",
    "density",
    "build_graph",
    "degree",
    "topo_sort",
    "phase1_total",
    "pagerank",
    "pagerank_timeout",
    "betweenness",
    "betweenness_timeout",
    "eigenvector",
    "hits",
    "hits_timeout",
    "critical_path",
    "cycles",
    "cycles_timeout",
    "cycle_count",
    "kcore",
    "articulation",
    "slack",
    "phase2_total",
    "config",
    "total",
];

/// Go's `AnalysisConfig` field order (pkg/analysis/config.go:44-79).
const CONFIG_KEYS: &[&str] = &[
    "ComputeBetweenness",
    "BetweennessTimeout",
    "BetweennessSkipReason",
    "BetweennessMode",
    "BetweennessSampleSize",
    "BetweennessIsApproximate",
    "ComputePageRank",
    "PageRankTimeout",
    "PageRankSkipReason",
    "ComputeHITS",
    "HITSTimeout",
    "HITSSkipReason",
    "ComputeCycles",
    "CyclesTimeout",
    "MaxCyclesToStore",
    "CyclesSkipReason",
    "ComputeEigenvector",
    "ComputeCriticalPath",
    "ComputeKCore",
    "ComputeArticulation",
    "ComputeSlack",
];

/// The oracle's own output for `medium_tree`, captured from
/// `./beads_viewer/.bv-go --profile-startup --db tests/fixtures/medium_tree/.beads`
/// at the v0.25.0 parity commit. Every count, the density, the config and the
/// size tier are reproducible; only the nanosecond timings are not, so the
/// fixture stores those as `null` and this module masks them.
const MEDIUM_TREE_GO: &str = include_str!("fixtures/profile_medium_tree_go_v0.25.0.json");

#[test]
fn profile_json_uses_go_key_order() {
    let out = json(&["--profile-startup", "--profile-json"]);
    let keys: Vec<&str> = out
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    // Go's anonymous struct at main.go:5004-5012, not alphabetical.
    assert_eq!(
        keys,
        [
            "generated_at",
            "data_path",
            "load_jsonl",
            "profile",
            "total_with_load",
            "recommendations"
        ]
    );
}

#[test]
fn profile_json_keys_and_config_order_match_go() {
    let out = json(&["--profile-startup", "--profile-json"]);
    let profile = out["profile"].as_object().expect("profile object");
    let keys: Vec<&str> = profile.keys().map(String::as_str).collect();
    assert_eq!(keys, PROFILE_KEYS, "StartupProfile field order");
    let config = profile["config"].as_object().expect("config object");
    let config_keys: Vec<&str> = config.keys().map(String::as_str).collect();
    assert_eq!(config_keys, CONFIG_KEYS, "AnalysisConfig field order");
}

#[test]
fn profile_json_data_path_resolves_through_beads_dir() {
    // Go main.go:4971-4976: GetBeadsDir("") then FindJSONLPath, falling back
    // to the directory itself. `--db .beads/issues.jsonl` makes BEADS_DB point
    // at the `.beads` directory, so the JSONL inside it is the answer.
    let out = json(&["--profile-startup", "--profile-json"]);
    let data_path = out["data_path"].as_str().expect("data_path is a string");
    // Separators are normalized before the comparison: Go's `filepath.Join`
    // uses `\` on Windows, so the path really does end `.beads\issues.jsonl`
    // there and only the literal in this assertion is wrong.
    assert!(
        to_slashes(data_path).ends_with(".beads/issues.jsonl"),
        "data_path resolved through GetBeadsDir, got {data_path}"
    );
}

#[test]
fn profile_json_load_and_total_durations_use_go_syntax() {
    // Go stamps both with `time.Duration.String()` (main.go:5017/5019), so
    // they are strings, never numbers — "9.052709ms", "1.5s", "0s". A JSON
    // number here would mean the handler forgot `.String()`, which is the one
    // mistake that would survive a Go-shaped field list.
    let out = json(&["--profile-startup", "--profile-json"]);
    for key in ["load_jsonl", "total_with_load"] {
        let v = out[key].as_str().unwrap_or_else(|| {
            panic!(
                "{key} is a duration string, not a number: {v:?}",
                v = out[key]
            )
        });
        // Go's Duration.String picks a unit per magnitude: ns, µs, ms, or a
        // compound h/m/s form. Every spelling ends in "s".
        assert!(v.ends_with('s'), "{key} ends in a time unit, got {v:?}");
        assert!(
            v.chars()
                .all(|c| c.is_ascii_digit() || ".nµms h".contains(c)),
            "{key} is a Go duration literal, got {v:?}"
        );
    }
    // total_with_load = loadDuration + profile.total, so it can only be
    // non-zero, and the profile's own `total` is the analysis half of it.
    let total = out["profile"]["total"].as_u64().expect("total is ns");
    assert!(total > 0, "profile.total is {total}ns");
}

#[test]
fn profile_json_matches_go_field_for_field_on_medium_tree() {
    let mut got = json_in(FIXTURES[1].1, &["--profile-startup", "--profile-json"]);
    let mut want: Value = serde_json::from_str(MEDIUM_TREE_GO).expect("fixture is valid JSON");

    // Everything wall-clock is unmatchable by construction.
    for doc in [&mut got, &mut want] {
        let profile = doc["profile"].as_object_mut().expect("profile object");
        for key in [
            "build_graph",
            "degree",
            "topo_sort",
            "phase1_total",
            "pagerank",
            "betweenness",
            "eigenvector",
            "hits",
            "critical_path",
            "cycles",
            "kcore",
            "articulation",
            "slack",
            "phase2_total",
            "total",
        ] {
            profile.insert(key.to_string(), Value::Null);
        }
        for key in ["generated_at", "load_jsonl", "total_with_load"] {
            doc.as_object_mut()
                .expect("object")
                .insert(key.to_string(), Value::Null);
        }
    }
    // `data_path` is an absolute path, so only its tail is portable; the
    // fixture stores the redacted form and the run re-derives the same tail.
    // Normalized first, because the runner's separator decides the split —
    // without it the tail came back empty on Windows.
    let tail = |v: &Value| -> String {
        to_slashes(v.as_str().expect("data_path is a string"))
            .rsplit_once("tests/fixtures/")
            .map(|(_, t)| t.to_string())
            .unwrap_or_default()
    };
    let got_tail = tail(&got["data_path"]);
    let _ = tail(&want["data_path"]);
    assert_eq!(
        got_tail, "medium_tree/.beads/issues.jsonl",
        "data_path tail"
    );
    for doc in [&mut got, &mut want] {
        doc.as_object_mut()
            .expect("object")
            .insert("data_path".into(), Value::Null);
        // `⚠ Betweenness taking N% of Phase 2 time` is a ratio of two
        // wall-clock measurements, so whether it fires at all depends on how
        // fast this machine is. The fixture keeps only the timing verdict,
        // which is not.
        let first = doc["recommendations"][0].clone();
        doc.as_object_mut()
            .expect("object")
            .insert("recommendations".into(), Value::Array(vec![first]));
    }
    assert_eq!(got, want);
}

#[test]
fn profile_json_node_and_edge_counts_match_the_graph() {
    // Go graph.go:1884-1887 stamps the profile from the ANALYZER, not from
    // the issue list: `NodeCount = len(a.issueMap)` (one entry per issue) and
    // `EdgeCount = a.g.Edges().Len()`. The graph only keeps blocking
    // dependency kinds, so the edge count is the blocking-edge count — which
    // is a different number from the issue-derived one `ConfigForSize` is
    // handed at main.go:4985-4991. Both are recomputed here from the JSONL so
    // a swap between them cannot pass.
    const BLOCKING: &[&str] = &["blocks", "conditional-blocks", "waits-for"];
    for (name, dir) in FIXTURES {
        let out = json_in(dir, &["--profile-startup", "--profile-json"]);
        let profile = &out["profile"];

        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("repo root")
            .join(dir)
            .join("issues.jsonl");
        let raw = std::fs::read_to_string(&path).expect("fixture issues.jsonl");
        let issues: Vec<Value> = raw
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("issue line is JSON"))
            .collect();
        let ids: std::collections::HashSet<&str> = issues
            .iter()
            .map(|i| i["id"].as_str().expect("issue id"))
            .collect();

        let node_count = issues.len() as u64;
        // A graph edge needs a blocking dependency kind AND a target that is
        // itself a node (bv-analysis `build_graph`:731-745).
        let edge_count: u64 = issues
            .iter()
            .flat_map(|i| i["dependencies"].as_array().into_iter().flatten())
            .filter(|d| {
                let kind = d["type"].as_str().unwrap_or("");
                // Go's DependencyType.parse maps "" to Blocks (legacy default).
                let kind = if kind.is_empty() { "blocks" } else { kind };
                BLOCKING.contains(&kind) && ids.contains(d["issue_id"].as_str().unwrap_or(""))
            })
            .count() as u64;

        assert_eq!(profile["node_count"].as_u64(), Some(node_count), "{name}");
        assert_eq!(profile["edge_count"].as_u64(), Some(edge_count), "{name}");

        // `density` is Go graph.go:1958-1963 — the GRAPH's, e / (n * (n-1)),
        // left at 0 for a single-node graph. Recomputed rather than read back
        // so a change to the formula cannot pass.
        let expected_density = if node_count > 1 {
            edge_count as f64 / (node_count as f64 * (node_count - 1) as f64)
        } else {
            0.0
        };
        let got = profile["density"].as_f64().expect("density");
        assert!(
            (got - expected_density).abs() <= expected_density * 1e-12,
            "{name} density: got {got}, want {expected_density}"
        );
    }
}

#[test]
fn profile_json_size_tiers_follow_the_config_boundaries() {
    // Go `ConfigForSize` (pkg/analysis/config.go:109-121) keys on node count at
    // 100 / 500 / 2000; the XL tier is the only one that drops cycles, so the
    // reported config has to move with the tier rather than being a constant.
    // The node count is asserted alongside so a fixture swap cannot make the
    // config assertions pass vacuously.
    for (name, dir) in FIXTURES {
        let out = json_in(dir, &["--profile-startup", "--profile-json"]);
        let profile = &out["profile"];
        let n = profile["node_count"].as_u64().expect("node_count");
        let config = &profile["config"];
        match *name {
            "small_chain" => {
                assert_eq!(n, 12, "{name} no longer exercises the <100 tier");
                assert_eq!(config["BetweennessMode"], "exact");
                assert_eq!(config["MaxCyclesToStore"], 1000);
                assert_eq!(config["BetweennessTimeout"], 2_000_000_000i64);
            }
            "medium_tree" => {
                assert_eq!(n, 121, "{name} no longer exercises the <500 tier");
                assert_eq!(config["BetweennessMode"], "exact");
                assert_eq!(config["MaxCyclesToStore"], 100);
                assert_eq!(config["BetweennessTimeout"], 500_000_000i64);
            }
            "large_cyclic_600" => {
                assert_eq!(n, 600, "{name} no longer exercises the <2000 tier");
                assert_eq!(config["MaxCyclesToStore"], 50);
                assert_eq!(config["BetweennessTimeout"], 500_000_000i64);
            }
            "xl_2500" => {
                assert_eq!(n, 2500, "{name} no longer exercises the XL tier");
                // ConfigForSize disables cycle detection above the XL threshold.
                assert_eq!(config["ComputeCycles"], false);
                assert_eq!(config["MaxCyclesToStore"], 10);
            }
            other => panic!("unhandled fixture {other}"),
        }
    }
}

#[test]
fn profile_json_force_full_analysis_uses_the_full_config() {
    // Go main.go:4984-4986 — `--force-full-analysis` replaces the tier config
    // with `FullAnalysisConfig` (config.go:230-254), whose 30s budgets and
    // 10000-cycle cap no size tier produces. That is what the small tier's
    // 2s / 1000 must become.
    for (_, dir) in FIXTURES {
        let plain = json_in(dir, &["--profile-startup", "--profile-json"]);
        let full = json_in(
            dir,
            &[
                "--profile-startup",
                "--profile-json",
                "--force-full-analysis",
            ],
        );
        let a = &plain["profile"]["config"];
        let b = &full["profile"]["config"];
        assert_eq!(b["BetweennessTimeout"], 30_000_000_000i64);
        assert_eq!(b["PageRankTimeout"], 30_000_000_000i64);
        assert_eq!(b["HITSTimeout"], 30_000_000_000i64);
        assert_eq!(b["CyclesTimeout"], 30_000_000_000i64);
        assert_eq!(b["MaxCyclesToStore"], 10000);
        if a != b {
            // The override must actually be visible on every tier, not just
            // the one where the numbers happen to differ.
            assert_ne!(a["BetweennessTimeout"], b["BetweennessTimeout"]);
        }
    }
}

#[test]
fn profile_json_recommendations_carry_at_least_the_timing_verdict() {
    // Go main.go:5143-5157 always appends exactly one of four timing
    // verdicts, so the array is never empty (and never the `null` a nil slice
    // would encode as).
    let out = json(&["--profile-startup", "--profile-json"]);
    let recs = out["recommendations"].as_array().expect("array, not null");
    assert!(!recs.is_empty(), "a startup timing is always reported");
    let first = recs[0].as_str().expect("string");
    assert!(
        [
            "✓ Startup within acceptable range (<500ms)",
            "✓ Startup acceptable (<1s)",
            "⚠ Startup is slow (1-2s)",
            "⚠ Startup is slow (1-2s) - if using --force-full-analysis, consider removing it",
            "⚠ Startup is very slow (>2s) - optimization recommended",
        ]
        .contains(&first),
        "unexpected timing verdict {first:?}"
    );
    for rec in recs {
        let rec = rec.as_str().expect("string");
        assert!(
            !rec.contains("cycle"),
            "the cycle warning only fires when cycle_count > 0: {rec:?}"
        );
    }
}

#[test]
fn profile_human_report_has_go_headings_and_phase_blocks() {
    let (code, stdout, stderr) = bvr(&["--profile-startup"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let lines: Vec<&str> = stdout.lines().collect();

    // Go main.go:5038-5050, in order. The `===============` rule is 15 '='.
    assert_eq!(lines[0], "Startup Profile");
    assert_eq!(lines[1], "===============");
    // `Data: %d issues, %d dependencies, density=%.4f` — the counts are the
    // issue list's, and `%.4f` means the density is truncated to four
    // decimals rather than printed at full f64 precision.
    assert!(lines[2].starts_with("Data: "), "got {:?}", lines[2]);
    let (_, counts) = lines[2].split_once("Data: ").expect("Data: prefix");
    let (issues_part, rest) = counts.split_once(" issues, ").expect("\"N issues, \"");
    let (deps_part, density) = rest
        .split_once(" dependencies, density=")
        .expect("\"N dependencies, density=\"");
    assert!(
        issues_part.parse::<u64>().is_ok(),
        "issue count {issues_part:?}"
    );
    assert!(
        deps_part.parse::<u64>().is_ok(),
        "dependency count {deps_part:?}"
    );
    let decimals = density.split_once('.').map(|(_, d)| d).unwrap_or("");
    assert_eq!(decimals.len(), 4, "`%.4f` density, got {density:?}");
    assert!(lines[3].is_empty(), "blank line after the data line");

    assert_eq!(lines[4], "Phase 1 (blocking):");
    // Each Phase 1 line is a literal prefix plus the 6-wide duration field
    // (main.go:5042-5047), so the padding before the number is a contract too.
    for (prefix, line) in [
        ("  Load JSONL:      ", &lines[5]),
        ("  Build graph:     ", &lines[6]),
        ("  Degree:          ", &lines[7]),
        ("  TopoSort:        ", &lines[8]),
        ("  Total Phase 1:   ", &lines[9]),
    ] {
        assert!(line.starts_with(prefix), "{line:?} starts with {prefix:?}");
        assert!(line.len() > prefix.len(), "{line:?} carries a duration");
    }
    assert!(lines[10].is_empty(), "blank line after phase 1");
    assert_eq!(
        lines[11],
        "Phase 2 (async in normal mode, sync for profiling):"
    );
    // printMetricLine / printCyclesLine left-justify `name + ":"` in 14
    // columns, so every metric line's label ends at the same offset.
    for (label, line) in [
        ("PageRank:", &lines[12]),
        ("Betweenness:", &lines[13]),
        ("Eigenvector:", &lines[14]),
        ("HITS:", &lines[15]),
        ("Critical Path:", &lines[16]),
        ("Cycles:", &lines[17]),
    ] {
        assert!(line.starts_with(&format!("  {label:<14}")), "{line:?}");
    }
    assert!(
        lines[18].starts_with("  Total Phase 2:   "),
        "{:?}",
        lines[18]
    );
    assert!(lines[19].is_empty(), "blank line after phase 2");
    assert!(
        lines[20].starts_with("Total startup:     "),
        "{:?}",
        lines[20]
    );
    assert!(lines[21].is_empty(), "blank line after the total");
    assert_eq!(lines[22], "Configuration:");
    assert!(
        lines[23].starts_with("  Size tier: Small (<100 issues)"),
        "{:?}",
        lines[23]
    );
    assert_eq!(lines[24], "  All metrics computed");
    assert!(lines[25].is_empty());
    assert_eq!(lines[26], "Recommendations:");
    assert!(lines[27].starts_with("  "), "{:?}", lines[27]);
}

#[test]
fn profile_human_report_reports_skipped_metrics_in_go_order() {
    // Go main.go:5059-5068 — the names come from `Config.SkippedMetrics()`,
    // which appends in a fixed order (Betweenness, PageRank, HITS, Cycles —
    // config.go:314-340), joined with ", ". `xl_2500` is the fixture whose
    // tier drops cycles.
    let (code, stdout, stderr) = run("tests/fixtures/xl_2500/.beads", &["--profile-startup"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stdout.contains("  Size tier: XL (>2000 issues)"),
        "{stdout}"
    );
    assert!(stdout.contains("  Skipped metrics: Cycles\n"), "{stdout}");
    assert!(
        stdout.contains("  Cycles:        [Skipped]"),
        "the metric line is [Skipped] in the same place the config is: {stdout}"
    );
    assert!(
        !stdout.contains("All metrics computed"),
        "a config with a skipped metric must not claim otherwise: {stdout}"
    );
}

#[test]
fn profile_json_without_profile_startup_is_rejected() {
    // Go flags.rs:1750 / main.go:1806-1810 pair `--profile-json` with
    // `--profile-startup`; the rule was already live before this handler, so
    // this asserts the validation still fires first and the new dispatch does
    // not swallow the flag.
    let (code, stdout, stderr) = bvr(&["--profile-json"]);
    assert_eq!(code, 1, "stdout: {stdout}");
    assert_eq!(
        stderr.trim_end(),
        "Error: --profile-json requires --profile-startup"
    );
    assert!(stdout.is_empty(), "nothing may reach stdout: {stdout}");
}

#[test]
fn profile_startup_does_not_open_the_tui() {
    // The regression this handler exists to fix: before it, `--profile-startup`
    // matched no dispatch, reached the interactive launcher and hung forever
    // with no output. It must now produce a report and exit 0 promptly.
    let out = Command::new(env!("CARGO_BIN_EXE_bvr"))
        .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .args(["--db", ".beads/issues.jsonl", "--profile-startup"])
        .output()
        .expect("binary runs");
    assert_eq!(out.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&out.stdout).starts_with("Startup Profile\n"),
        "the report, not a TUI escape sequence"
    );
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("launching TUI"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
