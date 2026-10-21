//! `--related-include-closed` parity with Go v0.25.0.
//!
//! Go registers it as a pflag `flag.Bool` with default `false`
//! (beads_viewer/cmd/bv/main.go:1585) and hands it to the correlation engine
//! at robot_registry.go:3283-3284:
//!
//! ```go
//! if cfg.RelatedIncludeClosed != nil { options.IncludeClosed = *cfg.RelatedIncludeClosed }
//! ```
//!
//! Two things make a pflag boolean different from a bare switch, and both are
//! reproduced here.
//!
//! First, the value is converted by `boolValue.Set`, which is nothing but
//! `strconv.ParseBool` (vendor/github.com/spf13/pflag/bool.go:20-24). So
//! `--flag=VALUE` is legal only for the twelve spellings ParseBool accepts —
//! `1 t T TRUE true True` and `0 f F FALSE false False`. `yes`, `on`, `TrUe`,
//! `2`, an empty value and a value with a trailing space are all *parse
//! errors*, not falsey. Because a bool gets `NoOptDefVal = "true"`, the bare
//! `--flag` never consumes the following token: `--related-include-closed
//! false` leaves `false` as a positional, which Go rejects as an unknown
//! command.
//!
//! Second, on rejection Go prints pflag's message with a bare
//! `fmt.Fprintln(os.Stderr, ...)` and exits 1 (main.go:4548-4550) — there is no
//! `Error: ` prefix.

use std::process::Command;

const REPO_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

/// A real bead from the workspace's own `.beads/issues.jsonl`, so the
/// correlation report is built and the category fields are serialised.
/// `--db` is pinned to the same JSONL on both binaries: the Go oracle prefers
/// `beads.db` when it is fresher, while `bvr` always prefers the JSONL.
const BEAD: &str = "beads_viewer_rust-api-freeze-b73";

