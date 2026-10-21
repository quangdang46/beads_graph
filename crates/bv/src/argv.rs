//! Argv rewriter — a port of Go's `cmd/bv` argv normalization
//! (`rewriteAgentIntentArgs` and friends, cmd/bv/main.go:533-1014).
//!
//! Every rewrite here mirrors a Go function one-for-one:
//!
//! - `normalize_flag_spelling`      <- `rewriteSingleDashLongFlags` + the
//!   `-f/-l/-r` shorthands pflag resolves
//! - `rewrite_agent_intent_command` <- `rewriteAgentIntentCommand` /
//!   `rewriteCanonicalRobotCommandIntent`
//! - `rewrite_value_intent`         <- `rewriteRobotValueIntent`
//! - `rewrite_docs/schema/search/graph_intent` <- their `rewriteRobot*Intent`
//! - `rewrite_upgrade_intent`       <- `rewriteUpgradeIntent`
//! - `rewrite_flag_aliases`         <- `rewriteAgentIntentFlagAliases`
//! - `consume_leading_flag_aliases` <- `consumeLeadingAgentIntentFlagAliases`
//! - the bare `--json` auto-promote <- `rewriteAgentIntentArgs`'s tail
//!
//! Indexing note: Go calls `rewriteAgentIntentArgs(os.Args[1:])`, so `args[0]`
//! is the first *user* argument. `main` does the same (`std::env::args().skip(1)`),
//! so there is no program-name slot to account for anywhere in this module.

/// Short-flag -> long-flag aliases advertised in Go `bv --help` (issue #5).
/// Go declares `-f, --format`, `-l, --label`, `-r, --recipe`; `-h, --help` is
/// handled separately by the help path, not by alias expansion.
const SHORT_ALIASES: &[(&str, &str)] = &[("-f", "--format"), ("-l", "--label"), ("-r", "--recipe")];

/// Rewrite raw args (argv *without* the program name) into canonical form.
pub fn rewrite_args(args: &[String]) -> Vec<String> {
    let normalized: Vec<String> = args.iter().map(|a| normalize_flag_spelling(a)).collect();

    // Go: a recognized first positional is a subcommand and fully determines
    // the output shape, so it returns before the auto-promote is considered.
    if let Some(rewritten) = rewrite_agent_intent_command(&normalized) {
        return rewritten;
    }

    // Asked before the alias rewrite consumes the spelling, exactly as Go does:
    // a bare `--format` with no primary drops into the TUI rather than
    // defaulting to `--robot-triage`, so only the aliases may promote.
    let wants_structured_output = contains_structured_output_alias(&normalized);

    let rewritten = rewrite_flag_aliases(&normalized, "");

    // Go `rewriteAgentIntentArgs` (cmd/bv/main.go:565-567): promote to
    // `--robot-triage` only when a structured-output alias is present *and*
    // neither a robot primary nor a non-robot primary (version/help/update/
    // pages/export/...) claims the invocation.
    if wants_structured_output
        && !has_primary_robot_arg(&rewritten)
        && !has_non_robot_primary_arg(&rewritten)
    {
        let mut promoted = Vec::with_capacity(rewritten.len() + 1);
        promoted.push("--robot-triage".to_string());
        promoted.extend(rewritten);
        return promoted;
    }

    rewritten
}

/// Go `rewriteSingleDashLongFlags` (cmd/bv/main.go:533-553), plus the
/// `-f/-l/-r` shorthand expansion Go's pflag performs when it parses the
/// result.
///
/// Go decides whether `-foo` names a long flag by asking the registry
/// (`flags.Lookup(name) == nil`, main.go:548) — never by looking at the shape
/// of the name. That distinction is load-bearing for every value-taking flag
/// whose value may be negative: `--relations-threshold -1e400` must reach
/// pflag as `-1e400` so the value errors are reported against the value, but
/// a shape-based test would read `1e400` as a flag name and rewrite the value
/// into `--1e400`, which pflag then rejects as a malformed spelling. The
/// registry is the only test that keeps the two apart.
///
/// A one-character name is a short flag, which is why `-o=json` survives as
/// `-o=json` and becomes a `--format` alias rather than a doubled-dash flag;
/// Go reaches the same branch through its `len(name) <= 1` guard.
fn normalize_flag_spelling(arg: &str) -> String {
    let Some(rest) = arg.strip_prefix('-') else {
        return arg.to_string();
    };
    if rest.starts_with('-') {
        return arg.to_string();
    }

    let name_len = rest.find('=').unwrap_or(rest.len());
    let (name, tail) = rest.split_at(name_len);
    if name.len() == 1 {
        if let Some((_, long)) = SHORT_ALIASES
            .iter()
            .find(|(short, _)| short.trim_start_matches('-') == name)
        {
            return format!("{long}{tail}");
        }
    }
    if name.len() > 1 && crate::flags::flag_lookup(name) {
        return format!("-{arg}");
    }
    arg.to_string()
}

/// Go `rewriteAgentIntentCommand` (cmd/bv/main.go:571): the first bare
/// positional names a subcommand, the canonical `robot-<command>` form, or
/// nothing at all. `None` means "not a subcommand" and the caller falls
/// through to the flag-alias pass.
fn rewrite_agent_intent_command(args: &[String]) -> Option<Vec<String>> {
    let first = args.first()?;
    if first.starts_with('-') {
        return None;
    }
    let command = first.to_lowercase();
    let command = command.trim();
    let rest = &args[1..];

    if let Some(rewritten) = rewrite_canonical_robot_command(command, rest) {
        return Some(rewritten);
    }

    let rewritten = match command {
        "triage" | "recommend" | "recommendations" => lead_flag(rest, "--robot-triage", "triage"),
        "next" | "pick" => lead_flag(rest, "--robot-next", "next"),
        "plan" => lead_flag(rest, "--robot-plan", "plan"),
        "insights" | "insight" | "analysis" | "analyze" => {
            lead_flag(rest, "--robot-insights", "insights")
        }
        "priority" | "priorities" => lead_flag(rest, "--robot-priority", "priority"),
        "alerts" => lead_flag(rest, "--robot-alerts", "alerts"),
        "suggest" | "suggestions" => lead_flag(rest, "--robot-suggest", "suggest"),
        "recipes" => lead_flag(rest, "--robot-recipes", "recipes"),
        "metrics" => lead_flag(rest, "--robot-metrics", "metrics"),
        "capabilities" | "capability" | "manifest" => {
            lead_flag(rest, "--robot-capabilities", "capabilities")
        }
        "docs" | "doc" => rewrite_docs_intent(rest),
        "schema" | "schemas" => rewrite_schema_intent(rest),
        "search" | "find" => rewrite_search_intent(rest),
        "graph" => rewrite_graph_intent(rest),
        "diff" | "changes" => {
            rewrite_value_intent(rest, "diff", "--robot-diff", "--diff-since", "")
        }
        "history" => rewrite_value_intent(rest, "history", "--robot-history", "--bead-history", ""),
        "labels" | "label-health" => lead_flag(rest, "--robot-label-health", "label-health"),
        "label-flow" => lead_flag(rest, "--robot-label-flow", "label-flow"),
        "label-attention" => lead_flag(rest, "--robot-label-attention", "label-attention"),
        "hotspots" | "file-hotspots" => lead_flag(rest, "--robot-file-hotspots", "file-hotspots"),
        "file-beads" => rewrite_value_intent(rest, "file-beads", "", "--robot-file-beads", ""),
        "file-relations" => {
            rewrite_value_intent(rest, "file-relations", "", "--robot-file-relations", "")
        }
        "impact" => rewrite_value_intent(rest, "impact", "", "--robot-impact", ""),
        "related" => rewrite_value_intent(rest, "related", "", "--robot-related", ""),
        "blockers" | "blocker-chain" => {
            rewrite_value_intent(rest, "blocker-chain", "", "--robot-blocker-chain", "")
        }
        "impact-network" => {
            rewrite_value_intent(rest, "impact-network", "", "--robot-impact-network", "all")
        }
        "causality" => rewrite_value_intent(rest, "causality", "", "--robot-causality", ""),
        "sprints" | "sprint-list" => lead_flag(rest, "--robot-sprint-list", "sprint-list"),
        "sprint" | "sprint-show" => {
            rewrite_value_intent(rest, "sprint-show", "", "--robot-sprint-show", "")
        }
        "forecast" => rewrite_value_intent(rest, "forecast", "", "--robot-forecast", "all"),
        "capacity" => lead_flag(rest, "--robot-capacity", "capacity"),
        "burndown" => rewrite_value_intent(rest, "burndown", "", "--robot-burndown", "current"),
        "upgrade" | "self-update" | "selfupdate" => rewrite_upgrade_intent(rest),
        // bvr ships three bare words Go only accepts in `robot-` prefixed
        // form. They are pure conveniences over flags Go already defines, so
        // they stay; every word Go does know is routed through Go's own table.
        "drift" => lead_flag(rest, "--robot-drift", "drift"),
        "orphans" => lead_flag(rest, "--robot-orphans", "orphans"),
        "correlation-stats" => lead_flag(rest, "--robot-correlation-stats", "correlation-stats"),
        _ => return None,
    };
    Some(rewritten)
}

