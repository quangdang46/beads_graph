//! `--relations-threshold` parity with Go v0.25.0.
//!
//! Go registers it as a pflag `flag.Float64` with default 0.5
//! (`beads_viewer/cmd/bv/main.go:1571`), threads the pointer into
//! `handleRobotFileRelations` (`robot_registry.go:3010-3011`), and the value
//! only becomes a number once `CoChangeMatrix.GetRelatedFiles` floors it
//! (`pkg/correlation/file_index.go:484-486`):
//!
//! ```go
//! if threshold <= 0 {
//!     threshold = 0.5 // Default: 50% co-occurrence
//! }
//! if limit <= 0 {
//!     limit = 10
//! }
//! ```
//!
//! So the value pflag hands over is parsed by `strconv.ParseFloat(s, 64)`, and
//! three separate things can go wrong between the command line and that
//! comparison, all of which this file pins:
//!
//!   * the `0.5` floor — `0` and a negative are "50% co-occurrence", never
//!     "no filter";
//!   * the grammar — Go accepts signed exponents (`1e-5`), underscore
//!     separators (`1_0`) and hexadecimal floats (`0x1p-2`, `0x1.8p1`), all of
//!     which Rust's own `f64` parser rejects;
//!   * the `inf`/`nan` literals, which parse fine and are then rejected by
//!     `json.Marshal` as `json: unsupported value: +Inf`, exit 1.

use std::process::Command;

const REPO_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

/// `--relations-threshold=<value> --robot-file-relations <target>`.
///
/// The `--flag=value` spelling is used throughout: pflag accepts both forms,
/// but only this one leaves the value untouched by the single-dash
/// normalisation Go itself performs (main.go:533-553), which has its own
/// separate, known divergence for values that begin with `-`.
fn run(target: &str, value: &str) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_bvr"))
        .current_dir(REPO_ROOT)
        .args([
            "--robot-file-relations",
            target,
            &format!("--relations-threshold={value}"),
        ])
        .output()
        .expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn payload(stdout: &str) -> serde_json::Value {
    serde_json::from_str(stdout)
        .unwrap_or_else(|e| panic!("stdout is one JSON object: {e}\n{stdout}"))
}

fn threshold_of(target: &str, value: &str) -> f64 {
    let (code, stdout, stderr) = run(target, value);
    assert_eq!(code, 0, "exit code, stderr: {stderr}");
    payload(&stdout)["threshold"]
        .as_f64()
        .unwrap_or_else(|| panic!("`threshold` is a number in {stdout}"))
}

