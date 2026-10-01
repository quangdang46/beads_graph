//! Machine-readable robot documentation — port of Go `generateRobotDocs`
//! (main.go:8922) with topics guide/commands/examples/env/exit-codes/all.
//!
//! Command docs mirror Go `robotCommandDocs()`; flag example forms mirror
//! `robotFlagExampleForm`. Map keys serialize sorted like Go's maps.

use serde_json::{json, Value};

pub const ROBOT_DOCS_TOPICS: &[&str] =
    &["guide", "commands", "examples", "env", "exit-codes", "all"];

struct CommandDoc {
    flag: &'static str,
    description: &'static str,
    key_fields: &'static [&'static str],
    params: &'static [&'static str],
    needs_issues: bool,
    needs_git: bool,
    needs_sprint: bool,
    needs_baseline: bool,
    mutates_state: bool,
}

#[allow(clippy::too_many_arguments)]
const fn c(
    flag: &'static str,
    description: &'static str,
    key_fields: &'static [&'static str],
    params: &'static [&'static str],
    needs_issues: bool,
    needs_git: bool,
    needs_sprint: bool,
    needs_baseline: bool,
    mutates_state: bool,
) -> CommandDoc {
    CommandDoc {
        flag,
        description,
        key_fields,
        params,
        needs_issues,
        needs_git,
        needs_sprint,
        needs_baseline,
        mutates_state,
    }
}

