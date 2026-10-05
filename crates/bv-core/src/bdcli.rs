//! `bd` (Dolt-backed) reader.
//!
//! `bd` keeps issues in Dolt and only materialises `.beads/issues.jsonl`
//! when an export is explicitly asked for — `export.auto` is off by default.
//! A workspace can therefore hold a live issue graph with no JSONL on disk for
//! [`crate::discovery::load_issues_from_repo`] to find, which is the state a
//! fresh `bd init` leaves behind.
//!
//! `bd export --all` writes beads JSONL to **stdout** by default: `-o` is
//! optional (`cmd/bd/export.go:67`, falling through to `w = os.Stdout` at
//! `:129`), and each record carries its dependency edges inlined
//! (`issue.Dependencies = rel.deps[issue.ID]`, `cmd/bd/export.go:229`). That
//! is the same shape the JSONL [`crate::loader`] already parses, so this
//! shells out and hands the bytes to that parser instead of growing a second
//! decode path.
//!
//! Invariant: read-only. `-o` is deliberately omitted so nothing is written to
//! the workspace, and this is only ever reached for a workspace
//! [`crate::tracker::is_bd_workspace`] already vouches for.

use crate::loader::{parse_issues_with_options, ParseOptions, ParseStats};
use crate::model::Issue;
use std::path::Path;
use std::time::{Duration, Instant};

/// Upper bound on one `bd export`.
///
/// Go `bv` never shells out to `bd`, so there is no upstream constant to
/// mirror and this bound is ours alone. It is deliberately far looser than
/// [`crate::tracker`]'s 2s help probe: that one only reads `--help`, whereas
/// this walks the whole issue table out of Dolt, which on a large workspace is
/// legitimately slow. The bound still exists so a wedged `bd` cannot hang the
/// whole `bv` invocation — the failure `probe_tracker` guards against for the
/// same reason.
const EXPORT_TIMEOUT: Duration = Duration::from_secs(30);

