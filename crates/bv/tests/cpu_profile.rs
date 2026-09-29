//! `--cpu-profile` parity with Go v0.25.0.
//!
//! Go registers it as a pflag `flag.String` with default `""` and the help
//! string `Write CPU profile to file` (beads_viewer/cmd/bv/main.go:1460), then
//! uses the value once, at main.go:1908-1933:
//!
//! ```go
//! var stopCPUProfile func()
//! if *cpuProfile != "" {
//!     f, err := os.Create(*cpuProfile)
//!     if err != nil {
//!         fmt.Fprintf(os.Stderr, "Could not create CPU profile: %v\n", err)
//!         os.Exit(1)
//!     }
//!     if err := pprof.StartCPUProfile(f); err != nil {
//!         _ = f.Close()
//!         fmt.Fprintf(os.Stderr, "Could not start CPU profile: %v\n", err)
//!         os.Exit(1)
//!     }
//!     profileActive := true
//!     stopCPUProfile = func() {
//!         if !profileActive { return }
//!         pprof.StopCPUProfile()
//!         profileActive = false
//!         if err := f.Close(); err != nil {
//!             fmt.Fprintf(os.Stderr, "Could not close CPU profile: %v\n", err)
//!         }
//!     }
//!     defer stopCPUProfile()
//! }
//! ```
//!
//! and hands the same closure to the robot dispatchers as
//! `RobotContext.FinalizeBeforeExit` (main.go:2050), because a robot handler
//! returns through `os.Exit` and would otherwise skip the `defer`.
//!
//! Every one of those behaviours was missing here. The flag was registered
//! (`flags.rs`, `FlagKind::Str`) and `flag_takes_value` deliberately keeps it
//! out of the boolean list so `--cpu-profile out.pprof` swallows the path
//! (`argv.rs:754-757`), so `bvr --cpu-profile out.pprof --robot-triage` parsed
//! cleanly, exited 0, printed its usual robot JSON and wrote nothing at all.
//! Nothing in the golden corpus could see it: the profile is a side-effect
//! file, and pprof output is nondeterministic, so it can never be a
//! byte-golden. These tests assert the two things that are observable — the
//! file appears and holds a real pprof CPU profile, and the failure paths
//! carry Go's message and Go's exit code.

// Unix only. The sampler is pprof-rs, which does not build for
// `*-pc-windows-msvc` (see the `[target.'cfg(unix)'.dependencies]` entry in
// `crates/bv/Cargo.toml`), so `--cpu-profile` is gated and every assertion
// below describes behaviour Windows does not have. The Windows path is covered
// by the error `start_cpu_profile` returns there, not by these tests.
#![cfg(unix)]

use std::io::Read as _;
use std::process::Command;