/// Go `robotCommandDocs()` — raw flag forms before example substitution.
fn raw_command_docs() -> Vec<(&'static str, CommandDoc)> {
    vec![
        ("robot-help", c("--robot-help", "Agent-focused command help. Use robot-docs guide for structured JSON documentation.", &[], &[], false, false, false, false, false)),
        ("robot-triage", c("--robot-triage", "Unified triage: top picks, recommendations, quick wins, blockers, project health, velocity.", &["triage.quick_ref.top_picks", "triage.recommendations", "triage.quick_wins", "triage.blockers_to_clear", "triage.project_health"], &["--graph-root <id>"], true, false, false, false, false)),
        ("robot-next", c("--robot-next", "Single top recommendation with claim/show commands.", &["id", "title", "score", "reasons", "unblocks", "claim_command", "show_command"], &["--graph-root <id>"], true, false, false, false, false)),
        ("robot-plan", c("--robot-plan", "Dependency-respecting execution plan with parallel tracks.", &["tracks", "items", "unblocks", "summary"], &[], true, false, false, false, false)),
        ("robot-insights", c("--robot-insights", "Deep graph analysis: PageRank, betweenness, HITS, eigenvector, k-core, cycle detection.", &["pagerank", "betweenness", "hits", "eigenvector", "k_core", "cycles"], &[], true, false, false, false, false)),
        ("robot-priority", c("--robot-priority", "Priority misalignment detection: items whose graph importance differs from assigned priority.", &["misalignments", "suggestions"], &[], true, false, false, false, false)),
        ("robot-triage-by-track", c("--robot-triage-by-track", "Triage grouped by independent parallel execution tracks.", &["triage.recommendations_by_track[].track_id", "triage.recommendations_by_track[].top_pick", "triage.recommendations_by_track[].claim_command"], &["--graph-root <id>"], true, false, false, false, false)),
        ("robot-triage-by-label", c("--robot-triage-by-label", "Triage grouped by label for area-focused agents.", &["triage.recommendations_by_label[].label", "triage.recommendations_by_label[].top_pick", "triage.recommendations_by_label[].claim_command"], &["--graph-root <id>"], true, false, false, false, false)),
        ("robot-alerts", c("--robot-alerts", "Stale issues, blocking cascades, priority mismatches.", &["alerts", "severity", "affected_issues"], &["--severity info|warning|critical", "--alert-type <type>", "--alert-label <label>"], true, false, false, false, false)),
        ("robot-suggest", c("--robot-suggest", "Smart suggestions: potential duplicates, missing dependencies, label assignments, cycle warnings.", &["suggestions", "type", "confidence"], &["--suggest-type duplicate|dependency|label|cycle", "--suggest-confidence 0.0-1.0", "--suggest-bead <id>"], true, false, false, false, false)),
        ("robot-capabilities", c("--robot-capabilities", "Machine-readable capability manifest: version, contract, commands, env vars, exit codes, and output formats.", &["tool", "version", "contract_version", "commands", "environment_variables", "exit_codes"], &[], false, false, false, false, false)),
        ("robot-recipes", c("--robot-recipes", "Recipe names, descriptions, and usage hints for pre-filtering work.", &["recipes"], &[], false, false, false, false, false)),
        ("robot-schema", c("--robot-schema", "JSON Schema definitions for all robot command outputs.", &["schema_version", "envelope", "commands"], &["--schema-command <cmd>"], false, false, false, false, false)),
        ("robot-docs", c("--robot-docs <topic>", "Machine-readable JSON documentation. Topics: guide, commands, examples, env, exit-codes, all.", &[], &[], false, false, false, false, false)),
        ("robot-history", c("--robot-history", "Bead-to-commit correlations from git history.", &["correlations", "confidence", "commit_sha", "bead_id"], &["--bead-history <id>", "--history-since <date>", "--history-limit <n>", "--min-confidence 0.0-1.0"], true, true, false, false, false)),
        ("robot-diff", c("--robot-diff", "Changes since a historical point (commit, branch, tag, or date).", &[], &["--diff-since <ref>"], true, true, false, false, false)),
        ("robot-correlation-stats", c("--robot-correlation-stats", "Summary counts for saved correlation feedback.", &["total_feedback", "confirmed", "rejected", "ignored", "accuracy_rate"], &[], false, false, false, false, false)),
        ("robot-explain-correlation", c("--robot-explain-correlation <sha:bead>", "Explain why a commit is linked to a bead.", &["commit", "bead", "score", "reasons"], &[], true, true, false, false, false)),
        ("robot-confirm-correlation", c("--robot-confirm-correlation <sha:bead>", "Record positive feedback for a commit-to-bead correlation.", &[], &["--correlation-by agent", "--correlation-reason verified"], true, true, false, false, true)),
        ("robot-reject-correlation", c("--robot-reject-correlation <sha:bead>", "Record negative feedback for a commit-to-bead correlation.", &[], &["--correlation-by agent", "--correlation-reason unrelated"], true, true, false, false, true)),
        ("robot-search", c("--robot-search", "Hashed keyword or graph-weighted hybrid search over issue text.", &[], &["--search <query>", "--search-limit <n>", "--search-min-score SCORE", "--search-mode text|hybrid"], true, false, false, false, false)),
        ("robot-label-health", c("--robot-label-health", "Per-label health metrics: open/closed counts, velocity, staleness.", &[], &[], true, false, false, false, false)),
        ("robot-label-flow", c("--robot-label-flow", "Cross-label dependency flow analysis.", &[], &[], true, false, false, false, false)),
        ("robot-label-attention", c("--robot-label-attention", "Attention-ranked labels requiring focus.", &[], &["--attention-limit <n>"], true, false, false, false, false)),
        ("robot-graph", c("--robot-graph", "Dependency graph export in JSON, DOT, or Mermaid format.", &[], &["--graph-format json|dot|mermaid", "--graph-root <id>", "--graph-depth <n>"], true, false, false, false, false)),
        ("robot-metrics", c("--robot-metrics", "Performance metrics: timing, cache hit rates, memory usage.", &[], &[], true, false, false, false, false)),
        ("robot-orphans", c("--robot-orphans", "Orphan commit candidates that should be linked to beads.", &["git_range", "stats.candidate_count", "candidates", "candidates[].probable_beads", "by_bead"], &["--orphans-min-score 0-100", "--label <label>"], true, true, false, false, false)),
        ("robot-file-beads", c("--robot-file-beads <path>", "Beads that touched a specific file path.", &["file_path", "total_beads", "open_beads", "closed_beads"], &["--file-beads-limit <n>"], true, true, false, false, false)),
        ("robot-file-hotspots", c("--robot-file-hotspots", "Files touched by the most beads.", &["hotspots", "stats.total_files", "stats.total_bead_links", "stats.files_with_multiple_beads"], &["--hotspots-limit <n>"], true, true, false, false, false)),
        ("robot-file-relations", c("--robot-file-relations <path>", "Files that frequently co-change with a given file.", &["file_path", "total_commits", "threshold", "related_files"], &["--relations-threshold 0.0-1.0", "--relations-limit <n>"], true, true, false, false, false)),
        ("robot-related", c("--robot-related <id>", "Beads related to a specific bead ID.", &["target_bead_id", "total_related", "file_overlap", "commit_overlap", "dependency_cluster", "concurrent"], &["--related-min-relevance 0-100 or 0.0-1.0", "--related-max-results <n>", "--related-include-closed"], true, true, false, false, false)),
        ("robot-blocker-chain", c("--robot-blocker-chain <id>", "Full blocker chain analysis for an issue.", &["result.target_id", "result.is_blocked", "result.root_blockers", "result.chain", "result.has_cycle"], &[], true, false, false, false, false)),
        ("robot-impact-network", c("--robot-impact-network [<id>|all]", "Impact network graph (full or subnetwork for a bead).", &["network.nodes", "network.edges", "stats.total_nodes", "top_clusters", "top_connected"], &["--network-depth 1-3"], true, true, false, false, false)),
        ("robot-causality", c("--robot-causality <id>", "Causal chain analysis for a bead.", &["chain.bead_id", "chain.events", "insights.summary", "insights.critical_path", "insights.recommendations"], &[], true, true, false, false, false)),
        ("robot-sprint-list", c("--robot-sprint-list", "List all sprints as JSON.", &[], &[], true, false, true, false, false)),
        ("robot-sprint-show", c("--robot-sprint-show <id>", "Show details for a specific sprint.", &[], &[], true, false, true, false, false)),
        ("robot-forecast", c("--robot-forecast <id|all>", "ETA predictions for bead completion.", &[], &["--forecast-label <label>", "--forecast-sprint <id>", "--forecast-agents <n>"], true, false, false, false, false)),
        ("robot-capacity", c("--robot-capacity", "Capacity simulation and completion projections.", &[], &["--agents <n>", "--capacity-label <label>"], true, false, false, false, false)),
        ("robot-burndown", c("--robot-burndown <sprint|current>", "Sprint burndown data.", &[], &[], true, false, true, false, false)),
        ("robot-drift", c("--robot-drift", "Drift detection from saved baseline.", &[], &[], true, false, false, true, false)),
        ("robot-impact", c("--robot-impact <path[,path...]>", "Analyze bead impact for files that may be modified.", &["files", "risk_level", "risk_score", "warnings", "affected_beads"], &[], true, true, false, false, false)),
    ]
}

