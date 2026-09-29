//! The four `--feedback-*` flags against Go v0.25.0.
//!
//! **These are the recommendation-weight store, not the correlation store.**
//! Go keeps two files that both read as "feedback", and conflating them is the
//! trap this file exists to prevent:
//!
//! | store | file | Go source | flags |
//! |---|---|---|---|
//! | recommendation weights | `<beads>/feedback.json` | `pkg/analysis/feedback.go:15` | `--feedback-accept/-ignore/-reset/-show` |
//! | bead↔commit correlation | `<beads>/correlation_feedback.jsonl` | `pkg/correlation/feedback.go:19` | `--robot-confirm-correlation` / `--robot-reject-correlation` |
//!
//! The Go block that owns these four flags is `cmd/bv/main.go:2441-2530`. It
//! runs before recipe loading and before any `--robot-*` dispatch, and every
//! branch ends in `os.Exit`, so the block is a terminal decision:
//!
//! ```text
//! main.go:2441  if *feedbackAccept != "" || *feedbackIgnore != "" || *feedbackReset || *feedbackShow
//! main.go:2454      if *feedbackReset   -> Reset, Save, "Feedback data reset to defaults.", exit 0
//! main.go:2464      if *feedbackShow    -> ToJSON, MarshalIndent(2 spaces), exit 0
//! main.go:2472      record              -> issueID defaults to accept, ignore overrides (main.go:2475)
//! ```
//!
//! So precedence is reset > show > record, and `--feedback-ignore` beats
//! `--feedback-accept` for the issue id when both carry a value.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A hub that blocks three fresh tasks plus one stale loner — the shape Go's own
/// `tests/e2e/feedback_effect_test.go` uses. Structure (the hub's PageRank and
/// betweenness) and staleness pull in opposite directions, so the per-factor
/// contributions the store smooths are all non-zero and distinguishable.
const ISSUES: &str = concat!(
    r#"{"id":"hub","title":"Hub","status":"open","issue_type":"task","priority":2,"created_at":"2026-08-30T00:00:00Z","updated_at":"2026-08-31T00:00:00Z"}"#,
    "\n",
    r#"{"id":"d1","title":"Dependent 1","status":"open","issue_type":"task","priority":2,"created_at":"2026-08-30T00:00:00Z","updated_at":"2026-08-31T00:00:00Z","dependencies":[{"issue_id":"d1","depends_on_id":"hub","type":"blocks"}]}"#,
    "\n",
    r#"{"id":"d2","title":"Dependent 2","status":"open","issue_type":"task","priority":2,"created_at":"2026-08-30T00:00:00Z","updated_at":"2026-08-31T00:00:00Z","dependencies":[{"issue_id":"d2","depends_on_id":"hub","type":"blocks"}]}"#,
    "\n",
    r#"{"id":"d3","title":"Dependent 3","status":"open","issue_type":"task","priority":2,"created_at":"2026-08-30T00:00:00Z","updated_at":"2026-08-31T00:00:00Z","dependencies":[{"issue_id":"d3","depends_on_id":"hub","type":"blocks"}]}"#,
    "\n",
    r#"{"id":"stale","title":"Stale loner","status":"open","issue_type":"task","priority":2,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-02T00:00:00Z"}"#,
    "\n",
);

fn repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bvr_feedback_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".beads")).expect("temp repo");
    std::fs::write(dir.join(".beads").join("issues.jsonl"), ISSUES).expect("issues.jsonl");
    dir
}

