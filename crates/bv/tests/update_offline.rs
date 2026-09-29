//! The four self-update flags, driven as a subprocess with no network
//! reachable.
//!
//! With the network down, `GetLatestRelease` / `CheckUpdateAvailable` fail
//! immediately, so the only branch each flag can reach is its *first* exit-1
//! path. Go prints the same prefixes (cmd/bv/main.go:2060, 2078, 2110) and
//! exits the same codes; only the transport error text behind the colon
//! differs, because the two binaries use different HTTP stacks. These tests
//! pin the prefix, the stream and the exit code — never the error detail.
//!
//! `--rollback` is run from a throwaway directory holding a *copy* of the
//! binary. `perform_rollback` resolves `<current_exe>.backup`, so running the
//! build-tree binary directly would find (or lack) a backup depending on
//! whether some earlier update had touched it — the copy makes "no backup
//! exists" a property of the fixture rather than of the machine.
//!
//! The remaining branches — the release plan, the validation gate, the
//! version-compare failure and the whole `[Y/n]` gate — sit behind a
//! successful release fetch and are covered offline by the `update_cli_tests`
//! unit module in `crates/bv/src/main.rs`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// A proxy that is never listening. `ureq` and Go's `http.ProxyFromEnvironment`
/// both read these, so every request fails at connect without leaving the host.
const DEAD_PROXY: &str = "http://127.0.0.1:1";

/// A fixture directory: an empty but well-formed issue database, so the test
/// never depends on the repository's own `.beads/` state.
///
/// `name` must be unique per test. Cargo runs the tests in one binary
/// concurrently, and `isolated_binary` rewrites the executable it copies —
/// two tests sharing a directory would execute a half-written binary, and
/// `--rollback` additionally renames its own executable away.
fn fixture(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("bvr-update-offline-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("fixture dir");
    std::fs::write(dir.join("issues.jsonl"), "").expect("empty issues.jsonl");
    dir
}

/// Normalize a path or message for comparison across platforms: forward
/// slashes, and no Windows verbatim (`\\?\`) prefix. `\\?\UNC\server\share`
/// becomes `//server/share`.
fn comparable_path(path: &std::path::Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    match s.strip_prefix("//?/UNC/") {
        Some(rest) => format!("//{rest}"),
        None => s.strip_prefix("//?/").unwrap_or(&s).to_string(),
    }
}

fn run_in(dir: &Path, binary: &Path, args: &[&str]) -> Output {
    Command::new(binary)
        .current_dir(dir)
        .args(args)
        .args(["--db", "issues.jsonl"])
        // Both HTTP stacks honour these; setting every alias covers the
        // upper/lower-case split without depending on which one is read.
        .env("HTTP_PROXY", DEAD_PROXY)
        .env("HTTPS_PROXY", DEAD_PROXY)
        .env("ALL_PROXY", DEAD_PROXY)
        .env("http_proxy", DEAD_PROXY)
        .env("https_proxy", DEAD_PROXY)
        .env("all_proxy", DEAD_PROXY)
        .env("NO_PROXY", "")
        .env("no_proxy", "")
        .stdin(Stdio::null())
        .output()
        .expect("binary runs")
}

/// Copy the binary under test into `dir` so its `<exe>.backup` lookup is
/// scoped to the fixture.
fn isolated_binary(dir: &Path) -> PathBuf {
    let copy = dir.join("bvr-under-test");
    std::fs::copy(env!("CARGO_BIN_EXE_bvr"), &copy).expect("copy binary into fixture");
    copy
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Assert exit 1, nothing on stdout, and `prefix` at the head of stderr.
fn assert_fails_with(out: &Output, args: &[&str], prefix: &str) {
    let stderr = stderr_of(out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "expected exit 1 for {args:?}\nstderr: {stderr}"
    );
    assert_eq!(stdout_of(out), "", "expected empty stdout for {args:?}");
    assert!(
        stderr.starts_with(prefix),
        "expected {args:?} stderr to start with {prefix:?}, got {stderr:?}"
    );
}

// --- main.go:2057 --check-update -------------------------------------------

#[test]
fn check_update_reports_the_fetch_failure_on_stderr() {
    let dir = fixture("check-update");
    let binary = isolated_binary(&dir);
    assert_fails_with(
        &run_in(&dir, &binary, &["--check-update"]),
        &["--check-update"],
        "Error checking for updates: ",
    );
}

// --- main.go:2074 --update-dry-run ----------------------------------------

#[test]
fn update_dry_run_reports_the_fetch_failure_on_stderr() {
    let dir = fixture("update-dry-run");
    let binary = isolated_binary(&dir);
    assert_fails_with(
        &run_in(&dir, &binary, &["--update-dry-run"]),
        &["--update-dry-run"],
        "Error fetching release info: ",
    );
}

// --- main.go:2106 --update -------------------------------------------------

#[test]
fn update_reports_the_fetch_failure_before_prompting() {
    let dir = fixture("update");
    let binary = isolated_binary(&dir);
    let out = run_in(&dir, &binary, &["--update"]);
    // The prompt is unreachable offline: the release fetch fails first, so
    // nothing may be written to stdout.
    assert_fails_with(&out, &["--update"], "Error fetching release info: ");
}

// --- main.go:2160 --rollback -----------------------------------------------

#[test]
fn rollback_without_a_backup_exits_one() {
    let dir = fixture("rollback");
    let binary = isolated_binary(&dir);
    let out = run_in(&dir, &binary, &["--rollback"]);
    assert_fails_with(&out, &["--rollback"], "Rollback failed: ");
    // Go updater.go:1560 names the path it looked at, so the message must
    // point inside the fixture rather than at the build tree.
    //
    // Both sides are compared through `comparable_path`. The binary's own path
    // comes from `current_binary_path`, which canonicalizes; the fixture path
    // here comes from `%TEMP%`, and on a Windows runner that is the 8.3 short
    // name `C:\Users\RUNNER~1\...` while canonicalize reports the long
    // `C:\Users\runneradmin\...`. Those are two spellings of one directory, so
    // the literal comparison was failing on the spelling rather than on the
    // resolution — which is the thing being asserted.
    let stderr = stderr_of(&out);
    let expected = format!(
        "{}/bvr-under-test.backup",
        comparable_path(&dir.canonicalize().expect("fixture canonicalizes"))
    );
    let got = comparable_path(std::path::Path::new(&stderr));
    assert!(
        got.contains(&expected),
        "stderr should name the fixture backup path, wanted {expected:?}, got {stderr:?}"
    );
    // main.go:2160-2164: the handler adds nothing on stdout.
    assert_eq!(stdout_of(&out), "");
}

// --- the `bv upgrade` rewrite (argv.rs:393, Go main.go:667) ---------------

/// `bv upgrade <mode>` must reach the same handler as the flag it expands to,
/// with the same exit code. argv.rs is where that mapping lives and main.rs is
/// where it lands, so only a subprocess can observe the join.
#[test]
fn upgrade_subcommand_reaches_the_same_handlers() {
    let dir = fixture("upgrade");
    let binary = isolated_binary(&dir);
    for (mode, prefix) in [
        (vec!["upgrade", "--check"], "Error checking for updates: "),
        (
            vec!["upgrade", "--dry-run"],
            "Error fetching release info: ",
        ),
        (vec!["upgrade"], "Error fetching release info: "),
        (vec!["upgrade", "--rollback"], "Rollback failed: "),
    ] {
        let out = run_in(&dir, &binary, &mode);
        assert_fails_with(&out, &mode, prefix);
    }
}
