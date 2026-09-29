//! `--robot-history-timeout-ms` parity with Go v0.25.0 (issue #166).
//!
//! Go registers the flag as a pflag `flag.Int` defaulting to -1, meaning UNSET
//! (`cmd/bv/main.go:1549`), and spends it in exactly one place:
//! `handleRobotTriage` calls `generateTriageHistoryBounded`
//! (`robot_registry.go:2187-2189`), which wraps the git-history correlation
//! prologue — `correlator.GenerateReportCached` — and nothing else. Reading
//! the handler end to end matters here: `ComputeTriageWithOptionsAndTime`, which
//! consumes the report, is deliberately OUTSIDE the budget. Only the prologue is
//! bounded; triage scoring is not.
//!
//! Before this flag was wired, Rust hardcoded `history_status` to `"ok"`
//! whenever there was open work, so the flag changed nothing and the prologue
//! ran unbounded. The tests below drive the real binary against the workspace
//! (itself a git repository with a `.beads` file, so the prologue really runs)
//! and assert on the one field the budget is allowed to move:
//! `triage.meta.history_status`.
//!
//! ## Why this builds its own repository
//!
//! These tests used to run against the workspace, on the reasoning that the
//! workspace "is itself a git repository with a `.beads` file, so the prologue
//! really runs". That coupled them to the project's own backlog, and Go gates
//! the whole feature on that backlog having open work:
//!
//! ```text
//! robot_registry.go:2156  hasOpenIssues := false
//! robot_registry.go:2158      if issue.Status != closed && != tombstone { hasOpenIssues = true }
//! robot_registry.go:2165  if hasOpenIssues && sourceDateEpochActive() { "skipped" }
//! robot_registry.go:2167  else if hasOpenIssues { ...run the bounded prologue... }
//! ```
//!
//! With every bead closed, `hasOpenIssues` is false, neither branch runs,
//! `historyStatus` stays `""`, and `triage.go:71`'s `omitempty` drops the key
//! — in *both* binaries, identically. So the tests went red as a side effect of
//! finishing the work, which is the worst possible reason for a parity test to
//! fail. The fixture below pins the precondition instead of inheriting it.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

/// Enough open work that Go's `hasOpenIssues` is true no matter what else the
/// project is doing, with a dependency edge so the prologue has something to
/// correlate. Mirrors the shape `pkg/analysis/feedback.go`'s own e2e test uses.
const ISSUES: &str = concat!(
    r#"{"id":"hub","title":"Hub","status":"open","issue_type":"task","priority":1,"created_at":"2026-08-30T00:00:00Z","updated_at":"2026-08-31T00:00:00Z"}"#,
    "\n",
    r#"{"id":"leaf","title":"Leaf","status":"open","issue_type":"task","priority":2,"created_at":"2026-08-30T00:00:00Z","updated_at":"2026-08-31T00:00:00Z","dependencies":[{"issue_id":"leaf","depends_on_id":"hub","type":"blocks"}]}"#,
    "\n",
    r#"{"id":"done","title":"Already closed","status":"closed","issue_type":"task","priority":3,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-02T00:00:00Z"}"#,
    "\n",
);

/// Commits in the fixture. The 1ms cases assert the prologue *times out*, and
/// that is only a property of the bound if the walk has real work to do — over
/// a one-commit repository `git log` can finish inside the budget and the test
/// would report a spurious failure. Sixty empty commits keeps the walk well
/// past 1ms without making the suite slow.
const COMMITS: usize = 60;

/// Build the fixture once per test process and share it. The tests only read
/// from it, so concurrent readers are safe.
fn fixture() -> &'static Path {
    static FIXTURE: OnceLock<PathBuf> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("bvr_history_timeout_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".beads")).expect("fixture .beads");
        std::fs::write(dir.join(".beads").join("issues.jsonl"), ISSUES).expect("issues.jsonl");

        let git = |args: &[&str]| {
            let status = Command::new("git")
                .args(args)
                .current_dir(&dir)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
            assert!(status.success(), "git {args:?} failed");
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "fixture@example.invalid"]);
        git(&["config", "user.name", "fixture"]);
        git(&["add", "-A"]);
        for i in 0..COMMITS {
            git(&[
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                Box::leak(format!("commit {i}").into_boxed_str()),
            ]);
        }
        dir
    })
}

/// Run `--robot-triage` in the fixture repository, pinning the two environment
/// inputs the resolution order reads. `flag` is appended only when Some, so "the
/// flag is absent" is expressible — that is the case that lets the env var and
/// then the default apply.
///
/// No `--db`: Go's chain reaches the prologue through `loader.GetBeadsDir("")`
/// and `loader.FindJSONLPath` (robot_registry.go:2168-2171), which do their own
/// discovery from the working directory. Pointing `--db` straight at a file
/// satisfies neither, so the status would be empty for a reason that has nothing
/// to do with the budget.
fn run(flag: Option<&str>, env_timeout: Option<&str>, source_date_epoch: Option<&str>) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_bvr"));
    cmd.current_dir(fixture()).args(["--robot-triage"]);
    if let Some(v) = flag {
        cmd.args(["--robot-history-timeout-ms", v]);
    }
    // Always cleared: an inherited value from the developer's shell would
    // silently decide the outcome of the "flag absent" cases.
    cmd.env_remove("BV_ROBOT_HISTORY_TIMEOUT_MS");
    if let Some(v) = env_timeout {
        cmd.env("BV_ROBOT_HISTORY_TIMEOUT_MS", v);
    }
    cmd.env_remove("SOURCE_DATE_EPOCH");
    if let Some(v) = source_date_epoch {
        cmd.env("SOURCE_DATE_EPOCH", v);
    }
    cmd.output().expect("binary runs")
}