/// Go `robotFlagExampleForm` — replace placeholder tokens with examples.
fn flag_example_form(flag: &str) -> String {
    let replacements: &[(&str, &str)] = &[
        ("[<id>|all]", "all"),
        ("<path[,path...]>", "README.md"),
        ("<sprint|current>", "current"),
        ("<sha:bead>", "deadbeef:ISSUE_ID"),
        ("<id|all>", "all"),
        ("<query>", "\"login oauth\""),
        ("<topic>", "guide"),
        ("<cmd>", "robot-triage"),
        ("<date>", "\"30 days ago\""),
        ("<label>", "backend"),
        ("<path>", "README.md"),
        ("<type>", "critical"),
        ("<ref>", "HEAD~1"),
        ("<id>", "ISSUE_ID"),
        ("<n>", "10"),
    ];
    let mut out = flag.to_string();
    for (old, new) in replacements {
        out = out.replace(old, new);
    }
    out
}

/// Go `robotFlagExampleFormForCommand` — command-specific pre-substitutions.
fn command_flag_example_form(command: &str, flag: &str) -> String {
    let flag = match command {
        "robot-sprint-show" => flag.replace("<id>", "SPRINT_ID"),
        "robot-burndown" => flag.replace("<sprint|current>", "current"),
        "robot-forecast" => flag.replace("--forecast-sprint <id>", "--forecast-sprint SPRINT_ID"),
        _ => flag.to_string(),
    };
    flag_example_form(&flag)
}