fn run(dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_bvr"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// `--feedback-show` with an empty store. Go loads defaults when `feedback.json`
/// is absent (feedback.go:86-89) and prints `ToJSON` re-indented with two
/// spaces (main.go:2464-2469) — deliberately NOT inside the robot envelope.
#[test]
fn show_on_an_empty_store_prints_go_defaults() {
    let dir = repo("show_empty");
    let (code, stdout, stderr) = run(&dir, &["--feedback-show"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stderr.is_empty(), "Go writes this to stdout: {stderr}");

    let fb: serde_json::Value = serde_json::from_str(&stdout).expect("one JSON object");
    // MinFeedbackSamples = 3 (feedback.go:310), the gate below which the
    // adjusted weights are tracked but not applied to scoring.
    assert_eq!(fb["min_samples"], 3);
    assert_eq!(fb["enabled"], false);
    assert_eq!(fb["applied"], false);
    assert_eq!(fb["total_events"], 0);
    assert_eq!(fb["accepted_count"], 0);
    assert_eq!(fb["ignored_count"], 0);
    // All eight factors default to a 1.0x adjustment (feedback.go:65-80).
    let adjustments = fb["weight_adjustments"].as_object().expect("map");
    assert_eq!(adjustments.len(), 8, "{}", fb["weight_adjustments"]);
    for (name, v) in adjustments {
        assert_eq!(*v, 1.0, "{name} must start unadjusted");
    }
    // Effective weights are the defaults renormalized (feedback.go:335-370):
    // PageRank 0.22 + Betweenness 0.20 + BlockerRatio 0.13 + PriorityBoost 0.10
    // + Staleness 0.05 + TimeToImpact 0.10 + Urgency 0.10 + Risk 0.10 = 1.0.
    let effective = fb["effective_weights"].as_object().expect("map");
    assert_eq!(effective.len(), 8);
    let sum: f64 = effective.values().map(|v| v.as_f64().unwrap()).sum();
    assert!(
        (sum - 1.0).abs() < 1e-9,
        "effective weights must sum to 1, got {sum}"
    );
    assert!((effective["PageRank"].as_f64().unwrap() - 0.22).abs() < 1e-9);
    assert!((effective["Staleness"].as_f64().unwrap() - 0.05).abs() < 1e-9);
}

/// The four flags write the RECOMMENDATION store. `feedback.json` is the file
/// Go names at feedback.go:15; `correlation_feedback.jsonl` is a different
/// store entirely (pkg/correlation/feedback.go:19) owned by the
/// confirm/reject-correlation pair. Nothing here may create the latter.
#[test]
fn the_four_flags_write_feedback_json_not_the_correlation_store() {
    let dir = repo("two_stores");
    let (code, _, stderr) = run(&dir, &["--feedback-accept", "hub"]);
    assert_eq!(code, 0, "stderr: {stderr}");

    assert!(
        dir.join(".beads").join("feedback.json").exists(),
        "--feedback-accept must write <beads>/feedback.json"
    );
    assert!(
        !dir.join(".beads")
            .join("correlation_feedback.jsonl")
            .exists(),
        "the correlation store is owned by --robot-confirm/--robot-reject-correlation \
         and must not be created by the recommendation-weight flags"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// main.go:2526-2527 — the record path prints the action, the issue and the
/// impact score to three decimals, then `Summary()`. `Summary` is the
/// human-readable one-liner from feedback.go:392-397, and its empty case is the
/// separate "No feedback recorded yet." string at feedback.go:389.
///
/// **The two averages are not equally stable, and the test must not pretend
/// they are.** `stale`'s score comes out 0.251 on any date, so its rounded
/// average is pinned below. `hub`'s score is computed against the real clock —
/// `SOURCE_DATE_EPOCH` does not move it (checked at 1750000000, 1787407612 and
/// 1800000000: all 0.546), the same `time.Now()` that makes `triage`'s score
/// unpinnable. It drifts upward as the fixture ages and crossed the 0.545
/// rounding boundary, so a literal here read 0.54 while the oracle said 0.55.
///
/// What is asserted is therefore: the counts (deterministic), the accepted
/// average (stable), and for the ignored average its exact *shape* plus a range.
/// A drift in Go's scoring shows up as a failure of the parity gate, not as a
/// silent edit to a constant here. Measured against the oracle at 18afafa on
/// 2026-09-30, both binaries print `score: 0.546` / `avg score 0.55`.
#[test]
fn accept_and_ignore_report_the_score_and_the_summary() {
    let dir = repo("record");
    let (code, stdout, stderr) = run(&dir, &["--feedback-accept", "stale"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "two lines, got {stdout:?}");
    assert!(
        lines[0].starts_with("Recorded accept feedback for stale (score: "),
        "got {:?}",
        lines[0]
    );
    assert!(lines[0].ends_with(')'), "got {:?}", lines[0]);
    assert_eq!(
        lines[1],
        "Feedback: 1 accepted (avg score 0.25), 0 ignored (avg score 0.00), 1 total events"
    );

    let (code, stdout, stderr) = run(&dir, &["--feedback-ignore", "hub"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(
        lines[0].starts_with("Recorded ignore feedback for hub (score: "),
        "got {:?}",
        lines[0]
    );
    // Three decimals, and a real score rather than a placeholder.
    let score = lines[0]
        .trim_end_matches(')')
        .rsplit_once("score: ")
        .map(|(_, v)| v.to_string())
        .expect("score in record line");
    assert_eq!(score.len(), 5, "three decimals: {score:?}");
    assert!(
        (0.0..=1.0).contains(&score.parse::<f64>().expect("numeric score")),
        "score in range: {score:?}"
    );
    // The counts are the deterministic part of the summary.
    assert!(
        lines[1].starts_with("Feedback: 1 accepted (avg score 0.25), 1 ignored (avg score 0."),
        "got {:?}",
        lines[1]
    );
    assert!(
        lines[1].ends_with("), 2 total events"),
        "got {:?}",
        lines[1]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// main.go:2495-2497 — an id that is not in the loaded source is a hard error
/// on stderr at exit 1. Recording against a guessed id would silently tune the
/// weights for an issue nobody scored.
#[test]
fn unknown_issue_id_is_an_error_at_exit_one() {
    let dir = repo("unknown");
    for flag in ["--feedback-accept", "--feedback-ignore"] {
        let (code, stdout, stderr) = run(&dir, &[flag, "does-not-exist"]);
        assert_eq!(code, 1, "{flag}: stdout: {stdout}");
        assert!(stdout.is_empty(), "{flag} must print nothing: {stdout}");
        assert_eq!(stderr.trim(), "Issue not found: does-not-exist", "{flag}");
    }
    assert!(
        !dir.join(".beads").join("feedback.json").exists(),
        "a failed record must not create the store"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// main.go:2454, :2464, :2472 — one block, three exits, in that order. A caller
/// that passes several of these flags gets the first branch, never a mixture.
#[test]
fn precedence_is_reset_then_show_then_record() {
    let dir = repo("precedence");
    // reset wins over show and over record. Go's reset branch calls `Save`
    // unconditionally (main.go:2455-2460), so it writes `feedback.json` even
    // when the store had never been used.
    let (code, stdout, _) = run(
        &dir,
        &[
            "--feedback-reset",
            "--feedback-show",
            "--feedback-accept",
            "hub",
        ],
    );
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "Feedback data reset to defaults.");
    assert!(
        dir.join(".beads").join("feedback.json").exists(),
        "main.go:2455-2460 saves after Reset unconditionally"
    );

    // show wins over record, and prints JSON rather than a record line.
    let (code, stdout, _) = run(&dir, &["--feedback-show", "--feedback-accept", "hub"]);
    assert_eq!(code, 0);
    let fb: serde_json::Value = serde_json::from_str(&stdout).expect("one JSON object");
    assert_eq!(fb["total_events"], 0, "show must not record: {fb}");

    // record alone, both spellings: main.go:2473-2477 lets --feedback-ignore
    // override the issue id and the action.
    let (code, stdout, _) = run(
        &dir,
        &["--feedback-accept", "hub", "--feedback-ignore", "d1"],
    );
    assert_eq!(code, 0);
    assert!(
        stdout.starts_with("Recorded ignore feedback for d1 (score: "),
        "got {stdout:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Go's `flag` package assigns the value through `Set` on every occurrence, so
/// a repeated flag keeps the LAST one. The Rust scan returned on the first
/// match, which scored `d1` (score 0.241) when the user wrote
/// `--feedback-accept d1 --feedback-accept hub` and Go scored `hub` (0.536) —
/// the wrong issue's weight profile, recorded as if it were right.
#[test]
fn a_repeated_accept_flag_records_the_last_issue_id() {
    let dir = repo("repeat");
    let (code, stdout, stderr) = run(
        &dir,
        &["--feedback-accept", "d1", "--feedback-accept", "hub"],
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stdout.starts_with("Recorded accept feedback for hub (score: "),
        "the last --feedback-accept wins, as in Go's flag.Set; got {stdout:?}"
    );

    // The `=` spelling must resolve the same way.
    let dir2 = repo("repeat_eq");
    let (code, stdout, stderr) = run(&dir2, &["--feedback-ignore=d1", "--feedback-ignore=hub"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stdout.starts_with("Recorded ignore feedback for hub (score: "),
        "got {stdout:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
}

/// main.go:2454-2462 — `Reset` clears events, adjustments and stats while
/// keeping `created_at`, then `Save` persists. `--feedback-show` afterwards must
/// report the empty store, so the reset actually reached the file.
#[test]
fn reset_clears_the_store() {
    let dir = repo("reset");
    assert_eq!(run(&dir, &["--feedback-accept", "stale"]).0, 0);
    assert_eq!(run(&dir, &["--feedback-ignore", "hub"]).0, 0);

    let (code, stdout, stderr) = run(&dir, &["--feedback-reset"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stdout.trim(), "Feedback data reset to defaults.");

    let (code, stdout, stderr) = run(&dir, &["--feedback-show"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let fb: serde_json::Value = serde_json::from_str(&stdout).expect("one JSON object");
    assert_eq!(fb["total_events"], 0);
    assert_eq!(fb["accepted_count"], 0);
    assert_eq!(fb["ignored_count"], 0);
    assert_eq!(fb["enabled"], false);
    assert_eq!(fb["applied"], false);
    let adjustments = fb["weight_adjustments"].as_object().expect("map");
    for (name, v) in adjustments {
        assert_eq!(*v, 1.0, "{name} must be back to its unadjusted default");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The block at main.go:2441 runs before recipe loading and before any
/// `--robot-*` dispatch, so `--feedback-show` alongside a robot primary is
/// answered as a feedback query rather than a triage payload.
#[test]
fn feedback_flags_win_over_a_robot_primary() {
    let dir = repo("robot_precedence");
    let (code, stdout, stderr) = run(&dir, &["--robot-triage", "--feedback-show"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let fb: serde_json::Value =
        serde_json::from_str(&stdout).expect("feedback JSON, not a triage envelope");
    assert_eq!(fb["min_samples"], 3);
    assert!(
        fb["triage"].is_null(),
        "must not emit a triage payload: {fb}"
    );

    // ...and the record path exits before the robot handler runs.
    let (code, stdout, _) = run(&dir, &["--feedback-accept", "hub", "--robot-triage"]);
    assert_eq!(code, 0);
    assert!(stdout.starts_with("Recorded accept feedback for hub (score: "));
    let _ = std::fs::remove_dir_all(&dir);
}
