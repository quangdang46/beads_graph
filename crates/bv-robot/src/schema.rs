//! JSON Schema definitions for robot command outputs — port of Go
//! `generateRobotSchemas` (main.go:9011). Detailed schemas for the primary
//! commands; generic fallback (Go `genericRobotCommandSchema`) for the rest.

use serde_json::{json, Map, Value};

use crate::docs;

/// Go `robotContractVersion`.
pub const SCHEMA_VERSION: &str = "1.0.0";

const DRAFT: &str = "https://json-schema.org/draft/2020-12/schema";

fn s(t: &str) -> Value {
    json!({"type": t})
}

fn array_of(items: Value) -> Value {
    json!({"type": "array", "items": items})
}

fn string_array() -> Value {
    array_of(s("string"))
}

fn format_enum() -> Value {
    json!({"type": "string", "enum": ["json", "toon"]})
}

/// Go envelope schema (shared preamble in every robot output).
fn envelope_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "generated_at": {"type": "string", "format": "date-time", "description": "ISO 8601 timestamp when output was generated"},
            "data_hash": {"type": "string", "description": "Fingerprint of source beads.jsonl for cache validation"},
            "output_format": {"type": "string", "enum": ["json", "toon"], "description": "Output format used (json or toon)"},
            "version": {"type": "string", "description": "bv version that generated this output"},
            // v0.25.0 added the source-authority and scope half of the envelope.
            "source_path": {"type": "string", "description": "File the issue set was loaded from (or '<beads>@<rev>' for --as-of, or the workspace config path)"},
            "source_kind": {"type": "string", "enum": ["jsonl_local", "jsonl_worktree", "sqlite", "git", "workspace"], "description": "Kind of source behind source_path"},
            "source_authority": {
                "type": "object",
                "description": "Per-source accounting; readiness is provisional and claim_safe is false when any required source is failed, incomplete, stale, or unknown",
                "properties": {
                    "state": {"type": "string", "enum": ["complete", "partial", "unknown"]},
                    "claim_safe": {"type": "boolean"},
                    "readiness": {"type": "string", "enum": ["proven", "provisional"]},
                    "sources": {"type": "array", "items": {"type": "object"}},
                },
                "required": ["state", "claim_safe", "readiness", "sources"],
            },
            "authority_hash": {"type": "string", "description": "Fingerprint of source identities, source data, and completeness diagnostics before view projection"},
            "scope": {
                "type": "object",
                "description": "Active --label/--recipe/--repo scoping; 'unsupported' lists scoping flags this command could not honour (for example as_of for commands that read sprint files or live git history)",
                "properties": {
                    "label": {"type": "string"},
                    "recipe": {"type": "string"},
                    "repo": {"type": "string"},
                    "unsupported": {"type": "array", "items": {"type": "string"}},
                },
            },
            "scope_hash": {"type": "string", "description": "Fingerprint of selected candidates and active scope, distinct from source authority"},
            "as_of": {"type": "string", "description": "The --as-of ref when time-travelling"},
            "as_of_commit": {"type": "string", "description": "Resolved commit SHA for --as-of"},
            "load_stats": {"type": "object", "description": "Present only when records were dropped during load (#190)"},
        },
        "required": ["generated_at", "data_hash"],
    })
}

fn recommendation_def() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": s("string"),
            "title": s("string"),
            "type": s("string"),
            "status": s("string"),
            "priority": s("integer"),
            "labels": string_array(),
            "score": s("number"),
            "reasons": string_array(),
            "unblocks": s("integer"),
            // v0.25.0 attaches the live tracker route to every recommendation.
            "actions": issue_actions_schema(),
        },
        "required": ["id", "title", "score"],
    })
}

fn alert_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "type": s("string"),
            "severity": json!({"type": "string", "enum": ["critical", "warning", "info"]}),
            "message": s("string"),
            "baseline_value": s("number"),
            "current_value": s("number"),
            "delta": s("number"),
            "details": string_array(),
            "issue_id": s("string"),
            "label": s("string"),
            "detected_at": {"type": "string", "format": "date-time"},
            "unblocks_count": s("integer"),
            "downstream_priority_sum": s("integer"),
        },
        "required": ["type", "severity", "message"],
    })
}

fn alert_summary_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "total": s("integer"),
            "critical": s("integer"),
            "warning": s("integer"),
            "info": s("integer"),
        },
        "required": ["total", "critical", "warning", "info"],
    })
}

fn recipe_summary_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": s("string"),
            "description": s("string"),
            // v0.25.0 added project-file recipes, which carry the defining
            // file path they were loaded from.
            "source": {"type": "string", "enum": ["builtin", "user", "project", "project-file"]},
            "path": {"type": "string", "description": "Defining file for project-file recipes"},
        },
        "required": ["name", "description", "source"],
    })
}

fn suggestion_set_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "suggestions": array_of(json!({
                "type": "object",
                "properties": {
                    "type": s("string"),
                    "target_bead": s("string"),
                    "related_bead": s("string"),
                    "summary": s("string"),
                    "reason": s("string"),
                    "confidence": s("number"),
                    "action_command": s("string"),
                    // v0.25.0 reports the action as a structured route rather
                    // than a bare command string.
                    "action": {
                        "description": "Literal argv with an explicit working directory and equivalent POSIX shell command",
                        "type": "object",
                        "properties": {
                            "working_directory": {"type": "string", "minLength": 1},
                            "argv": {"type": "array", "items": {"type": "string"}, "minItems": 1},
                            "shell": {"type": "string", "minLength": 1},
                        },
                        "required": ["working_directory", "argv", "shell"],
                    },
                    "generated_at": {"type": "string", "format": "date-time"},
                    "metadata": {"type": "object", "additionalProperties": true},
                },
                "required": ["type", "target_bead", "summary", "reason", "confidence", "generated_at"],
            })),
            "generated_at": {"type": "string", "format": "date-time"},
            "data_hash": s("string"),
            "stats": {
                "type": "object",
                "properties": {
                    "total": s("integer"),
                    "by_type": {"type": "object", "additionalProperties": s("integer")},
                    "by_confidence": {"type": "object", "additionalProperties": s("integer")},
                    "high_confidence_count": s("integer"),
                    "actionable_count": s("integer"),
                },
                "required": ["total", "by_type", "by_confidence", "high_confidence_count", "actionable_count"],
            },
        },
        "required": ["suggestions", "generated_at", "stats"],
    })
}

fn label_health_config_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "stale_threshold_days": s("integer"),
            "velocity_weight": s("number"),
            "freshness_weight": s("number"),
            "flow_weight": s("number"),
            "criticality_weight": s("number"),
            "min_issues_for_health": s("integer"),
            "include_closed_in_flow": s("boolean"),
        },
    })
}

fn label_analysis_result_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "generated_at": {"type": "string", "format": "date-time"},
            "total_labels": s("integer"),
            "healthy_count": s("integer"),
            "warning_count": s("integer"),
            "critical_count": s("integer"),
            "labels": {"type": "array"},
            "summaries": {"type": "array"},
            "attention_needed": {
                "items": {"type": "string"},
                "type": "array",
            },
            "cross_label_flow": cross_label_flow_schema(),
        },
        "required": ["generated_at", "total_labels", "healthy_count", "warning_count", "critical_count", "labels", "summaries", "attention_needed"],
    })
}

fn cross_label_flow_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "labels": {
                "items": {"type": "string"},
                "type": "array",
            },
            "flow_matrix": {"type": "array"},
            "dependencies": {"type": "array"},
            "critical_paths": {"type": "array"},
            "bottleneck_labels": {
                "items": {"type": "string"},
                "type": "array",
            },
            "total_cross_label_deps": s("integer"),
        },
        "required": ["labels", "flow_matrix", "dependencies", "critical_paths", "bottleneck_labels", "total_cross_label_deps"],
    })
}