/// Poll cadence while waiting on the child. Matches `probe_tracker`.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Locate the `bd` executable, or `None` when it is not installed.
///
/// Windows needs this because a package manager may install `bd` as shims
/// (`bd`, `bd.cmd`, `bd.ps1`) with no `bd.exe` at all — `npm install -g
/// @beads/bd` does exactly that — and `CreateProcess` only ever appends
/// `.exe`, so a bare `Command::new("bd")` reports "program not found" on a
/// machine where `bd` works fine from a shell. We search PATH ourselves and
/// honour PATHEXT. The shim itself runs without a `cmd /c` wrapper.
///
/// Exposed so tests can skip on the same condition the reader actually fails
/// on, rather than a looser one that makes them pass vacuously.
pub fn bd_executable() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    {
        let path = std::env::var_os("PATH")?;
        // Windows' own default, in case PATHEXT is unset.
        let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
        // PATHEXT is a `;`-separated list of extensions, not of paths.
        let exts: Vec<&str> = pathext.split(';').filter(|s| !s.is_empty()).collect();
        for dir in std::env::split_paths(&path) {
            for ext in &exts {
                let cand = dir.join(format!("bd{ext}"));
                if cand.is_file() {
                    return Some(cand);
                }
            }
            // A bare `bd` shim has no extension and so never matches PATHEXT.
            let bare = dir.join("bd");
            if bare.is_file() {
                return Some(bare);
            }
        }
        None
    }
    #[cfg(not(windows))]
    {
        // Every Unix `Command::new` resolves bare names through PATH itself,
        // and no shim scheme exists to get wrong.
        Some(std::path::PathBuf::from("bd"))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BdExportError {
    /// `bd` is not on PATH, or could not be started at all. Callers should
    /// fall through to the next datasource rather than surface this — a
    /// Dolt workspace on a machine without `bd` installed is a normal state,
    /// not a hard error.
    #[error("cannot run `bd export`: {0}")]
    Spawn(String),
    #[error("`bd export` did not finish within {secs}s")]
    Timeout { secs: u64 },
    #[error("`bd export` exited unsuccessfully{}", detail.as_ref().map(|d| format!(": {d}")).unwrap_or_default())]
    Failed { detail: Option<String> },
    #[error("`bd export` produced unreadable beads JSONL: {0}")]
    Parse(String),
}

/// Run `bd export --all` in `repo_root` and parse stdout as beads JSONL.
///
/// `repo_root` is the working directory `bd` resolves the workspace from, so
/// it must be the repo root, not the `.beads` directory.
pub fn load_issues_from_bd_export(
    repo_root: &Path,
) -> Result<(Vec<Issue>, ParseStats), BdExportError> {
    let stdout = run_bd_export(repo_root)?;
    let mut reader = stdout.as_slice();
    parse_issues_with_options(&mut reader, &ParseOptions::default(), |_| {})
        .map_err(|e| BdExportError::Parse(e.to_string()))
}

/// Spawn `bd export --all`, returning raw stdout bytes.
///
/// Polls rather than blocking on `wait`, so a `bd` that never exits is killed
/// at the deadline instead of hanging the caller.
fn run_bd_export(repo_root: &Path) -> Result<Vec<u8>, BdExportError> {
    let exe = bd_executable()
        .ok_or_else(|| BdExportError::Spawn("`bd` was not found on PATH".to_string()))?;
    let mut child = std::process::Command::new(exe)
        .args(["export", "--all"])
        .current_dir(repo_root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| BdExportError::Spawn(e.to_string()))?;

    let deadline = Instant::now() + EXPORT_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut buf = Vec::new();
                use std::io::Read;
                if let Some(mut out) = child.stdout.take() {
                    let _ = out.read_to_end(&mut buf);
                }
                return if status.success() {
                    Ok(buf)
                } else {
                    let mut err = String::new();
                    if let Some(mut e) = child.stderr.take() {
                        let _ = e.read_to_string(&mut err);
                    }
                    Err(BdExportError::Failed {
                        detail: (!err.trim().is_empty()).then(|| err.trim().to_string()),
                    })
                };
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL_INTERVAL),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(BdExportError::Timeout {
                    secs: EXPORT_TIMEOUT.as_secs(),
                });
            }
            Err(e) => return Err(BdExportError::Spawn(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `bd export --all` capture: two issues joined by one `blocks` edge,
    /// field-for-field the shape `cmd/bd/export.go:226-229` emits. Pins the
    /// contract this reader relies on — edges inlined on the issue record as
    /// raw rows, `_type` discriminator present, `owner` (not `assignee`).
    const EXPORT_JSONL: &str = concat!(
        r#"{"_type":"issue","id":"test-9ix","title":"Root feature","status":"open","priority":1,"issue_type":"task","owner":"dev@example.test","created_at":"2026-10-05T01:50:10Z","created_by":"tester","updated_at":"2026-10-05T01:50:10Z","dependency_count":0,"dependent_count":0,"comment_count":0}"#,
        "\n",
        r#"{"_type":"issue","id":"test-303","title":"Task B","status":"open","priority":2,"issue_type":"task","owner":"dev@example.test","created_at":"2026-10-05T01:50:26Z","created_by":"tester","updated_at":"2026-10-05T01:50:26Z","dependencies":[{"issue_id":"test-303","depends_on_id":"test-9ix","type":"blocks","created_at":"2026-10-05T01:50:43Z","created_by":"tester","metadata":"{}"}],"dependency_count":1,"dependent_count":0,"comment_count":0}"#,
        "\n",
    );

    fn parse(bytes: &[u8]) -> (Vec<Issue>, ParseStats) {
        let mut reader = bytes;
        parse_issues_with_options(&mut reader, &ParseOptions::default(), |_| {}).expect("parses")
    }

    #[test]
    fn export_records_parse_into_issues_with_edges() {
        let (issues, stats) = parse(EXPORT_JSONL.as_bytes());

        assert_eq!(issues.len(), 2);
        assert_eq!(stats.valid, 2);
        assert_eq!(stats.errors, 0);

        let a = issues.iter().find(|i| i.id == "test-9ix").expect("root");
        assert_eq!(a.title, "Root feature");
        assert_eq!(a.priority, 1);
        assert!(a.dependencies.is_empty());

        let b = issues.iter().find(|i| i.id == "test-303").expect("task b");
        assert_eq!(b.dependencies.len(), 1);
        let dep = &b.dependencies[0];
        assert_eq!(dep.issue_id, "test-303");
        assert_eq!(dep.effective_depends_on(), "test-9ix");
        assert_eq!(dep.created_by, "tester");
        assert!(dep.created_at.as_deref().is_some_and(|t| !t.is_empty()));
    }

    /// `bd` reports the assignee as `owner`; the frozen model calls it
    /// `assignee`. Without the alias this is silently empty and every
    /// assignee-keyed robot command loses its grouping.
    #[test]
    fn bd_owner_lands_in_assignee() {
        let (issues, _) = parse(EXPORT_JSONL.as_bytes());
        assert!(issues.iter().all(|i| i.assignee == "dev@example.test"));
    }

    /// Keys bd emits that the model does not carry (`dependency_count`,
    /// `comment_count`, `created_by`, and the `metadata` string on a
    /// dependency) must not fail the record.
    #[test]
    fn unknown_bd_keys_are_tolerated() {
        let (issues, stats) = parse(EXPORT_JSONL.as_bytes());
        assert_eq!(stats.errors, 0, "unknown keys must not count as errors");
        assert_eq!(issues.len(), 2);
    }

    /// `bd export` emits `"_type":"memory"` records for non-issue rows
    /// (`cmd/bd/export.go:333`). They share the stream and must be skipped
    /// silently, not counted as parse errors.
    #[test]
    fn memory_records_are_skipped() {
        let stream = format!(
            "{}{}",
            concat!(
                r#"{"_type":"memory","id":"mem-1","text":"not an issue"}"#,
                "\n",
            ),
            EXPORT_JSONL
        );
        let (issues, stats) = parse(stream.as_bytes());
        assert_eq!(issues.len(), 2);
        assert_eq!(stats.errors, 0);
        assert_eq!(stats.skipped, 1);
    }

    /// `bd` spells the due date `due_at`; the model calls it `due_date`.
    #[test]
    fn bd_due_at_lands_in_due_date() {
        let line = concat!(
            r#"{"id":"d1","title":"Dated","status":"open","priority":1,"#,
            r#""created_at":"2026-10-05T01:50:10Z","updated_at":"2026-10-05T01:50:10Z","#,
            r#""due_at":"2027-01-14T17:00:00Z"}"#,
            "\n"
        );
        let (issues, stats) = parse(line.as_bytes());
        assert_eq!(stats.errors, 0);
        assert_eq!(
            issues[0].due_date.as_deref(),
            Some("2027-01-14T17:00:00Z"),
            "due_at must not be dropped"
        );
    }

    /// A missing `bd` must degrade, not explode — the caller falls through to
    /// the next datasource. Exercised in `tests/bdcli_export.rs` against a
    /// real binary; here we only pin that the error variants say something
    /// actionable.
    #[test]
    fn errors_carry_usable_text() {
        assert!(BdExportError::Spawn("No such file or directory".into())
            .to_string()
            .contains("bd export"));
        assert!(BdExportError::Timeout { secs: 30 }
            .to_string()
            .contains("30s"));
        assert!(BdExportError::Failed {
            detail: Some("not a workspace".into())
        }
        .to_string()
        .contains("not a workspace"));
        assert!(BdExportError::Failed { detail: None }
            .to_string()
            .contains("exited unsuccessfully"));
    }
}