/// Every run pins the source with `--db`, so the loader cannot pick `beads.db`
/// on one side and `issues.jsonl` on the other.
fn bvr(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_bvr"))
        .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .args(["--db", ".beads/issues.jsonl"])
        .args(args)
        .output()
        .expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// A fresh scratch directory per test.
///
/// Nothing here cleans up: this repo forbids deleting a file without explicit
/// permission, so the few-kilobyte profiles these runs leave under the OS temp
/// directory are the price of that rule.
fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("bvr-cpu-profile-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Inflate a gzip file into the protobuf it wraps. `flate2` is a dependency of
/// the `bv` package, so the integration test can reach it.
fn gunzip(path: &std::path::Path) -> Vec<u8> {
    let raw = std::fs::read(path).expect("profile was written");
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(&raw[..])
        .read_to_end(&mut out)
        .expect("payload is a well-formed gzip stream");
    out
}

/// Go's `pprof.StartCPUProfile` gzips what it writes, so the file opens with
/// the gzip magic. Asserting the magic rather than just "the file exists" is
/// what separates a profile from an empty file created by `os.Create` and
/// never written — which is exactly what a run that returns before the
/// `defer` used to leave behind.
fn assert_gzip(bytes: &[u8]) {
    assert_eq!(
        &bytes[..2],
        b"\x1f\x8b",
        "profile is not gzip-framed the way Go's runtime/pprof writes it"
    );
}

/// pprof-rs builds the profile's string table with these four sample-type
/// names and the `thread` label unconditionally (`pprof-0.15.0/src/report.rs`,
/// `Report::pprof`), so their presence in the decoded protobuf proves the file
/// carries a CPU profile rather than a valid-but-empty gzip.
fn assert_cpu_profile_table(payload: &[u8]) {
    for marker in [
        b"samples".as_slice(),
        b"count",
        b"cpu",
        b"nanoseconds",
        b"thread",
    ] {
        assert!(
            payload.windows(marker.len()).any(|w| w == marker),
            "decoded profile is missing the pprof marker {:?}; the file is not a CPU profile",
            std::str::from_utf8(marker).unwrap()
        );
    }
}

#[test]
fn cpu_profile_writes_a_real_pprof_file() {
    let dir = scratch("write");
    let path = dir.join("out.pprof");
    let path_str = path.to_str().unwrap();

    let (code, _stdout, stderr) = bvr(&["--cpu-profile", path_str, "--robot-triage"]);

    assert_eq!(code, 0, "profiling must not change the exit code");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    assert!(path.is_file(), "--cpu-profile did not create its file");

    let raw = std::fs::read(&path).unwrap();
    assert_gzip(&raw);
    assert!(
        raw.len() > 200,
        "profile is implausibly small: {} bytes",
        raw.len()
    );
    assert_cpu_profile_table(&gunzip(&path));
}

#[test]
fn profile_is_flushed_on_the_robot_exit_path() {
    // Go's robot handlers return through `os.Exit`, which is why
    // `RobotContext.FinalizeBeforeExit` exists (main.go:2050) — a plain
    // `defer` would not run. A run that only ever works on the normal-return
    // path passes the first test and fails this one.
    let dir = scratch("robot");
    let path = dir.join("robot.pprof");
    let path_str = path.to_str().unwrap();

    let (code, stdout, stderr) = bvr(&["--cpu-profile", path_str, "--robot-triage"]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    // stdout must still be the robot envelope, unmodified by profiling.
    serde_json::from_str::<serde_json::Value>(&stdout).expect("stdout is still one JSON object");
    assert!(path.is_file(), "robot path did not flush the profile");
    assert_gzip(&std::fs::read(&path).unwrap());
}

/// Go's `os.Create` is `O_RDWR|O_CREATE|O_TRUNC`, so a re-run at the same path
/// replaces the previous profile rather than appending to it. Rust's
/// `File::create` truncates for the same reason; the assertion is that the
/// result is a small, complete gzip and not a concatenation whose leading
/// member is the stale one.
#[test]
fn cpu_profile_truncates_a_previous_profile() {
    let dir = scratch("truncate");
    let path = dir.join("stale.pprof");
    std::fs::write(&path, vec![0x41u8; 64 * 1024]).expect("seed a large stale file");
    let path_str = path.to_str().unwrap();

    let (code, _stdout, stderr) = bvr(&["--cpu-profile", path_str, "--robot-triage"]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    let raw = std::fs::read(&path).unwrap();
    assert_gzip(&raw);
    assert!(
        raw.len() < 64 * 1024,
        "stale bytes survived; the file was not truncated ({} bytes)",
        raw.len()
    );
    assert_cpu_profile_table(&gunzip(&path));
}

/// Go main.go:1910-1915. The whole line, including the errno, is asserted
/// because Go formats the failure as an `*fs.PathError` —
/// `open <path>: no such file or directory` — and `Could not create CPU
/// profile: %v` wraps that verbatim. Nothing reaches stdout, and the profile is
/// never started, so this is the one branch that changes the exit code.
#[test]
fn cpu_profile_bad_path_exits_1_with_go_message() {
    let path = "/nonexistent-bvr-cpu-profile-dir/out.pprof";
    assert!(
        !std::path::Path::new(path).parent().unwrap().exists(),
        "the fixture's parent directory must not exist"
    );

    let (code, stdout, stderr) = bvr(&["--cpu-profile", path, "--robot-triage"]);

    assert_eq!(code, 1, "a create failure is exit 1 in Go");
    assert!(stdout.is_empty(), "nothing goes to stdout: {stdout}");
    assert_eq!(
        stderr,
        format!("Could not create CPU profile: open {path}: no such file or directory\n"),
        "stderr must match Go's `Could not create CPU profile: %v` with a Go PathError"
    );
    assert!(
        !std::path::Path::new(path).exists(),
        "no file may be left behind by a failed create"
    );
}

/// A path that cannot be opened for a reason other than ENOENT, to pin that
/// the message carries Go's errno wording rather than Rust's
/// `... (os error N)` rendering. A directory is the portable choice: every
/// platform rejects it with EISDIR.
#[test]
fn cpu_profile_directory_target_reports_go_errno() {
    let dir = scratch("isdir");
    let path_str = dir.to_str().unwrap();

    let (code, _stdout, stderr) = bvr(&["--cpu-profile", path_str, "--robot-triage"]);

    assert_eq!(code, 1);
    assert_eq!(
        stderr,
        format!("Could not create CPU profile: open {path_str}: is a directory\n"),
        "stderr must carry Go's errno text, not Rust's `Is a directory (os error 21)`"
    );
}

/// Go's guard is `if *cpuProfile != ""` (main.go:1910), so an empty value is
/// the unset case: no sampler, no file, no error. The path is one that could
/// be created, so "the file is absent" is a real assertion about the flag
/// rather than about the filesystem.
#[test]
fn cpu_profile_empty_value_creates_nothing() {
    let dir = scratch("empty");
    let path = dir.join("never.pprof");
    let path_str = path.to_str().unwrap();

    let (code, _stdout, stderr) = bvr(&["--cpu-profile=", "--robot-triage"]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    assert!(
        !path.exists(),
        "an empty --cpu-profile is Go's unset case and must not create {path_str}"
    );
}

/// Go's `flag` package accepts both `--name value` and `--name=value`, and the
/// rest of this CLI reads either spelling through `flag_value`. Pin the `=`
/// form so the two cannot drift apart.
#[test]
fn cpu_profile_accepts_the_equals_spelling() {
    let dir = scratch("equals");
    let path = dir.join("eq.pprof");
    let arg = format!("--cpu-profile={}", path.to_str().unwrap());

    let (code, _stdout, stderr) = bvr(&[&arg, "--robot-triage"]);

    assert_eq!(code, 0, "unexpected stderr: {stderr}");
    assert!(
        path.is_file(),
        "`--cpu-profile=<path>` must work like `--cpu-profile <path>`"
    );
    assert_cpu_profile_table(&gunzip(&path));
}

/// The profile starts at main.go:1908, after `validateModifierFlags`,
/// `validateEnumFlags` and `validateExclusivePrimaryCommands` (main.go:1890-1901)
/// and after the `--watch-export`/`--as-of` cross-check (main.go:1903-1906). A
/// run rejected by any of them exits 1 without ever creating the file. This
/// pins the block's position, which is the one part of the port that a
/// "does the file appear?" test cannot see.
#[test]
fn rejected_run_creates_no_profile_file() {
    let dir = scratch("rejected");
    let path = dir.join("rejected.pprof");
    let path_str = path.to_str().unwrap();

    // `--graph-format` is a modifier flag, so the run is rejected at
    // main.go:1890 before :1908 is reached.
    let (code, _stdout, stderr) = bvr(&[
        "--cpu-profile",
        path_str,
        "--graph-format=bogus",
        "--robot-triage",
    ]);

    assert_eq!(code, 1, "a modifier-requires violation is exit 1");
    // The exact wording is deliberately not pinned: Go runs
    // `validateModifierFlags` before `validateEnumFlags` and so reports the
    // modifier-requires message, while this CLI reports the enum one. That
    // ordering is its own pre-existing divergence, and what this flag owes
    // Go is only that the run is rejected before the profiler starts.
    assert!(
        stderr.starts_with("Error: "),
        "a validation failure is reported on stderr: {stderr}"
    );
    assert!(
        !path.exists(),
        "validation runs before the profiler, so {path_str} must not exist"
    );
}

/// `--help` is the one early exit that Go also takes before the profile
/// block: cobra consults the help flag before the root command runs, so Go
/// never reaches main.go:1908 for it either. Both binaries agree that no file
/// appears; this keeps it that way if the block is ever moved earlier.
#[test]
fn help_creates_no_profile_file() {
    let dir = scratch("help");
    let path = dir.join("help.pprof");
    let path_str = path.to_str().unwrap();

    let (code, _stdout, _stderr) = bvr(&["--cpu-profile", path_str, "--help"]);

    assert_eq!(code, 0);
    assert!(
        !path.exists(),
        "`--help` short-circuits in Go before main.go:1908, so no file is created"
    );
}