fn doc_to_json(name: &str, doc: &CommandDoc) -> Value {
    let mut entry = serde_json::Map::new();
    entry.insert(
        "flag".into(),
        json!(command_flag_example_form(name, doc.flag)),
    );
    entry.insert("description".into(), json!(doc.description));
    if !doc.key_fields.is_empty() {
        entry.insert("key_fields".into(), json!(doc.key_fields));
    }
    if !doc.params.is_empty() {
        let params: Vec<String> = doc
            .params
            .iter()
            .map(|p| command_flag_example_form(name, p))
            .collect();
        entry.insert("params".into(), json!(params));
    }
    entry.insert("needs_issues".into(), json!(doc.needs_issues));
    entry.insert("needs_git".into(), json!(doc.needs_git));
    entry.insert("needs_sprint".into(), json!(doc.needs_sprint));
    entry.insert("needs_baseline".into(), json!(doc.needs_baseline));
    entry.insert("mutates_state".into(), json!(doc.mutates_state));
    Value::Object(entry)
}

/// Go `robotCommandDocsForAgentOutput` — sorted map of docs with example forms.
pub fn commands_doc() -> Value {
    let mut entries: Vec<(String, Value)> = raw_command_docs()
        .iter()
        .map(|(name, doc)| (name.to_string(), doc_to_json(name, doc)))
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut map = serde_json::Map::new();
    for (name, doc) in entries {
        map.insert(name, doc);
    }
    Value::Object(map)
}

fn guide_doc() -> Value {
    json!({
        "description": "bvr (Beads Viewer) provides structural analysis of the beads issue tracker DAG. It is the primary interface for AI agents to understand project state, plan work, and discover high-impact tasks.",
        "quickstart": [
            "bvr robot-triage --json           # Full triage with recommendations",
            "bvr robot-next --json             # Single top pick plus claim/show commands",
            "bvr robot-plan --json             # Dependency-respecting execution plan",
            "bvr robot-insights --json         # Deep graph analysis (PageRank, betweenness, etc.)",
            "bvr robot-triage-by-track --json  # Parallel work streams for multi-agent coordination",
            "bvr robot-capabilities --json     # Machine-readable command manifest",
            "bvr robot-schema --json           # JSON Schema definitions for all commands",
            "bvr triage --json                 # Short alias for robot-triage",
            "bvr capabilities --json           # Short alias for robot-capabilities",
        ],
        "data_source": ".beads/beads.jsonl, .beads/issues.jsonl, or BEADS_DB plus git history (correlations)",
        "output_modes": {
            "json": "Default structured output",
            "toon": "Tabular notation; measured smaller than JSON only for wide payloads such as --robot-graph (~7%), and 9-15% larger for nested ones (--robot-triage, --robot-plan, --robot-insights, --robot-label-health). See tests/artifacts/perf/toon_vs_json.md and check --stats before adopting it.",
        },
        "agent_intent_aliases": [
            {"agent_instinct": "bvr --json", "canonical": "bvr --robot-triage --format json"},
            {"agent_instinct": "bvr robot-triage --json", "canonical": "bvr --robot-triage --format json"},
            {"agent_instinct": "bvr triage --json", "canonical": "bvr --robot-triage --format json"},
            {"agent_instinct": "bvr next --json", "canonical": "bvr --robot-next --format json"},
            {"agent_instinct": "bvr plan --json", "canonical": "bvr --robot-plan --format json"},
            {"agent_instinct": "bvr insights --json", "canonical": "bvr --robot-insights --format json"},
            {"agent_instinct": "bvr robot-capabilities --json", "canonical": "bvr --robot-capabilities --format json"},
            {"agent_instinct": "bvr capabilities --json", "canonical": "bvr --robot-capabilities --format json"},
            {"agent_instinct": "bvr robot-docs guide --json", "canonical": "bvr --robot-docs guide --format json"},
            {"agent_instinct": "bvr docs guide --json", "canonical": "bvr --robot-docs guide --format json"},
            {"agent_instinct": "bvr robot-schema triage --json", "canonical": "bvr --robot-schema --schema-command robot-triage --format json"},
            {"agent_instinct": "bvr schema triage --json", "canonical": "bvr --robot-schema --schema-command robot-triage --format json"},
            {"agent_instinct": "bvr robot-search login oauth --json --limit 5", "canonical": "bvr --search 'login oauth' --robot-search --format json --search-limit 5"},
            {"agent_instinct": "bvr search login oauth --json --limit 5", "canonical": "bvr --search 'login oauth' --robot-search --format json --search-limit 5"},
            {"agent_instinct": "bvr robot-graph mermaid --json", "canonical": "bvr --robot-graph --graph-format mermaid --format json"},
            {"agent_instinct": "bvr graph mermaid --json", "canonical": "bvr --robot-graph --graph-format mermaid --format json"},
            {"agent_instinct": "bvr robot-related bv-123 --json", "canonical": "bvr --robot-related bv-123 --format json"},
            {"agent_instinct": "bvr --name backend --json", "canonical": "bvr --label backend --robot-triage --format json"},
        ],
    })
}

