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

/// Locate the `bd` executable.
///
/// On Windows this walks PATH honouring PATHEXT, and returns `None` when
/// nothing matches. That search is necessary because a package manager may
/// install `bd` as shims (`bd`, `bd.cmd`, `bd.ps1`) with no `bd.exe` at all
/// — `npm install -g @beads/bd` does exactly that — and `CreateProcess` only
/// ever appends `.exe`, so a bare `Command::new("bd")` reports "program not
/// found" on a machine where `bd` works fine from a shell. Script shims are
/// not directly executable, though: [`spawn_bd_export`] wraps `.cmd`/`.bat`
/// in `cmd /c` and `.ps1` in `powershell -File` (post-CVE-2024-24576,
/// `CreateProcess` refuses to run them bare).
///
/// On Unix it always returns the bare name: there `Command` resolves through
/// PATH itself, so there is nothing to search. **That means `Some` does not
/// imply `bd` is installed** — a missing `bd` surfaces as a spawn error from
/// [`load_issues_from_bd_export`], which callers treat as "try the next
/// datasource". Anything that needs a real availability check (the tests
/// gate on it) must additionally prove the binary starts.
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

/// Go `loader.exportBDIssuesJSONL` (loader.go:512-560): run
/// `bd export -o <issuesPath>` inside the workspace so the compatibility
/// JSONL materialises on disk, exactly like Go's
/// `PrepareBeadsDirForRead` refresh. `bd` resolves the workspace from
/// `BEADS_DIR`, which is set to the absolute beads dir; `BEADS_DB` is
/// stripped like Go's `withoutBeadsAuthorityEnv`.
///
/// Unlike [`load_issues_from_bd_export`] this WRITES the export file — it is
/// only called on the bd/Dolt datasource path where Go does the same.
pub fn export_bd_issues_jsonl(beads_dir: &Path, issues_path: &Path) -> Result<(), String> {
    let exe = bd_executable().ok_or_else(|| "bd binary not found in PATH".to_string())?;
    let abs_beads = std::fs::canonicalize(beads_dir).unwrap_or_else(|_| beads_dir.to_path_buf());
    let abs_issues =
        std::fs::canonicalize(issues_path).unwrap_or_else(|_| issues_path.to_path_buf());
    let repo_root = abs_beads
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| abs_beads.clone());

    let mut cmd = bd_command(&exe);
    cmd.arg("export")
        .arg("-o")
        .arg(&abs_issues)
        .current_dir(&repo_root)
        .env("BEADS_DIR", &abs_beads);
    // Go strips BEADS_DIR and BEADS_DB from the child env
    // (`withoutBeadsAuthorityEnv`), then re-adds BEADS_DIR=absBeadsDir.
    cmd.env_remove("BEADS_DB");
    let out = cmd
        .output()
        .map_err(|e| format!("bd binary not found in PATH: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
    let msg = if msg.is_empty() {
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    } else {
        msg
    };
    Err(if msg.is_empty() {
        format!("bd export exited with {}", out.status)
    } else {
        msg
    })
}

/// Build the base `bd` command, applying the Windows script-shim wrapping
/// shared with [`spawn_bd_export`].
#[cfg(windows)]
fn bd_command(exe: &Path) -> std::process::Command {
    let ext = exe
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "cmd" | "bat" => {
            let mut c = std::process::Command::new("cmd");
            c.arg("/c").arg(exe);
            c
        }
        "ps1" => {
            let mut c = std::process::Command::new("powershell");
            c.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
                .arg(exe);
            c
        }
        _ => std::process::Command::new(exe),
    }
}

#[cfg(not(windows))]
fn bd_command(exe: &Path) -> std::process::Command {
    std::process::Command::new(exe)
}

/// Spawn `bd export --all`, returning raw stdout bytes.
///
/// Polls rather than blocking on `wait`, so a `bd` that never exits is killed
/// at the deadline instead of hanging the caller. Both pipes are drained on
/// reader threads for the whole wait — see [`collect_with_timeout`].
fn run_bd_export(repo_root: &Path) -> Result<Vec<u8>, BdExportError> {
    let exe = bd_executable()
        .ok_or_else(|| BdExportError::Spawn("`bd` was not found on PATH".to_string()))?;
    let mut child =
        spawn_bd_export(&exe, repo_root).map_err(|e| BdExportError::Spawn(e.to_string()))?;

    let (status, buf, err) = collect_with_timeout(&mut child, EXPORT_TIMEOUT)?;
    if status.success() {
        return Ok(buf);
    }
    let err = String::from_utf8_lossy(&err);
    Err(BdExportError::Failed {
        detail: (!err.trim().is_empty()).then(|| err.trim().to_string()),
    })
}