fn label_attention_item_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "label": s("string"),
            "attention_score": s("number"),
            "normalized_score": s("number"),
            "rank": s("integer"),
            "reason": s("string"),
            "pagerank_sum": s("number"),
            "velocity_factor": s("number"),
            "open_count": s("integer"),
            "blocked_count": s("integer"),
            "stale_count": s("integer"),
        },
        "required": ["rank", "label", "attention_score", "normalized_score", "reason", "open_count", "blocked_count", "stale_count", "pagerank_sum", "velocity_factor"],
    })
}

fn sprint_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": s("string"),
            "name": s("string"),
            "start_date": {"type": "string", "format": "date-time"},
            "end_date": {"type": "string", "format": "date-time"},
            "bead_ids": string_array(),
            "velocity_target": s("number"),
            "created_at": {"type": "string", "format": "date-time"},
            "updated_at": {"type": "string", "format": "date-time"},
        },
        "required": ["id", "name"],
    })
}

fn capacity_bottleneck_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": s("string"),
            "title": s("string"),
            "blocks_count": s("integer"),
            "blocks": string_array(),
        },
        "required": ["id", "title", "blocks_count"],
    })
}

fn burndown_point_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "date": {"type": "string", "format": "date-time"},
            "remaining": s("integer"),
            "completed": s("integer"),
        },
        "required": ["date", "remaining", "completed"],
    })
}

fn timing_metric_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": s("string"),
            "count": s("integer"),
            "total_ms": s("number"),
            "avg_ms": s("number"),
            "max_ms": s("number"),
            "min_ms": s("number"),
        },
        "required": ["name", "count", "total_ms", "avg_ms", "max_ms"],
    })
}

fn cache_metric_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": s("string"),
            "hits": s("integer"),
            "misses": s("integer"),
            "total": s("integer"),
            "hit_rate": s("number"),
        },
        "required": ["name", "hits", "misses", "total", "hit_rate"],
    })
}

fn memory_metric_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "heap_alloc_mb": s("number"),
            "heap_sys_mb": s("number"),
            "heap_objects_k": s("number"),
            "gc_cycles": s("integer"),
            "gc_pause_ms": s("number"),
            "goroutine_count": s("integer"),
        },
        "required": ["heap_alloc_mb", "heap_sys_mb", "heap_objects_k", "gc_cycles", "gc_pause_ms", "goroutine_count"],
    })
}

/// Go `titleCaseRobotCommand` — "robot-triage" -> "Robot Triage".
fn title_case_robot_command(name: &str) -> String {
    name.split('-')
        .map(|part| {
            if part.is_empty() {
                part.to_string()
            } else {
                let mut c = part.chars();
                format!("{}{}", c.next().unwrap().to_uppercase(), c.as_str())
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn blocker_chain_entry_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": s("string"),
            "title": s("string"),
            "status": s("string"),
            "priority": s("integer"),
            "depth": s("integer"),
            "is_root": s("boolean"),
            "actionable": s("boolean"),
            "blocks_count": s("integer"),
        },
    })
}

fn blocker_chain_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "Robot Blocker Chain Output",
        "description": "Full blocker chain analysis for an issue",
        "type": "object",
        "properties": {
            "generated_at": {"type": "string", "format": "date-time"},
            "data_hash": s("string"),
            "output_format": format_enum(),
            "version": s("string"),
            "result": {
                "type": "object",
                "properties": {
                    "target_id": s("string"),
                    "target_title": s("string"),
                    "is_blocked": s("boolean"),
                    "chain_length": s("integer"),
                    "root_blockers": array_of(blocker_chain_entry_schema()),
                    "chain": array_of(blocker_chain_entry_schema()),
                    "has_cycle": s("boolean"),
                    "cycle_ids": {"type": ["array", "null"], "items": s("string")},
                },
            },
        },
        "required": ["generated_at", "data_hash", "output_format", "version", "result"],
    })
}

/// Go `genericRobotCommandSchema` — envelope + description fallback.
fn generic_command_schema(name: &str, doc: &Value) -> Value {
    let mut properties = Map::new();
    properties.insert(
        "generated_at".into(),
        json!({"type": "string", "format": "date-time"}),
    );
    if doc["needs_issues"].as_bool().unwrap_or(true) {
        properties.insert("data_hash".into(), s("string"));
    }
    properties.insert("output_format".into(), format_enum());
    properties.insert("version".into(), s("string"));

    // Go genericRobotCommandSchema: commands with extra dynamic fields get
    // additionalProperties: true so the schema is permissive.
    let needs_extra = matches!(
        name,
        "robot-drift"
            | "robot-confirm-correlation"
            | "robot-explain-correlation"
            | "robot-reject-correlation"
    );
    let mut schema = json!({
        "$schema": DRAFT,
        "title": format!("{} Output", title_case_robot_command(name)),
        "description": doc["description"],
        "type": "object",
        "properties": Value::Object(properties),
    });
    if needs_extra {
        schema["additionalProperties"] = json!(true);
    }
    schema
}

fn triage_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "Robot Triage Output",
        "description": "Unified triage recommendations with quick picks, blockers, and project health",
        "type": "object",
        "properties": {
            "generated_at": {"type": "string", "format": "date-time"},
            "data_hash": s("string"),
            "triage": {
                "type": "object",
                "properties": {
                    "meta": {
                        "type": "object",
                        "properties": {
                            "version": s("string"),
                            "generated_at": s("string"),
                            "phase2_ready": s("boolean"),
                            "issue_count": s("integer"),
                            "history_status": {"type": "string", "enum": ["ok", "error", "timeout", "skipped"], "description": "Outcome of the git-history correlation prologue; omitted when history was not attempted (#166)"},
                        },
                    },
                    "quick_ref": {
                        "type": "object",
                        "properties": {
                            "actionable_count": {"type": "integer", "description": "Issues ready to work on: status open or in_progress, no open blocking dependencies, no future defer_until (parked statuses such as blocked/deferred/draft are excluded, matching br ready)"},
                            "blocked_count": {"type": "integer", "description": "Strict count of issues with status == blocked (equals project_health.counts.by_status.blocked)"},
                            "in_progress_count": {"type": "integer", "description": "Strict count of issues with status == in_progress"},
                            "not_actionable_count": {"type": "integer", "description": "Non-closed issues that are not actionable: blocked by open dependencies, parked in a non-actionable status, or scheduler-deferred"},
                            "not_closed_count": {"type": "integer", "description": "All non-closed issues (open+in_progress+blocked+deferred); equals actionable_count + not_actionable_count"},
                            "open_count": {"type": "integer", "description": "Strict count of issues with status == open (equals project_health.counts.by_status.open)"},
                            "top_picks": array_of(json!({"$ref": "#/$defs/recommendation"})),
                        },
                    },
                    "recommendations": array_of(json!({"$ref": "#/$defs/recommendation"})),
                    "quick_wins": {"type": "array"},
                    "blockers_to_clear": {"type": "array"},
                    "project_health": s("object"),
                    "commands": s("object"),
                },
            },
            "usage_hints": array_of(s("string")),
        },
        "$defs": {"recommendation": recommendation_def()},
    })
}

