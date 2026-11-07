//! Validation — modifier-requires + exclusive-primary checks.
//! Port of Go validateModifierFlags / validateExclusivePrimaryCommands.

use crate::flags::{FlagKind, MODIFIER_REQUIRES};
use std::collections::HashMap;

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
    /// Go's `multiple primary robot commands specified: %s`
    /// (cmd/bv/main.go:365) — the active groups, joined with `" | "`.
    #[error("multiple primary robot commands specified: {groups}")]
    ExclusivePrimaries { groups: String },
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
    #[error("flag needs an argument: --{flag}\nUse --{flag} VALUE. Run `bvr --help` for all flags or `bvr --robot-help` for agent-focused docs.")]
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

/// Which flags are active in the parsed invocation, answered the way Go's
/// `isFlagActive` (cmd/bv/main.go:471-486) answers it — NOT by token
/// presence.
///
/// `values` records the effective raw value of every flag token, pflag-style:
/// `--name=value` and `--name value` both land here, a bare bool records
/// `"true"` (pflag's `NoOptDefVal`), a value-taking flag consumes the next
/// argv element unconditionally, and a later occurrence overwrites an earlier
/// one. [`Presence::has`] then applies Go's per-kind rule to that value:
///
/// - **bool** — active iff the last parsed value is `true`, so
///   `--flag=false` is present-but-INACTIVE (this is how Go turns off a
///   default-true flag such as `--pages-include-history`).
/// - **string** (`Str`/`StringArray`) — active iff
///   `strings.TrimSpace(value) != ""`, so `--db ""` and `--db "  "` are both
///   inactive.
/// - **every other kind** — active on presence; Go's parse pass has already
///   rejected unparseable values before validation runs.
///
/// Recording values during the scan (rather than re-scanning raw argv per
/// query, the way `argv::go_bool_flag` does) is what keeps a flag's VALUE
/// from being mistaken for a flag: `--export-md --robot-triage` feeds
/// `--robot-triage` to `--export-md` as its value in pflag, so `robot-triage`
/// is NOT active — a raw token scan would report it active.
pub struct Presence {
    values: HashMap<String, String>,
}

impl Presence {
    /// Build the active-flag view of the caller's argv.
    ///
    /// Only tokens that are a REGISTERED flag name count, and a value-taking
    /// flag's value is skipped — Go's `isFlagActive` looks the name up in the
    /// `FlagSet` and ignores anything it does not find, so
    /// `--schema-command robot-capabilities` leaves only `schema-command`
    /// active. Inserting every token made the VALUE look like a second primary
    /// command, and `--robot-schema --schema-command robot-capabilities`
    /// failed with "only one primary command allowed" where the oracle emits
    /// the schema.
    ///
    /// `--` ends flag parsing exactly as it does for pflag: everything after
    /// it is positional and can never activate a flag.
    ///
    /// The scan itself is [`crate::argv::each_flag_set`], the shared
    /// pflag-faithful walk: value-taking flags consume the next argv element
    /// unconditionally, bools never do, `---x` is a malformed spelling rather
    /// than flag `x`, and `-h`/`-h=` report under `"-h"`. Last `Set` wins.
    /// Unregistered `--name` tokens are recorded too (Go's parse rejects
    /// them before validation); they simply never match a registry lookup in
    /// [`Presence::has`].
    pub fn from_args(args: &[String]) -> Self {
        let mut values: HashMap<String, String> = HashMap::new();
        crate::argv::each_flag_set(args, |name, value| {
            values.insert(name.to_string(), value.to_string());
        });
        Presence { values }
    }

    /// Whether `flag` is ACTIVE in the Go `isFlagActive` sense — see the type
    /// docs for the per-kind rules. This is the only accessor; every caller
    /// (`validate_*`, the `main` dispatch arms, `run_agents_commands`) wants
    /// exactly this answer.
    pub fn has(&self, flag: &str) -> bool {
        let Some(raw) = self.values.get(flag) else {
            return false;
        };
        match crate::flags::flag_kind(flag) {
            Some(FlagKind::Bool) => crate::argv::go_parse_bool(raw).unwrap_or(false),
            _ if crate::flags::flag_is_string(flag) => !raw.trim().is_empty(),
            _ => true,
        }
    }

    /// pflag's `Changed`: the flag was `Set` at all, whatever the value —
    /// `--brief=false` still counts. This is the predicate Go's
    /// `validateModifierFlags` applies to the MODIFIER side of a rule
    /// (`flags.Changed(rule.modifier)`, cmd/bv/main.go), which is subtly
    /// different from [`Presence::has`]: a present-but-false bool modifier
    /// still fires its requirement.
    pub fn changed(&self, flag: &str) -> bool {
        self.values.contains_key(flag)
    }