/// Go `rewriteCanonicalRobotCommandIntent` (cmd/bv/main.go:707): the documented
/// `bv robot-<command>` spellings, which accept the same trailing arguments as
/// their short forms.
fn rewrite_canonical_robot_command(command: &str, rest: &[String]) -> Option<Vec<String>> {
    let rewritten = match command {
        // Go special-cases `robot-help --json`: the help payload is prose, so a
        // structured-output request redirects to the docs guide.
        "robot-help" => {
            if contains_structured_output_alias(rest) {
                let mut with_guide = vec!["guide".to_string()];
                with_guide.extend_from_slice(rest);
                return Some(rewrite_docs_intent(&with_guide));
            }
            lead_flag(rest, "--robot-help", "help")
        }
        "robot-triage"
        | "robot-triage-by-track"
        | "robot-triage-by-label"
        | "robot-next"
        | "robot-plan"
        | "robot-insights"
        | "robot-priority"
        | "robot-alerts"
        | "robot-suggest"
        | "robot-recipes"
        | "robot-metrics"
        | "robot-label-health"
        | "robot-label-flow"
        | "robot-label-attention"
        | "robot-file-hotspots"
        | "robot-sprint-list"
        | "robot-capacity"
        | "robot-capabilities"
        | "robot-orphans"
        | "robot-correlation-stats" => {
            let context = command.strip_prefix("robot-").unwrap_or(command);
            lead_flag(rest, &format!("--{command}"), context)
        }
        "robot-docs" => rewrite_docs_intent(rest),
        "robot-schema" => rewrite_schema_intent(rest),
        "robot-search" => rewrite_search_intent(rest),
        "robot-graph" => rewrite_graph_intent(rest),
        "robot-diff" => rewrite_value_intent(rest, "diff", "--robot-diff", "--diff-since", ""),
        "robot-history" => {
            rewrite_value_intent(rest, "history", "--robot-history", "--bead-history", "")
        }
        "robot-explain-correlation" => {
            rewrite_value_intent(rest, "correlation", "", "--robot-explain-correlation", "")
        }
        "robot-confirm-correlation" => {
            rewrite_value_intent(rest, "correlation", "", "--robot-confirm-correlation", "")
        }
        "robot-reject-correlation" => {
            rewrite_value_intent(rest, "correlation", "", "--robot-reject-correlation", "")
        }
        "robot-file-beads" => {
            rewrite_value_intent(rest, "file-beads", "", "--robot-file-beads", "")
        }
        "robot-file-relations" => {
            rewrite_value_intent(rest, "file-relations", "", "--robot-file-relations", "")
        }
        "robot-impact" => rewrite_value_intent(rest, "impact", "", "--robot-impact", ""),
        "robot-related" => rewrite_value_intent(rest, "related", "", "--robot-related", ""),
        "robot-blocker-chain" => {
            rewrite_value_intent(rest, "blocker-chain", "", "--robot-blocker-chain", "")
        }
        "robot-impact-network" => {
            rewrite_value_intent(rest, "impact-network", "", "--robot-impact-network", "all")
        }
        "robot-causality" => rewrite_value_intent(rest, "causality", "", "--robot-causality", ""),
        "robot-sprint-show" => {
            rewrite_value_intent(rest, "sprint-show", "", "--robot-sprint-show", "")
        }
        "robot-forecast" => rewrite_value_intent(rest, "forecast", "", "--robot-forecast", "all"),
        "robot-burndown" => {
            rewrite_value_intent(rest, "burndown", "", "--robot-burndown", "current")
        }
        // Drift carries its companion flag in the rewrite so the pair can
        // never be requested half-way.
        "robot-drift" => {
            let mut out = vec!["--check-drift".to_string(), "--robot-drift".to_string()];
            out.extend(rewrite_flag_aliases(rest, "drift"));
            out
        }
        _ => return None,
    };
    Some(rewritten)
}

/// Go `rewriteRobotValueIntent` (cmd/bv/main.go:822) — the shape shared by
/// every subcommand that takes one positional target (`diff HEAD~1`,
/// `related bv-123`, `forecast`, ...).
///
/// `bool_flag` is emitted first and is empty for commands that have none;
/// `value_flag` names the flag the positional is bound to; `default_value`
/// supplies that flag when the positional is absent but the command has a
/// sensible default.
fn rewrite_value_intent(
    rest: &[String],
    context: &str,
    bool_flag: &str,
    value_flag: &str,
    default_value: &str,
) -> Vec<String> {
    let (prefix, mut rest) = consume_leading_flag_aliases(rest, context);
    let mut out: Vec<String> = Vec::with_capacity(rest.len() + 3);
    if !bool_flag.is_empty() {
        out.push(bool_flag.to_string());
    }
    if let Some(target) = rest.first().filter(|a| is_positional_value(a)) {
        out.push(value_flag.to_string());
        out.push(target.clone());
        rest = &rest[1..];
    } else if !default_value.is_empty() {
        out.push(value_flag.to_string());
        out.push(default_value.to_string());
    } else if !value_flag.is_empty() && bool_flag.is_empty() {
        // No value and no default: Go pushes the bare flag to the *end* so
        // the parser names the missing argument against the right flag
        // instead of consuming the user's next token as its value.
        out.extend(prefix);
        out.extend(rewrite_flag_aliases(rest, context));
        out.push(value_flag.to_string());
        return out;
    }
    out.extend(prefix);
    out.extend(rewrite_flag_aliases(rest, context));
    out
}

/// Go `rewriteRobotDocsIntent` (cmd/bv/main.go:762).
fn rewrite_docs_intent(rest: &[String]) -> Vec<String> {
    let (prefix, rest) = consume_leading_flag_aliases(rest, "docs");
    let mut out = vec!["--robot-docs".to_string()];
    let topic = match rest.first() {
        Some(first) if is_positional_value(first) => {
            out.push(first.clone());
            &rest[1..]
        }
        _ => {
            out.push("guide".to_string());
            rest
        }
    };
    out.extend(prefix);
    out.extend(rewrite_flag_aliases(topic, "docs"));
    out
}

/// Go `rewriteRobotSchemaIntent` (cmd/bv/main.go:775).
fn rewrite_schema_intent(rest: &[String]) -> Vec<String> {
    let (prefix, rest) = consume_leading_flag_aliases(rest, "schema");
    let mut out = vec!["--robot-schema".to_string()];
    if let Some(command) = rest.first() {
        if is_positional_value(command) {
            out.push("--schema-command".to_string());
            out.push(normalize_robot_command_name(command));
            out.extend(prefix);
            out.extend(rewrite_flag_aliases(&rest[1..], "schema"));
            return out;
        }
    }
    out.extend(prefix);
    out.extend(rewrite_flag_aliases(rest, "schema"));
    out
}

