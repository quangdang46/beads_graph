//! `--robot-capabilities` manifest parity with Go bv v0.25.0.
//!
//! The manifest is a *static* hand-authored table in Go
//! (`cmd/bv/main.go:7652` `robotCommandDocs`, plus the example-form
//! transforms at `:8050`/`:8079`/`:8095`), so the only way it stays correct
//! is to compare the whole thing against the oracle rather than spot-check a
//! few fields. `fixtures/robot_capabilities_go_v0.25.0.json` is the Go
//! binary's own `--robot-capabilities` output at the parity commit, with the
//! `generated_at` timestamp removed; this test asserts the Rust manifest is
//! equal to it field for field.
//!
//! Regenerate the fixture only against a Go build of v0.25.0:
//!
//! ```sh
//! ./beads_viewer/.bv-go --robot-capabilities \
//!   | python3 -c 'import json,sys; d=json.load(sys.stdin); d.pop("generated_at"); print(json.dumps(d,indent=2,sort_keys=True))' \
//!   > crates/bv/tests/fixtures/robot_capabilities_go_v0.25.0.json
//! ```

use serde_json::Value;

fn capabilities() -> Value {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_bvr"))
        .args(["--robot-capabilities"])
        .output()
        .expect("binary runs");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("stdout is JSON")
}

fn command<'a>(caps: &'a Value, name: &str) -> &'a Value {
    caps["commands"]
        .as_array()
        .expect("commands is an array")
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("no command named {name}"))
}

#[test]
fn manifest_matches_go_v0_25_0_field_for_field() {
    let mut got = capabilities();
    // The only field allowed to differ: Go stamps a wall-clock timestamp.
    got.as_object_mut()
        .expect("manifest is an object")
        .remove("generated_at");
    assert!(
        got.get("generated_at").is_none(),
        "sanity: the timestamp was removed, not the whole key set"
    );

    let want: Value =
        serde_json::from_str(include_str!("fixtures/robot_capabilities_go_v0.25.0.json"))
            .expect("fixture is valid JSON");

    assert_eq!(
        got, want,
        "--robot-capabilities drifted from Go v0.25.0 (beads_viewer/cmd/bv/main.go:7903)"
    );
}

#[test]
fn command_table_has_go_s_forty_one_rows_sorted_by_name() {
    let caps = capabilities();
    let commands = caps["commands"].as_array().expect("commands is an array");

    assert_eq!(
        commands.len(),
        41,
        "Go's robotCommandDocs() has 41 entries; a count change means a row \
         was dropped or duplicated in the transcription"
    );

    let names: Vec<&str> = commands
        .iter()
        .map(|c| c["name"].as_str().expect("name is a string"))
        .collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "Go does sort.Strings(names) before emitting");

    // A duplicate name would still sort cleanly, so check the set too.
    let unique: std::collections::BTreeSet<&str> = names.iter().copied().collect();
    assert_eq!(unique.len(), names.len(), "duplicate command name");
}

#[test]
fn no_entry_invents_a_status_field() {
    // Go's entry has no `status` key. An earlier Rust version emitted
    // "implemented"/"not_implemented" derived from a local dispatch list;
    // that field is not part of the contract and must not reappear.
    for c in capabilities()["commands"].as_array().expect("array") {
        assert!(
            c.get("status").is_none(),
            "{} invents a `status` field Go does not emit",
            c["name"]
        );
    }
}

#[test]
fn self_descriptions_name_the_installed_binary() {
    let caps = capabilities();
    assert_eq!(caps["tool"], "bvr");
    assert_eq!(caps["default_robot_command"], "bvr --robot-triage");
    assert_eq!(caps["schema_command"], "bvr --robot-schema");
    assert_eq!(caps["output_formats"], serde_json::json!(["json", "toon"]));
    assert_eq!(caps["version"], "v0.25.0");
    assert_eq!(caps["contract_version"], "1.0.0");
}

#[test]
fn flag_carries_the_placeholder_the_accepted_invocations_use() {
    // `robotFlagExampleForm` (main.go:8095) rewrites `<id>` to `ISSUE_ID`,
    // which is why the flag and both invocations agree on the placeholder.
    let caps = capabilities();
    for name in [
        "robot-blocker-chain",
        "robot-causality",
        "robot-related",
        "robot-sprint-show",
        "robot-forecast",
        "robot-burndown",
        "robot-impact",
        "robot-impact-network",
        "robot-file-beads",
        "robot-file-relations",
        "robot-docs",
        "robot-explain-correlation",
    ] {
        let flag = command(&caps, name)["flag"]
            .as_str()
            .expect("flag is a string");
        assert!(
            !flag.contains('<'),
            "{name} flag {flag} still carries an unexpanded <placeholder>"
        );
    }

    assert_eq!(
        command(&caps, "robot-blocker-chain")["flag"],
        "--robot-blocker-chain ISSUE_ID"
    );
    assert_eq!(
        command(&caps, "robot-related")["preferred_invocation"],
        "bvr robot-related ISSUE_ID --json"
    );
    // The per-command pre-pass wins over the generic table: `--forecast-sprint
    // <id>` names a sprint, not a bead.
    assert_eq!(
        command(&caps, "robot-forecast")["params"][1],
        "--forecast-sprint SPRINT_ID"
    );
    assert_eq!(
        command(&caps, "robot-sprint-show")["flag"],
        "--robot-sprint-show SPRINT_ID"
    );
}