    /// The raw value of the last `Set` for `flag`, or `None` when absent.
    /// For rules that read the DEREFERENCED flag value rather than
    /// `isFlagActive` — Go's `--watch-export`/`--as-of` guard
    /// (main.go:1903: `*watchExport && *asOf != ""`).
    pub fn raw_value(&self, flag: &str) -> Option<&str> {
        self.values.get(flag).map(String::as_str)
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
///
/// Go's rule is asymmetric (main.go `validateModifierFlags`): the MODIFIER
/// side is `flags.Changed` — set at all, whatever the value — while each
/// REQUIRED flag uses `isFlagActive`, which [`Presence::has`] answers:
/// bool flags need a truthy last value and string flags need a
/// TrimSpace-surviving value. So `--brief=false` DOES fire the `brief` row
/// (Changed), `--export-format ""` fires its row too, and `--diff-since ""`
/// does not satisfy the `robot-diff` row — all matching the oracle.
///
/// `args` is retained for signature compatibility with the `main` call site;
/// [`Presence`] already captured everything it needs at construction.
///
/// Singular return, not a list: `validateModifierFlags` builds the message
/// inside its loop and `return`s on the first broken rule
/// (cmd/bv/main.go:230-232), so `--watch-export --no-live-reload` reports only
/// the `--watch-export` row. Collecting every violation made a second error
/// appear that Go never prints, which is what an agent parsing stderr has to
/// reconcile.
pub fn validate_modifier_requires_with(
    present: &Presence,
    _args: Option<&[String]>,
) -> Option<ValidationError> {
    for (modifier, required) in MODIFIER_REQUIRES {
        if present.changed(modifier) && !required.iter().any(|r| present.has(r)) {
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
///
/// Go counts ACTIVE GROUPS, not active flags: every group in
/// `primaryRobotCommandGroups` that has an active flag contributes one entry
/// to `activeGroups`, and more than one entry is the violation. The message
/// joins those entries with `" | "`, and each entry joins the active flags of
/// its own group with `", "`.
///
/// The group boundaries therefore decide both directions of the rule, which is
/// why they live in [`crate::flags::PRIMARY_GROUPS`] rather than in a
/// per-flag tag: `--robot-triage --robot-triage-by-track` are two spellings of
/// one command and are allowed, while `--robot-next --robot-triage` and
/// `--robot-insights --robot-plan` are two commands and are rejected.
///
/// A group's flags are listed with `--` in Go's message (`activePrimaryFlags`
/// prefixes each name), and the groups appear in Go's declaration order.
pub fn validate_exclusive_primaries(present: &Presence) -> Vec<ValidationError> {
    let mut active_groups: Vec<String> = Vec::new();
    for group in crate::flags::PRIMARY_GROUPS {
        let active: Vec<String> = group
            .iter()
            .filter(|name| present.has(name))
            .map(|name| format!("--{name}"))
            .collect();
        if active.is_empty() {
            continue;
        }
        active_groups.push(active.join(", "));
    }
    if active_groups.len() <= 1 {
        return Vec::new();
    }
    vec![ValidationError::ExclusivePrimaries {
        groups: active_groups.join(" | "),
    }]
}

/// Go's fourth and last validation stage (cmd/bv/main.go:1903-1906): reject
/// `--watch-export` alongside `--as-of`. Watching would re-export the snapshot
/// on every change instead of exporting the one fixed ref the caller asked for.
///
/// Separate from [`validate_exclusive_primaries`] because Go's four stages each
/// end in their own `os.Exit(1)`; a caller that merges stages reports messages
/// for stages the oracle never reached.
pub fn validate_watch_export_as_of(present: &Presence) -> Vec<ValidationError> {
    // Go reads the DEREFERENCED values here (main.go:1903:
    // `*watchExport && *asOf != ""`), not `isFlagActive`: the bool needs
    // its parsed value, and `as-of` needs only be non-empty — `" "` still
    // conflicts because there is no TrimSpace on this path.
    let watch = present
        .raw_value("watch-export")
        .is_some_and(|v| crate::argv::go_parse_bool(v).unwrap_or(false));
    let as_of = present.raw_value("as-of").is_some_and(|v| !v.is_empty());
    if watch && as_of {
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

    /// Go counts ACTIVE GROUPS, not active flags (cmd/bv/main.go:351-366), so
    /// `--robot-next` and `--robot-triage` — different groups — conflict even
    /// though neither group holds more than one flag.
    #[test]
    fn two_triage_family_flags_conflict() {
        let v = check(&["bvr", "--robot-triage", "--robot-next"]);
        assert_eq!(
            v.first().map(|e| e.to_string()),
            Some(
                "multiple primary robot commands specified: --robot-next | --robot-triage"
                    .to_string()
            )
        );
    }

    /// The other half of the same rule: the two triage spellings share ONE
    /// group, so naming both is one command, not two.
    #[test]
    fn two_spellings_of_one_group_are_allowed() {
        assert!(check(&["bvr", "--robot-triage", "--robot-triage-by-track"]).is_empty());
        assert!(check(&["bvr", "--robot-triage-by-track", "--robot-triage-by-label"]).is_empty());
    }

    /// `--robot-insights` and `--robot-plan` are different groups, each with a
    /// single flag, and are still a conflict.
    #[test]
    fn one_flag_in_each_of_two_groups_conflicts() {
        let v = check(&["bvr", "--robot-insights", "--robot-plan"]);
        assert_eq!(
            v.first().map(|e| e.to_string()),
            Some(
                "multiple primary robot commands specified: --robot-insights | --robot-plan"
                    .to_string()
            )
        );
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

    /// Go's `isFlagActive` bool arm: `--flag=false` parses fine but is NOT
    /// active, so it can neither satisfy a requirement nor claim a primary
    /// group. Token-presence counting got both directions wrong.
    #[test]
    fn bool_flag_false_is_inactive() {
        // `--robot-triage=false` must not count toward the triage group, so
        // pairing it with `--robot-next` is ONE active group, not two.
        assert!(check(&["bvr", "--robot-triage=false", "--robot-next"]).is_empty());
        // `--check-drift=false` does not satisfy `--robot-drift`'s requirement.
        assert!(!check(&["bvr", "--robot-drift", "--check-drift=false"]).is_empty());
        assert!(check(&["bvr", "--robot-drift", "--check-drift"]).is_empty());
        // `--brief=false` is not an active modifier, so no requires-rule fires.
        assert!(check(&["bvr", "--robot-triage", "--brief=false"]).is_empty());
        // `--watch-export=false` does not conflict with `--as-of`.
        assert!(check(&[
            "bvr",
            "--export-pages",
            "out",
            "--watch-export=false",
            "--as-of",
            "HEAD"
        ])
        .is_empty());
    }

    /// String flags are active only when the value survives TrimSpace, and
    /// the LAST occurrence wins (pflag `Set` overwrites).
    #[test]
    fn string_flag_active_needs_nonblank_last_value() {
        // `--diff-since ""` is present but inactive: robot-diff still fails.
        assert!(!check(&["bvr", "--robot-diff", "--diff-since", ""]).is_empty());
        // `--as-of ""` is inactive, so the watch-export pair does not fire.
        assert!(check(&[
            "bvr",
            "--export-pages",
            "out",
            "--watch-export",
            "--as-of",
            ""
        ])
        .is_empty());
        // A blank LAST occurrence beats an earlier real value.
        assert!(!check(&[
            "bvr",
            "--robot-diff",
            "--diff-since",
            "HEAD~1",
            "--diff-since",
            ""
        ])
        .is_empty());
        // ...and a real LAST occurrence beats an earlier blank one — the
        // first-match scan used to reject this exact shape.
        assert!(check(&[
            "bvr",
            "--robot-diff",
            "--diff-since",
            "",
            "--diff-since",
            "HEAD~1"
        ])
        .is_empty());
        // Value-carrying primaries follow the same string rule:
        // `--robot-related ""` is inactive and claims no primary group.
        assert!(check(&["bvr", "--robot-related", "", "--robot-triage"]).is_empty());
        assert!(check(&["bvr", "--robot-related", "bv-123", "--robot-triage"]).len() == 1);
    }

    /// A flag's VALUE is never itself a flag: pflag feeds the next token to
    /// the value-taking flag unconditionally, so `--export-md
    /// --robot-triage` leaves `robot-triage` inactive.
    #[test]
    fn flag_value_is_not_a_flag() {
        assert!(check(&["bvr", "--export-md", "--robot-triage"]).is_empty());
    }

    /// The modifier side of a rule is pflag's `Changed`, not `isFlagActive`:
    /// `--brief=false` was still SET, so it fires the `brief` requirement
    /// even though the flag itself reads inactive. Same for a string
    /// modifier set to the empty string.
    #[test]
    fn changed_modifier_fires_even_when_value_is_inactive() {
        assert!(!check(&["bvr", "--brief=false"]).is_empty());
        assert!(check(&["bvr", "--robot-triage", "--brief=false"]).is_empty());
        assert!(!check(&["bvr", "--export-format", ""]).is_empty());
        assert!(check(&["bvr", "--export", "out", "--export-format", ""]).is_empty());
    }

    /// `--as-of " "` is non-empty for Go's `*asOf != ""` guard — this path
    /// has no TrimSpace — so it still conflicts with `--watch-export`.
    #[test]
    fn watch_export_conflicts_with_whitespace_as_of() {
        assert!(!check(&[
            "bvr",
            "--export-pages",
            "out",
            "--watch-export",
            "--as-of",
            " "
        ])
        .is_empty());
    }

    /// `--` ends flag parsing for `Presence` too: flag-looking tokens after
    /// the terminator activate nothing.
    #[test]
    fn double_dash_ends_presence_scan() {
        assert!(check(&["bvr", "--", "--brief"]).is_empty());
        assert!(check(&["bvr", "--robot-triage", "--", "--robot-next"]).is_empty());
    }
}