/// Go `model.IssueActions` (pkg/model/types.go:66) — the live tracker route
/// attached to a suggestion or a top pick.
fn issue_actions_schema() -> Value {
    let command = |desc: &str| {
        json!({
            "description": desc,
            "type": "object",
            "properties": {
                "working_directory": {"type": "string", "minLength": 1},
                "argv": {"type": "array", "items": {"type": "string"}, "minItems": 1},
                "shell": {"type": "string", "minLength": 1},
            },
            "required": ["working_directory", "argv", "shell"],
        })
    };
    json!({
        "type": "object",
        "properties": {
            "working_directory": s("string"),
            "local_id": s("string"),
            "tracker": {"type": "string", "enum": ["br", "bd"]},
            "show": command("Literal argv with an explicit working directory and equivalent POSIX shell command"),
            "claim": command("Literal argv with an explicit working directory and equivalent POSIX shell command"),
            "unavailable_reason": s("string"),
        },
    })
}

fn next_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "Robot Next Output",
        "description": "A verified live claim route when actionable, otherwise diagnostics without a claim command",
        "type": "object",
        "properties": {
            "generated_at": {"type": "string", "format": "date-time"},
            "data_hash": s("string"),
            "id": s("string"),
            "title": s("string"),
            "score": s("number"),
            "reasons": string_array(),
            "unblocks": s("integer"),
            "claim_command": s("string"),
            "show_command": s("string"),
            // v0.25.0: a pick without a live tracker route is reported as a
            // degradation rather than an actionable item, so the schema has to
            // describe those fields too.
            "actionable": {"type": "boolean"},
            "phase2_ready": {"type": "boolean"},
            "status": s("object"),
            "message": s("string"),
            "diagnostic_top_pick": s("object"),
            "degraded": {"type": "array", "items": {"type": "object"}},
            "actions": issue_actions_schema(),
        },
        "required": [
            "generated_at", "data_hash", "actionable", "phase2_ready", "status",
        ],
        // The two shapes are mutually exclusive: an actionable pick must carry
        // the live claim route, a degraded one must not.
        "if": {"properties": {"actionable": {"const": true}}},
        "then": {
            "properties": {"actions": {"required": ["claim", "show"]}},
            "required": [
                "id", "title", "score", "claim_command", "show_command", "actions",
            ],
        },
        "else": {
            "properties": {
                "actions": {"properties": {"claim": false}},
                "claim_command": false,
                "diagnostic_top_pick": {
                    "properties": {"actions": {"properties": {"claim": false}}},
                },
            },
        },
    })
}

fn plan_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "Robot Plan Output",
        "description": "Dependency-respecting execution plan with parallel tracks",
        "type": "object",
        "properties": {
            "generated_at": {"type": "string", "format": "date-time"},
            "data_hash": s("string"),
            "plan": {
                "type": "object",
                "properties": {
                    "phases": array_of(json!({
                        "type": "object",
                        "properties": {
                            "phase": s("integer"),
                            "issues": {"type": "array"},
                        },
                    })),
                    "summary": s("object"),
                },
            },
            "status": s("object"),
            "usage_hints": {"type": "array"},
        },
    })
}

fn insights_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "Robot Insights Output",
        "description": "Full graph analysis metrics including PageRank, betweenness, HITS, cycles",
        "type": "object",
        "properties": {
            "generated_at": {"type": "string", "format": "date-time"},
            "data_hash": s("string"),
            "Stats": s("object"),
            "Cycles": {"type": "array"},
            "Keystones": {"type": "array"},
            "Bottlenecks": {"type": "array"},
            "Influencers": {"type": "array"},
            "Hubs": {"type": "array"},
            "Authorities": {"type": "array"},
            "Orphans": {"type": "array"},
            "Cores": {"type": "array"},
            "Articulation": {"type": "array"},
            "Slack": {"type": "array"},
            "Velocity": s("object"),
            "status": s("object"),
            "advanced_insights": s("object"),
            "usage_hints": {"type": "array"},
        },
    })
}

fn priority_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "Robot Priority Output",
        "description": "Priority misalignment detection with recommendations",
        "type": "object",
        "properties": {
            "generated_at": {"type": "string", "format": "date-time"},
            "data_hash": s("string"),
            "as_of": s("string"),
            "as_of_commit": s("string"),
            "analysis_config": s("object"),
            "status": s("object"),
            "label_scope": s("string"),
            "label_context": s("object"),
            "recommendations": {"type": "array"},
            "field_descriptions": {"type": "object", "additionalProperties": s("string")},
            "filters": {
                "type": "object",
                "properties": {
                    "min_confidence": s("number"),
                    "max_results": s("integer"),
                    "by_label": s("string"),
                    "by_assignee": s("string"),
                },
            },
            "summary": {
                "type": "object",
                "properties": {
                    "total_issues": s("integer"),
                    "recommendations": s("integer"),
                    "high_confidence": s("integer"),
                },
            },
            "usage_hints": {
                "items": {"type": "string"},
                "type": "array",
            },
        },
        "required": ["generated_at", "data_hash", "analysis_config", "status", "recommendations", "field_descriptions", "filters", "summary", "usage_hints"],
    })
}

fn graph_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "Robot Graph Output",
        "description": "Dependency graph in JSON/DOT/Mermaid format",
        "type": "object",
        "properties": {
            "format": {"type": "string", "enum": ["json", "dot", "mermaid"]},
            "graph": s("string"),
            "nodes": s("integer"),
            "edges": s("integer"),
            "filters_applied": {"type": "object", "additionalProperties": s("string")},
            "explanation": {
                "type": "object",
                "properties": {
                    "what": s("string"),
                    "how_to_render": s("string"),
                    "when_to_use": s("string"),
                },
                "required": ["what", "when_to_use"],
            },
            "data_hash": s("string"),
            "adjacency": {
                "type": "object",
                "properties": {
                    "nodes": {"type": "array"},
                    "edges": {"type": "array"},
                },
            },
        },
        "required": ["format", "nodes", "edges", "explanation"],
    })
}

fn diff_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "Robot Diff Output",
        "description": "Changes since a historical point (commit, branch, date)",
        "type": "object",
        "properties": {
            "generated_at": {"type": "string", "format": "date-time"},
            "resolved_revision": s("string"),
            "as_of": s("string"),
            "as_of_commit": s("string"),
            "from_data_hash": s("string"),
            "to_data_hash": s("string"),
            "diff": {
                "type": "object",
                "properties": {
                    "from_timestamp": {"type": "string", "format": "date-time"},
                    "to_timestamp": {"type": "string", "format": "date-time"},
                    "from_revision": s("string"),
                    "to_revision": s("string"),
                    "new_issues": {"type": "array"},
                    "closed_issues": {"type": "array"},
                    "removed_issues": {"type": "array"},
                    "reopened_issues": {"type": "array"},
                    "modified_issues": {"type": "array"},
                    "new_cycles": {"type": "array"},
                    "resolved_cycles": {"type": "array"},
                    "metric_deltas": s("object"),
                    "summary": s("object"),
                },
            },
        },
        "required": ["generated_at", "resolved_revision", "from_data_hash", "to_data_hash", "diff"],
    })
}

fn alerts_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "Robot Alerts Output",
        "description": "Stale issues, blocking cascades, priority mismatches",
        "type": "object",
        "properties": {
            "generated_at": {"type": "string", "format": "date-time"},
            "data_hash": s("string"),
            "output_format": format_enum(),
            "version": s("string"),
            "alerts": array_of(alert_schema()),
            "summary": alert_summary_schema(),
            "usage_hints": string_array(),
        },
        "required": ["generated_at", "data_hash", "output_format", "version", "alerts", "summary", "usage_hints"],
    })
}

fn recipes_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "Robot Recipes Output",
        "description": "Recipe names, descriptions, and sources for pre-filtering work",
        "type": "object",
        "properties": {
            "generated_at": {"type": "string", "format": "date-time"},
            "output_format": format_enum(),
            "version": s("string"),
            "recipes": array_of(recipe_summary_schema()),
        },
        "required": ["generated_at", "output_format", "version", "recipes"],
    })
}