#[test]
fn accepted_invocations_use_the_override_lists_where_go_has_them() {
    let caps = capabilities();

    // Two entries have an override list that shares no element with the
    // default `bvr <flag> --format json` / `bvr <name> ... --json` pair.
    assert_eq!(
        command(&caps, "robot-search")["accepted_invocations"],
        serde_json::json!([
            "bvr --search \"login oauth\" --robot-search --format json",
            "bvr robot-search \"login oauth\" --json",
            "bvr search \"login oauth\" --json",
        ])
    );
    assert_eq!(
        command(&caps, "robot-help")["accepted_invocations"],
        serde_json::json!(["bvr robot-help --json", "bvr robot-docs guide --json"])
    );
    assert_eq!(
        command(&caps, "robot-drift")["preferred_invocation"],
        "bvr --check-drift --robot-drift --format json"
    );
    // Everything else gets the two-element default.
    assert_eq!(
        command(&caps, "robot-triage")["accepted_invocations"],
        serde_json::json!([
            "bvr --robot-triage --format json",
            "bvr robot-triage --json",
        ])
    );
}

#[test]
fn key_fields_and_params_are_omitted_not_emptied() {
    // main.go:7921 and :7924 guard both keys on a non-empty slice, so a
    // command with neither must not carry the key at all.
    let caps = capabilities();
    for name in ["robot-triage", "robot-metrics", "robot-help"] {
        let c = command(&caps, name);
        if c.get("key_fields").is_some() {
            assert!(
                !c["key_fields"].as_array().expect("array").is_empty(),
                "{name} emits an empty key_fields array instead of omitting it"
            );
        }
        if c.get("params").is_some() {
            assert!(
                !c["params"].as_array().expect("array").is_empty(),
                "{name} emits an empty params array instead of omitting it"
            );
        }
    }

    // robot-alerts has key fields but no `params` in Go... it has params, so
    // pick the two directions explicitly instead.
    assert!(command(&caps, "robot-metrics").get("key_fields").is_none());
    assert!(command(&caps, "robot-triage").get("params").is_some());
}

#[test]
fn needs_and_mutates_flags_match_the_table() {
    let caps = capabilities();
    // Spot-checks across every column, including the two commands that mutate
    // state and the two that need neither issues nor git.
    assert_eq!(command(&caps, "robot-drift")["needs_baseline"], true);
    assert_eq!(command(&caps, "robot-burndown")["needs_sprint"], true);
    assert_eq!(command(&caps, "robot-history")["needs_git"], true);
    assert_eq!(command(&caps, "robot-history")["needs_issues"], true);
    assert_eq!(
        command(&caps, "robot-confirm-correlation")["mutates_state"],
        true
    );
    assert_eq!(
        command(&caps, "robot-reject-correlation")["mutates_state"],
        true
    );
    // `needs_issues` is false for these four in Go, not true-by-default.
    for name in [
        "robot-capabilities",
        "robot-recipes",
        "robot-schema",
        "robot-docs",
        "robot-help",
        "robot-correlation-stats",
    ] {
        assert_eq!(
            command(&caps, name)["needs_issues"],
            false,
            "{name} should not need issues"
        );
    }
}

#[test]
fn top_level_support_tables_are_present() {
    let caps = capabilities();
    assert_eq!(
        caps["docs_topics"],
        serde_json::json!(["guide", "commands", "examples", "env", "exit-codes", "all"])
    );
    assert_eq!(
        caps["exit_codes"],
        serde_json::json!({
            "0": "Success",
            "1": "Error (general failure, drift critical)",
            "2": "Invalid arguments or drift warning",
        })
    );
    let env = caps["environment_variables"].as_object().expect("object");
    assert_eq!(env.len(), 11, "Go's robotEnvVars() has 11 entries");
    for key in [
        "BEADS_DB",
        "BEADS_DIR",
        "BV_OUTPUT_FORMAT",
        "TOON_DEFAULT_FORMAT",
        "TOON_STATS",
        "TOON_KEY_FOLDING",
        "TOON_INDENT",
        "BV_PRETTY_JSON",
        "BV_ROBOT",
        "BV_SEARCH_MODE",
        "BV_SEARCH_PRESET",
    ] {
        assert!(env.contains_key(key), "missing env var {key}");
    }
}

#[test]
fn agent_intent_aliases_map_near_misses_to_canonical_forms() {
    let caps = capabilities();
    let aliases = caps["agent_intent_aliases"]
        .as_array()
        .expect("agent_intent_aliases is an array");

    assert_eq!(
        aliases.len(),
        18,
        "Go's agentIntentAliasDocs() has 18 entries"
    );
    for alias in aliases {
        let keys: Vec<&String> = alias.as_object().expect("object").keys().collect();
        assert_eq!(keys, vec!["agent_instinct", "canonical"]);
    }
    // Every canonical form must name `bvr`, the binary this project installs.
    // A `bv` here is what Go printed, and an agent that followed it literally
    // ran a command that does not exist — `install.sh` ships `bvr` alone.
    for alias in aliases {
        let canonical = alias["canonical"].as_str().expect("string");
        assert!(
            canonical.starts_with("bvr ") || canonical.starts_with("bvr\t"),
            "canonical form {canonical:?} does not name the bvr tool"
        );
        // Catch a half-rewritten form like `bvr --robot-triage --json` that
        // still carries a bare `bv` token further along (env prefixes, `bv@`,
        // or a stale subcommand) rather than only at the head.
        assert!(
            !canonical.split_whitespace().any(|tok| tok == "bv"),
            "canonical form still names the Go tool: {canonical}"
        );
    }
    assert_eq!(
        aliases[0],
        serde_json::json!({
            "agent_instinct": "bvr --json",
            "canonical": "bvr --robot-triage --format json",
        })
    );
}
