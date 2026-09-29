//! Validation — modifier-requires + exclusive-primary checks.
//! Port of Go validateModifierFlags / validateExclusivePrimaryCommands.

use crate::flags::{FlagKind, MODIFIER_FLAGS, MODIFIER_REQUIRES, ROBOT_PRIMARIES};
use std::collections::{HashMap, HashSet};

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ValidationError {
    /// Go renders the acceptable set as a comma list with a final " or"
    /// (robot_registry.go:3797, joinRobotFlags at 3830).
    #[error("{message}")]
    MissingRequirement {
        modifier: String,
        required: String,
        message: String,
    },
    #[error("only one primary command allowed (found {count}: {found})")]
    ExclusivePrimaries { count: usize, found: String },
    /// Go rejects `--watch-export` alongside `--as-of` before any export runs
    /// (cmd/bv/main.go:1903-1906): watching would re-export the snapshot
    /// repeatedly instead of exporting the one fixed ref the user asked for.
    #[error("--watch-export cannot be combined with --as-of; omit --watch-export to export a fixed historical snapshot.")]
    WatchExportWithAsOf,
    /// Enum-value check happens at clap parse time (Phase 3c wiring).
    #[allow(dead_code)]
    #[error("invalid value for --{flag}: {value}")]
    BadEnum { flag: String, value: String },
    /// Go's `flag` package refuses a value-taking flag that has nothing left to
    /// consume, and `enrichFlagParseError` (cmd/bv/main.go:1269-1271) appends the
    /// recovery line. Rust accepted the bare flag instead and ran the command
    /// with an empty value — `--export-md` with no argument wrote `report.md`.
    #[error("flag needs an argument: --{flag}\nUse --{flag} VALUE. Run `bv --help` for all flags or `bv --robot-help` for agent-focused docs.")]
    MissingFlagArgument { flag: String },
}

/// Go `flag.Parse` (stdlib), restricted to the one rule Rust was missing: a
/// non-boolean flag written without `=` must have an argv element left to
/// consume. Go takes that element unconditionally — even one that looks like
/// another flag, which is how `--export-md --format json` ends with `json` as a
/// positional "unknown command" — and only errors when nothing follows.
///
/// Returns the first offending flag, because Go's parse stops there.
pub fn validate_flag_arguments(args: &[String]) -> Option<ValidationError> {
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        // Go stops flag parsing at the first non-flag word and treats the rest
        // as positional arguments.
        if !arg.starts_with('-') || arg == "-" || arg == "--" {
            return None;
        }
        let (name, has_inline_value) = match arg.split_once('=') {
            Some((n, _)) => (n, true),
            None => (arg.as_str(), false),
        };
        let kind = crate::flags::flag_kind(name);
        let is_bool = matches!(kind, None | Some(crate::flags::FlagKind::Bool));
        if !is_bool && !has_inline_value {
            if i + 1 >= args.len() {
                return Some(ValidationError::MissingFlagArgument {
                    flag: name.trim_start_matches('-').to_string(),
                });
            }
            i += 1; // the next element is consumed as the value
        }
        i += 1;
    }
    None
}

/// Which flags are present in the parsed invocation. Owns its strings —
/// no leaks; lifetimes are self-contained.
pub struct Presence {
    present: HashSet<String>,
}

impl Presence {
    /// Build the set of flags the caller actually passed.
    ///
    /// Only tokens that are a REGISTERED flag name count, and a value-taking
    /// flag's value is skipped. Go's `isFlagActive` (main.go:471-480) looks
    /// the name up in the `FlagSet` and ignores anything it does not find, so
    /// `--schema-command robot-capabilities` leaves only `schema-command`
    /// active. Inserting every token made the VALUE look like a second primary
    /// command, and `--robot-schema --schema-command robot-capabilities`
    /// failed with "only one primary command allowed" where the oracle emits
    /// the schema.
    pub fn from_args(args: &[String]) -> Self {
        let mut present = HashSet::new();
        let mut skip_next = false;
        for (i, arg) in args.iter().enumerate() {
            if skip_next {
                skip_next = false;
                continue;
            }
            let Some(rest) = arg.strip_prefix("--") else {
                continue;
            };
            // `--name=value` carries its own value; `--name value` consumes
            // the next token when the flag is registered and takes one.
            match rest.split_once('=') {
                Some((name, _)) => {
                    present.insert(name.to_string());
                }
                None => {
                    present.insert(rest.to_string());
                    if takes_value(rest) {
                        skip_next = true;
                    }
                }
            }
            let _ = i;
        }
        Presence { present }
    }

    pub fn has(&self, flag: &str) -> bool {
        self.present.contains(flag)
    }
}

