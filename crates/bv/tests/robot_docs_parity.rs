//! `--robot-docs` payload parity with Go bv v0.25.0.
//!
//! The payload is a static hand-authored table in Go (`generateRobotDocs`,
//! cmd/bv/main.go:8143, over `robotCommandDocs` at :7612), so the only way it
//! stays correct is to compare the whole thing against the oracle rather than
//! spot-check a few fields. `fixtures/robot_docs_go_v0.25.0.json` is the Go
//! binary's own `--robot-docs all` output at the parity commit with
//! `generated_at` removed; this asserts the Rust payload is equal to it.
//!
//! Byte order matters as much as content, and is the reason the comparison is
//! not just structural: Go builds the topic, the guide, `guide.output_modes`,
//! each example, the env table and the exit-code table as `map[...]`, which
//! `encoding/json` sorts at every level. `commands` is
//! `map[string]robotCommandDoc`, so the command NAMES sort but each doc is a
//! struct and keeps its declaration order.
//!
//! Regenerate the fixture only against a Go build of v0.25.0:
//!
//! ```sh
//! ./beads_viewer/.bv-go --robot-docs all \
//!   | python3 -c 'import json,sys; d=json.load(sys.stdin); d.pop("generated_at"); print(json.dumps(d,indent=2,sort_keys=True))' \
//!   > crates/bv/tests/fixtures/robot_docs_go_v0.25.0.json
//! ```

use serde_json::Value;

