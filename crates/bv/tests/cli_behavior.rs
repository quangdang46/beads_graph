//! CLI behavior integration tests (Phase 3a acceptance).

fn run(args: &[&str]) -> (i32, String, String) {
    let bin = concat!(env!("CARGO_BIN_EXE_bvr"));
    // CARGO_BIN_EXE only available in integration tests via env — fallback:
    let _ = bin;
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_bvr"))
        .args(args)
        .output()
        .expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[test]
fn version_exits_zero() {
    // Go prints `bv <version>` from `pkg/version` (fallback v0.25.0 at the
    // parity commit), which is also what the robot envelope reports as
    // `version`. The Rust crate's own name and version must not leak here —
    // that would make `--version` contradict the binary's own output.
    let (code, stdout, _) = run(&["--version"]);
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "bv v0.25.0");
}

#[test]
fn modifier_violation_exits_one() {
    let (code, _, stderr) = run(&["--robot-diff"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("requires --diff-since"), "{stderr}");
}

#[test]
fn all_robot_primaries_are_dispatched_or_validated() {
    // Every robot primary in flags::ROBOT_PRIMARIES is either dispatched
    // (has a handler) or caught by modifier-requires validation.
    // Verify that --robot-drift without --check-drift triggers modifier
    // validation (exit 1) rather than the "not yet implemented" fallback.
    let (code, _, stderr) = run(&["--robot-drift"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("requires --check-drift"), "{stderr}");
}

#[test]
fn exclusive_primaries_exit_one() {
    let (code, _, stderr) = run(&["--robot-triage", "--robot-next"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("only one primary command"), "{stderr}");
}

#[test]
fn valid_triage_dispatches_and_exits_zero() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_bvr"))
        .arg("--robot-triage")
        .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .output()
        .expect("binary runs");
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"quick_ref\""), "{stdout}");
    assert!(stdout.contains("\"data_hash\""));
}

#[test]
fn argv_alias_triage_works() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_bvr"))
        .args(["triage"])
        .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .output()
        .expect("binary runs");
    assert_eq!(
        out.status.code(),
        Some(0),
        "alias rewrites to --robot-triage"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("quick_ref"));
}