/// Render a co-flag set the way Go's `joinRobotFlags` does: every flag
/// prefixed with `--`, comma-separated, with a final " or" before the last
/// (robot_registry.go:3830). A two-entry set reads "a or b"; a one-entry set
/// has no conjunction at all.
fn format_required(required: &[&str]) -> String {
    let flags: Vec<String> = required.iter().map(|r| format!("--{r}")).collect();
    match flags.len() {
        0 => String::new(),
        1 => flags[0].clone(),
        _ => {
            let head = flags[..flags.len() - 1].join(", ");
            format!("{head} or {}", flags[flags.len() - 1])
        }
    }
}

/// Validate modifier-requires rules. Returns the first violation, if any.
/// `args` is the raw argv when the caller has it.
///
/// Go's rule for a REQUIRED flag is `isFlagActive` (main.go:196-207), and for
/// a string flag that is `strings.TrimSpace(v) != ""`. `Presence` only records
/// that the token appeared, so `--diff-since ""` satisfies a
/// `("robot-diff", &["diff-since"])` rule here while Go rejects the pair as
/// "not active". Several rows in the table require a *string* flag —
/// `diff-since`, `search`, `export`, `export-md`, `graph-root` — so this is not
/// hypothetical.
///
/// When `args` is available, string requirements are re-checked with
/// [`crate::argv::go_string_flag_active`], which is Go's TrimSpace test.
///
/// Singular return, not a list: `validateModifierFlags` builds the message
/// inside its loop and `return`s on the first broken rule
/// (cmd/bv/main.go:230-232), so `--watch-export --no-live-reload` reports only
/// the `--watch-export` row. Collecting every violation made a second error
/// appear that Go never prints, which is what an agent parsing stderr has to
/// reconcile.
pub fn validate_modifier_requires_with(
    present: &Presence,
    args: Option<&[String]>,
) -> Option<ValidationError> {
    let requirement_active = |r: &&str| -> bool {
        if let Some(a) = args {
            if crate::flags::flag_is_string(r) && !crate::argv::go_string_flag_active(a, r) {
                return false;
            }
        }
        present.has(r)
    };
    for (modifier, required) in MODIFIER_REQUIRES {
        if present.has(modifier) && !required.iter().any(requirement_active) {
            let rendered = format_required(required);
            return Some(ValidationError::MissingRequirement {
                modifier: (*modifier).to_string(),
                required: rendered.clone(),
                // Go appends the recovery examples to the same message
                // (cmd/bv/main.go:238), so an agent reading stderr sees the
                // concrete invocations that satisfy the rule, not just the
                // requirement.
                message: if required.len() > 1 {
                    format!("--{modifier} requires one of {rendered}")
                } else {
                    format!("--{modifier} requires {rendered}")
                }
                .trim_end()
                .to_string()
                    + &crate::flags::format_modifier_recovery_examples(modifier),
            });
        }
    }
    None
}

/// Validate exclusive primary groups. Returns violation when >1 primary set.
///
/// This is Go's third stage (`validateExclusivePrimaryCommands`,
/// cmd/bv/main.go:1898-1901). It deliberately does NOT carry the
/// `--watch-export`/`--as-of` check: Go runs that as a fourth stage at
/// main.go:1903-1906, and every stage exits on its own. Folding two stages
/// into one call made the later message print even when an earlier stage had
/// already rejected the run.
/// Whether a registered flag consumes the following token as its value.
/// Unknown names are assumed to take none, which matches Go looking the flag
/// up and finding nothing.
fn takes_value(name: &str) -> bool {
    ROBOT_PRIMARIES
        .iter()
        .chain(MODIFIER_FLAGS.iter())
        .find(|f| f.name == name)
        .is_some_and(|f| {
            matches!(
                f.kind,
                FlagKind::Str | FlagKind::Int | FlagKind::Float | FlagKind::StringArray
            )
        })
}

pub fn validate_exclusive_primaries(present: &Presence) -> Vec<ValidationError> {
    let mut group_counts: HashMap<&str, Vec<&str>> = HashMap::new();
    for f in ROBOT_PRIMARIES {
        if f.primary && present.has(f.name) {
            if let Some(g) = f.group {
                group_counts.entry(g).or_default().push(f.name);
            }
        }
    }
    let mut violations = Vec::new();
    for (_g, names) in group_counts {
        if names.len() > 1 {
            violations.push(ValidationError::ExclusivePrimaries {
                count: names.len(),
                found: names.join(", "),
            });
        }
    }
    violations
}