fn metrics_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "Robot Metrics Output",
        "description": "Performance metrics: timing, cache hit rates, and memory usage",
        "type": "object",
        "properties": {
            "generated_at": {"type": "string", "format": "date-time"},
            "data_hash": s("string"),
            "output_format": format_enum(),
            "version": s("string"),
            "timing": array_of(timing_metric_schema()),
            "cache": array_of(cache_metric_schema()),
            "memory": memory_metric_schema(),
        },
        "required": ["generated_at", "data_hash", "output_format", "version", "memory"],
    })
}

fn suggest_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "Robot Suggest Output",
        "description": "Smart suggestions for duplicates, dependencies, labels, cycle breaks",
        "type": "object",
        "properties": {
            "generated_at": {"type": "string", "format": "date-time"},
            "data_hash": s("string"),
            "filters": {
                "type": "object",
                "properties": {
                    "type": s("string"),
                    "min_confidence": s("number"),
                    "bead_id": s("string"),
                },
            },
            "suggestions": suggestion_set_schema(),
            "usage_hints": string_array(),
        },
        "required": ["generated_at", "data_hash", "filters", "suggestions", "usage_hints"],
    })
}

fn label_health_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "Robot Label Health Output",
        "description": "Per-label health metrics with analysis config and usage hints",
        "type": "object",
        "properties": {
            "generated_at": {"type": "string", "format": "date-time"},
            "data_hash": s("string"),
            "analysis_config": label_health_config_schema(),
            "results": label_analysis_result_schema(),
            "usage_hints": {
                "items": {"type": "string"},
                "type": "array",
            },
        },
        "required": ["generated_at", "data_hash", "analysis_config", "results", "usage_hints"],
    })
}

fn label_flow_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "Robot Label Flow Output",
        "description": "Cross-label dependency flow analysis with analysis config and usage hints",
        "type": "object",
        "properties": {
            "generated_at": {"type": "string", "format": "date-time"},
            "data_hash": s("string"),
            "flow": cross_label_flow_schema(),
            "analysis_config": label_health_config_schema(),
            "usage_hints": {
                "items": {"type": "string"},
                "type": "array",
            },
        },
        "required": ["generated_at", "data_hash", "flow", "analysis_config", "usage_hints"],
    })
}

fn label_attention_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "Robot Label Attention Output",
        "description": "Attention-ranked labels requiring focus",
        "type": "object",
        "properties": {
            "generated_at": {"type": "string", "format": "date-time"},
            "data_hash": s("string"),
            "limit": s("integer"),
            "total_labels": s("integer"),
            "labels": array_of(label_attention_item_schema()),
            "usage_hints": {
                "items": {"type": "string"},
                "type": "array",
            },
        },
        "required": ["generated_at", "data_hash", "limit", "total_labels", "labels", "usage_hints"],
    })
}

fn sprint_list_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "Robot Sprint List Output",
        "description": "All configured sprints with the standard robot envelope",
        "type": "object",
        "properties": {
            "generated_at": {"type": "string", "format": "date-time"},
            "data_hash": s("string"),
            "output_format": format_enum(),
            "version": s("string"),
            "sprint_count": s("integer"),
            "sprints": array_of(sprint_schema()),
        },
        "required": ["generated_at", "data_hash", "output_format", "version", "sprint_count", "sprints"],
    })
}

fn sprint_show_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "Robot Sprint Show Output",
        "description": "A single configured sprint with the standard robot envelope",
        "type": "object",
        "properties": {
            "generated_at": {"type": "string", "format": "date-time"},
            "data_hash": s("string"),
            "output_format": format_enum(),
            "version": s("string"),
            "sprint": sprint_schema(),
        },
        "required": ["generated_at", "data_hash", "output_format", "version", "sprint"],
    })
}

fn capacity_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "Robot Capacity Output",
        "description": "Capacity simulation and dependency-aware completion projection",
        "type": "object",
        "properties": {
            "generated_at": {"type": "string", "format": "date-time"},
            "data_hash": s("string"),
            "output_format": format_enum(),
            "version": s("string"),
            "agents": s("integer"),
            "label": s("string"),
            "open_issue_count": s("integer"),
            "total_minutes": s("integer"),
            "total_days": s("number"),
            "serial_minutes": s("integer"),
            "parallel_minutes": s("integer"),
            "parallelizable_pct": s("number"),
            "estimated_days": s("number"),
            "critical_path_length": s("integer"),
            "critical_path": string_array(),
            "actionable_count": s("integer"),
            "actionable": string_array(),
            "bottlenecks": array_of(capacity_bottleneck_schema()),
        },
        "required": [
            "generated_at", "data_hash", "output_format", "version", "agents",
            "open_issue_count", "total_minutes", "total_days", "serial_minutes",
            "parallel_minutes", "parallelizable_pct", "estimated_days",
            "critical_path_length", "critical_path", "actionable_count", "actionable",
        ],
    })
}

fn burndown_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "Robot Burndown Output",
        "description": "Sprint burndown data with issue counts, burn rates, daily points, and optional scope changes",
        "type": "object",
        "properties": {
            "generated_at": {"type": "string", "format": "date-time"},
            "data_hash": s("string"),
            "output_format": format_enum(),
            "version": s("string"),
            "sprint_id": s("string"),
            "sprint_name": s("string"),
            "start_date": {"type": "string", "format": "date-time"},
            "end_date": {"type": "string", "format": "date-time"},
            "total_days": s("integer"),
            "elapsed_days": s("integer"),
            "remaining_days": s("integer"),
            "total_issues": s("integer"),
            "completed_issues": s("integer"),
            "remaining_issues": s("integer"),
            "ideal_burn_rate": s("number"),
            "actual_burn_rate": s("number"),
            "projected_complete": {"type": "string", "format": "date-time"},
            "on_track": s("boolean"),
            "daily_points": {"type": ["array", "null"], "items": burndown_point_schema()},
            "ideal_line": {"type": ["array", "null"], "items": burndown_point_schema()},
            "scope_changes": {
                "type": ["array", "null"],
                "items": {
                    "type": "object",
                    "properties": {
                        "date": {"type": "string", "format": "date-time"},
                        "issue_id": s("string"),
                        "issue_title": s("string"),
                        "action": {"type": "string", "enum": ["added", "removed"]},
                    },
                },
            },
        },
        "required": [
            "generated_at", "data_hash", "output_format", "version",
            "sprint_id", "sprint_name", "start_date", "end_date",
            "total_days", "elapsed_days", "remaining_days",
            "total_issues", "completed_issues", "remaining_issues",
            "ideal_burn_rate", "actual_burn_rate", "on_track",
            "daily_points", "ideal_line",
        ],
    })
}

fn forecast_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "Robot Forecast Output",
        "description": "ETA predictions with dependency-aware scheduling",
        "type": "object",
        "properties": {
            "generated_at": {"type": "string", "format": "date-time"},
            "data_hash": s("string"),
            "output_format": format_enum(),
            "version": s("string"),
            "agents": s("integer"),
            "filters": {"type": "object", "additionalProperties": s("string")},
            "forecast_count": s("integer"),
            "forecasts": {"type": "array"},
            "summary": {
                "type": "object",
                "properties": {
                    "total_minutes": s("integer"),
                    "total_days": s("number"),
                    "avg_confidence": s("number"),
                    "earliest_eta": {"type": "string", "format": "date-time"},
                    "latest_eta": {"type": "string", "format": "date-time"},
                },
            },
        },
        "required": ["generated_at", "data_hash", "output_format", "version", "agents", "forecast_count", "forecasts"],
    })
}