fn examples_doc() -> Value {
    json!([
        {"description": "Get top 3 picks for immediate work", "command": "bvr robot-triage --json | jq '.triage.quick_ref.top_picks[:3]'"},
        {"description": "Inspect the claim command for the top recommendation", "command": "bvr robot-next --json | jq -r '.claim_command'"},
        {"description": "Find high-impact blockers to clear", "command": "bvr robot-triage --json | jq '.triage.blockers_to_clear | map(.id)'"},
        {"description": "Get bug-only recommendations", "command": "bvr robot-triage --json | jq '.triage.recommendations[] | select(.type == \"bug\")'"},
        {"description": "Multi-agent: top pick per parallel track", "command": "bvr robot-triage-by-track --json | jq '.triage.recommendations_by_track[].top_pick'"},
        {"description": "Find beads related to a specific file", "command": "bvr robot-file-beads README.md --json"},
        {"description": "Search for issues by keyword", "command": "bvr robot-search \"authentication\" --json"},
        {"description": "Get TOON output for a wide payload (measured ~7% smaller than JSON)", "command": "bvr robot-graph --toon"},
        {"description": "Use env for default format", "command": "BV_OUTPUT_FORMAT=toon bvr robot-triage"},
        {"description": "Compare JSON and TOON size for this payload (TOON can be larger)", "command": "TOON_STATS=1 bvr robot-triage --toon"},
    ])
}

fn env_vars_doc() -> Value {
    json!({
        "BEADS_DB": "Path to beads database file or .beads directory (overrides BEADS_DIR; overridden by --db flag)",
        "BEADS_DIR": "Path to .beads directory (fallback when BEADS_DB and --db are not set)",
        "BV_OUTPUT_FORMAT": "Default output format: json or toon (overridden by --format)",
        "TOON_DEFAULT_FORMAT": "Fallback format if BV_OUTPUT_FORMAT not set",
        "TOON_STATS": "Set to 1 to show JSON vs TOON token estimates on stderr",
        "TOON_KEY_FOLDING": "TOON key folding mode",
        "TOON_INDENT": "TOON indentation level (0-16)",
        "BV_PRETTY_JSON": "Set to 1 for indented JSON output",
        "BV_ROBOT": "Set to 1 to force robot mode (clean stdout)",
        "BV_SEARCH_MODE": "Search mode: text or hybrid",
        "BV_SEARCH_PRESET": "Hybrid search preset name",
    })
}

fn exit_codes_doc() -> Value {
    json!({
        "0": "Success",
        "1": "Error (general failure, drift critical)",
        "2": "Invalid arguments or drift warning",
    })
}