/// Go's fourth and last validation stage (cmd/bv/main.go:1903-1906): reject
/// `--watch-export` alongside `--as-of`. Watching would re-export the snapshot
/// on every change instead of exporting the one fixed ref the caller asked for.
///
/// Separate from [`validate_exclusive_primaries`] because Go's four stages each
/// end in their own `os.Exit(1)`; a caller that merges stages reports messages
/// for stages the oracle never reached.
pub fn validate_watch_export_as_of(present: &Presence) -> Vec<ValidationError> {
    if present.has("watch-export") && present.has("as-of") {
        vec![ValidationError::WatchExportWithAsOf]
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::argv::rewrite_args;

    /// Reproduce Go's four-stage chain (main.go:1890-1906): each stage exits
    /// on its own, so `check` returns the first stage that produced a message.
    fn check(args: &[&str]) -> Vec<ValidationError> {
        let rewritten = rewrite_args(&args.iter().map(|x| x.to_string()).collect::<Vec<_>>());
        let p = Presence::from_args(&rewritten);
        let stages = [
            validate_modifier_requires_with(&p, Some(&rewritten))
                .map(|e| vec![e])
                .unwrap_or_default(),
            validate_exclusive_primaries(&p),
            validate_watch_export_as_of(&p),
        ];
        stages
            .into_iter()
            .find(|stage| !stage.is_empty())
            .unwrap_or_default()
    }

    #[test]
    fn robot_diff_requires_diff_since() {
        let v = check(&["bvr", "--robot-diff"]);
        assert!(matches!(
            v.first(),
            Some(ValidationError::MissingRequirement { .. })
        ));
        assert!(check(&["bvr", "--robot-diff", "--diff-since", "HEAD~5"]).is_empty());
    }

    #[test]
    fn brief_requires_triage_family() {
        // Go rule: brief requires triage/by-track/by-label — NOT robot-next
        assert!(!check(&["bvr", "--brief"]).is_empty());
        assert!(check(&["bvr", "--robot-triage", "--brief"]).is_empty());
        assert!(!check(&["bvr", "--robot-next", "--brief"]).is_empty());
    }

    #[test]
    fn history_timeout_includes_next() {
        // Go rule: robot-history-timeout-ms DOES include robot-next
        assert!(check(&["bvr", "--robot-next", "--robot-history-timeout-ms", "5000"]).is_empty());
    }

    #[test]
    fn history_window_modifiers_accept_robot_causality() {
        // Go main.go:1811-1812 — both history-since and history-limit list
        // robot-causality alongside the two history commands.
        for modifier in ["--history-since", "--history-limit"] {
            assert!(
                check(&["bvr", "--robot-causality", "A-1", modifier, "30d"]).is_empty(),
                "{modifier} must be allowed with --robot-causality"
            );
            assert!(
                !check(&["bvr", "--robot-triage", modifier, "30d"]).is_empty(),
                "{modifier} must still be rejected without a history command"
            );
        }
    }

    #[test]
    fn two_triage_family_flags_conflict() {
        let v = check(&["bvr", "--robot-triage", "--robot-next"]);
        assert!(matches!(
            v.first(),
            Some(ValidationError::ExclusivePrimaries { count: 2, .. })
        ));
    }

    #[test]
    fn single_primary_is_fine() {
        assert!(check(&["bvr", "--robot-triage"]).is_empty());
        assert!(check(&["bvr", "--robot-insights"]).is_empty());
    }

    #[test]
    fn unrelated_modifiers_ignored() {
        assert!(check(&["bvr", "--robot-insights", "--format", "toon"]).is_empty());
    }

    /// Go rejects `--watch-export` together with `--as-of` (cmd/bv/main.go:1903-1906,
    /// exit 1) instead of exporting a snapshot it would then re-watch. Either
    /// flag alone stays valid.
    #[test]
    fn watch_export_conflicts_with_as_of() {
        let v = check(&[
            "bvr",
            "--export-pages",
            "out",
            "--watch-export",
            "--as-of",
            "HEAD",
        ]);
        assert!(
            v.iter()
                .any(|e| matches!(e, ValidationError::WatchExportWithAsOf)),
            "expected the watch-export/as-of conflict, got {v:?}"
        );
        // The recovery advice is the whole point of the message.
        assert_eq!(
            v.iter()
                .find(|e| matches!(e, ValidationError::WatchExportWithAsOf))
                .map(|e| e.to_string()),
            Some("--watch-export cannot be combined with --as-of; omit --watch-export to export a fixed historical snapshot.".to_string())
        );

        assert!(check(&["bvr", "--export-pages", "out", "--watch-export"]).is_empty());
        assert!(check(&["bvr", "--export-pages", "out", "--as-of", "HEAD"]).is_empty());
    }
}
