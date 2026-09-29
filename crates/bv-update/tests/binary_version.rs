//! Post-download verification of the extracted binary.
//!
//! Oracle: `beads_viewer/pkg/updater/updater.go` at commit 18afafa —
//! `parseBinaryVersionOutput` (1453-1462), `limitedOutputBuffer` (1466-1484),
//! `verifyBinaryVersion` (1486-1519). The `4 << 10` output cap and the
//! `bv <version>` two-field shape are Go's.
//!
//! Before the port, Rust only checked that `--version` exited zero, so a
//! wrong-architecture or truncated binary was installed silently. A straight
//! port of the value comparison, however, is *also* wrong here: `bvr --version`
//! prints the frozen Go-compatibility constant `v0.25.0`
//! (`crates/bv/src/main.rs` `GO_APP_VERSION`), which never equals the release
//! tag. Both failure directions are covered below.

use bv_update::update::parse_binary_version_output;
// The tests that exercise `verify_binary_version` shell out to a fake `bvr`
// script, so they are all `#[cfg(unix)]`. The import has to carry the same
// gate: left ungated it is unused on Windows, which `-D warnings` turns into a
// build failure.
#[cfg(unix)]
use bv_update::update::verify_binary_version;
use std::path::PathBuf;

// --- pure parsing ----------------------------------------------------------

#[test]
fn the_version_output_must_be_exactly_two_fields_named_bv() {
    assert_eq!(
        parse_binary_version_output("bv v0.2.1\n").unwrap(),
        "v0.2.1"
    );
    assert_eq!(parse_binary_version_output("bv 1.2.3").unwrap(), "1.2.3");

    for bad in [
        "",
        "bv",
        "bv v0.2.1 extra",
        "bvr v0.2.1",
        "bev v0.2.1",
        "0.2.1",
        "some other program v0.2.1",
    ] {
        let err = parse_binary_version_output(bad).unwrap_err();
        assert!(
            err.contains("unexpected --version output"),
            "{bad:?} should be refused, got {err:?}"
        );
    }
}

#[test]
fn the_reported_version_must_itself_parse() {
    let err = parse_binary_version_output("bv 1.2.3.4").unwrap_err();
    assert!(
        err.contains("invalid version in --version output"),
        "got {err:?}"
    );
    assert!(parse_binary_version_output("bv v01.2.3").is_err());
}

// --- running a synthetic "binary" -----------------------------------------

/// Write an executable shell script that stands in for a built `bvr`.
#[cfg(unix)]
fn fake_binary(tag: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "bvr-fakebin-{}-{}-{}",
        tag,
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A script that behaves like a stock bvr: exits 0 and prints `bv <v>`.
#[cfg(unix)]
fn version_script(tag: &str, version: &str) -> PathBuf {
    fake_binary(tag, &format!("#!/bin/sh\necho 'bv {version}'\n"))
}

#[cfg(unix)]
fn cleanup(paths: &[PathBuf]) {
    for p in paths {
        let _ = std::fs::remove_file(p);
    }
}

/// The real-world case: a stock bvr release binary self-reports the Go-parity
/// version, not the tag. Demanding an exact match would make every
/// `bvr --update` fail, so this must be accepted.
#[cfg(unix)]
#[test]
fn a_stock_bvr_reporting_the_go_compat_version_is_accepted() {
    let bin = version_script("stock", "v0.25.0");
    let result = verify_binary_version(&bin, "v0.2.1");
    cleanup(&[bin]);
    assert_eq!(
        result,
        Ok(()),
        "a real bvr --version prints v0.25.0; refusing it would break every update"
    );
}

#[cfg(unix)]
#[test]
fn an_exact_or_equivalent_match_is_accepted() {
    for (reported, expected) in [
        ("v0.2.1", "v0.2.1"),
        ("0.2.1", "v0.2.1"),
        ("v0.2.1", "0.2.1"),
        ("  v0.2.1  ", "v0.2.1"),
    ] {
        let bin = version_script("exact", reported);
        let result = verify_binary_version(&bin, expected);
        cleanup(&[bin]);
        assert_eq!(result, Ok(()), "{reported:?} vs {expected:?}");
    }
}

#[cfg(unix)]
#[test]
fn a_mismatched_version_that_is_not_the_go_compat_string_is_refused() {
    let bin = version_script("wrong", "v0.1.0");
    let err = verify_binary_version(&bin, "v0.2.1").unwrap_err();
    cleanup(&[bin]);
    assert_eq!(
        err, "downloaded binary reports v0.1.0, expected v0.2.1",
        "Go's message (updater.go:1516) must survive"
    );
}

#[cfg(unix)]
#[test]
fn something_that_is_not_bvr_is_refused() {
    let bin = fake_binary("other", "#!/bin/sh\necho 'malware 9.9.9'\n");
    let err = verify_binary_version(&bin, "v0.2.1").unwrap_err();
    cleanup(&[bin]);
    assert!(err.contains("unexpected --version output"), "got {err:?}");
}

#[cfg(unix)]
#[test]
fn a_non_zero_exit_is_refused_with_stderr_attached() {
    let bin = fake_binary(
        "fail",
        "#!/bin/sh\necho 'cannot allocate segment' >&2\nexit 3\n",
    );
    let err = verify_binary_version(&bin, "v0.2.1").unwrap_err();
    cleanup(&[bin]);
    assert!(err.contains("run --version:"), "got {err:?}");
    assert!(
        err.contains("cannot allocate segment"),
        "Go attaches stderr (updater.go:1500); got {err:?}"
    );
}

#[cfg(unix)]
#[test]
fn a_non_executable_or_broken_file_is_refused() {
    let path = std::env::temp_dir().join(format!("bvr-notexec-{}", std::process::id()));
    std::fs::write(&path, b"not a program").unwrap();
    assert!(verify_binary_version(&path, "v0.2.1").is_err());
    let _ = std::fs::remove_file(&path);
}

#[cfg(unix)]
#[test]
fn output_beyond_the_four_kib_cap_is_refused() {
    // Go's `limitedOutputBuffer` records truncation and
    // `verifyBinaryVersion` refuses rather than parsing a partial string.
    let filler = "x".repeat(8192);
    let bin = fake_binary("chatty", &format!("#!/bin/sh\necho 'bv v0.2.1 {filler}'\n"));
    let err = verify_binary_version(&bin, "v0.2.1").unwrap_err();
    cleanup(&[bin]);
    assert!(
        err.contains("--version output exceeds 4096 bytes"),
        "got {err:?}"
    );
}
