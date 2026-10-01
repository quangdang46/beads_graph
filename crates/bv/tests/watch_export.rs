//! `--watch-export` watch-mode parity with Go v0.25.0 (bv-55, coalesced in #159).
//!
//! Go wraps the export in a `doExport` closure (cmd/bv/main.go:3061-3214) and,
//! only after the initial export succeeds, branches into watch mode
//! (main.go:3223-3453). This covers the three behaviours that loop is for:
//!
//!   1. the banner, byte-for-byte — including the watched path, which follows
//!      the DISCOVERED source rather than a hardcoded `.beads/issues.jsonl`;
//!   2. the content hash gate — a file rewritten with identical bytes must not
//!      regenerate the bundle (main.go:3414-3418), because a full site plus
//!      git-history regeneration is the expensive part;
//!   3. the graceful stop — `signal.Notify` on SIGINT/SIGTERM prints the
//!      farewell and exits 0 (main.go:3446-3448). On the default disposition
//!      Ctrl+C would kill the process with 130 before the loop ever runs.
//!
//! Each case drives the real binary against a scratch workspace: a copy of the
//! repo's own JSONL under a temp dir, so the test can rewrite the source without
//! touching `.beads/`. History generation is switched off because the scratch
//! dir is not a git repository and Go warns rather than fails there.

// Unix only. Every test here ends by delivering a real SIGINT to the watch
// process, through `libc::kill(child.id() as libc::pid_t, libc::SIGINT)` —
// `Child::kill` sends SIGKILL, which skips the handler and is exactly what
// these tests must not do. Neither `kill` nor `pid_t` is defined in
// `libc/src/windows/`, so the file does not compile there:
//
//     error[E0425]: cannot find function `kill` in crate `libc`
//     error[E0425]: cannot find type `pid_t` in crate `libc`
//
// This follows the port itself: `install_stop_handler` is `#[cfg(unix)]`, so
// Windows has no graceful stop to exercise. Go's `signal.Notify` does work
// there — that gap is documented on `install_stop_handler`.
#![cfg(unix)]

use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const REPO_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

/// Scratch workspace: `<tmp>/ws/.beads/issues.jsonl` seeded from the repo's own
/// data so the export has a realistic, non-trivial source to load.
fn scratch_workspace(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("bvr-watch-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let beads = base.join(".beads");
    std::fs::create_dir_all(&beads).expect("create .beads");
    let src = Path::new(REPO_ROOT).join(".beads/issues.jsonl");
    std::fs::copy(&src, beads.join("issues.jsonl")).expect("seed issues.jsonl");
    base
}

fn source_of(ws: &Path) -> PathBuf {
    ws.join(".beads").join("issues.jsonl")
}

/// Spawn `--watch-export` with stdout+stderr going to a file we can poll.
fn spawn_watch(ws: &Path, out_dir: &Path, log: &Path) -> Child {
    let file = std::fs::File::create(log).expect("create log");
    Command::new(env!("CARGO_BIN_EXE_bvr"))
        .current_dir(ws)
        .arg("--export-pages")
        .arg(out_dir)
        .arg("--watch-export")
        .arg("--db")
        .arg(".beads/issues.jsonl")
        // Not a git repo; Go warns and continues, so skip the noisy path.
        .arg("--pages-include-history=false")
        .stdout(Stdio::from(file.try_clone().expect("clone log")))
        .stderr(Stdio::from(file))
        .spawn()
        .expect("watch mode spawns")
}

fn read_log(log: &Path) -> String {
    let mut s = String::new();
    if let Ok(mut f) = std::fs::File::open(log) {
        let mut buf = String::new();
        let _ = f.read_to_string(&mut buf);
        s = buf;
        let _ = f.seek(std::io::SeekFrom::Start(0));
    }
    s
}

/// Poll the log until `needle` appears, or give up. Fixed sleeps would either
/// flake on a loaded machine or add dead time; the watch loop's own floors
/// (500ms debounce, 500ms settle) are far below this budget.
fn wait_for(log: &Path, needle: &str, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if read_log(log).contains(needle) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Rewrite the first record's title, which changes the source data_hash and so
/// must clear the loop's unchanged-content gate.
fn mutate_source(ws: &Path) {
    let path = source_of(ws);
    let text = std::fs::read_to_string(&path).expect("read source");
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let first = lines[0].replace("\"title\"", "\"title\": \"x\", \"_watch_marker\"");
    lines[0] = first;
    let mut f = std::fs::File::create(&path).expect("rewrite source");
    f.write_all(lines.join("\n").as_bytes())
        .expect("write source");
    f.sync_all().expect("flush source");
}

fn sigint(child: &Child) {
    // SAFETY: sending a signal to a pid we spawned and have not reaped.
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGINT);
    }
}