fn bvr(extra: &[&str]) -> (i32, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_bvr"));
    cmd.current_dir(REPO_ROOT)
        .args(["--robot-related", BEAD, "--db", ".beads/issues.jsonl"])
        .args(extra);
    let out = cmd.output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn with_flag(value: &str) -> (i32, String, String) {
    bvr(&[&format!("--related-include-closed={value}")])
}

fn payload(stdout: &str) -> serde_json::Value {
    serde_json::from_str(stdout).expect("stdout is one JSON object")
}

/// Every spelling `strconv.ParseBool` accepts, on both sides of the default.
/// The closed candidates in this repo's dependency graph are what makes the
/// boolean observable: with the flag on, `concurrent` is a real array; with it
/// off (or absent) Go's nil accumulator serialises as JSON `null`.
///
/// `closed` is the only status the flag gates — tombstones are skipped either
/// way (related.go:509-518) — so `concurrent` is the cleanest probe.
#[test]
fn every_parsebool_spelling_is_honoured() {
    for value in ["1", "t", "T", "TRUE", "true", "True"] {
        let (code, stdout, stderr) = with_flag(value);
        assert_eq!(code, 0, "= {value:?} stderr: {stderr}");
        let parsed = payload(&stdout);
        assert!(
            !parsed["concurrent"].is_null(),
            "= {value:?} is truthy in Go, so closed candidates must appear: {stdout}"
        );
    }
    for value in ["0", "f", "F", "FALSE", "false", "False"] {
        let (code, stdout, stderr) = with_flag(value);
        assert_eq!(code, 0, "= {value:?} stderr: {stderr}");
        let parsed = payload(&stdout);
        assert!(
            parsed["concurrent"].is_null(),
            "= {value:?} is falsey in Go, so closed candidates stay excluded: {stdout}"
        );
    }
}

/// The bare switch is Go's `NoOptDefVal = "true"`, so it reads as true — and
/// the flag is absent by default, so both paths must include closed beads.
/// This is the spelling every real invocation uses.
#[test]
fn bare_switch_is_true_and_absent_means_false() {
    let (code, stdout, stderr) = bvr(&["--related-include-closed"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        !payload(&stdout)["concurrent"].is_null(),
        "bare bool flag is NoOptDefVal=true: {stdout}"
    );

    let (code, stdout, stderr) = bvr(&[]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        payload(&stdout)["concurrent"].is_null(),
        "Go's registered default is false: {stdout}"
    );
}

/// `strconv.ParseBool` is a whitelist, not a "not falsey" test. Each of these
/// reads false to a naive `!matches!(v, "false" | "0" | "f")`, and none of them
/// is in Go's accepted set, so every one is a parse error: pflag's wrapper,
/// no `Error: ` prefix, exit 1, and no payload on stdout.
#[test]
fn values_outside_parsebool_are_parse_errors() {
    for value in [
        "yes", "on", "TrUe", "2", "", "00", "TRUE ", "true ", "y", "n",
    ] {
        let (code, stdout, stderr) = with_flag(value);
        assert_eq!(
            code, 1,
            "= {value:?} must be rejected like Go's strconv.ParseBool"
        );
        assert_eq!(
            stderr.trim(),
            format!(
                "invalid argument {value:?} for \"--related-include-closed\" flag: \
                 strconv.ParseBool: parsing {value:?}: invalid syntax"
            ),
            "stderr must be pflag's bare message: {stderr}"
        );
        assert!(stdout.is_empty(), "no payload is emitted: {stdout}");
    }
}

/// The regression this file exists for: the parse arm is right, but the
/// rejection used to be printed as `Error: invalid argument ...`. Go prints it
/// with a bare `fmt.Fprintln(os.Stderr, ...)` (main.go:4549) and adds no prefix,
/// so the extra word is a byte-level drift on the error path.
#[test]
fn parse_error_carries_no_error_prefix() {
    let (code, _, stderr) = with_flag("yes");
    assert_eq!(code, 1);
    assert!(
        !stderr.contains("Error:"),
        "Go's pflag error is printed bare, got: {stderr}"
    );
    assert!(
        stderr.starts_with("invalid argument \"yes\" for \"--related-include-closed\" flag:"),
        "got: {stderr}"
    );
}

/// pflag applies flags left to right and each `Set` overwrites, so the LAST
/// occurrence is the effective one — including when a valued form follows the
/// bare switch and vice versa.
#[test]
fn last_occurrence_wins() {
    let (code, stdout, stderr) = bvr(&[
        "--related-include-closed=true",
        "--related-include-closed=false",
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        payload(&stdout)["concurrent"].is_null(),
        "the trailing =false overwrites the earlier =true: {stdout}"
    );

    let (code, stdout, stderr) = bvr(&[
        "--related-include-closed=false",
        "--related-include-closed=true",
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        !payload(&stdout)["concurrent"].is_null(),
        "the trailing bare switch overwrites the earlier =false: {stdout}"
    );
}

/// A bool gets `NoOptDefVal = "true"`, so it never consumes the following
/// token: the switch reads true and `false` is left over as a separate
/// argument.
///
/// Go then rejects that leftover positional with `unknown command "false" for
/// "bv"` and exit 1 (cmd/bv/main.go:4548-4550) — the flag's own parse is
/// right, and the run fails on the word that was left over. bvr used to
/// accept the leftover silently; `argv::unconsumed_positional` now reports
/// it, as the oracle does.
#[test]
fn space_separated_token_is_not_consumed_as_the_value() {
    let (code, stdout, stderr) = bvr(&["--related-include-closed", "false"]);
    assert_eq!(code, 1, "stderr: {stderr}");
    assert!(
        stderr.contains("unknown command \"false\""),
        "the leftover positional is rejected: {stderr}"
    );
    assert!(
        stdout.is_empty(),
        "a rejected run publishes no payload: {stdout}"
    );
}