/// Go `rewriteRobotSearchIntent` (cmd/bv/main.go:786): gathers every bare
/// token as query text (joined with spaces) and hoists it onto `--search`,
/// leaving `--robot-search` as the primary.
fn rewrite_search_intent(rest: &[String]) -> Vec<String> {
    let mut prefix: Vec<String> = Vec::new();
    let mut query_parts: Vec<String> = Vec::new();
    let mut rest = rest;
    while !rest.is_empty() {
        if is_positional_value(&rest[0]) {
            query_parts.push(rest[0].clone());
            rest = &rest[1..];
            continue;
        }
        let (aliases, remaining) = consume_leading_flag_aliases(rest, "search");
        if aliases.is_empty() {
            break;
        }
        prefix.extend(aliases);
        rest = remaining;
    }

    let mut out = Vec::with_capacity(query_parts.len() + prefix.len() + 3);
    if !query_parts.is_empty() {
        out.push("--search".to_string());
        out.push(query_parts.join(" "));
    }
    out.push("--robot-search".to_string());
    out.extend(prefix);
    out.extend(rewrite_flag_aliases(rest, "search"));
    out
}

/// Go `rewriteRobotGraphIntent` (cmd/bv/main.go:811).
fn rewrite_graph_intent(rest: &[String]) -> Vec<String> {
    let (prefix, rest) = consume_leading_flag_aliases(rest, "graph");
    let mut out = vec!["--robot-graph".to_string()];
    if let Some(format) = rest.first() {
        if is_graph_format(format) {
            out.push("--graph-format".to_string());
            out.push(format.to_lowercase());
            out.extend(prefix);
            out.extend(rewrite_flag_aliases(&rest[1..], "graph"));
            return out;
        }
    }
    out.extend(prefix);
    out.extend(rewrite_flag_aliases(rest, "graph"));
    out
}

/// Go `rewriteUpgradeIntent` (cmd/bv/main.go:667) — the ergonomic `bv upgrade`
/// verb over the self-update flags:
///
/// ```text
/// bv upgrade            -> --update
/// bv upgrade --yes/-y   -> --update --yes
/// bv upgrade --check    -> --check-update
/// bv upgrade --dry-run  -> --update-dry-run
/// bv upgrade --rollback -> --rollback
/// ```
///
/// Bare-word aliases are accepted too; unrecognized tokens pass through so the
/// parser reports genuine typos instead of silently dropping them.
fn rewrite_upgrade_intent(rest: &[String]) -> Vec<String> {
    let mut mode = "update";
    let mut yes = false;
    let mut passthrough: Vec<String> = Vec::new();
    for arg in rest {
        match arg.to_lowercase().trim() {
            "check" | "--check" | "check-update" | "--check-update" => {
                if mode == "update" {
                    mode = "check";
                }
            }
            "dry-run" | "--dry-run" | "dryrun" | "--dryrun" => {
                if mode == "update" {
                    mode = "dry-run";
                }
            }
            "rollback" | "--rollback" => mode = "rollback",
            "yes" | "--yes" | "-y" | "force" | "--force" => yes = true,
            _ => passthrough.push(arg.clone()),
        }
    }

    let mut out = match mode {
        "check" => vec!["--check-update".to_string()],
        "dry-run" => vec!["--update-dry-run".to_string()],
        "rollback" => vec!["--rollback".to_string()],
        _ => {
            let mut v = vec!["--update".to_string()];
            if yes {
                v.push("--yes".to_string());
            }
            v
        }
    };
    out.extend(passthrough);
    out
}

/// `[]string{"<flag>"} + rewriteAgentIntentFlagAliases(rest, context)` — the
/// common shape of every subcommand that takes no positional.
fn lead_flag(rest: &[String], flag: &str, context: &str) -> Vec<String> {
    let mut out = vec![flag.to_string()];
    out.extend(rewrite_flag_aliases(rest, context));
    out
}

/// Go `rewriteAgentIntentFlagAliases` (cmd/bv/main.go:880).
fn rewrite_flag_aliases(args: &[String], context: &str) -> Vec<String> {
    let mut rewritten: Vec<String> = Vec::with_capacity(args.len() + 2);
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "--json" => rewritten.extend(["--format".to_string(), "json".to_string()]),
            "--toon" => rewritten.extend(["--format".to_string(), "toon".to_string()]),
            "--output" | "-o" => {
                // Only a format value is an alias; a file path (the export use)
                // is left for the normal parse path.
                match args.get(i + 1).filter(|next| is_robot_output_format(next)) {
                    Some(next) => {
                        rewritten.extend(["--format".to_string(), next.to_lowercase()]);
                        i += 1;
                    }
                    None => rewritten.push(arg.clone()),
                }
            }
            "--name" => rewritten.push("--label".to_string()),
            "--limit" => rewritten.push(limit_flag_for_context(context).to_string()),
            other => rewritten.push(rewrite_equals_arg(other, context)),
        }
        i += 1;
    }
    rewritten
}

/// Go `consumeLeadingAgentIntentFlagAliases` (cmd/bv/main.go:842): like
/// `rewrite_flag_aliases` but stops at the first token it does not recognize,
/// so a subcommand can read its positional *after* a leading alias
/// (`bv search --limit 5 login oauth`).
fn consume_leading_flag_aliases<'a>(
    args: &'a [String],
    context: &str,
) -> (Vec<String>, &'a [String]) {
    let mut rewritten: Vec<String> = Vec::new();
    let mut rest = args;
    while let Some(arg) = rest.first() {
        match arg.as_str() {
            "--json" => {
                rewritten.extend(["--format".to_string(), "json".to_string()]);
                rest = &rest[1..];
            }
            "--toon" => {
                rewritten.extend(["--format".to_string(), "toon".to_string()]);
                rest = &rest[1..];
            }
            "--output" | "-o" => match rest.get(1).filter(|next| is_robot_output_format(next)) {
                Some(next) => {
                    rewritten.extend(["--format".to_string(), next.to_lowercase()]);
                    rest = &rest[2..];
                }
                None => return (rewritten, rest),
            },
            "--limit" => match rest.get(1) {
                Some(value) => {
                    rewritten.push(limit_flag_for_context(context).to_string());
                    rewritten.push(value.clone());
                    rest = &rest[2..];
                }
                None => return (rewritten, rest),
            },
            other => match rewrite_leading_equals_alias(other, context) {
                Some(alias) => {
                    rewritten.push(alias);
                    rest = &rest[1..];
                }
                None => return (rewritten, rest),
            },
        }
    }
    (rewritten, rest)
}

/// Go `rewriteAgentIntentEqualsArg` (cmd/bv/main.go:907).
fn rewrite_equals_arg(arg: &str, context: &str) -> String {
    if let Some(alias) = rewrite_leading_equals_alias(arg, context) {
        return alias;
    }
    if let Some(value) = arg.strip_prefix("--name=") {
        return format!("--label={value}");
    }
    arg.to_string()
}

/// Go `rewriteLeadingAgentIntentEqualsAlias` (cmd/bv/main.go:925).
fn rewrite_leading_equals_alias(arg: &str, context: &str) -> Option<String> {
    match arg {
        // A boolean spelling never changes the requested format: `--toon=false`
        // still asks for structured output, it just does not ask for TOON.
        "--json=true" | "--json=false" => return Some("--format=json".to_string()),
        "--toon=true" => return Some("--format=toon".to_string()),
        "--toon=false" => return Some("--format=json".to_string()),
        _ => {}
    }
    for prefix in ["--output=", "-o="] {
        if let Some(value) = arg.strip_prefix(prefix) {
            if is_robot_output_format(value) {
                return Some(format!("--format={}", value.to_lowercase()));
            }
        }
    }
    if let Some(value) = arg.strip_prefix("--limit=") {
        return Some(format!("{}={value}", limit_flag_for_context(context)));
    }
    None
}