#[test]
fn robot_help_lists_primaries() {
    let (code, stdout, _) = run(&["--robot-help"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("--robot-triage"));
    // The four structural sections Go emits (robot_registry.go:559-616). The
    // previous assertion looked for "exit 0=success", a line that only the old
    // stub printed — Go's output has never contained it — so it pinned the
    // stub rather than the contract.
    for section in [
        "Every payload carries:",
        "All robot commands:",
        "TUI Key Bindings:",
        "Run bvr --help for all options.",
    ] {
        assert!(stdout.contains(section), "missing {section:?} in --robot-help");
    }
    // The key-bindings table is generated from KEY_BINDING_DOCS; one binding
    // per category is enough to catch an empty or truncated table.
    for category in [
        "[Navigation]",
        "[Views]",
        "[Filters]",
        "[Actions]",
        "[Graph]",
        "[Board]",
        "[Tree]",
        "[Insights]",
        "[History]",
        "[Attention]",
        "[Sprint]",
    ] {
        assert!(stdout.contains(category), "missing {category:?}");
    }
}

/// Go `generateRobotCapabilities` (cmd/bv/main.go:7903-7953) emits a manifest of
/// what each command NEEDS, not a status ledger.
///
/// This test used to assert `implemented_count` and `total_count` — Rust-only
/// inventions, since removed. Asserting them meant the test could not pass
/// against Go's actual shape, the same defect as the old
/// `robot_related_builds_dependency_edges`, which asserted a `related` field
/// that `RelatedWorkResult` has never had.
#[test]
fn robot_capabilities_reports_the_fields_go_emits() {
    let (code, stdout, _) = run(&["--robot-capabilities"]);
    assert_eq!(code, 0);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");

    // Go's manifest-level keys (main.go:7944-7952).
    for key in [
        "tool",
        "version",
        "contract_version",
        "default_robot_command",
        "output_formats",
        "commands",
        "docs_topics",
        "schema_command",
        "agent_intent_aliases",
        "environment_variables",
        "exit_codes",
        "stream_contract",
    ] {
        assert!(parsed.get(key).is_some(), "missing manifest key: {key}");
    }
    // `tool` is the Go binary name; the port must not rename it to bvr.
    assert_eq!(parsed["tool"], "bv", "Go's manifest says bv");

    let commands = parsed["commands"].as_array().expect("commands array");
    assert!(!commands.is_empty());
    for c in commands {
        for key in [
            "name",
            "flag",
            "description",
            "preferred_invocation",
            "accepted_invocations",
            "needs_issues",
            "needs_git",
            "needs_sprint",
            "needs_baseline",
            "mutates_state",
        ] {
            assert!(c.get(key).is_some(), "command missing {key}: {c}");
        }
        // `status` is a Rust invention with no Go counterpart.
        assert!(c.get("status").is_none(), "invented field `status` on {c}");
    }

    let names: Vec<&str> = commands.iter().filter_map(|c| c["name"].as_str()).collect();
    assert!(names.contains(&"robot-triage"), "{names:?}");
    assert!(names.contains(&"robot-drift"), "{names:?}");
}

#[test]
fn robot_schema_unknown_command_exits_one_with_suggestions() {
    let (code, _, stderr) = run(&["--robot-schema", "--schema-command", "bogus-cmd"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("Unknown command"));
    assert!(stderr.contains("robot-triage"), "{stderr}");
}

#[test]
fn robot_docs_unknown_topic_emits_error_json() {
    // Go parity: unknown topics emit JSON with error+available_topics and exit 0.
    // `did_you_mean` only appears when the topic is close enough (Levenshtein);
    // "bogus-topic" is too far from any valid topic.
    let (code, stdout, _) = run(&["--robot-docs", "bogus-topic"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("\"error\""), "{stdout}");
    assert!(stdout.contains("\"available_topics\""), "{stdout}");
    // Close miss should get did_you_mean (Go: suggestClosest ≤3 for len≤10).
    let (code2, stdout2, _) = run(&["--robot-docs", "guied"]);
    assert_eq!(code2, 0);
    assert!(stdout2.contains("\"did_you_mean\""), "{stdout2}");
}

fn run_at_repo_root(args: &[&str]) -> (i32, String, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_bvr"))
        .args(args)
        .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .output()
        .expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[test]
fn robot_blocker_chain_unknown_issue_exits_one() {
    let (code, _, stderr) = run_at_repo_root(&["--robot-blocker-chain", "nonexistent-id-xyz"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("Issue not found"), "{stderr}");
}

#[test]
fn robot_correlation_stats_runs_without_crashing() {
    // This repo's real .beads + git history exercise the correlator
    // pipeline end-to-end (git log walk + explicit/temporal scoring).
    let (code, stdout, _) = run_at_repo_root(&["--robot-correlation-stats"]);
    assert_eq!(code, 0);
    // Go's handler (robot_registry.go:2805-2828) embeds correlation.FeedbackStats
    // and three envelope-shaped scalars; it does NOT embed RobotEnvelope, so
    // there is no data_hash / source_path / scope_hash, and none of the
    // per-correlation counters the old assertion looked for.
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    for key in [
        "total_feedback",
        "confirmed",
        "rejected",
        "ignored",
        "accuracy_rate",
        "avg_confirm_conf",
        "avg_reject_conf",
        "generated_at",
        "output_format",
        "version",
    ] {
        assert!(parsed.get(key).is_some(), "missing {key} in {stdout}");
    }
    assert!(parsed.get("data_hash").is_none(), "no envelope: {stdout}");
    assert!(parsed.get("stats").is_none(), "no nested stats: {stdout}");
    // total_feedback is the sum of the three buckets.
    let buckets = ["confirmed", "rejected", "ignored"]
        .iter()
        .map(|k| parsed[*k].as_u64().unwrap())
        .sum::<u64>();
    assert_eq!(parsed["total_feedback"].as_u64().unwrap(), buckets);
}

#[test]
fn robot_file_hotspots_runs_without_crashing() {
    let (code, stdout, _) = run_at_repo_root(&["--robot-file-hotspots"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("\"hotspots\""));
}

#[test]
fn robot_causality_runs_for_real_bead() {
    let (code, stdout, _) =
        run_at_repo_root(&["--robot-causality", "beads_viewer_rust-api-freeze-b73"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("\"chain\""));
    assert!(stdout.contains("\"insights\""));
    assert!(stdout.contains("\"commit_count\""));
}

#[test]
fn robot_causality_unknown_bead_exits_one() {
    let (code, _, stderr) = run_at_repo_root(&["--robot-causality", "nonexistent-xyz"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("Bead not found"), "{stderr}");
}

#[test]
fn robot_related_builds_dependency_edges() {
    // Subject choice: `beads_viewer_rust-api-freeze-b73` is present in this
    // repo's `.beads/issues.jsonl`, survives into the correlation report (so
    // `FindRelatedWorkAt` returns non-nil rather than "Bead not found"), and
    // genuinely carries dependency edges — it depends on
    // `beads_viewer_rust-fort-epic-u31` and `beads_viewer_rust-phase0-scaffold-xbe`.
    //
    // Every dependency edge in this repo points at a *closed* bead, and Go's
    // `findDependencyCluster` drops closed candidates unless IncludeClosed is
    // set (related.go). So the edge assertion needs `--related-include-closed`;
    // without it the detector correctly returns Go's nil slice, i.e. `null`.
    let (code, stdout, _) = run_at_repo_root(&[
        "--robot-related",
        "beads_viewer_rust-api-freeze-b73",
        "--related-include-closed",
    ]);
    assert_eq!(code, 0);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");

    // Go's RelatedWorkResult field names (related.go:37-46). There is no
    // `related` and no `bead_id` key — the identity fields are `target_bead_id`
    // and `target_title`.
    assert_eq!(parsed["target_bead_id"], "beads_viewer_rust-api-freeze-b73");
    assert_eq!(
        parsed["target_title"],
        "Phase 0.5: API Contract Freeze (api-freeze-v1)"
    );
    for key in [
        "file_overlap",
        "commit_overlap",
        "dependency_cluster",
        "concurrent",
    ] {
        assert!(parsed.get(key).is_some(), "missing {key} in {stdout}");
    }

    // The bead has two real dependency edges, so the cluster is non-empty.
    let cluster = parsed["dependency_cluster"].as_array().expect("array");
    assert!(
        !cluster.is_empty(),
        "should find at least one dependency edge: {stdout}"
    );
    let ids: Vec<&str> = cluster
        .iter()
        .map(|b| b["bead_id"].as_str().unwrap())
        .collect();
    assert!(
        ids.contains(&"beads_viewer_rust-fort-epic-u31"),
        "direct dependency missing from {ids:?}"
    );
    for entry in cluster {
        assert_eq!(entry["relation_type"], "dependency_cluster");
        assert!(entry["relevance"].as_i64().unwrap() >= 20);
    }

    // `total_related` is the sum of the four category lengths.
    let total: usize = [
        "file_overlap",
        "commit_overlap",
        "dependency_cluster",
        "concurrent",
    ]
    .iter()
    .map(|k| parsed[*k].as_array().map(Vec::len).unwrap_or(0))
    .sum();
    assert_eq!(parsed["total_related"].as_u64().unwrap() as usize, total);
}

#[test]
fn robot_related_default_excludes_closed_candidates() {
    // Same bead, no `--related-include-closed`: Go's nil accumulator must
    // serialize as `null`, not `[]`, and the dependency cluster must be empty
    // because every one of this repo's dependency targets is closed.
    let (code, stdout, _) =
        run_at_repo_root(&["--robot-related", "beads_viewer_rust-api-freeze-b73"]);
    assert_eq!(code, 0);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    assert_eq!(parsed["target_bead_id"], "beads_viewer_rust-api-freeze-b73");
    assert!(
        parsed["dependency_cluster"].is_null(),
        "closed dependencies are excluded by default: {stdout}"
    );
}

#[test]
fn robot_impact_network_all_returns_full_network() {
    let (code, stdout, _) = run_at_repo_root(&["--robot-impact-network", "all"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("\"network\""));
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    // Go's `ImpactNetworkResult` (network.go:845) counts in `stats`, not in a
    // top-level `node_count` / `edge_count`.
    assert!(parsed["stats"]["total_nodes"].as_u64().unwrap() > 0);
    assert!(parsed["stats"]["total_edges"].as_u64().unwrap() > 0);
    assert_eq!(
        parsed["stats"]["total_nodes"],
        parsed["network"]["nodes"].as_object().unwrap().len() as u64,
        "stats.total_nodes counts the emitted nodes"
    );
}

#[test]
fn robot_sprint_list_handles_no_sprints_jsonl() {
    // This repo has no .beads/sprints.jsonl; sprint-list must succeed
    // with an empty array, not crash.
    let (code, stdout, _) = run_at_repo_root(&["--robot-sprint-list"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("\"sprint_count\""));
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    assert_eq!(parsed["sprints"].as_array().unwrap().len(), 0);
}

#[test]
fn robot_sprint_show_unknown_sprint_exits_one() {
    let (code, _, stderr) = run_at_repo_root(&["--robot-sprint-show", "nonexistent-sprint"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("Sprint not found"), "{stderr}");
}

#[test]
fn robot_burndown_no_active_sprint_exits_one() {
    // The sprint selector is a required value: Go's flag package rejects a bare
    // `--robot-burndown` before the command runs, verified against the oracle.
    // Passing it explicitly is what reaches the no-sprint path.
    let (code, _, stderr) = run_at_repo_root(&["--robot-burndown", "current"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("No active sprint found"), "{stderr}");
}

#[test]
fn robot_burndown_without_value_reports_the_missing_argument() {
    let (code, _, stderr) = run_at_repo_root(&["--robot-burndown"]);
    assert_eq!(code, 1);
    assert!(
        stderr.contains("flag needs an argument: --robot-burndown"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("No active sprint found"),
        "the flag error must replace the sprint lookup: {stderr}"
    );
}

#[test]
fn robot_capacity_runs_without_crashing() {
    // Go robot_registry.go:3651-3682 emits the fields at the envelope TOP
    // LEVEL; there is no nested `capacity` object and no `open_count` key.
    let (code, stdout, _) = run_at_repo_root(&["--robot-capacity"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("\"open_issue_count\""), "{stdout}");
    assert!(stdout.contains("\"agents\""), "{stdout}");
    assert!(!stdout.contains("\"capacity\":"), "{stdout}");
}

#[test]
fn robot_explain_correlation_bad_format_exits_two() {
    let (code, _, stderr) =
        run_at_repo_root(&["--robot-explain-correlation", "not-a-valid-format"]);
    // Go's handler returns the parse error, so the dispatcher prints
    // `Error handling <flag>: <err>` and exits 1 — not a usage exit of 2.
    assert_eq!(code, 1);
    assert!(
        stderr.contains(
            "Error handling --robot-explain-correlation: expected format: SHA:beadID, got: \"not-a-valid-format\""
        ),
        "{stderr}"
    );
}

#[test]
fn robot_search_ranks_and_respects_limit() {
    let (code, stdout, _) = run_at_repo_root(&[
        "--robot-search",
        "--search",
        "triage",
        "--search-limit",
        "3",
    ]);
    assert_eq!(code, 0);
    assert!(stdout.contains("\"results\""));
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    assert_eq!(parsed["query"], "triage");
    let results = parsed["results"].as_array().expect("results array");
    assert!(results.len() <= 3, "must respect --search-limit");
}

#[test]
fn robot_search_missing_query_caught_by_modifier_requires() {
    // --robot-search without --search is rejected by the shared
    // modifier-requires validator before it ever reaches the handler.
    let (code, _, stderr) = run(&["--robot-search"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("requires --search"), "{stderr}");
}

#[test]
fn robot_search_unknown_preset_exits_one() {
    // Go presets.go:63 reports `unknown preset %q` for the LOWERCASED name and
    // main.go:2833 exits 1 — it is a resolveSearchConfig failure, not a usage
    // error.
    let (code, _, stderr) = run_at_repo_root(&[
        "--robot-search",
        "--search",
        "x",
        "--search-mode",
        "hybrid",
        "--search-preset",
        "BOGUS-PRESET",
    ]);
    assert_eq!(code, 1);
    assert!(
        stderr.contains(r#"unknown preset "bogus-preset""#),
        "{stderr}"
    );
}

#[test]
fn robot_search_non_integer_limit_is_a_flag_parse_error() {
    // pflag owns integer flags, so a non-integer never reaches the handler:
    // main.go:4548 prints `invalid argument %q for %q flag: %v` and exits 1.
    let (code, _, stderr) =
        run_at_repo_root(&["--robot-search", "--search", "x", "--search-limit", "abc"]);
    assert_eq!(code, 1);
    assert!(
        stderr.contains(r#"invalid argument "abc" for "--search-limit" flag: strconv.ParseInt: parsing "abc": invalid syntax"#),
        "{stderr}"
    );
}

/// A minimal beads repo with one labelled issue, for the scoping tests below.
/// Returns the directory; the caller must keep it alive for the process run.
fn labelled_fixture_repo(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("bvr_cli_scope_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".beads")).unwrap();
    std::fs::write(
        dir.join(".beads").join("issues.jsonl"),
        concat!(
            r#"{"id":"L-1","title":"One","status":"open","priority":1,"issue_type":"task","#,
            r#""labels":["cli"],"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","dependencies":[]}"#,
            "\n",
        ),
    )
    .unwrap();
    dir
}

#[test]
fn envelope_scope_sits_between_source_kind_and_source_authority() {
    // Go's RobotEnvelope declares Scope between SourceKind/AsOfCommit and
    // SourceAuthority (cmd/bv/main.go:7213-7229); serde preserves insertion
    // order, so a wrong position is a byte diff on every scoped invocation.
    let dir = labelled_fixture_repo("order");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_bvr"))
        .args(["--robot-plan", "--label", "cli"])
        .current_dir(&dir)
        .output()
        .expect("binary runs");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.status.code(), Some(0));
    let parsed: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("valid json");
    let keys: Vec<&str> = parsed
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    let source_kind = keys
        .iter()
        .position(|k| *k == "source_kind")
        .expect("present");
    let scope = keys.iter().position(|k| *k == "scope").expect("present");
    let source_authority = keys
        .iter()
        .position(|k| *k == "source_authority")
        .expect("present");
    assert!(
        source_kind < scope && scope < source_authority,
        "expected source_kind < scope < source_authority, got {keys:?}"
    );
}

#[test]
fn unscoped_run_omits_scope_and_label_scope_entirely() {
    let dir = labelled_fixture_repo("unscoped");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_bvr"))
        .args(["--robot-plan"])
        .current_dir(&dir)
        .output()
        .expect("binary runs");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.status.code(), Some(0));
    let parsed: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("valid json");
    let obj = parsed.as_object().expect("object");
    assert!(!obj.contains_key("scope"), "{obj:?}");
    assert!(!obj.contains_key("label_scope"), "{obj:?}");
    assert!(!obj.contains_key("label_context"), "{obj:?}");
}

#[test]
fn label_scope_and_context_appear_on_plan_and_priority_but_not_next() {
    // Go emits label_scope/label_context on exactly three commands
    // (robot_registry.go:893, :1009, :1985): plan, priority, insights. Every
    // other command — including next, which also accepts --label — gets its
    // scoping only through the envelope's scope.label.
    for cmd in ["--robot-plan", "--robot-priority"] {
        let dir = labelled_fixture_repo("emits");
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_bvr"))
            .args([cmd, "--label", "cli"])
            .current_dir(&dir)
            .output()
            .expect("binary runs");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(out.status.code(), Some(0), "{cmd}");
        let parsed: serde_json::Value =
            serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("valid json");
        assert_eq!(parsed["label_scope"], "cli", "{cmd}: {parsed}");
        assert_eq!(parsed["label_context"]["label"], "cli", "{cmd}: {parsed}");
        assert_eq!(parsed["label_context"]["issue_count"], 1, "{cmd}: {parsed}");
    }

    let dir = labelled_fixture_repo("next");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_bvr"))
        .args(["--robot-next", "--label", "cli"])
        .current_dir(&dir)
        .output()
        .expect("binary runs");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.status.code(), Some(0));
    let parsed: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("valid json");
    let obj = parsed.as_object().expect("object");
    assert!(!obj.contains_key("label_scope"), "{obj:?}");
    assert!(!obj.contains_key("label_context"), "{obj:?}");
}

#[test]
fn robot_metrics_emits_timing_and_cache_entries() {
    let (code, stdout, _) = run(&["--robot-metrics"]);
    assert_eq!(code, 0);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    assert!(parsed["timing"].is_array(), "timing must be array");
    assert!(parsed["cache"].is_array(), "cache must be array");
    assert!(parsed["memory"].is_object(), "memory must be object");
    let timing = parsed["timing"].as_array().unwrap();
    assert!(!timing.is_empty(), "at least one timing metric");
}

/// Go `pkg/loader/loader.go:918` wraps the file parse in
/// `metrics.Timer(metrics.LoaderParse)`, and `AllTimingStats`
/// (`pkg/metrics/timing.go:252`) appends a metric only when
/// `m.Count() > 0`. So `--robot-metrics` reports exactly the one metric the
/// load path instrumented — the 13 other registered names are idle and are
/// dropped, not reported as zero rows.
#[test]
fn robot_metrics_reports_only_instrumented_timing_metrics() {
    let (code, stdout, _) = run(&["--robot-metrics"]);
    assert_eq!(code, 0);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    let timing = parsed["timing"].as_array().expect("timing array");
    let names: Vec<&str> = timing
        .iter()
        .map(|t| t["name"].as_str().expect("name"))
        .collect();
    assert_eq!(names, vec!["loader.parse"], "timing must be Go's one entry");
    // Go's TimingStats field set (pkg/metrics/timing.go:159).
    let entry = &timing[0];
    assert_eq!(entry["count"], 1);
    for field in ["total_ms", "avg_ms", "max_ms", "min_ms"] {
        assert!(entry[field].is_number(), "{field} must be numeric: {entry}");
    }
    assert!(
        entry["avg_ms"].as_f64().unwrap() > 0.0,
        "measured, not stubbed"
    );
}

/// Go `pkg/metrics/cache.go:128 AllCacheStats` reports all four registered
/// caches unconditionally — "a reader of --robot-metrics should see which
/// caches exist and that they were idle, not an empty list that looks like
/// 'no caches'". Order is Go's `AllCacheMetrics` order.
#[test]
fn robot_metrics_cache_reports_go_registry_in_order() {
    let (code, stdout, _) = run(&["--robot-metrics"]);
    assert_eq!(code, 0);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    let cache = parsed["cache"].as_array().expect("cache array");
    let names: Vec<&str> = cache
        .iter()
        .map(|c| c["name"].as_str().expect("name"))
        .collect();
    assert_eq!(
        names,
        vec![
            "graph_cache",
            "triage_cache",
            "search_cache",
            "correlation_cache"
        ],
        "cache registry must match Go's four, in Go's order"
    );
    // Go's CacheStats field set (pkg/metrics/cache.go:94).
    for entry in cache {
        for field in ["hits", "misses", "total", "hit_rate"] {
            assert!(entry[field].is_number(), "{field} must be numeric: {entry}");
        }
        assert_eq!(
            entry["total"].as_u64(),
            Some(entry["hits"].as_u64().unwrap() + entry["misses"].as_u64().unwrap()),
            "total must equal hits + misses: {entry}"
        );
    }
}

/// `BV_METRICS=0` disables collection in Go (`pkg/metrics/timing.go:32`),
/// which leaves `AllTimingStats` empty; `omitempty` on `MetricsOutput.Timing`
/// then drops the key. `AllCacheStats` is unconditional, so the four caches
/// still report — as idle rows, not as an absent key.
#[test]
fn robot_metrics_omits_timing_when_collection_disabled() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_bvr"))
        .args(["--robot-metrics"])
        .env("BV_METRICS", "0")
        .output()
        .expect("binary runs");
    assert_eq!(out.status.code(), Some(0));
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid json");
    assert!(
        parsed.get("timing").is_none(),
        "timing must be omitted, not an empty array: {parsed}"
    );
    assert_eq!(
        parsed["cache"].as_array().map(|c| c.len()),
        Some(4),
        "cache registry is reported even with collection disabled"
    );
}

/// Issue #5 regression: `--generate-docs` and `--export` are registered flags,
/// so they must terminate with a real result. Before these handlers existed
/// they fell through to the TUI launcher — which hangs in any TTY and is the
/// footgun AGENTS.md warns about ("NEVER run bare bv/bvr").
#[test]
fn generate_docs_exits_zero_without_launching_tui() {
    let (code, stdout, stderr) = run(&["--generate-docs"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(!stderr.contains("launching TUI"), "{stderr}");
    assert!(stdout.contains("Generated docs"), "{stdout}");
}

#[test]
fn export_writes_a_report_and_exits_zero() {
    let out_path = std::env::temp_dir().join("bvr-export-regression.md");
    let _ = std::fs::remove_file(&out_path);
    let (code, stdout, stderr) = run(&["--export", out_path.to_str().unwrap()]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(!stderr.contains("launching TUI"), "{stderr}");
    assert!(stdout.contains("Exported"), "{stdout}");
    let written = std::fs::read_to_string(&out_path).expect("report written");
    // Go's `ResolveReportOptions` seeds Title with "Beads Export"
    // (pkg/export/markdown.go:50); "Beads Report" was never a Go value.
    assert!(written.starts_with("# Beads Export"), "{}", &written[..40]);
    let _ = std::fs::remove_file(&out_path);
}

/// Go's section order, from `GenerateMarkdown` (pkg/export/markdown.go:359).
/// `--export-md` forces `format=markdown`, which keeps the graph block on.
#[test]
fn export_md_writes_go_section_order() {
    let out_path = std::env::temp_dir().join("bvr-export-md-order.md");
    let _ = std::fs::remove_file(&out_path);
    let (code, _, stderr) = run(&["--export-md", out_path.to_str().unwrap()]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let written = std::fs::read_to_string(&out_path).expect("report written");
    let order = ["## Summary", "## Table of Contents", "## Dependency Graph"];
    let mut cursor = 0;
    for section in order {
        let at = written[cursor..]
            .find(section)
            .unwrap_or_else(|| panic!("missing or out-of-order section {section}"));
        cursor += at + section.len();
    }
    assert!(
        written.contains("| Metric | Count |\n|--------|-------|"),
        "{written}"
    );
    let _ = std::fs::remove_file(&out_path);
}

#[test]
fn export_rejects_conflicting_output_paths() {
    let (code, _, stderr) = run(&["--export", "/tmp/a.md", "--export-md", "/tmp/b.md"]);
    assert_eq!(code, 2, "stderr: {stderr}");
    assert!(stderr.contains("conflicting output paths"), "{stderr}");
}

#[test]
fn export_rejects_unknown_format() {
    let (code, _, stderr) = run(&["--export", "/tmp/x", "--export-format", "bogus"]);
    assert_eq!(code, 2, "stderr: {stderr}");
    // Go `ReportOptions.validate` (pkg/export/markdown.go:78).
    assert!(
        stderr.contains("export format \"bogus\" must be markdown, json, csv or mermaid"),
        "{stderr}"
    );
}