fn related_files_of(target: &str, value: &str) -> Vec<serde_json::Value> {
    let (code, stdout, stderr) = run(target, value);
    assert_eq!(code, 0, "exit code, stderr: {stderr}");
    // `null` (Go's nil `entries` slice) reads as an empty set here; the
    // null-vs-`[]` distinction has its own assertion in
    // `the_threshold_reaches_the_filter`.
    payload(&stdout)["related_files"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// `file_index.go:484-486` floors anything `<= 0` to 0.5, so a bare `0` and a
/// negative are indistinguishable from omitting the flag — neither of them
/// means "return every co-changing file".
#[test]
fn zero_and_negative_floor_to_the_half_cooccurrence_default() {
    for value in ["0", "0.0", "-0.0", "-1", "-1e-3"] {
        assert_eq!(
            threshold_of("README.md", value),
            0.5,
            "--relations-threshold={value} must floor to 0.5, not to 0"
        );
    }
}

/// Omitting the flag entirely is the same floor, reached through pflag's
/// `0.5` default rather than through `GetRelatedFiles`.
#[test]
fn omitted_flag_uses_the_same_default() {
    let out = Command::new(env!("CARGO_BIN_EXE_bvr"))
        .current_dir(REPO_ROOT)
        .args(["--robot-file-relations", "README.md"])
        .output()
        .expect("binary runs");
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert_eq!(
        payload(&stdout)["threshold"].as_f64(),
        Some(0.5),
        "{stdout}"
    );
}

/// Rust's `f64::from_str` rejects a signed exponent, so `--relations-threshold
/// 1e-5` used to be answered with pflag's syntax error and exit 1 while Go
/// printed a result. `1e-5` is the natural way to ask for a low threshold, so
/// this is the most visible half of the grammar gap.
#[test]
fn signed_decimal_exponent_is_accepted() {
    assert_eq!(threshold_of("README.md", "1e-5"), 1e-5);
    assert_eq!(threshold_of("README.md", "1E+5"), 100000.0);
    assert_eq!(threshold_of("README.md", "0.5e-1"), 0.05);
}

/// Rust's parser has no hexadecimal-float support at all, so every spelling Go
/// reads was an error here. `0x3` still is one: Go requires the `p` exponent.
#[test]
fn hexadecimal_floats_are_accepted_and_still_require_the_p_exponent() {
    assert_eq!(threshold_of("README.md", "0x1p-2"), 0.25);
    assert_eq!(threshold_of("README.md", "0x1.8p1"), 3.0);
    assert_eq!(threshold_of("README.md", "0x.8p1"), 1.0);
    assert_eq!(threshold_of("README.md", "0X1P2"), 4.0);
    // Underflow is a successful parse of zero, which the floor then lifts to
    // the default; only overflow is an error.
    assert_eq!(threshold_of("README.md", "0x1p-1075"), 0.5);

    let (code, _, stderr) = run("README.md", "0x3");
    assert_eq!(code, 1, "0x3 has no p exponent and must not parse");
    assert_eq!(
        stderr.trim(),
        "invalid argument \"0x3\" for \"--relations-threshold\" flag: \
         strconv.ParseFloat: parsing \"0x3\": invalid syntax"
    );
}

/// Go's underscore separators, including inside the exponent, and the overflow
/// that Go reports as `value out of range` where Rust returned `inf`.
#[test]
fn underscore_separators_and_overflow_match_go() {
    assert_eq!(threshold_of("README.md", "1_0"), 10.0);
    assert_eq!(threshold_of("README.md", "1_0.5_0"), 10.5);
    assert_eq!(threshold_of("README.md", "0x1_0p0"), 16.0);
    assert_eq!(threshold_of("README.md", "0x1.8p1_0"), 1536.0);
    assert_eq!(threshold_of("README.md", "1e1_0"), 1e10);

    for value in ["1e400", "0x1p2000"] {
        let (code, _, stderr) = run("README.md", value);
        assert_eq!(code, 1, "{value} overflows f64");
        assert_eq!(
            stderr.trim(),
            format!(
                "invalid argument {value:?} for \"--relations-threshold\" flag: \
                 strconv.ParseFloat: parsing {value:?}: value out of range"
            )
        );
    }
}

/// pflag treats a value `strconv.ParseFloat` cannot read as fatal. Rust used to
/// `.parse().ok().unwrap_or(0.5)` it, so these answered with the default and
/// exit 0.
#[test]
fn unparsable_value_is_pflags_parse_error_at_exit_one() {
    for (value, reason) in [
        ("abc", "invalid syntax"),
        ("", "invalid syntax"),
        (" 0.5", "invalid syntax"),
        ("0.5 ", "invalid syntax"),
        ("1e", "invalid syntax"),
        ("1__0", "invalid syntax"),
        ("0x1p_2", "invalid syntax"),
    ] {
        let (code, _, stderr) = run("README.md", value);
        assert_eq!(code, 1, "{value:?} must be a parse error");
        assert_eq!(
            stderr.trim(),
            format!(
                "invalid argument {value:?} for \"--relations-threshold\" flag: \
                 strconv.ParseFloat: parsing {value:?}: {reason}"
            )
        );
    }
}

/// `inf`/`nan` are ordinary literals to `strconv.ParseFloat`, and neither
/// survives `threshold <= 0`, so they reach `json.Marshal` and fail there
/// (robot_registry.go:3028-3032). Rust's `serde_json::json!` turns a
/// non-finite f64 into `null` and carries on, which silently answered with
/// `"threshold": null` and exit 0.
#[test]
fn non_finite_literals_fail_at_json_encoding_as_go_does() {
    for (value, rendered) in [("Inf", "+Inf"), ("infinity", "+Inf"), ("NaN", "NaN")] {
        let (code, stdout, stderr) = run("README.md", value);
        assert_eq!(code, 1, "{value} must fail to encode, stdout: {stdout}");
        assert_eq!(stdout, "", "Go writes nothing when Marshal fails");
        assert_eq!(
            stderr.trim(),
            format!(
                "Error handling --robot-file-relations: \
                 encoding file relations: json: unsupported value: {rendered}"
            )
        );
    }
}

/// The floored value is what `GetRelatedFiles` actually filters on, so
/// `--relations-threshold 0` and `--relations-threshold 0.5` must select the
/// same files, and raising the threshold can only ever shrink the set.
#[test]
fn the_threshold_reaches_the_filter() {
    // A file with real commit history, so `related_files` is not trivially
    // empty. The counts themselves are history-dependent; the invariants are
    // not.
    let target = "crates/bv/src/main.rs";
    let floored = related_files_of(target, "0");
    assert_eq!(
        floored,
        related_files_of(target, "0.5"),
        "0 floors to 0.5, so both must select the same files"
    );
    assert_eq!(
        related_files_of(target, "-1"),
        floored,
        "a negative floors the same way"
    );
    // A threshold nothing can reach leaves Go's nil `entries` slice in
    // place, which `json.Marshal` writes as `null` — not `[]` (the `[]`
    // shape is reserved for the early returns in GetRelatedFiles).
    let (code, stdout, stderr) = run(target, "1.1");
    assert_eq!(code, 0, "exit code, stderr: {stderr}");
    assert!(
        payload(&stdout)["related_files"].is_null(),
        "no co-change correlation reaches 110%, and Go reports null"
    );
    let wider = related_files_of(target, "0.05");
    assert!(
        wider.len() >= floored.len(),
        "a lower threshold cannot select fewer files: {} < {}",
        wider.len(),
        floored.len()
    );
}