/// Go `robotCapabilitiesSchema` — machine-readable command manifest.
fn capabilities_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "Robot Capabilities Output",
        "description": "Machine-readable command manifest for agent command discovery.",
        "type": "object",
        "properties": {
            "agent_intent_aliases": {"type": "array"},
            "commands": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": true,
                    "properties": {
                        "accepted_invocations": {"type": "array", "items": {"type": "string"}},
                        "description": s("string"),
                        "flag": s("string"),
                        "key_fields": {"type": "array", "items": {"type": "string"}},
                        "mutates_state": {"type": "boolean"},
                        "name": s("string"),
                        "needs_baseline": {"type": "boolean"},
                        "needs_git": {"type": "boolean"},
                        "needs_issues": {"type": "boolean"},
                        "needs_sprint": {"type": "boolean"},
                        "params": {"type": "array", "items": {"type": "string"}},
                        "preferred_invocation": s("string"),
                    },
                    "required": ["name", "flag", "description", "preferred_invocation", "accepted_invocations", "needs_issues", "needs_git", "needs_sprint", "needs_baseline", "mutates_state"],
                },
            },
            "contract_version": s("string"),
            "default_robot_command": s("string"),
            "docs_topics": {"type": "array", "items": {"type": "string"}},
            "environment_variables": {"type": "object", "additionalProperties": {"type": "string"}},
            "exit_codes": {"type": "object", "additionalProperties": {"type": "string"}},
            "generated_at": {"type": "string", "format": "date-time"},
            "output_formats": {"type": "array", "items": {"type": "string"}},
            "schema_command": s("string"),
            "stream_contract": {"type": "object", "additionalProperties": {"type": "string"}},
            "tool": s("string"),
            "version": s("string"),
        },
        "required": ["generated_at", "tool", "version", "contract_version", "commands", "environment_variables", "exit_codes"],
    })
}

// ──────────────────────────────────────────────────────────────────────
// Detailed schemas for commands that previously used generic fallback
// ──────────────────────────────────────────────────────────────────────

/// A blocked interval. Shared by blocked_periods, dependency_wait_periods
/// and explicit_blocked_periods, which Go gives the same shape.
fn causality_blocked_period_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "blocker_id": s("string"),
            "blocker_ids": {"type": ["array", "null"], "items": s("string")},
            "start_time": {"type": "string", "format": "date-time"},
            "end_time": {"type": "string", "format": "date-time"},
            "duration": s("integer"),
            "kind": {"type": "string", "enum": ["union", "explicit_status", "dependency"]},
            "ongoing": {"type": "boolean"},
            "start_observed": {"type": "boolean"},
        },
    })
}

/// Causality insights block. Lifted out of `causality_schema` because the
/// inline nesting exceeds the json! macro's recursion limit.
fn causality_insights_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
                "summary": s("string"),
                // Every duration is nullable: the analysis reports what it
                // could not attribute rather than guessing a zero.
                "total_duration": {"type": ["integer", "null"]},
                "active_duration": {"type": ["integer", "null"]},
                "blocked_duration": {"type": ["integer", "null"]},
                "blocked_percentage": {"type": ["number", "null"]},
                "duration_known": {"type": "boolean"},
                "blocked_duration_known": {"type": "boolean"},
                "critical_path_duration_known": {"type": "boolean"},
                "dependency_duration_known": {"type": "boolean"},
                "explicit_duration_known": {"type": "boolean"},
                "critical_path_duration": {"type": ["integer", "null"], "minimum": 0},
                "dependency_wait_duration": {"type": ["integer", "null"]},
                "explicit_blocked_duration": {"type": ["integer", "null"]},
                "coverage": {
                    "type": "string",
                    "enum": ["complete", "partial", "inconsistent", "unavailable"],
                },
                "limitations": {"type": ["array", "null"], "items": s("string")},
                "dependency_wait_periods": {
                    "type": ["array", "null"],
                    "items": causality_blocked_period_schema(),
                },
                "explicit_blocked_periods": {
                    "type": ["array", "null"],
                    "items": causality_blocked_period_schema(),
                },
                "blocked_periods": {
                    "type": ["array", "null"],
                    "items": causality_blocked_period_schema(),
                },
                "commit_count": s("integer"),
                "longest_gap": {"type": ["integer", "null"]},
                "longest_gap_desc": s("string"),
                "avg_time_between": {"type": ["integer", "null"]},
                "estimated_without": {"type": "null"},
                "critical_path": {"type": ["array", "null"], "items": s("integer")},
                "critical_path_desc": s("string"),
                "recommendations": {"type": ["array", "null"], "items": s("string")},
        },
    })
}

/// One causal event. Split out of `causality_schema` because the inline
/// `json!` nesting exceeded the macro's recursion limit once v0.25.0 added
/// the before/after issue snapshots.
fn causality_event_schema() -> Value {
    let snapshot = || {
        json!({
            "type": ["object", "null"],
            "properties": {
                "id": s("string"),
                "title": s("string"),
                "status": s("string"),
                "dependencies": {
                    "type": ["array", "null"],
                    "items": json!({
                        "type": "object",
                        "properties": {
                            "depends_on_id": s("string"),
                            "type": s("string"),
                        },
                    }),
                },
            },
        })
    };
    json!({
        "type": "object",
        "properties": {
            "id": s("integer"),
            "type": s("string"),
            "description": s("string"),
            "timestamp": {"type": "string", "format": "date-time"},
            "commit_sha": s("string"),
            "blocker_id": s("string"),
            "caused_by_id": s("integer"),
            "duration_next": s("integer"),
            "enables_ids": {"type": ["array", "null"], "items": s("integer")},
            "committed_at": {"type": "string", "format": "date-time"},
            "source_bead_id": s("string"),
            "transition_observed": {"type": "boolean"},
            "before": snapshot(),
            "after": snapshot(),
        },
    })
}

fn causality_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "Robot Causality Output",
        "description": "Causal chain analysis for a bead",
        "type": "object",
        "properties": {
            "generated_at": {"type": "string", "format": "date-time"},
            "data_hash": s("string"),
            "output_format": format_enum(),
            "version": s("string"),
            "chain": {
                "type": "object",
                "properties": {
                    "bead_id": s("string"),
                    "title": s("string"),
                    "status": s("string"),
                    "start_time": {"type": "string", "format": "date-time"},
                    "end_time": {"type": "string", "format": "date-time"},
                    "total_time": {"type": ["integer", "null"]},
                    "edge_count": s("integer"),
                    "is_complete": s("boolean"),
                    // v0.25.0 reports how much of the chain's timing is
                    // actually known, and the evidence behind each link.
                    "duration_known": {"type": "boolean"},
                    "time_basis": s("string"),
                    "related_commits": {"type": ["array", "null"], "items": s("object")},
                    "links": {
                        "type": ["array", "null"],
                        "items": json!({
                            "type": "object",
                            "properties": {
                                "from": s("integer"),
                                "to": s("integer"),
                                "duration": {"type": "integer", "minimum": 0},
                                "kind": {
                                    "type": "string",
                                    "enum": [
                                        "dependency_transition",
                                        "observed_wait",
                                        "ongoing_wait",
                                    ],
                                },
                                "evidence": s("string"),
                            },
                            "required": ["from", "to", "kind", "evidence", "duration"],
                        }),
                    },
                    "events": array_of(causality_event_schema()),
                },
            },
            "insights": causality_insights_schema(),
        },
        "required": ["generated_at", "data_hash", "output_format", "version", "chain", "insights"],
    })
}