/// Go `suggestClosest` — Levenshtein-based closest topic suggestion.
fn suggest_closest(value: &str, allowed: &[&str]) -> Option<String> {
    let value = value.trim().to_lowercase();
    if value.is_empty() || allowed.is_empty() {
        return None;
    }
    let best_dist = match value.len() {
        n if n <= 4 => 2,
        n if n <= 10 => 3,
        _ => 5,
    };
    let mut best: Option<(String, usize)> = None;
    for candidate in allowed {
        let normalized = candidate.trim().to_lowercase();
        if normalized.is_empty() {
            continue;
        }
        let dist = levenshtein_distance(&value, &normalized);
        let better = match &best {
            None => dist <= best_dist,
            Some((_, bd)) => {
                dist < *bd
                    || (dist <= best_dist && dist == *bd && normalized < best.as_ref().unwrap().0)
            }
        };
        if better {
            best = Some((candidate.to_string(), dist));
        }
    }
    best.map(|(b, _)| b)
}

fn levenshtein_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Go `generateRobotDocs` — full machine-readable docs payload.
pub fn generate_robot_docs(topic: &str, app_version: &str, now: &str) -> Value {
    let mut result = json!({
        "generated_at": now,
        "output_format": "json",
        "version": app_version,
        "topic": topic,
    });

    let guide = guide_doc();
    let commands = commands_doc();
    let examples = examples_doc();
    let env_vars = env_vars_doc();
    let exit_codes = exit_codes_doc();

    match topic {
        "guide" => result["guide"] = guide,
        "commands" => result["commands"] = commands,
        "examples" => result["examples"] = examples,
        "env" => result["environment_variables"] = env_vars,
        "exit-codes" => result["exit_codes"] = exit_codes,
        "all" => {
            result["guide"] = guide;
            result["commands"] = commands;
            result["examples"] = examples;
            result["environment_variables"] = env_vars;
            result["exit_codes"] = exit_codes;
        }
        _ => {
            result["error"] = json!(format!("Unknown topic: {topic}"));
            result["available_topics"] = json!(ROBOT_DOCS_TOPICS);
            match suggest_closest(topic, ROBOT_DOCS_TOPICS) {
                Some(suggestion) => {
                    result["did_you_mean"] = json!(suggestion);
                    result["suggested_action"] =
                        json!(format!("Run `bvr --robot-docs {suggestion}`"));
                }
                None => {
                    result["suggested_action"] =
                        json!("Run `bvr --robot-docs guide` or `bvr --robot-docs all`");
                }
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docs_topics_match_go() {
        assert_eq!(
            ROBOT_DOCS_TOPICS,
            &["guide", "commands", "examples", "env", "exit-codes", "all"]
        );
    }

    #[test]
    fn unknown_topic_gets_did_you_mean() {
        let out = generate_robot_docs("guied", "v0.20.0", "2026-01-01T00:00:00Z");
        assert_eq!(out["did_you_mean"], json!("guide"));
        assert_eq!(
            out["suggested_action"],
            json!("Run `bvr --robot-docs guide`")
        );
    }

    #[test]
    fn all_topic_includes_every_section() {
        let out = generate_robot_docs("all", "v0.20.0", "2026-01-01T00:00:00Z");
        for key in [
            "guide",
            "commands",
            "examples",
            "environment_variables",
            "exit_codes",
        ] {
            assert!(out.get(key).is_some(), "missing {key}");
        }
    }

    #[test]
    fn commands_docs_cover_all_go_commands() {
        let out = generate_robot_docs("commands", "v0.20.0", "2026-01-01T00:00:00Z");
        let n = out["commands"].as_object().unwrap().len();
        assert_eq!(n, 41, "expected 41 command docs, got {n}");
    }

    #[test]
    fn flag_example_forms_substituted() {
        let out = generate_robot_docs("commands", "v0.20.0", "2026-01-01T00:00:00Z");
        assert_eq!(out["commands"]["robot-next"]["flag"], json!("--robot-next"));
        assert_eq!(
            out["commands"]["robot-related"]["flag"],
            json!("--robot-related ISSUE_ID")
        );
        assert_eq!(
            out["commands"]["robot-search"]["params"][0],
            json!("--search \"login oauth\"")
        );
        assert_eq!(
            out["commands"]["robot-burndown"]["flag"],
            json!("--robot-burndown current")
        );
    }
}