fn docs(topic: &str) -> String {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_bvr"))
        .args(["--robot-docs", topic])
        .output()
        .expect("binary runs");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn go_all() -> Value {
    serde_json::from_str(include_str!("fixtures/robot_docs_go_v0.25.0.json"))
        .expect("fixture is valid JSON")
}

#[test]
fn docs_all_matches_go_v0_25_0_field_for_field() {
    let mut got: Value = serde_json::from_str(&docs("all")).expect("stdout is JSON");
    got.as_object_mut()
        .expect("payload is an object")
        .remove("generated_at");

    let want = go_all();
    let want_sections: Vec<&str> = [
        "guide",
        "commands",
        "examples",
        "environment_variables",
        "exit_codes",
    ]
    .into_iter()
    .collect();
    for section in want_sections {
        assert_eq!(
            got[section], want[section],
            "`{section}` diverges from Go v0.25.0"
        );
    }
    // The topic selector and the format/version stamps are the rest of the
    // object; nothing else may appear.
    let got_keys: Vec<&String> = got.as_object().unwrap().keys().collect();
    let mut sorted_got = got_keys.clone();
    sorted_got.sort();
    let mut sorted_want: Vec<&String> = want.as_object().unwrap().keys().collect();
    sorted_want.sort();
    assert_eq!(sorted_got, sorted_want, "top-level key set");
}

#[test]
fn docs_all_orders_keys_the_way_go_maps_and_structs_do() {
    // The structural comparison above cannot see key ORDER, which is a byte
    // diff on every payload: Go's maps sort, `serde_json` is built with
    // `preserve_order` and would leak the builder's insertion order.
    //
    // `serde_json` here also has `preserve_order`, so re-parsing the raw bytes
    // preserves the order the binary actually emitted.
    let got: Value = serde_json::from_str(&docs("all")).expect("stdout is JSON");

    fn keys(v: &Value) -> Vec<&str> {
        v.as_object().unwrap().keys().map(String::as_str).collect()
    }
    fn assert_sorted(v: &Value, what: &str) {
        let k = keys(v);
        let mut sorted = k.clone();
        sorted.sort_unstable();
        assert_eq!(k, sorted, "`{what}` must be sorted like a Go map");
    }

    // Go maps: the topic, the guide, guide.output_modes, the env table, the
    // exit-code table, and each example / agent_intent_aliases entry.
    assert_sorted(&got, "top level");
    assert_sorted(&got["guide"], "guide");
    assert_sorted(&got["guide"]["output_modes"], "guide.output_modes");
    assert_sorted(&got["environment_variables"], "environment_variables");
    assert_sorted(&got["exit_codes"], "exit_codes");
    for (i, ex) in got["examples"].as_array().unwrap().iter().enumerate() {
        assert_sorted(ex, &format!("examples[{i}]"));
    }
    for (i, alias) in got["guide"]["agent_intent_aliases"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        assert_sorted(alias, &format!("agent_intent_aliases[{i}]"));
    }

    // `commands` is a map of STRUCTS (Go robotCommandDoc, main.go:7612): the
    // command names sort, but each doc keeps its declaration order — sorting
    // those would be a byte diff on every command in the table.
    assert_sorted(&got["commands"], "commands");
    const GO_STRUCT_ORDER: [&str; 9] = [
        "flag",
        "description",
        "key_fields",
        "params",
        "needs_issues",
        "needs_git",
        "needs_sprint",
        "needs_baseline",
        "mutates_state",
    ];
    for (name, doc) in got["commands"].as_object().unwrap() {
        let mut k = keys(doc);
        // omitempty drops the two slice fields when empty; keep the relative
        // order of whatever survived.
        let filtered: Vec<&str> = k
            .iter()
            .copied()
            .filter(|f| GO_STRUCT_ORDER.contains(f))
            .collect();
        let expected: Vec<&str> = GO_STRUCT_ORDER
            .iter()
            .copied()
            .filter(|f| filtered.contains(f))
            .collect();
        assert_eq!(filtered, expected, "commands.{name} field order");
        k.clear();
    }
}

/// The five surfaces that were individually wrong before the port: a stale
/// description, a dropped param, a wrong `needs_sprint`, an over-claiming
/// token-saving string, and an example that measured the wrong payload.
/// Named individually so a regression points at the exact string.
#[test]
fn every_corrected_string_matches_go_verbatim() {
    let got: Value = serde_json::from_str(&docs("all")).expect("stdout is JSON");

    assert_eq!(
        got["commands"]["robot-capabilities"]["description"],
        "Machine-readable capability manifest: version, contract, commands, env vars, exit codes, and output formats."
    );
    assert_eq!(
        got["commands"]["robot-orphans"]["params"],
        serde_json::json!(["--orphans-min-score 0-100", "--label backend"])
    );
    assert_eq!(
        got["commands"]["robot-search"]["description"],
        "Hashed keyword or graph-weighted hybrid search over issue text."
    );
    assert_eq!(
        got["commands"]["robot-search"]["params"],
        serde_json::json!([
            "--search \"login oauth\"",
            "--search-limit 10",
            "--search-min-score SCORE",
            "--search-mode text|hybrid"
        ])
    );
    assert_eq!(got["commands"]["robot-sprint-list"]["needs_sprint"], true);

    assert_eq!(got["examples"][7]["command"], "bvr robot-graph --toon");
    assert_eq!(
        got["examples"][7]["description"],
        "Get TOON output for a wide payload (measured ~7% smaller than JSON)"
    );
    assert_eq!(
        got["examples"][9]["description"],
        "Compare JSON and TOON size for this payload (TOON can be larger)"
    );
    assert_eq!(
        got["guide"]["output_modes"]["toon"],
        "Tabular notation; measured smaller than JSON only for wide payloads such as --robot-graph (~7%), and 9-15% larger for nested ones (--robot-triage, --robot-plan, --robot-insights, --robot-label-health). See tests/artifacts/perf/toon_vs_json.md and check --stats before adopting it."
    );
}

/// Each topic is a subset of `all`, so a change to one must not disturb the
/// others. Go's `switch topic` (main.go:8182) has one arm per topic plus the
/// `all` arm that assigns all five; an unknown topic takes the `default` arm
/// with its `did_you_mean` suggestion.
#[test]
fn every_topic_is_a_subset_of_all() {
    let all: Value = serde_json::from_str(&docs("all")).expect("stdout is JSON");
    for (topic, key) in [
        ("guide", "guide"),
        ("commands", "commands"),
        ("examples", "examples"),
        ("env", "environment_variables"),
        ("exit-codes", "exit_codes"),
    ] {
        let got: Value = serde_json::from_str(&docs(topic)).expect("stdout is JSON");
        assert_eq!(got[key], all[key], "`{topic}` diverges from `all`");
        // Only the one topic section, plus the four envelope stamps.
        let mut keys: Vec<&String> = got.as_object().unwrap().keys().collect();
        keys.sort();
        let mut expected = vec!["generated_at", "output_format", "topic", "version"];
        expected.push(key);
        expected.sort();
        assert_eq!(keys, expected, "`{topic}` carries unexpected sections");
    }
}

/// The `default` arm: an unknown topic reports itself, lists what exists, and
/// suggests the nearest by edit distance (Go `suggestClosest`, main.go:8224).
///
/// Exit code is 2, not 0. Measured against the oracle at 18afafa: an unknown
/// topic writes that JSON to *stdout* and returns 2, which is contract #4's
/// usage-error code — `main.go:8922` sets `ErrorTopic` and the handler
/// propagates it. This test went through [`docs`], which asserts exit 0, so it
/// was really asserting that a usage error is not a usage error.
#[test]
fn unknown_topic_suggests_the_nearest_real_topic() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_bvr"))
        .args(["--robot-docs", "guied"])
        .output()
        .expect("binary runs");
    assert_eq!(
        out.status.code(),
        Some(2),
        "oracle exits 2 on an unknown topic: stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let got: Value = serde_json::from_slice(&out.stdout).expect("the error payload goes to stdout");
    assert_eq!(got["error"], "Unknown topic: guied");
    assert_eq!(got["did_you_mean"], "guide");
    assert_eq!(got["suggested_action"], "Run `bvr --robot-docs guide`");
    assert_eq!(
        got["available_topics"],
        serde_json::json!(["guide", "commands", "examples", "env", "exit-codes", "all"])
    );
    assert!(
        got.get("guide").is_none() && got.get("commands").is_none(),
        "an unknown topic must not emit a section"
    );
}