fn file_beads_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "Robot File Beads Output",
        "description": "Beads that touched a specific file path",
        "type": "object",
        "properties": {
            "generated_at": {"type": "string", "format": "date-time"},
            "data_hash": s("string"),
            "output_format": format_enum(),
            "version": s("string"),
            "file_path": s("string"),
            "total_beads": s("integer"),
            "open_beads": {
                "type": ["array", "null"],
                "items": json!({
                    "type": "object",
                    "properties": {
                        "bead_id": s("string"),
                        "title": s("string"),
                        "status": s("string"),
                        "last_touch": {"type": "string", "format": "date-time"},
                        "total_changes": s("integer"),
                        "commit_shas": {"type": ["array", "null"], "items": s("string")},
                    },
                }),
            },
            "closed_beads": {
                "type": ["array", "null"],
                "items": json!({
                    "type": "object",
                    "properties": {
                        "bead_id": s("string"),
                        "title": s("string"),
                        "status": s("string"),
                        "last_touch": {"type": "string", "format": "date-time"},
                        "total_changes": s("integer"),
                        "commit_shas": {"type": ["array", "null"], "items": s("string")},
                    },
                }),
            },
        },
        "required": ["generated_at", "data_hash", "output_format", "version", "file_path", "total_beads", "open_beads", "closed_beads"],
    })
}

fn file_hotspots_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "Robot File Hotspots Output",
        "description": "Files touched by the most beads",
        "type": "object",
        "properties": {
            "generated_at": {"type": "string", "format": "date-time"},
            "data_hash": s("string"),
            "output_format": format_enum(),
            "version": s("string"),
            "hotspots": {
                "type": ["array", "null"],
                "items": json!({
                    "type": "object",
                    "properties": {
                        "file_path": s("string"),
                        "total_beads": s("integer"),
                        "open_beads": s("integer"),
                        "closed_beads": s("integer"),
                    },
                }),
            },
            "stats": {
                "type": "object",
                "properties": {
                    "total_files": s("integer"),
                    "total_bead_links": s("integer"),
                    "files_with_multiple_beads": s("integer"),
                },
            },
        },
        "required": ["generated_at", "data_hash", "output_format", "version", "hotspots", "stats"],
    })
}

fn file_relations_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "Robot File Relations Output",
        "description": "Files that frequently co-change with a given file",
        "type": "object",
        "properties": {
            "generated_at": {"type": "string", "format": "date-time"},
            "data_hash": s("string"),
            "output_format": format_enum(),
            "version": s("string"),
            "file_path": s("string"),
            "total_commits": s("integer"),
            "threshold": s("number"),
            "related_files": {
                "type": ["array", "null"],
                "items": json!({
                    "type": "object",
                    "properties": {
                        "file_path": s("string"),
                        "co_change_count": s("integer"),
                        "correlation": s("number"),
                        "total_commits": s("integer"),
                        "sample_commits": {"type": ["array", "null"], "items": s("string")},
                    },
                }),
            },
        },
        "required": ["generated_at", "data_hash", "output_format", "version", "file_path", "total_commits", "threshold", "related_files"],
    })
}

fn impact_network_schema() -> Value {
    let network_node_item = json!({
        "type": "object",
        "properties": {
            "bead_id": s("string"),
            "title": s("string"),
            "status": s("string"),
            "priority": s("integer"),
            "degree": s("integer"),
            "connectivity": s("number"),
            "commit_count": s("integer"),
            "file_count": s("integer"),
            "cluster_id": s("integer"),
            "last_activity": {"type": "string", "format": "date-time"},
        },
    });
    let network_stats = json!({
        "type": "object",
        "properties": {
            "total_nodes": s("integer"),
            "total_edges": s("integer"),
            "density": s("number"),
            "avg_degree": s("number"),
            "max_degree": s("integer"),
            "cluster_count": s("integer"),
            "largest_cluster": s("integer"),
            "isolated_nodes": s("integer"),
        },
    });
    let cluster_item = json!({
        "type": "object",
        "properties": {
            "cluster_id": s("integer"),
            "label": s("string"),
            "central_bead": s("string"),
            "bead_ids": {"type": "array", "items": s("string")},
            "internal_edges": s("integer"),
            "external_edges": s("integer"),
            "internal_connectivity": s("number"),
            "total_commits": s("integer"),
            "shared_files": {"type": "array", "items": s("string")},
        },
    });
    json!({
        "$schema": DRAFT,
        "title": "Robot Impact Network Output",
        "description": "Impact network graph for all beads or one bead subnetwork",
        "type": "object",
        "properties": {
            "generated_at": {"type": "string", "format": "date-time"},
            "data_hash": s("string"),
            "output_format": format_enum(),
            "version": s("string"),
            "bead_id": s("string"),
            "depth": s("integer"),
            "network": {
                "type": "object",
                "properties": {
                    "data_hash": s("string"),
                    "generated_at": {"type": "string", "format": "date-time"},
                    "nodes": {
                        "type": "object",
                        "additionalProperties": network_node_item,
                    },
                    "edges": {
                        "type": ["array", "null"],
                        "items": json!({"type": "object", "additionalProperties": true}),
                    },
                    "stats": network_stats.clone(),
                    "clusters": {
                        "type": ["array", "null"],
                        "items": cluster_item.clone(),
                    },
                },
            },
            "stats": network_stats,
            "top_clusters": {
                "type": ["array", "null"],
                "items": cluster_item.clone(),
            },
            "top_connected": {
                "type": ["array", "null"],
                "items": network_node_item,
            },
        },
        "required": ["generated_at", "data_hash", "output_format", "version", "depth", "network", "stats"],
    })
}

fn orphans_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "Robot Orphans Output",
        "description": "Orphan commit candidates that should be linked to beads",
        "type": "object",
        "properties": {
            "generated_at": {"type": "string", "format": "date-time"},
            "data_hash": s("string"),
            "output_format": format_enum(),
            "version": s("string"),
            "git_range": s("string"),
            "stats": {
                "type": "object",
                "properties": {
                    "total_commits": s("integer"),
                    "orphan_count": s("integer"),
                    "candidate_count": s("integer"),
                    "correlated_count": s("integer"),
                    "orphan_ratio": s("number"),
                    "avg_suspicion_score": s("number"),
                },
            },
            "candidates": array_of(json!({
                "type": "object",
                "properties": {
                    "sha": s("string"),
                    "short_sha": s("string"),
                    "message": s("string"),
                    "author": s("string"),
                    "author_email": s("string"),
                    "timestamp": {"type": "string", "format": "date-time"},
                    "files": {"type": ["array", "null"], "items": s("string")},
                    "suspicion_score": s("integer"),
                    "probable_beads": array_of(json!({
                        "type": "object",
                        "properties": {
                            "bead_id": s("string"),
                            "bead_title": s("string"),
                            "bead_status": s("string"),
                            "confidence": s("integer"),
                            "reasons": {"type": "array", "items": s("string")},
                        },
                    })),
                    "signals": array_of(json!({
                        "type": "object",
                        "properties": {
                            "signal": s("string"),
                            "details": s("string"),
                            "weight": s("integer"),
                        },
                    })),
                },
            })),
            "by_bead": {
                "type": "object",
                "additionalProperties": {"type": "array", "items": s("string")},
            },
        },
        "required": ["generated_at", "data_hash", "output_format", "version", "git_range", "stats", "candidates"],
    })
}