/// Go `limitFlagForAgentContext` (cmd/bv/main.go:951): each context has its own
/// limit flag; everything else shares the generic `--robot-max-results`.
fn limit_flag_for_context(context: &str) -> &'static str {
    match context {
        "search" => "--search-limit",
        "label-attention" => "--attention-limit",
        "file-beads" => "--file-beads-limit",
        "file-hotspots" => "--hotspots-limit",
        "history" => "--history-limit",
        "related" => "--related-max-results",
        "file-relations" => "--relations-limit",
        _ => "--robot-max-results",
    }
}

/// Go `normalizeRobotCommandName` (cmd/bv/main.go:972).
fn normalize_robot_command_name(value: &str) -> String {
    let value = value.to_lowercase();
    let value = value.trim();
    if value.is_empty() || value.starts_with("robot-") {
        value.to_string()
    } else {
        format!("robot-{value}")
    }
}

/// Go `isPositionalValue` (cmd/bv/main.go:1141).
fn is_positional_value(arg: &str) -> bool {
    !arg.is_empty() && !arg.starts_with('-')
}

/// Go `isRobotOutputFormat` (cmd/bv/main.go:1145).
fn is_robot_output_format(value: &str) -> bool {
    matches!(value.to_lowercase().trim(), "json" | "toon")
}

/// Go `isGraphFormat` (cmd/bv/main.go:1155).
fn is_graph_format(value: &str) -> bool {
    matches!(value.to_lowercase().trim(), "json" | "dot" | "mermaid")
}

/// Check whether args carry an agent-intent structured-output alias.
///
/// Deliberately does *not* count a bare `--format`: Go's auto-promote keys off
/// the aliases, so `bv --format toon` with no primary drops into the TUI
/// rather than defaulting to `--robot-triage`. Called before the alias rewrite,
/// while the alias spellings are still present.
fn contains_structured_output_alias(args: &[String]) -> bool {
    args.iter().enumerate().any(|(i, arg)| {
        let lower = arg.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "--json"
                | "--json=true"
                | "--json=false"
                | "--toon"
                | "--toon=true"
                | "--toon=false"
                | "--output=json"
                | "--output=toon"
                | "-o=json"
                | "-o=toon"
        ) {
            return true;
        }
        (arg == "--output" || arg == "-o")
            && args
                .get(i + 1)
                .is_some_and(|next| is_robot_output_format(next))
    })
}

/// Robot primary flag names (stripped of `--` prefix) for the auto-promote check.
/// Go `primaryRobotFlagNames` (cmd/bv/main.go:1096).
const ROBOT_PRIMARY_NAMES: &[&str] = &[
    "robot-help",
    "robot-capabilities",
    "robot-docs",
    "robot-insights",
    "robot-plan",
    "robot-priority",
    "robot-triage",
    "robot-next",
    "robot-triage-by-track",
    "robot-triage-by-label",
    "robot-diff",
    "robot-recipes",
    "robot-metrics",
    "robot-schema",
    "robot-suggest",
    "robot-graph",
    "robot-search",
    "robot-drift",
    "robot-history",
    "robot-explain-correlation",
    "robot-confirm-correlation",
    "robot-reject-correlation",
    "robot-correlation-stats",
    "robot-orphans",
    "robot-file-beads",
    "robot-file-hotspots",
    "robot-impact",
    "robot-file-relations",
    "robot-related",
    "robot-blocker-chain",
    "robot-impact-network",
    "robot-causality",
    "robot-sprint-list",
    "robot-sprint-show",
    "robot-forecast",
    "robot-capacity",
    "robot-burndown",
    "robot-label-health",
    "robot-label-flow",
    "robot-label-attention",
];

/// Flags that claim an invocation on their own. Go `hasNonRobotPrimaryArg`
/// (cmd/bv/main.go:1005).
const NON_ROBOT_PRIMARY_NAMES: &[&str] = &[
    "version",
    "help",
    "check-update",
    "update",
    "update-dry-run",
    "rollback",
    "pages",
    "export-pages",
    "preview-pages",
    "export",
    "export-md",
    "export-graph",
];

/// Go `hasPrimaryRobotArg` (cmd/bv/main.go:995).
fn has_primary_robot_arg(args: &[String]) -> bool {
    args.iter()
        .any(|arg| primary_flag_name(arg, ROBOT_PRIMARY_NAMES))
}

/// Go `hasNonRobotPrimaryArg` (cmd/bv/main.go:1005).
fn has_non_robot_primary_arg(args: &[String]) -> bool {
    args.iter()
        .any(|arg| primary_flag_name(arg, NON_ROBOT_PRIMARY_NAMES))
}

/// Shared body of both `has*PrimaryArg` helpers: strip a `--flag=value` token
/// down to its bare name and test membership.
fn primary_flag_name(arg: &str, names: &[&str]) -> bool {
    let name = arg.split('=').next().unwrap_or(arg);
    let name = name.strip_prefix("--").unwrap_or(name);
    names.contains(&name)
}

/// First bare positional left unconsumed after `rewrite_args`, if any.
///
/// Go's flag parser rejects any non-flag argument with
/// `unknown command "X" for "bv"`, so only agent-intent words (which
/// `rewrite_args` rewrites into `--robot-*`) and flag values are legal.
/// `bvr` must do the same instead of silently falling through to the
/// interactive TUI — an agent or CI caller has no way to drive a TUI and
/// would just block. Returns `None` when every bare token was consumed
/// (rewritten to a flag) or belongs to a flag's value.
pub fn unconsumed_positional(rewritten: &[String]) -> Option<String> {
    // A value-taking flag swallows the NEXT token unconditionally, even one
    // that looks like a flag — Go's pflag does not re-examine it
    // (pflag's `parseLongArg`: `case len(args) > 0: value = args[0]`). So
    // `bv --export-md --export-format json` gives `--export-md` the value
    // `--export-format` and leaves `json` as the leftover word, which Go
    // reports as `unknown command "json"`. Reading the next token as a flag
    // instead swallowed `json` and the run continued.
    let mut skip_next = false;
    // Input is argv-minus-program (see `rewrite_args`), so index 0 is
    // the first real argument — do not skip it.
    for tok in rewritten.iter() {
        if skip_next {
            skip_next = false;
            continue;
        }
        if tok.starts_with('-') && tok != "-" {
            if tok == "--" {
                // Go's pflag stops parsing at `--`; everything after it is
                // positional.
                continue;
            }
            skip_next = flag_takes_value(tok);
            continue;
        }
        return Some(tok.clone());
    }
    None
}

/// The first long flag Go's `flag` package would reject as undefined.
///
/// Go's parse walks argv left to right and stops at the first non-flag word,
/// so anything after a positional is not this function's business. Short
/// flags are out of scope for the same reason `unknownLongFlagName` is: the
/// oracle's message and suggestion path are both written for `--long`.
///
/// The guard this feeds is not cosmetic. Without it a typo like
/// `--robot-triagee` fell through to the interactive TUI, which an agent or CI
/// caller cannot drive — the process blocks until it is killed.
pub fn unknown_long_flag(args: &[String]) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if !arg.starts_with('-') || arg == "-" || arg == "--" {
            return None;
        }
        let (name, has_inline_value) = match arg.split_once('=') {
            Some((n, _)) => (n, true),
            None => (arg.as_str(), false),
        };
        let bare = name.trim_start_matches('-');
        // `-` prefixes of any length name the same flag in Go (`-label` and
        // `--label` are equivalent), and Go does not define a bare `--`.
        let known = !bare.is_empty() && crate::flags::flag_kind(bare).is_some();
        if !known {
            return Some(bare.to_string());
        }
        let takes_value = !has_inline_value
            && !matches!(
                crate::flags::flag_kind(bare),
                Some(crate::flags::FlagKind::Bool) | None
            );
        i += if takes_value { 2 } else { 1 };
    }
    None
}

/// Go `maxSuggestionDistance` (cmd/bv/main.go:411-420): a longer name may sit
/// further from its match before the suggestion stops being useful.
fn max_suggestion_distance(value: &str) -> usize {
    match value.len() {
        0..=4 => 2,
        5..=10 => 3,
        _ => 4,
    }
}

