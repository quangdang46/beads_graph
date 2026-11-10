//! Empty-string values on string-valued `--robot-*` primaries.
//!
//! Every string-valued primary (`FlagKind::Str` in `ROBOT_PRIMARIES`, plus the
//! string-valued `bead-history` / `robot-not-ready-labels` modifiers that
//! dispatch as commands) used to gate on `Presence::has`, whose string arm
//! requires a TrimSpace-surviving value. `--robot-impact-network ""` and
//! `--robot-impact-network=` therefore matched NO dispatch arm and fell
//! through to the interactive TUI launcher — exit 1 with "could not open a
//! new TTY" on a host without a controlling terminal — and
//! `--robot-impact-network "" --robot-triage` silently dropped the
//! impact-network flag entirely.
//!
//! Dispatch now uses `Presence::command_active` (pflag `Changed` for string
//! flags): the flag's presence names the command, and the handler receives
//! the empty value through `flag_value`. Paired with a second primary the
//! exclusive-primary check reports the conflict instead of dropping one.
//!
//! The TUI launcher is the failure mode under test: its stderr always
//! carries the `/dev/tty` message when no controlling terminal exists, so
//! "did not reach the TUI" is asserted by its absence. Handlers may still
//! exit non-zero on an empty argument (usage errors are legitimate); what
//! must never happen is the TUI fallback or a silent drop.

use std::process::{Command, Output};

const REPO_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bvr"))
        .current_dir(REPO_ROOT)
        .args(args)
        .args(["--db", ".beads/issues.jsonl"])
        .output()
        .expect("binary runs")
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The interactive launcher's signature failure on a non-TTY host. Any
/// dispatch arm that reaches a robot handler exits before this point.
fn assert_not_tui(out: &Output, args: &[&str]) {
    let stderr = stderr_of(out);
    assert!(
        !stderr.contains("TTY") && !stderr.contains("tty"),
        "{args:?} fell through to the TUI launcher instead of dispatching; \
         stderr: {stderr}"
    );
}

/// Every string-valued robot primary, both spellings of "empty":
/// `--flag ""` (separate argv element) and `--flag=` (inline equals form).
/// `bead-history` and `robot-not-ready-labels` are string-valued modifiers
/// that dispatch as commands and are covered by the same rule.
const STRING_PRIMARIES: &[&str] = &[
    "robot-docs",
    "robot-explain-correlation",
    "robot-confirm-correlation",
    "robot-reject-correlation",
    "robot-file-beads",
    "robot-impact",
    "robot-file-relations",
    "robot-related",
    "robot-blocker-chain",
    "robot-impact-network",
    "robot-causality",
    "robot-sprint-show",
    "robot-forecast",
    "robot-burndown",
    "robot-not-ready-labels",
    "bead-history",
];

#[test]
fn empty_value_string_primary_never_reaches_tui() {
    for flag in STRING_PRIMARIES {
        for args in [
            vec![format!("--{flag}"), String::new()],
            vec![format!("--{flag}=")],
        ] {
            let argv: Vec<&str> = args.iter().map(String::as_str).collect();
            let out = run(&argv);
            assert_not_tui(&out, &argv);
        }
    }
}

/// The reported repro: `--robot-impact-network ""` must run the command —
/// in the repo fixture that means the full-network JSON payload, identical
/// to what `--robot-impact-network all` produces for the `bead_id`-less
/// shape (empty means "full" per the flag's own help text).
#[test]
fn impact_network_empty_value_runs_the_command() {
    let out = run(&["--robot-impact-network", ""]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout is not the command's JSON ({e}): {stdout}"));
    assert!(
        json.get("network").is_some(),
        "empty value means the full network: {json}"
    );
    assert!(
        json.get("bead_id").is_none(),
        "no bead_id for the empty/full target: {json}"
    );
}

/// `--robot-impact-network "" --robot-triage` used to silently drop the
/// impact-network flag. Two named primaries are the exclusive-primary
/// violation — a clean usage error, not a silent ignore and not the TUI.
#[test]
fn empty_value_primary_beside_a_second_primary_is_a_usage_error() {
    let out = run(&["--robot-impact-network", "", "--robot-triage"]);
    let stderr = stderr_of(&out);
    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
    assert!(
        stderr.contains("multiple primary robot commands specified"),
        "expected the exclusive-primary error, got: {stderr}"
    );
    assert!(
        stderr.contains("--robot-impact-network"),
        "the empty-valued flag must appear in the conflict, got: {stderr}"
    );
}

/// Bool primaries keep the `has` rule: `--robot-triage=false` is present
/// but inactive and must NOT claim the triage group — pairing it with
/// `--robot-next` stays legal.
#[test]
fn bool_primary_false_still_does_not_claim_a_group() {
    let out = run(&["--robot-triage=false", "--robot-next"]);
    assert_not_tui(&out, &["--robot-triage=false", "--robot-next"]);
}