fn related_schema() -> Value {
    let bead_item = json!({
        "type": "object",
        "properties": {
            "bead_id": s("string"),
            "title": s("string"),
            "status": s("string"),
            "relation_type": s("string"),
            "reason": s("string"),
            "relevance": s("integer"),
            "shared_files": {"type": ["array", "null"], "items": s("string")},
            "shared_commits": {"type": ["array", "null"], "items": s("string")},
        },
    });
    json!({
        "$schema": DRAFT,
        "title": "Robot Related Output",
        "description": "Beads related to a specific bead ID",
        "type": "object",
        "properties": {
            "generated_at": {"type": "string", "format": "date-time"},
            "data_hash": s("string"),
            "output_format": format_enum(),
            "version": s("string"),
            "target_bead_id": s("string"),
            "target_title": s("string"),
            "total_related": s("integer"),
            "file_overlap": array_of(bead_item.clone()),
            "commit_overlap": array_of(bead_item.clone()),
            "dependency_cluster": array_of(bead_item.clone()),
            "concurrent": array_of(bead_item),
        },
        "required": ["generated_at", "data_hash", "output_format", "version", "target_bead_id", "target_title", "file_overlap", "commit_overlap", "dependency_cluster", "concurrent", "total_related"],
    })
}

fn triage_by_label_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "Robot Triage By Label Output",
        "description": "Triage recommendations grouped by label for area-focused agents",
        "type": "object",
        "properties": {
            "generated_at": {"type": "string", "format": "date-time"},
            "data_hash": s("string"),
            "as_of": s("string"),
            "as_of_commit": s("string"),
            "feedback": s("object"),
            "triage": {
                "type": "object",
                "properties": {
                    "meta": s("object"),
                    "quick_ref": s("object"),
                    "recommendations": {"type": "array"},
                    "quick_wins": {"type": "array"},
                    "blockers_to_clear": {"type": "array"},
                    "project_health": s("object"),
                    "commands": s("object"),
                    "recommendations_by_label": {
                        "type": "array",
                        "items": json!({
                            "type": "object",
                            "properties": {
                                "label": s("string"),
                                "recommendations": {"type": "array"},
                                "top_pick": s("object"),
                                "claim_command": s("string"),
                                "total_unblocks": s("integer"),
                            },
                            "required": ["label", "recommendations", "total_unblocks"],
                        }),
                    },
                },
                "required": ["meta", "quick_ref", "recommendations"],
            },
            "usage_hints": {
                "items": {"type": "string"},
                "type": "array",
            },
        },
        "required": ["generated_at", "data_hash", "triage", "usage_hints"],
    })
}

fn triage_by_track_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "Robot Triage By Track Output",
        "description": "Triage recommendations grouped by independent parallel execution tracks",
        "type": "object",
        "properties": {
            "generated_at": {"type": "string", "format": "date-time"},
            "data_hash": s("string"),
            "as_of": s("string"),
            "as_of_commit": s("string"),
            "feedback": s("object"),
            "triage": {
                "type": "object",
                "properties": {
                    "meta": s("object"),
                    "quick_ref": s("object"),
                    "recommendations": {"type": "array"},
                    "quick_wins": {"type": "array"},
                    "blockers_to_clear": {"type": "array"},
                    "project_health": s("object"),
                    "commands": s("object"),
                    "recommendations_by_track": {
                        "type": "array",
                        "items": json!({
                            "type": "object",
                            "properties": {
                                "track_id": s("string"),
                                "reason": s("string"),
                                "recommendations": {"type": "array"},
                                "top_pick": s("object"),
                                "claim_command": s("string"),
                                "total_unblocks": s("integer"),
                            },
                            "required": ["track_id", "reason", "recommendations", "total_unblocks"],
                        }),
                    },
                },
                "required": ["meta", "quick_ref", "recommendations"],
            },
            "usage_hints": {
                "items": {"type": "string"},
                "type": "array",
            },
        },
        "required": ["generated_at", "data_hash", "triage", "usage_hints"],
    })
}

fn history_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "Robot History Output",
        "description": "Bead-to-commit correlation history report with aggregate stats and reverse commit index",
        "type": "object",
        "properties": {
            "generated_at": {"type": "string", "format": "date-time"},
            "data_hash": s("string"),
            "output_format": format_enum(),
            "version": s("string"),
            "git_range": s("string"),
            "stats": s("object"),
            "histories": s("object"),
            "commit_index": s("object"),
            "latest_commit_sha": s("string"),
        },
        "required": ["generated_at", "data_hash", "output_format", "version", "git_range", "stats", "histories", "commit_index"],
    })
}

fn search_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "Robot Search Output",
        "description": "Semantic or hybrid issue search results with index metadata and usage hints",
        "type": "object",
        "properties": {
            "generated_at": {"type": "string", "format": "date-time"},
            "data_hash": s("string"),
            "output_format": format_enum(),
            "version": s("string"),
            // v0.25.0 records index/candidate/ranking provenance so a cached
            // result can be tied back to exactly what was searched.
            "index_data_hash": {
                "description": "Hash of the complete indexed issue corpus before candidate filters",
                "type": "string",
            },
            "candidate_hash": {
                "description": "Hash of issues eligible for this search before score filtering",
                "type": "string",
            },
            "ranking_hash": {
                "description": "Hash of corpus, candidates, scope, query, and effective ranking configuration",
                "type": "string",
            },
            "ranking_time": {
                "description": "Pinned reference time used by hybrid recency scoring",
                "type": "string",
                "format": "date-time",
            },
            "min_score": {
                "description": "Inclusive minimum raw text similarity, before lexical boost or hybrid ranking",
                "type": "number",
                "minimum": -1,
                "maximum": 1,
            },
            "query": s("string"),
            "provider": s("string"),
            "dim": s("integer"),
            "index_path": s("string"),
            "index": s("object"),
            "loaded": s("boolean"),
            "limit": s("integer"),
            "mode": {"type": "string", "enum": ["text", "hybrid"]},
            "preset": s("string"),
            "model": s("string"),
            "weights": s("object"),
            "results": {
                "type": "array",
                "items": json!({
                    "type": "object",
                    "properties": {
                        "issue_id": s("string"),
                        "title": s("string"),
                        "score": s("number"),
                        "text_score": s("number"),
                        "component_scores": s("object"),
                    },
                }),
            },
            "usage_hints": {
                "items": {"type": "string"},
                "type": "array",
            },
        },
        "required": [
            "generated_at", "data_hash", "output_format", "version",
            "index_data_hash", "candidate_hash", "ranking_hash", "query",
            "provider", "dim", "index_path", "index", "loaded", "limit", "mode",
            "results",
        ],
    })
}

fn correlation_stats_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "Robot Correlation Stats Output",
        "description": "Summary counts and confidence aggregates for saved correlation feedback",
        "type": "object",
        "properties": {
            "generated_at": {"type": "string", "format": "date-time"},
            "output_format": format_enum(),
            "version": s("string"),
            "total_feedback": s("integer"),
            "confirmed": s("integer"),
            "rejected": s("integer"),
            "ignored": s("integer"),
            "accuracy_rate": s("number"),
            "avg_confirm_conf": s("number"),
            "avg_reject_conf": s("number"),
        },
        "required": ["generated_at", "output_format", "version", "total_feedback", "confirmed", "rejected", "ignored", "accuracy_rate", "avg_confirm_conf", "avg_reject_conf"],
    })
}

fn impact_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "Robot Impact Output",
        "description": "Bead impact analysis for files that may be modified",
        "type": "object",
        "properties": {
            "generated_at": {"type": "string", "format": "date-time"},
            "data_hash": s("string"),
            "output_format": format_enum(),
            "version": s("string"),
            "files": {
                "type": ["array", "null"],
                "items": s("string"),
            },
            "risk_level": s("string"),
            "risk_score": s("number"),
            "summary": s("string"),
            "warnings": {
                "type": ["array", "null"],
                "items": s("string"),
            },
            "affected_beads": {
                "type": ["array", "null"],
                "items": json!({
                    "type": "object",
                    "properties": {
                        "bead_id": s("string"),
                        "title": s("string"),
                        "status": s("string"),
                        "relevance": s("number"),
                        "overlap_count": s("integer"),
                        "overlap_files": {"type": ["array", "null"], "items": s("string")},
                        "total_changes": s("integer"),
                        "last_activity": {"type": "string", "format": "date-time"},
                    },
                }),
            },
        },
        "required": ["generated_at", "data_hash", "output_format", "version", "files", "risk_level", "risk_score", "summary", "warnings", "affected_beads"],
    })
}

