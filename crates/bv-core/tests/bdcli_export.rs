//! End-to-end coverage for the `bd` (Dolt) datasource against a real binary.
//!
//! The unit tests in `src/bdcli.rs` pin the parse contract from captured
//! output. These pin the thing a capture cannot: that `bd export --all`
//! actually writes beads JSONL to **stdout** with no `-o`, that dependency
//! edges come back inlined, and that `discovery` reaches for it in a Dolt
//! workspace that has no JSONL on disk.
//!
//! `bd` is not a build dependency, so every test skips cleanly when it is
//! absent rather than failing.

use bv_core::bdcli::bd_executable;
use bv_core::discovery::get_beads_dir;
use bv_core::tracker::is_bd_workspace;
use std::path::{Path, PathBuf};

/// Whether the reader itself can find a `bd`.
///
/// Deliberately the reader's own resolver rather than a shell-style
/// `Command::new("bd")`: on Windows a package manager can install only
/// shims, and a looser probe would report "yes" while the reader cannot
/// spawn anything — which is how these tests passed without running.
fn have_bd() -> bool {
    bv_core::bdcli::bd_executable().is_some()
}

/// A throwaway bd workspace. Never touches the caller's project.
struct Workspace(PathBuf, PathBuf);

impl Workspace {
    fn new(tag: &str) -> Option<Self> {
        let bd = bd_executable()?;
        let dir = std::env::temp_dir().join(format!("bv-bdcli-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok()?;
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .ok()?;
        let ok = std::process::Command::new(&bd)
            .args(["init", "--quiet", "--skip-hooks", "--skip-agents"])
            .current_dir(&dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            let _ = std::fs::remove_dir_all(&dir);
            return None;
        }
        // Opt out of the first-run metrics prompt so `bd` never blocks.
        let _ = std::process::Command::new(&bd)
            .args(["metrics", "off"])
            .current_dir(&dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        Some(Self(dir, bd))
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn bd(&self, args: &[&str]) -> String {
        let out = std::process::Command::new(&self.1)
            .args(args)
            .current_dir(&self.0)
            .output()
            .expect("bd runs");
        assert!(out.status.success(), "bd {args:?} failed");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// First issue id bd minted.
    fn first_id(&self) -> String {
        let raw = self.bd(&["list", "--all", "--json"]);
        serde_json::from_str::<serde_json::Value>(&raw).expect("list json")[0]["id"]
            .as_str()
            .expect("id")
            .to_string()
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The headline case: a Dolt workspace with **no** `.beads/issues.jsonl`
/// still yields its issues and edges.
#[test]
fn dolt_workspace_without_jsonl_loads_through_bd() {
    if !have_bd() {
        eprintln!("skipping: `bd` not on PATH");
        return;
    }
    let Some(ws) = Workspace::new("load") else {
        eprintln!("skipping: `bd init` unavailable");
        return;
    };

    ws.bd(&["create", "Root", "-p", "1"]);
    let child = ws.first_id();
    ws.bd(&["create", "Leaf", "-p", "2"]);
    let leaf = {
        let raw = ws.bd(&["list", "--all", "--json"]);
        let all = serde_json::from_str::<serde_json::Value>(&raw).unwrap();
        all.as_array()
            .unwrap()
            .iter()
            .find(|i| i["title"] == "Leaf")
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    ws.bd(&["dep", "add", &leaf, &child]);

    // Precondition: bd's default layout, with no JSONL to find.
    let beads_dir = get_beads_dir(ws.path()).expect("beads dir");
    assert!(is_bd_workspace(&beads_dir), "expected a Dolt workspace");
    assert!(
        !beads_dir.join("issues.jsonl").exists(),
        "test is only meaningful without an issues.jsonl"
    );

    let (issues, stats) = bv_core::discovery::load_issues_from_repo(ws.path()).expect("load");

    assert_eq!(issues.len(), 2, "both issues visible");
    assert_eq!(stats.errors, 0);

    let leaf_issue = issues.iter().find(|i| i.id == leaf).expect("leaf");
    assert_eq!(
        leaf_issue.dependencies.len(),
        1,
        "edge survived the round trip"
    );
    assert_eq!(
        leaf_issue.dependencies[0].effective_depends_on(),
        child,
        "edge points at the right bead"
    );

    // `bd` reports the assignee as `owner`; it must not land empty.
    assert!(
        issues.iter().all(|i| !i.assignee.is_empty()),
        "owner must be folded into assignee, got {:?}",
        issues.iter().map(|i| &i.assignee).collect::<Vec<_>>()
    );
}

/// `bd export --all` writes to stdout with no `-o`. If that ever regressed to
/// file-only, this reader would silently see zero issues.
#[test]
fn bd_export_writes_jsonl_to_stdout() {
    if !have_bd() {
        eprintln!("skipping: `bd` not on PATH");
        return;
    }
    let Some(ws) = Workspace::new("stdout") else {
        return;
    };
    ws.bd(&["create", "Only issue", "-p", "1"]);

    let raw = ws.bd(&["export", "--all"]);
    assert!(!raw.trim().is_empty(), "export produced nothing on stdout");

    let lines: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(!lines.is_empty(), "no JSONL lines");
    for line in &lines {
        serde_json::from_str::<serde_json::Value>(line)
            .unwrap_or_else(|e| panic!("export line is not JSON: {e}\n{line}"));
    }

    // And the reader parses it.
    let (issues, stats) = bv_core::bdcli::load_issues_from_bd_export(ws.path()).expect("parse");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].title, "Only issue");
    assert_eq!(stats.errors, 0);
}

/// A non-workspace directory must produce an error carrying bd's own stderr,
/// not an empty-but-successful load that would read as "no issues".
#[test]
fn failure_outside_a_workspace_is_reported() {
    if !have_bd() {
        eprintln!("skipping: `bd` not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("bv-bdcli-bare-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let err = bv_core::bdcli::load_issues_from_bd_export(&dir).expect_err("not a workspace");
    assert!(
        matches!(err, bv_core::bdcli::BdExportError::Failed { .. }),
        "expected Failed, got {err:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