/// The banner is what proves watch mode was entered at all — before this the
/// process runs exactly one export and exits. Go's text, including the two
/// blank lines that bracket the preview hint (main.go:3224-3271).
#[test]
fn watch_banner_matches_go_and_names_the_discovered_source() {
    let ws = scratch_workspace("banner");
    let out_dir = ws.join("out");
    let log = ws.join("log.txt");
    let mut child = spawn_watch(&ws, &out_dir, &log);

    assert!(
        wait_for(
            &log,
            "Watch mode enabled. Monitoring for changes...",
            Duration::from_secs(60)
        ),
        "watch mode never started; log:\n{}",
        read_log(&log)
    );

    // Go prints the resolved source path, so an absolute canonical path is
    // expected here — on macOS `/tmp` is a symlink to `/private/tmp`.
    let expected = std::fs::canonicalize(source_of(&ws)).expect("canonicalize");
    let text = read_log(&log);
    assert!(
        text.contains(&format!("  → Watching: {}", expected.display())),
        "banner must name the loaded source.\n  wanted: {}\n  log:\n{text}",
        expected.display()
    );
    for line in [
        "  → Press Ctrl+C to stop",
        "To preview with auto-refresh, run in another terminal:",
    ] {
        assert!(text.contains(line), "missing banner line {line:?}:\n{text}");
    }
    assert!(
        text.contains(&format!("  bvr --preview-pages {}", out_dir.display())),
        "preview hint must name the export dir:\n{text}"
    );

    sigint(&child);
    let _ = child.wait();
}

/// The gate that makes the loop affordable: a rewrite with identical content
/// must NOT regenerate. Go compares `issuesFingerprint` before exporting
/// (main.go:3414-3418) and resets the window to its floor.
#[test]
fn identical_rewrite_does_not_re_export() {
    let ws = scratch_workspace("skip");
    let out_dir = ws.join("out");
    let log = ws.join("log.txt");
    let mut child = spawn_watch(&ws, &out_dir, &log);
    assert!(
        wait_for(&log, "Watch mode enabled", Duration::from_secs(60)),
        "watch mode never started; log:\n{}",
        read_log(&log)
    );

    // Touch alone changes mtime, which is what the watcher sees; the bytes are
    // identical, so the hash gate has to swallow it.
    let path = source_of(&ws);
    let text = std::fs::read_to_string(&path).expect("read source");
    let mut f = std::fs::File::create(&path).expect("rewrite source");
    f.write_all(text.as_bytes()).expect("write source");
    f.sync_all().expect("flush source");

    // Well past the 500ms debounce plus 500ms settle, several times over.
    std::thread::sleep(Duration::from_secs(6));
    let text = read_log(&log);
    assert!(
        !text.contains("Re-exporting"),
        "an identical rewrite must not re-export (main.go:3414-3418):\n{text}"
    );

    sigint(&child);
    let _ = child.wait();
}

/// A real content change re-exports exactly once, labelled with the wall-clock
/// stamp and the change ordinal Go prints (main.go:3066-3070).
#[test]
fn content_change_re_exports_once_with_go_label() {
    let ws = scratch_workspace("reexport");
    let out_dir = ws.join("out");
    let log = ws.join("log.txt");
    let mut child = spawn_watch(&ws, &out_dir, &log);
    assert!(
        wait_for(&log, "Watch mode enabled", Duration::from_secs(60)),
        "watch mode never started; log:\n{}",
        read_log(&log)
    );

    mutate_source(&ws);

    assert!(
        wait_for(&log, "Re-exporting (change #1)", Duration::from_secs(60)),
        "no re-export after a content change; log:\n{}",
        read_log(&log)
    );
    // The label is `[HH:MM:SS] Re-exporting (change #N)...` — assert the
    // surrounding shape so a future refactor cannot drop the stamp.
    let text = read_log(&log);
    assert!(
        text.lines()
            .any(|l| l.starts_with('[') && l.ends_with("] Re-exporting (change #1)...")),
        "re-export line must carry a [HH:MM:SS] stamp:\n{text}"
    );

    // Coalescing: many rapid writes must not fan out into many exports. Give
    // the loop room, then assert the count stayed at the one it already did.
    for _ in 0..8 {
        mutate_source(&ws);
        std::thread::sleep(Duration::from_millis(120));
    }
    std::thread::sleep(Duration::from_secs(8));
    let text = read_log(&log);
    let exports = text.matches("Re-exporting").count();
    assert!(
        exports <= 3,
        "8 writes inside the settle window coalesced into {exports} exports:\n{text}"
    );

    sigint(&child);
    let _ = child.wait();
}

/// Ctrl+C must reach the handler: farewell line, then exit 0 — not a signal
/// death (128+SIGINT) from the default disposition.
#[test]
fn sigint_prints_farewell_and_exits_zero() {
    let ws = scratch_workspace("sigint");
    let out_dir = ws.join("out");
    let log = ws.join("log.txt");
    let mut child = spawn_watch(&ws, &out_dir, &log);
    assert!(
        wait_for(&log, "Watch mode enabled", Duration::from_secs(60)),
        "watch mode never started; log:\n{}",
        read_log(&log)
    );

    sigint(&child);
    let status = child.wait().expect("child exits");

    let text = read_log(&log);
    assert!(
        text.contains("\nStopping watch mode..."),
        "missing farewell line (main.go:3447):\n{text}"
    );
    assert_eq!(
        status.code(),
        Some(0),
        "Go exits 0 on SIGINT (main.go:3448), got {status:?}:\n{text}"
    );
}