fn docs_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "Robot Docs Output",
        "description": "Machine-readable documentation for one robot docs topic, or an unknown-topic diagnostic",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "generated_at": {"type": "string", "format": "date-time"},
            "output_format": format_enum(),
            "version": s("string"),
            "topic": s("string"),
            "guide": s("object"),
            "commands": {"type": "object", "additionalProperties": s("object")},
            "examples": {"type": "array", "items": s("object")},
            "environment_variables": {"type": "object", "additionalProperties": s("string")},
            "exit_codes": {"type": "object", "additionalProperties": s("string")},
            "error": s("string"),
            "available_topics": {"type": "array", "items": s("string")},
            "did_you_mean": s("string"),
            "suggested_action": s("string"),
        },
        "required": ["generated_at", "output_format", "version", "topic"],
    })
}

fn help_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "Robot Help Output",
        "description": "Machine-readable guide output emitted by the agent-friendly `bvr robot-help --json` invocation",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "generated_at": {"type": "string", "format": "date-time"},
            "output_format": format_enum(),
            "version": s("string"),
            "topic": {"type": "string", "const": "guide"},
            "guide": s("object"),
        },
        "required": ["generated_at", "output_format", "version", "topic", "guide"],
    })
}

fn schema_self_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "Robot Schema Output",
        "description": "JSON Schema definitions for all robot commands, or one command when --schema-command is set",
        "type": "object",
        "additionalProperties": false,
        "oneOf": [
            {"required": ["schema_version", "generated_at", "envelope", "commands"]},
            {"required": ["schema_version", "generated_at", "command", "schema"]},
        ],
        "properties": {
            "generated_at": {"type": "string", "format": "date-time"},
            "schema_version": s("string"),
            "envelope": s("object"),
            "commands": {"type": "object", "additionalProperties": s("object")},
            "command": s("string"),
            "schema": s("object"),
        },
        "required": ["schema_version", "generated_at"],
    })
}

/// Recursively sort all Map keys in a Value tree for deterministic JSON output
/// matching Go's `json.Marshal` which sorts map keys alphabetically.
fn sort_keys(v: Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<String> = map.keys().cloned().collect();
            keys.sort();
            let mut sorted = Map::new();
            for k in keys {
                sorted.insert(k.clone(), sort_keys(map[&k].clone()));
            }
            Value::Object(sorted)
        }
        Value::Array(arr) => Value::Array(arr.into_iter().map(sort_keys).collect()),
        other => other,
    }
}

/// Go `generateRobotSchemas` — full payload with per-command schemas.
/// Detailed schemas for primary commands; generic fallback for the rest.
pub fn generate_robot_schemas(now: &str) -> Value {
    let mut commands = Map::new();
    let detailed: &[(&str, Value)] = &[
        ("robot-triage", triage_schema()),
        ("robot-next", next_schema()),
        ("robot-plan", plan_schema()),
        ("robot-insights", insights_schema()),
        ("robot-priority", priority_schema()),
        ("robot-graph", graph_schema()),
        ("robot-diff", diff_schema()),
        ("robot-alerts", alerts_schema()),
        ("robot-recipes", recipes_schema()),
        ("robot-metrics", metrics_schema()),
        ("robot-suggest", suggest_schema()),
        ("robot-label-health", label_health_schema()),
        ("robot-label-flow", label_flow_schema()),
        ("robot-label-attention", label_attention_schema()),
        ("robot-sprint-list", sprint_list_schema()),
        ("robot-sprint-show", sprint_show_schema()),
        ("robot-capacity", capacity_schema()),
        ("robot-capabilities", capabilities_schema()),
        ("robot-burndown", burndown_schema()),
        ("robot-forecast", forecast_schema()),
        ("robot-blocker-chain", blocker_chain_schema()),
        ("robot-causality", causality_schema()),
        ("robot-file-beads", file_beads_schema()),
        ("robot-file-hotspots", file_hotspots_schema()),
        ("robot-file-relations", file_relations_schema()),
        ("robot-impact-network", impact_network_schema()),
        ("robot-orphans", orphans_schema()),
        ("robot-related", related_schema()),
        ("robot-triage-by-label", triage_by_label_schema()),
        ("robot-triage-by-track", triage_by_track_schema()),
        ("robot-history", history_schema()),
        ("robot-search", search_schema()),
        ("robot-correlation-stats", correlation_stats_schema()),
        ("robot-impact", impact_schema()),
        ("robot-docs", docs_schema()),
        ("robot-help", help_schema()),
        ("robot-schema", schema_self_schema()),
    ];
    for (name, schema) in detailed {
        commands.insert(name.to_string(), schema.clone());
    }

    // Generic fallback for every remaining documented command (Go behavior).
    let cmds = docs::commands_doc();
    if let Some(obj) = cmds.as_object() {
        for (name, doc) in obj {
            commands
                .entry(name.clone())
                .or_insert_with(|| generic_command_schema(name, doc));
        }
    }

    // Sort command keys for stable output (Go map serialization sorts).
    let mut keys: Vec<&String> = commands.keys().collect();
    keys.sort();
    let mut sorted = Map::new();
    for k in keys {
        sorted.insert(k.clone(), commands[k].clone());
    }

    // Go's RobotSchemas struct field order: SchemaVersion, GeneratedAt, Envelope, Commands.
    // Go's json.Marshal sorts map keys alphabetically at each nesting level.
    let envelope = sort_keys(envelope_schema());
    let sorted_cmds: Map<String, Value> =
        sorted.into_iter().map(|(k, v)| (k, sort_keys(v))).collect();
    // Build output with correct Go struct field order (not json! macro insertion order).
    let mut result = Map::new();
    result.insert("schema_version".into(), json!(SCHEMA_VERSION));
    result.insert("generated_at".into(), json!(now));
    result.insert("envelope".into(), envelope);
    result.insert("commands".into(), Value::Object(sorted_cmds));
    Value::Object(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_version_is_contract_version() {
        assert_eq!(SCHEMA_VERSION, "1.0.0");
    }

    #[test]
    fn triage_schema_has_recommendation_defs() {
        let out = generate_robot_schemas("2026-01-01T00:00:00Z");
        let triage = &out["commands"]["robot-triage"];
        assert_eq!(
            triage["$defs"]["recommendation"]["required"],
            json!(["id", "title", "score"])
        );
        assert_eq!(
            triage["properties"]["triage"]["properties"]["quick_ref"]["properties"]["open_count"]
                ["type"],
            json!("integer")
        );
    }

    #[test]
    fn every_documented_command_has_a_schema() {
        let out = generate_robot_schemas("2026-01-01T00:00:00Z");
        let cmds = out["commands"].as_object().unwrap();
        let doc_value = docs::commands_doc();
        let doc_cmds = doc_value.as_object().unwrap();
        for name in doc_cmds.keys() {
            assert!(cmds.contains_key(name), "missing schema for {name}");
        }
    }

    #[test]
    fn generic_fallback_omits_data_hash_when_no_issues_needed() {
        let out = generate_robot_schemas("2026-01-01T00:00:00Z");
        let docs_schema = &out["commands"]["robot-docs"];
        assert!(docs_schema["properties"].get("data_hash").is_none());
        assert_eq!(docs_schema["title"], json!("Robot Docs Output"));
        assert_eq!(docs_schema["$schema"], json!(DRAFT));
    }
}