fn history_status(out: &Output) -> String {
    assert_eq!(
        out.status.code(),
        Some(0),
        "expected success, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout is one JSON object");
    v["triage"]["meta"]["history_status"]
        .as_str()
        .expect("history_status is a string")
        .to_string()
}

/// The flag has to actually cut the prologue off. A 1ms budget is below any
/// plausible completion time for a real `git log` walk, so "timeout" here is a
/// property of the bound, not of machine speed. This is the test that fails
/// before the change, where the status was hardcoded "ok".
#[test]
fn tiny_budget_times_out_the_history_prologue() {
    let out = run(Some("1"), None, None);
    assert_eq!(history_status(&out), "timeout");
}

/// The inverse direction: the same repository that times out at 1ms must
/// report a completed prologue when the budget is generous, so the previous
/// test is detecting the bound and not a permanently broken prologue.
#[test]
fn generous_budget_lets_the_prologue_finish() {
    let out = run(Some("10000"), None, None);
    assert_eq!(history_status(&out), "ok");
}

/// Go installs a deadline only when `timeout > 0`; 0 is the documented
/// "unbounded" escape hatch (robot_registry.go:2047-2048), and 0 must therefore
/// NOT behave like a zero-length deadline that fires immediately.
#[test]
fn zero_budget_is_unbounded_not_immediately_expired() {
    let out = run(Some("0"), None, None);
    assert_eq!(history_status(&out), "ok");
}

/// The env var is the second precedence step and must bound the prologue on its
/// own when the flag is absent.
#[test]
fn env_var_bounds_the_prologue_when_the_flag_is_absent() {
    let out = run(None, Some("1"), None);
    assert_eq!(history_status(&out), "timeout");
}

/// Flag beats env. 0/unbounded paired with an env var that would time out, and
/// then the reverse, pins the precedence from both directions.
#[test]
fn flag_takes_precedence_over_the_env_var() {
    let out = run(Some("0"), Some("1"), None);
    assert_eq!(history_status(&out), "ok");

    let out = run(Some("1"), Some("0"), None);
    assert_eq!(history_status(&out), "timeout");
}

/// An unparsable env value is not an error: Go ignores the failed `ParseInt`
/// and falls through to the 10s default (robot_registry.go:2054-2059), so the
/// run succeeds with a completed prologue.
///
/// It is also parsed with base 10 there, unlike the flag's base 0, so `0x10`
/// is unreadable in the env and must fall through rather than become 16ms.
#[test]
fn unparsable_env_falls_back_to_the_default_instead_of_failing() {
    for value in ["abc", "", "  ", "0x10", "1_0", "-5"] {
        let out = run(None, Some(value), None);
        assert_eq!(history_status(&out), "ok", "env value {value:?}");
    }
}

/// Go registers the flag as a pflag `flag.Int`, so the value is parsed during
/// flag parsing and a non-integer is a parse error at exit 1 (main.go:4548-4550)
/// rather than a silent fall back to the default.
#[test]
fn unparsable_flag_is_pflags_parse_error_at_exit_one() {
    let out = run(Some("abc"), None, None);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr).trim(),
        "invalid argument \"abc\" for \"--robot-history-timeout-ms\" flag: \
         strconv.ParseInt: parsing \"abc\": invalid syntax"
    );
}

/// Base-0 parsing is the flag's rule (pflag's `intValue.Set`), so values Go
/// accepts must not be rejected: a `0x`/`0b` prefix, a bare leading `0` for
/// octal, and an underscore. All of these would be a parse error under base 10.
#[test]
fn flag_accepts_the_base_zero_literals_gof_pflag_accepts() {
    for value in ["0x10", "0b11", "010", "1_0", "+5", "-3"] {
        let out = run(Some(value), None, None);
        assert_eq!(out.status.code(), Some(0), "flag value {value:?}");
    }
}

/// A millisecond count past the range of `time.Duration` must saturate at
/// `math.MaxInt64` (robot_registry.go:2039-2041). Multiplying out instead would
/// wrap to a NEGATIVE duration, and the bounded path only installs a deadline
/// for `timeout > 0` — so an overflow would silently become unbounded.
#[test]
fn oversized_millisecond_count_saturates_instead_of_wrapping_negative() {
    // MAX_MILLIS + 1; still a valid i64, so this is not a parse error.
    let out = run(Some("9223372036855"), None, None);
    assert_eq!(history_status(&out), "ok");
}

/// A pinned clock outranks the budget: Go skips the prologue entirely and
/// reports "skipped", because racing a git history walk against a wall-clock
/// deadline cannot produce stable bytes. A 1ms budget must not turn that into a
/// "timeout".
#[test]
fn pinned_clock_skips_the_prologue_before_any_budget_applies() {
    let out = run(Some("1"), Some("1"), Some("1700000000"));
    assert_eq!(history_status(&out), "skipped");
}