/// Go `levenshteinDistance` (cmd/bv/main.go), the standard edit distance.
fn levenshtein(a: &str, b: &str) -> usize {
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

/// Go `suggestClosest` (cmd/bv/main.go:389-409) over the registered flag names.
///
/// The tie-break is load-bearing and easy to get backwards: Go keeps a
/// candidate when `dist <= bestDist` and then prefers the strictly-closer one,
/// or the lexicographically smaller name at equal distance. Reusing `bestDist`
/// as the running threshold is what makes a later equal-distance candidate win.
pub fn suggest_closest_flag(value: &str) -> Option<String> {
    let value = value.trim().to_lowercase();
    if value.is_empty() {
        return None;
    }
    let mut best: Option<(String, usize)> = None;
    for name in crate::flags::flag_names() {
        let normalized = name.trim().to_lowercase();
        if normalized.is_empty() {
            continue;
        }
        let dist = levenshtein(&value, &normalized);
        let within = dist <= max_suggestion_distance(&value);
        let better = match &best {
            None => within,
            Some((best_name, best_dist)) => {
                within && (dist < *best_dist || (dist == *best_dist && normalized < *best_name))
            }
        };
        if better {
            best = Some((normalized, dist));
        }
    }
    best.map(|(name, _)| name)
}

/// Go `correctedUnknownFlagCommand` (cmd/bv/main.go:1299-1323): the whole
/// invocation with the unknown flag swapped for the suggestion, so the reader
/// can copy it. Only reached when the unknown flag actually appears in argv.
pub fn corrected_unknown_flag_command(args: &[String], unknown: &str, suggestion: &str) -> String {
    let target = format!("--{unknown}");
    let replacement = format!("--{suggestion}");
    let mut corrected: Vec<String> = Vec::with_capacity(args.len() + 1);
    let mut replaced = false;
    for arg in args {
        if arg == &target {
            corrected.push(replacement.clone());
            replaced = true;
        } else if let Some(rest) = arg.strip_prefix(&format!("{target}=")) {
            corrected.push(format!("{replacement}={rest}"));
            replaced = true;
        } else {
            corrected.push(arg.clone());
        }
    }
    if !replaced {
        corrected.push(replacement);
    }
    std::iter::once("bv".to_string())
        .chain(corrected)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Go `strconv.ParseBool` — the parser pflag's `boolValue.Set` calls for the
/// optional `=` value of a boolean flag (cmd/bv's registry is a pflag set;
/// `github.com/spf13/pflag v1.0.10`, go.mod:27).
/// The accepted set is Go's, not a looser shell convention: `1 t T TRUE
/// true True` and `0 f F FALSE false False`. In particular `y`, `yes`, `n`,
/// `no` and `on` are syntax errors — verified against the v0.25.0 oracle,
/// where `bvr --pages-include-history=yes` prints
/// `invalid argument "yes" for "--pages-include-history" flag: strconv.ParseBool: parsing "yes": invalid syntax`.
pub fn go_parse_bool(raw: &str) -> Option<bool> {
    match raw {
        "1" | "t" | "T" | "TRUE" | "true" | "True" => Some(true),
        "0" | "f" | "F" | "FALSE" | "false" | "False" => Some(false),
        _ => None,
    }
}

/// The pflag error text for a malformed boolean flag value, byte-for-byte
/// (`pflag`'s `failf("invalid argument %q for %q flag: %v", value, flag, err)`
/// wrapping Go's own `strconv.NumError`).
pub fn go_bool_parse_error(flag: &str, raw: &str) -> String {
    format!(
        "invalid argument {raw:?} for \"--{flag}\" flag: strconv.ParseBool: parsing {raw:?}: invalid syntax"
    )
}

/// Go `isFlagActive` (cmd/bv/main.go:471-486) for a **bool** flag, read
/// straight off raw argv.
///
/// Go's version asks pflag for the already-parsed value:
/// `v, err := flags.GetBool(name); return err == nil && v`. The parse itself
/// happened in `boolValue.Set`, which rejects anything `strconv.ParseBool`
/// refuses — so an active bool flag is one that was written bare, or with a
/// truthy `=value`. `--flag=false` is present but *inactive*, which is the
/// whole point: `--pages-include-history=false` is how Go turns off a
/// default-true flag.
///
/// A presence scan (`args.iter().any(|a| a == "--flag")`) gets this exactly
/// backwards — neither `--flag=false` nor `--flag=true` string-equals the bare
/// name, so both read as absent and the flag falls back to its default.
///
/// `default` is Go's registered default, returned when the flag is absent.
/// `Err` carries Go's rejection text for a malformed value, so the caller can
/// print it and exit 1 the way pflag does.
pub fn go_bool_flag(args: &[String], name: &str, default: bool) -> Result<bool, String> {
    let long = format!("--{name}");
    let with_eq = format!("--{name}=");
    // pflag applies flags left to right and each `Set` overwrites, so the
    // LAST occurrence is the effective one.
    let mut raw: Option<&str> = None;
    for a in args.iter() {
        if let Some(v) = a.strip_prefix(&with_eq) {
            raw = Some(v);
        } else if a == &long {
            // A bare bool switch never consumes the next token (pflag gives
            // bools `NoOptDefVal = "true"`), so it is genuinely valueless and
            // parses as the truthy default.
            raw = Some("true");
        }
    }
    match raw {
        None => Ok(default),
        Some(v) => go_parse_bool(v).ok_or_else(|| go_bool_parse_error(name, v)),
    }
}

/// Go `isFlagActive`'s **string** arm (cmd/bv/main.go:479-481): a string flag
/// is active only when its value survives `strings.TrimSpace`. So
/// `--db ""` and `--db "  "` are both *inactive*.
pub fn go_string_flag_active(args: &[String], name: &str) -> bool {
    match go_string_flag_value(args, name) {
        Some(v) => !v.trim().is_empty(),
        None => false,
    }
}

/// Read `--name value` / `--name=value` out of raw argv, borrowed. Same scan
/// as `flag_value` in main.rs; lives here so the argv-level helpers that need
/// it do not have to reach across crate modules.
pub fn go_string_flag_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    let long = format!("--{name}");
    let with_eq = format!("--{name}=");
    for (i, a) in args.iter().enumerate() {
        if let Some(v) = a.strip_prefix(&with_eq) {
            return Some(v);
        }
        if a == &long {
            return args.get(i + 1).map(|s| s.as_str());
        }
    }
    None
}

/// Whether a `--flag` consumes the following token as its value.
fn flag_takes_value(tok: &str) -> bool {
    let name = tok.split('=').next().unwrap_or(tok);
    let name = name.trim_start_matches('-');
    // A `-`/`--` token written as `name=value` never takes a following value.
    if tok.contains('=') {
        return false;
    }
    // A BOOLEAN flag never consumes the next token; every other registered
    // flag does.
    //
    // The list of booleans used to be spelled out by hand, and it was short:
    // `--robot-triage` was missing from it, so the token after it was read
    // as its value. `bvr --robot-triage somejunk` therefore never reported
    // `somejunk` as the unknown command Go reports, and the stray word
    // disappeared instead. The registry already knows each flag's kind, so
    // asking it is both shorter and self-maintaining.
    //
    // `cpu-profile` is a STRING flag in Go (`flag.String("cpu-profile", "",
    // ...)` at cmd/bv/main.go:1460, wired into cobra via AddFlagSet at :518),
    // so `--cpu-profile out.pprof` swallows the path - as it must.
    !matches!(
        crate::flags::flag_kind(name),
        Some(crate::flags::FlagKind::Bool)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn single_dash_long_flag_normalized() {
        assert_eq!(rewrite_args(&s(&["-robot-triage"])), s(&["--robot-triage"]));
    }

    /// Go decides `-foo` is a long flag by asking the registry
    /// (`flags.Lookup(name) == nil`, main.go:548), never by the shape of the
    /// name. A value that merely *looks* like a flag must reach the value
    /// parser untouched, so that `--relations-threshold -1e400` is reported
    /// against the value instead of being rewritten into the nonsense flag
    /// `--1e400` (which pflag rejects as a malformed spelling).
    #[test]
    fn negative_value_is_not_rewritten_into_a_flag() {
        for value in [
            "-1e400", "-Inf", "-1e-400", "-NaN", "-0.5", "-1.5", "-foo", "-a.b", "-a_b", "-1e400x",
        ] {
            assert_eq!(
                rewrite_args(&s(&["--relations-threshold", value])),
                s(&["--relations-threshold", value]),
                "value {value} must survive normalization untouched"
            );
        }
    }

    /// The mirror image: a token that *is* a registered flag still gets the
    /// extra dash, whatever sits on the other side of the `=`.
    #[test]
    fn registered_single_dash_flags_are_still_rewritten() {
        for arg in [
            "-robot-triage",
            "-relations-threshold=-1e400",
            "-db",
            "-version",
        ] {
            assert_eq!(
                rewrite_args(&s(&[arg])),
                s(&[&format!("-{arg}")]),
                "{arg} names a registered flag and must be rewritten"
            );
        }
    }

    /// cobra adds its `--help` flag during `Execute`, i.e. *after*
    /// `rewriteSingleDashLongFlags` has already consulted the flag set
    /// (main.go:4546). So `help` is not a registry name at rewrite time and Go
    /// leaves `-help` for pflag, which answers
    /// `unknown shorthand flag: 'e' in -elp` (exit 1) rather than printing help.
    #[test]
    fn cobra_help_is_absent_from_the_rewrite_registry() {
        assert_eq!(rewrite_args(&s(&["-help"])), s(&["-help"]));
        assert_eq!(rewrite_args(&s(&["-h"])), s(&["-h"]));
    }

    #[test]
    fn short_flags_untouched() {
        // Unmapped short flags (e.g. -x) still pass through; Go only advertises
        // -f/-l/-r/-h, so anything else is left alone.
        assert_eq!(rewrite_args(&s(&["-x"])), s(&["-x"]));
    }

    /// Go's flag parser rejects bare positionals with
    /// `unknown command "X" for "bv"` (exit 1). `bvr` used to fall through to
    /// the TUI instead, which blocks any non-interactive caller.
    #[test]
    fn unknown_positional_is_reported() {
        for word in ["version", "help", "check-update", "rollback", "bogusxyz"] {
            let args = rewrite_args(&s(&[word]));
            assert_eq!(
                unconsumed_positional(&args).as_deref(),
                Some(word),
                "expected {word} to be reported as an unknown command"
            );
        }
    }

    /// A bare word that is an agent-intent alias is rewritten to `--robot-*`
    /// and must therefore NOT be reported as unknown.
    #[test]
    fn intent_alias_is_not_unknown() {
        let args = rewrite_args(&s(&["triage"]));
        assert_eq!(args, s(&["--robot-triage"]));
        assert_eq!(unconsumed_positional(&args), None);
    }

    /// A flag's value must be consumed, never mistaken for a command. This is
    /// the false-positive guard: `bv --label triage` is a filter, not a command.
    #[test]
    fn flag_value_is_not_unknown() {
        for argv in [
            vec!["--label", "triage"],
            vec!["--format", "toon"],
            vec!["--repo", "myrepo"],
            // `--flag=value` carries its own value.
            vec!["--label=triage"],
        ] {
            let args = rewrite_args(&s(&argv));
            assert_eq!(
                unconsumed_positional(&args),
                None,
                "expected {argv:?} to be fully consumed"
            );
        }
    }

    /// A boolean switch does not swallow the next token, so a following bare
    /// word is still an unknown command.
    #[test]
    fn bool_flag_does_not_swallow_next() {
        let args = rewrite_args(&s(&["--brief", "version"]));
        assert_eq!(unconsumed_positional(&args).as_deref(), Some("version"));
    }

    /// `--cpu-profile` is a Go `flag.String` (cmd/bv/main.go:1460), so the
    /// output path is its value. Treating it as a boolean made
    /// `bv --cpu-profile out.pprof` report the path as an unknown command and
    /// exit 1 (cmd/bv accepts the form and writes the profile there).
    #[test]
    fn cpu_profile_takes_a_value() {
        for argv in [
            vec!["--cpu-profile", "out.pprof"],
            vec!["--cpu-profile", "out.pprof", "--robot-triage"],
            // `--flag=value` already carried its own value.
            vec!["--cpu-profile=out.pprof"],
        ] {
            let args = rewrite_args(&s(&argv));
            assert_eq!(
                unconsumed_positional(&args),
                None,
                "expected {argv:?} to be fully consumed"
            );
        }
    }

    /// `--baseline-info` is a Go `flag.Bool` (cmd/bv/main.go:1542), so it
    /// never consumes the following token. Without it in the boolean list,
    /// `bv --baseline-info foo` swallowed `foo` instead of rejecting it with
    /// `unknown command "foo" for "bv"`.
    #[test]
    fn baseline_info_is_a_boolean_switch() {
        let args = rewrite_args(&s(&["--baseline-info", "foo"]));
        assert_eq!(unconsumed_positional(&args).as_deref(), Some("foo"));
        // `--baseline-info=value` is meaningless for a bool but must still not
        // be treated as consuming a following positional.
        let args = rewrite_args(&s(&["--baseline-info=true", "bar"]));
        assert_eq!(unconsumed_positional(&args).as_deref(), Some("bar"));
    }

    #[test]
    fn no_args_is_clean() {
        assert_eq!(unconsumed_positional(&rewrite_args(&[])), None);
    }

    /// Issue #5: Go `bv --help` advertises `-f/--format`, `-l/--label`, `-r/--recipe`.
    /// These were previously passing through unexpanded (a documented gap).
    #[test]
    fn short_flag_aliases_expanded() {
        assert_eq!(rewrite_args(&s(&["-f", "toon"])), s(&["--format", "toon"]));
        assert_eq!(
            rewrite_args(&s(&["-l", "backend"])),
            s(&["--label", "backend"])
        );
        assert_eq!(
            rewrite_args(&s(&["-r", "mytable"])),
            s(&["--recipe", "mytable"])
        );
    }

    #[test]
    fn short_flag_alias_with_equals_expanded() {
        assert_eq!(rewrite_args(&s(&["-f=toon"])), s(&["--format=toon"]));
        assert_eq!(rewrite_args(&s(&["-l=backend"])), s(&["--label=backend"]));
    }

    #[test]
    fn agent_intent_alias_rewritten() {
        assert_eq!(rewrite_args(&s(&["triage"])), s(&["--robot-triage"]));
        assert_eq!(rewrite_args(&s(&["next"])), s(&["--robot-next"]));
    }

    #[test]
    fn double_dash_passthrough() {
        assert_eq!(
            rewrite_args(&s(&["--robot-insights", "--format", "toon"])),
            s(&["--robot-insights", "--format", "toon"])
        );
    }

    #[test]
    fn bare_json_auto_promotes_to_triage() {
        assert_eq!(
            rewrite_args(&s(&["--json"])),
            s(&["--robot-triage", "--format", "json"])
        );
    }

    #[test]
    fn bare_toon_auto_promotes_to_triage() {
        assert_eq!(
            rewrite_args(&s(&["--toon"])),
            s(&["--robot-triage", "--format", "toon"])
        );
    }

    #[test]
    fn output_json_auto_promotes_to_triage() {
        // Go rewriteLeadingAgentIntentEqualsAlias maps the `=` forms to
        // `--format=`, so the alias is consumed, not just detected.
        assert_eq!(
            rewrite_args(&s(&["--output=json"])),
            s(&["--robot-triage", "--format=json"])
        );
    }

    #[test]
    fn o_json_auto_promotes_to_triage() {
        assert_eq!(
            rewrite_args(&s(&["-o=json"])),
            s(&["--robot-triage", "--format=json"])
        );
    }

    #[test]
    fn json_with_existing_primary_not_promoted() {
        assert_eq!(
            rewrite_args(&s(&["--robot-insights", "--format", "json"])),
            s(&["--robot-insights", "--format", "json"])
        );
    }

    #[test]
    fn output_space_json_auto_promotes() {
        assert_eq!(
            rewrite_args(&s(&["--output", "json"])),
            s(&["--robot-triage", "--format", "json"])
        );
    }

    #[test]
    fn alias_recommend_goes_to_triage() {
        assert_eq!(rewrite_args(&s(&["recommend"])), s(&["--robot-triage"]));
    }

    #[test]
    fn alias_pick_goes_to_next() {
        assert_eq!(rewrite_args(&s(&["pick"])), s(&["--robot-next"]));
    }

    /// A value command with no positional and no default still emits its
    /// boolean companion and nothing else.
    #[test]
    fn alias_diff_goes_to_robot_diff() {
        assert_eq!(rewrite_args(&s(&["diff"])), s(&["--robot-diff"]));
    }

    #[test]
    fn alias_find_goes_to_robot_search() {
        assert_eq!(rewrite_args(&s(&["find"])), s(&["--robot-search"]));
    }

    #[test]
    fn no_auto_promote_when_version() {
        assert_eq!(rewrite_args(&s(&["--version"])), s(&["--version"]));
    }

    #[test]
    fn alias_capabilities_goes_to_robot_capabilities() {
        assert_eq!(
            rewrite_args(&s(&["capabilities"])),
            s(&["--robot-capabilities"])
        );
    }

    #[test]
    fn alias_blockers_goes_to_robot_blocker_chain() {
        assert_eq!(
            rewrite_args(&s(&["blockers"])),
            s(&["--robot-blocker-chain"])
        );
    }

    /// Regression: the rewriter used to assume a program-name slot that `main`
    /// strips, so the subcommand table was consulted one position too far and
    /// every `bv upgrade <mode>` silently degraded to `bv --update <mode>`.
    #[test]
    fn upgrade_subcommand_variants() {
        assert_eq!(
            rewrite_args(&s(&["upgrade", "check"])),
            s(&["--check-update"])
        );
        assert_eq!(
            rewrite_args(&s(&["upgrade", "dry-run"])),
            s(&["--update-dry-run"])
        );
        assert_eq!(
            rewrite_args(&s(&["upgrade", "rollback"])),
            s(&["--rollback"])
        );
        assert_eq!(
            rewrite_args(&s(&["upgrade", "--yes"])),
            s(&["--update", "--yes"])
        );
        assert_eq!(
            rewrite_args(&s(&["upgrade", "-y"])),
            s(&["--update", "--yes"])
        );
    }

    /// Go's `TestAgentIntentArgRewrite` upgrade cases, verbatim.
    #[test]
    fn upgrade_flag_forms_match_go() {
        assert_eq!(rewrite_args(&s(&["upgrade"])), s(&["--update"]));
        assert_eq!(
            rewrite_args(&s(&["upgrade", "--check"])),
            s(&["--check-update"])
        );
        assert_eq!(
            rewrite_args(&s(&["upgrade", "--dry-run"])),
            s(&["--update-dry-run"])
        );
        assert_eq!(
            rewrite_args(&s(&["upgrade", "--rollback"])),
            s(&["--rollback"])
        );
        assert_eq!(rewrite_args(&s(&["self-update"])), s(&["--update"]));
        // Unknown tokens pass through so the parser reports the typo.
        assert_eq!(
            rewrite_args(&s(&["upgrade", "--bogus"])),
            s(&["--update", "--bogus"])
        );
    }

    /// Regression: the auto-promote fired on `--export-md` invocations. Go
    /// guards it with `hasNonRobotPrimaryArg`.
    #[test]
    fn non_robot_primary_suppresses_auto_promote() {
        for argv in [
            vec!["--version", "--json"],
            vec!["--export-md", "out.md", "--json"],
            vec!["--export", "out.md", "--json"],
            vec!["--export-graph", "g.html", "--toon"],
            vec!["--pages", "--json"],
            vec!["--export-pages", "./dash", "--json"],
            vec!["--preview-pages", "./dash", "--json"],
            vec!["--update-dry-run", "--json"],
        ] {
            let want = rewrite_flag_aliases(&s(&argv), "");
            assert_eq!(
                rewrite_args(&s(&argv)),
                want,
                "expected {argv:?} to keep its own primary"
            );
        }
    }

    /// Go `TestAgentIntentArgRewrite`: subcommand + alias tails.
    #[test]
    fn triage_subcommand_rewrites_flag_aliases() {
        assert_eq!(
            rewrite_args(&s(&[
                "triage", "--json", "--name", "backend", "--limit", "3"
            ])),
            s(&[
                "--robot-triage",
                "--format",
                "json",
                "--label",
                "backend",
                "--robot-max-results",
                "3"
            ])
        );
    }

    #[test]
    fn canonical_robot_command_name() {
        assert_eq!(
            rewrite_args(&s(&["robot-triage", "--json"])),
            s(&["--robot-triage", "--format", "json"])
        );
        assert_eq!(
            rewrite_args(&s(&["robot-triage-by-track", "--json", "--limit=2"])),
            s(&[
                "--robot-triage-by-track",
                "--format",
                "json",
                "--robot-max-results=2"
            ])
        );
    }

    #[test]
    fn canonical_robot_help_with_json_becomes_docs() {
        assert_eq!(
            rewrite_args(&s(&["robot-help", "--json"])),
            s(&["--robot-docs", "guide", "--format", "json"])
        );
    }

    #[test]
    fn json_false_still_avoids_tui() {
        assert_eq!(
            rewrite_args(&s(&["--json=false"])),
            s(&["--robot-triage", "--format=json"])
        );
    }

    #[test]
    fn toon_false_keeps_structured_output_without_forcing_toon() {
        assert_eq!(
            rewrite_args(&s(&["--toon=false"])),
            s(&["--robot-triage", "--format=json"])
        );
    }

    #[test]
    fn schema_subcommand() {
        assert_eq!(
            rewrite_args(&s(&["schema", "triage", "--json"])),
            s(&[
                "--robot-schema",
                "--schema-command",
                "robot-triage",
                "--format",
                "json"
            ])
        );
        assert_eq!(
            rewrite_args(&s(&["robot-schema", "triage", "--json"])),
            s(&[
                "--robot-schema",
                "--schema-command",
                "robot-triage",
                "--format",
                "json"
            ])
        );
        // Output alias may come before the command.
        assert_eq!(
            rewrite_args(&s(&["schema", "--json", "triage"])),
            s(&[
                "--robot-schema",
                "--schema-command",
                "robot-triage",
                "--format",
                "json"
            ])
        );
        // Mixed case is normalized.
        assert_eq!(
            rewrite_args(&s(&["schema", "Robot-Triage", "--json"])),
            s(&[
                "--robot-schema",
                "--schema-command",
                "robot-triage",
                "--format",
                "json"
            ])
        );
    }

    #[test]
    fn search_subcommand_collects_query_words() {
        assert_eq!(
            rewrite_args(&s(&["search", "login", "oauth", "--json", "--limit=5"])),
            s(&[
                "--search",
                "login oauth",
                "--robot-search",
                "--format",
                "json",
                "--search-limit=5"
            ])
        );
        // `--limit <n>` before the query: the space form consumes its value.
        assert_eq!(
            rewrite_args(&s(&["search", "--limit", "5", "login", "oauth", "--json"])),
            s(&[
                "--search",
                "login oauth",
                "--robot-search",
                "--search-limit",
                "5",
                "--format",
                "json"
            ])
        );
        // An alias may sit between two query words.
        assert_eq!(
            rewrite_args(&s(&["search", "login", "--json", "oauth"])),
            s(&[
                "--search",
                "login oauth",
                "--robot-search",
                "--format",
                "json"
            ])
        );
        assert_eq!(
            rewrite_args(&s(&[
                "robot-search",
                "login",
                "oauth",
                "--json",
                "--limit",
                "5"
            ])),
            s(&[
                "--search",
                "login oauth",
                "--robot-search",
                "--format",
                "json",
                "--search-limit",
                "5"
            ])
        );
    }

    /// `bv robot-search Q` used to exit 2 (`unknown command "robot-search"`).
    #[test]
    fn canonical_search_command_with_query_is_not_an_unknown_command() {
        let args = rewrite_args(&s(&["robot-search", "Q"]));
        assert_eq!(args, s(&["--search", "Q", "--robot-search"]));
        assert_eq!(unconsumed_positional(&args), None);
    }

    /// `bv diff HEAD~1` used to leave `HEAD~1` as an unknown command.
    #[test]
    fn diff_subcommand_consumes_its_positional() {
        let args = rewrite_args(&s(&["diff", "HEAD~1", "--json"]));
        assert_eq!(
            args,
            s(&["--robot-diff", "--diff-since", "HEAD~1", "--format", "json"])
        );
        assert_eq!(unconsumed_positional(&args), None);
    }

    #[test]
    fn graph_format_positional() {
        assert_eq!(
            rewrite_args(&s(&["graph", "mermaid", "--output", "json"])),
            s(&[
                "--robot-graph",
                "--graph-format",
                "mermaid",
                "--format",
                "json"
            ])
        );
        assert_eq!(
            rewrite_args(&s(&["robot-graph", "mermaid", "--json"])),
            s(&[
                "--robot-graph",
                "--graph-format",
                "mermaid",
                "--format",
                "json"
            ])
        );
        assert_eq!(
            rewrite_args(&s(&["graph", "--json", "mermaid"])),
            s(&[
                "--robot-graph",
                "--graph-format",
                "mermaid",
                "--format",
                "json"
            ])
        );
    }

    #[test]
    fn value_commands_consume_their_target() {
        // Alias before the target.
        assert_eq!(
            rewrite_args(&s(&["related", "--json", "bv-123"])),
            s(&["--robot-related", "bv-123", "--format", "json"])
        );
        assert_eq!(
            rewrite_args(&s(&["robot-related", "bv-123", "--json", "--limit=2"])),
            s(&[
                "--robot-related",
                "bv-123",
                "--format",
                "json",
                "--related-max-results=2"
            ])
        );
    }

    /// With no value and no default, Go parks the bare flag at the end so the
    /// parser names the right missing argument instead of eating the next token.
    #[test]
    fn missing_value_command_parks_the_flag_last() {
        assert_eq!(
            rewrite_args(&s(&["robot-related", "--json"])),
            s(&["--format", "json", "--robot-related"])
        );
        assert_eq!(
            rewrite_args(&s(&[
                "robot-confirm-correlation",
                "--correlation-by",
                "agent",
                "--json"
            ])),
            s(&[
                "--correlation-by",
                "agent",
                "--format",
                "json",
                "--robot-confirm-correlation"
            ])
        );
    }

    #[test]
    fn canonical_diff_and_drift_commands() {
        assert_eq!(
            rewrite_args(&s(&["robot-diff", "HEAD~1", "--json"])),
            s(&["--robot-diff", "--diff-since", "HEAD~1", "--format", "json"])
        );
        assert_eq!(
            rewrite_args(&s(&["robot-drift", "--json"])),
            s(&["--check-drift", "--robot-drift", "--format", "json"])
        );
    }

    #[test]
    fn docs_subcommand() {
        assert_eq!(
            rewrite_args(&s(&["docs", "--json", "guide"])),
            s(&["--robot-docs", "guide", "--format", "json"])
        );
        assert_eq!(
            rewrite_args(&s(&["robot-docs", "guide", "--json"])),
            s(&["--robot-docs", "guide", "--format", "json"])
        );
        // No topic: Go defaults to the guide.
        assert_eq!(rewrite_args(&s(&["docs"])), s(&["--robot-docs", "guide"]));
    }

    #[test]
    fn update_dry_run_stays_non_robot_with_structured_alias() {
        assert_eq!(
            rewrite_args(&s(&["--update-dry-run", "--json"])),
            s(&["--update-dry-run", "--format", "json"])
        );
    }

    #[test]
    fn name_equals_form_maps_to_label() {
        assert_eq!(
            rewrite_args(&s(&["triage", "--name=backend"])),
            s(&["--robot-triage", "--label=backend"])
        );
    }

    #[test]
    fn structured_output_aliases_normalise_to_format() {
        // Go rewriteAgentIntentFlagAliases (cmd/bv/main.go:880-905). Without
        // this the aliases only triggered the auto-promote and were then
        // dropped, so `bvr --toon` emitted JSON.
        assert_eq!(
            rewrite_args(&s(&["--toon"])),
            s(&["--robot-triage", "--format", "toon"])
        );
        assert_eq!(
            rewrite_args(&s(&["--json"])),
            s(&["--robot-triage", "--format", "json"])
        );
        assert_eq!(
            rewrite_args(&s(&["--output", "toon"])),
            s(&["--robot-triage", "--format", "toon"])
        );
        // Case is normalized on the --output value form.
        assert_eq!(
            rewrite_args(&s(&["--output", "TOON"])),
            s(&["--robot-triage", "--format", "toon"])
        );
    }

    #[test]
    fn format_alias_rewrite_respects_an_explicit_primary() {
        assert_eq!(
            rewrite_args(&s(&["--robot-next", "--toon"])),
            s(&["--robot-next", "--format", "toon"])
        );
    }

    #[test]
    fn output_without_a_format_value_is_not_an_alias() {
        // Go only rewrites `--output` when the next token is a format; a file
        // path (the export use) must pass through untouched.
        assert_eq!(
            rewrite_args(&s(&["--robot-next", "--output", "report.md"])),
            s(&["--robot-next", "--output", "report.md"])
        );
    }

    #[test]
    fn a_bare_format_flag_does_not_auto_promote() {
        // Verified against the oracle: `bv --format toon` with no primary
        // drops into the TUI in Go, it does not default to --robot-triage. The
        // auto-promote keys off the agent-intent aliases only.
        assert_eq!(
            rewrite_args(&s(&["--format", "toon"])),
            s(&["--format", "toon"])
        );
    }

    /// A non-ASCII first byte must not slice mid-character.
    #[test]
    fn multibyte_positional_is_left_alone() {
        assert_eq!(rewrite_args(&s(&["ü"])), s(&["ü"]));
        assert_eq!(
            unconsumed_positional(&rewrite_args(&s(&["ü"]))),
            Some("ü".into())
        );
    }

    // --profile-startup is dispatched on presence, not on a value, so this is
    // the "is a string flag set" question Go's isFlagActive answers. Kept here
    // with a real test because --cpu-profile (the first string flag that reads
    // it) needs a sampling backend that is not yet in the tree.
    #[test]
    fn string_flag_is_active_only_when_the_value_survives_trimspace() {
        // Go main.go:479-481: a string flag is active only when
        // strings.TrimSpace(value) != "". An empty or blank value is INACTIVE,
        // which is what makes a modifier-requires rule fire for `--db ""`.
        assert!(!go_string_flag_active(&s(&["--db", ""]), "db"));
        assert!(!go_string_flag_active(&s(&["--db", "   "]), "db"));
        assert!(!go_string_flag_active(&s(&["--db="]), "db"));
        assert!(!go_string_flag_active(&s(&["--other", "x"]), "db"));
        assert!(go_string_flag_active(&s(&["--db", "/tmp/x"]), "db"));
        assert!(go_string_flag_active(&s(&["--db=/tmp/x"]), "db"));
        // A trailing `--db` with nothing after it has no value at all.
        assert!(!go_string_flag_active(&s(&["--db"]), "db"));
    }

    #[test]
    fn string_flag_value_reads_both_spellings() {
        assert_eq!(
            go_string_flag_value(&s(&["--db", "/tmp/x"]), "db"),
            Some("/tmp/x")
        );
        assert_eq!(
            go_string_flag_value(&s(&["--db=/tmp/x"]), "db"),
            Some("/tmp/x")
        );
        // An `=`-form value keeps everything after the first `=`, so a path
        // containing `=` survives.
        assert_eq!(
            go_string_flag_value(&s(&["--db=/tmp/a=b"]), "db"),
            Some("/tmp/a=b")
        );
        assert_eq!(go_string_flag_value(&s(&["--other", "1"]), "db"), None);
    }
}