/// Build the `bd export --all` child process.
///
/// On Windows the resolved executable may be a script shim (`bd.cmd`,
/// `bd.bat`, `bd.ps1`) that `CreateProcess` cannot execute directly, so
/// those extensions are wrapped: `.cmd`/`.bat` go through `cmd /c`, `.ps1`
/// through `powershell -NoProfile -ExecutionPolicy Bypass -File`. Anything
/// else (`.exe`, extensionless) spawns directly.
fn spawn_bd_export(exe: &Path, repo_root: &Path) -> std::io::Result<std::process::Child> {
    let args = ["export", "--all"];
    #[cfg(windows)]
    {
        let ext = exe
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        match ext.as_str() {
            "cmd" | "bat" => {
                let mut c = std::process::Command::new("cmd");
                c.arg("/c").arg(exe).args(args);
                return c
                    .current_dir(repo_root)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn();
            }
            "ps1" => {
                let mut c = std::process::Command::new("powershell");
                c.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
                    .arg(exe)
                    .args(args);
                return c
                    .current_dir(repo_root)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn();
            }
            _ => {}
        }
    }
    std::process::Command::new(exe)
        .args(args)
        .current_dir(repo_root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
}

/// Read a child pipe to EOF on a helper thread, returning the bytes via the
/// join handle. A `None` pipe (child spawned without it) yields an empty
/// buffer immediately.
fn drain_pipe<R>(pipe: Option<R>) -> std::thread::JoinHandle<Vec<u8>>
where
    R: std::io::Read + Send + 'static,
{
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = pipe {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    })
}

/// Poll `child` to exit while both pipes drain concurrently, bounded by
/// `timeout`. Returns `(status, stdout, stderr)`.
///
/// The drains are the load-bearing part: `bd export` can emit far more than
/// the OS pipe buffer (~64KB), and a child blocked writing to a full pipe
/// never exits. Reading only after `try_wait` reports exit deadlocked every
/// large export into the timeout; draining while polling lets the child
/// finish. On timeout/`try_wait` error the child is killed and both drains
/// are joined — killing closes the pipes, so the reads terminate.
fn collect_with_timeout(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>), BdExportError> {
    let stdout_thread = drain_pipe(child.stdout.take());
    let stderr_thread = drain_pipe(child.stderr.take());

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL_INTERVAL),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_thread.join();
                let _ = stderr_thread.join();
                return Err(BdExportError::Timeout {
                    secs: timeout.as_secs(),
                });
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_thread.join();
                let _ = stderr_thread.join();
                return Err(BdExportError::Spawn(e.to_string()));
            }
        }
    };
    let stdout = stdout_thread.join().unwrap_or_default();
    let stderr = stderr_thread.join().unwrap_or_default();
    Ok((status, stdout, stderr))
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
            r#"{"id":"d1","title":"Dated","status":"open","priority":1,"issue_type":"task","#,
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

    /// Regression for the pipe-buffer deadlock: a child that writes far more
    /// than the OS pipe buffer (~64KB) must still be collected. The old
    /// read-after-`try_wait` shape left the child blocked on a full stdout
    /// pipe forever, so every large `bd export` burned the whole timeout.
    #[cfg(unix)]
    #[test]
    fn large_stdout_does_not_deadlock() {
        // ~220KB of stdout — several times the pipe buffer.
        let mut child = std::process::Command::new("sh")
            .args([
                "-c",
                "i=0; while [ $i -lt 20000 ]; do printf 'xxxxxxxxxx\\n'; i=$((i+1)); done",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn sh");
        let (status, stdout, stderr) =
            collect_with_timeout(&mut child, Duration::from_secs(20)).expect("collects");
        assert!(status.success());
        assert!(stdout.len() > 65536, "got {} bytes", stdout.len());
        assert!(stderr.is_empty());
    }

    /// A child that never exits is still killed at the deadline, and the
    /// drain threads are reaped rather than left blocked on the pipes.
    #[cfg(unix)]
    #[test]
    fn hung_child_is_killed_at_the_deadline() {
        let mut child = std::process::Command::new("sh")
            .args(["-c", "sleep 60"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn sh");
        let err = collect_with_timeout(&mut child, Duration::from_millis(300))
            .expect_err("must time out");
        assert!(matches!(err, BdExportError::Timeout { .. }), "{err}");
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
