//! `--debug-render` / `--debug-width` / `--debug-height` parity with Go v0.25.0.
//!
//! Go registers the three at `cmd/bv/main.go:1630-1632` (help text: "Render a
//! view and output to file (views: insights, board)", defaults 180x50) and
//! spends them in exactly one place, `cmd/bv/main.go:4526-4531`:
//!
//! ```go
//! // Debug render mode - output a view to file and exit
//! if *debugRender != "" {
//!     output := m.RenderDebugView(*debugRender, *debugWidth, *debugHeight)
//!     fmt.Println(output)
//!     os.Exit(0)
//! }
//! ```
//!
//! Two placement facts drive every test below. First, the block sits after
//! BOTH load paths converge and after `ui.NewModel`/`EnableWorkspaceMode`, so
//! it is reachable from workspace mode — a single-repo-only check sends
//! `--debug-render` straight into the TUI there. Second, `ui.NewModel`
//! (`pkg/ui/model.go`) is what populates the insights panel's metrics, so
//! `RenderDebugView` always draws real data.
//!
//! The flag help says "output to file" but the code does `fmt.Println`, so
//! stdout is what is compared — the help text is stale in Go itself.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Go main.go:4482-4485, verbatim. Printed to STDOUT, exit 0.
const NO_ISSUES: &str = "No issues found. Create some with 'br create'!";

static SEQ: AtomicUsize = AtomicUsize::new(0);

/// A scratch directory removed on drop, so a failing assertion cannot leak
/// fixtures into the next test.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("bvr_debug_render_{tag}_{}_{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("scratch dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    /// A single-repo checkout: `<dir>/.beads/issues.jsonl`, the shape
    /// `load_issues_from_repo` walks. Empty `rows` yields the zero-issue case.
    fn single_repo(&self, rows: &[&str]) -> &Self {
        let beads = self.0.join(".beads");
        std::fs::create_dir_all(&beads).unwrap();
        let mut body = String::new();
        for r in rows {
            body.push_str(r);
            body.push('\n');
        }
        std::fs::write(beads.join("issues.jsonl"), body).unwrap();
        self
    }

    /// A workspace checkout: `.bv/workspace.yaml` plus one `.beads` per repo.
    fn workspace(&self, repos: &[(&str, &str, &str)]) -> &Self {
        std::fs::create_dir_all(self.0.join(".bv")).unwrap();
        let mut yaml = String::from("name: t\nrepos:\n");
        for (name, prefix, rows) in repos {
            let beads = self.0.join(name).join(".beads");
            std::fs::create_dir_all(&beads).unwrap();
            let mut body = String::new();
            for r in rows.split_terminator('\n') {
                if r.is_empty() {
                    continue;
                }
                body.push_str(r);
                body.push('\n');
            }
            std::fs::write(beads.join("issues.jsonl"), body).unwrap();
            yaml.push_str(&format!(
                "  - name: {name}\n    path: {name}\n    prefix: {prefix}\n"
            ));
        }
        std::fs::write(self.0.join(".bv/workspace.yaml"), yaml).unwrap();
        self
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn issue(id: &str, title: &str, deps: &str) -> String {
    format!(
        r#"{{"id":"{id}","title":"{title}","status":"open","priority":1,"issue_type":"task","dependencies":[{deps}]}}"#
    )
}

/// Run `bvr` in `dir` with the given extra args. `Command::output` already
/// gives the child a null stdin, which is what keeps a regression in the
/// TUI-launch path from hanging the suite instead of failing it.
fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bvr"))
        .current_dir(dir)
        .args(args)
        .output()
        .expect("binary runs")
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A three-node chain, so betweenness/pagerank/eigenvector all rank non-empty
/// and every insights panel has something to print.
fn chain(prefix: &str) -> Vec<String> {
    vec![
        issue(&format!("{prefix}-1"), "Root one", ""),
        issue(
            &format!("{prefix}-2"),
            "Middle two",
            &format!(r#"{{"issue_id":"{prefix}-1","type":"blocks"}}"#),
        ),
        issue(
            &format!("{prefix}-3"),
            "Leaf three",
            &format!(r#"{{"issue_id":"{prefix}-2","type":"blocks"}}"#),
        ),
    ]
}

#[test]
fn insights_debug_render_draws_graph_metrics() {
    // The bug: graph metrics were computed inside `launch_tui` only, and
    // `--debug-render` returns before it. Every insights panel came out blank
    // even though Go's `ui.NewModel` has already filled them.
    let dir = Scratch::new("insights");
    let rows = chain("core");
    dir.single_repo(&rows.iter().map(String::as_str).collect::<Vec<_>>());

    let out = run(
        dir.path(),
        &[
            "--debug-render",
            "insights",
            "--debug-width",
            "100",
            "--debug-height",
            "20",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));

    let stdout = stdout_of(&out);
    for id in ["core-1", "core-2", "core-3"] {
        assert!(
            stdout.contains(id),
            "expected the {id} row in the rendered insights panel.\n{stdout}"
        );
    }
}

#[test]
fn debug_render_suppresses_the_tui_launch_banner() {
    // Go prints nothing on this path; the Rust banner said "launching TUI"
    // while rendering and exiting instead, and it leaked even onto the
    // parse-error exit.
    let dir = Scratch::new("banner");
    let rows = chain("core");
    dir.single_repo(&rows.iter().map(String::as_str).collect::<Vec<_>>());

    let out = run(dir.path(), &["--debug-render", "insights"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    assert_eq!(
        stderr_of(&out),
        "",
        "Go writes nothing to stderr on the debug-render path"
    );
    assert!(
        !stderr_of(&out).contains("launching TUI"),
        "the TUI banner must not run when --debug-render replaces the TUI"
    );
}

#[test]
fn workspace_mode_reaches_debug_render() {
    // The bug: the check lived only in the single-repo arm, so a workspace
    // load fell through to `launch_tui` and the TUI took over (or died with
    // "Device not configured") instead of rendering.
    let dir = Scratch::new("workspace");
    let api = chain("api")
        .iter()
        .map(|r| r.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let web = issue("web-1", "Web one", "");
    dir.workspace(&[("api", "api", &api), ("web", "web", &web)]);

    let out = run(
        dir.path(),
        &[
            "--workspace",
            ".bv/workspace.yaml",
            "--debug-render",
            "insights",
            "--debug-width",
            "60",
            "--debug-height",
            "10",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    assert_eq!(stderr_of(&out), "", "no TUI banner in workspace mode");

    let stdout = stdout_of(&out);
    // Only the api chain is asserted. `web-1` is an isolated node, so it
    // ranks last and drops out of the fixed-height panels — that is a
    // bv-tui layout property, not part of this flag's contract.
    for id in ["api-1", "api-2", "api-3"] {
        assert!(
            stdout.contains(id),
            "expected {id} from the aggregated workspace load.\n{stdout}"
        );
    }
}

#[test]
fn empty_repo_prints_go_no_issues_message() {
    // Go main.go:4482-4485 runs before `ui.NewModel`, so an empty load never
    // reaches RenderDebugView: it prints the line, exits 0.
    let dir = Scratch::new("empty");
    dir.single_repo(&[]);

    let out = run(dir.path(), &["--debug-render", "insights"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    assert_eq!(stdout_of(&out).trim_end_matches('\n'), NO_ISSUES);
    assert_eq!(stderr_of(&out), "");
}

#[test]
fn unknown_view_returns_go_string_and_exits_zero() {
    // Go's `RenderDebugView` default arm: `"Unknown view: " + viewName`, and
    // `main.go:4529` prints it then exits 0. Note "history" is accepted even
    // though the help text lists only insights and board.
    let dir = Scratch::new("unknown");
    let rows = chain("core");
    dir.single_repo(&rows.iter().map(String::as_str).collect::<Vec<_>>());

    for view in ["zzz", "Insights"] {
        let out = run(dir.path(), &["--debug-render", view]);
        assert_eq!(
            out.status.code(),
            Some(0),
            "view {view:?}: stderr: {}",
            stderr_of(&out)
        );
        assert_eq!(
            stdout_of(&out).trim_end_matches('\n'),
            format!("Unknown view: {view}")
        );
    }
}

#[test]
fn empty_view_value_is_not_a_render_request() {
    // Go main.go:4527 is `if *debugRender != ""`, so an explicit empty value
    // is "not requested" and falls through to the TUI — RenderDebugView's
    // default arm never runs. Asserted only on stdout, because the fall-through
    // really does try to open a TUI and its exit code depends on whether the
    // test runner has a terminal.
    let dir = Scratch::new("emptyview");
    let rows = chain("core");
    dir.single_repo(&rows.iter().map(String::as_str).collect::<Vec<_>>());

    let out = run(dir.path(), &["--debug-render", ""]);
    assert_eq!(
        stdout_of(&out),
        "",
        "an empty --debug-render must not render, and must not print \"Unknown view: \""
    );
}

#[test]
fn unparsable_dimension_reports_go_pflag_error() {
    // pflag's `intValue.Set` runs `strconv.ParseInt(s, 0, 64)` and main.go
    // prints the NumError verbatim after its own wrapper. The Rust side
    // hardcoded a bare `parse error` and dropped the detail tail.
    let dir = Scratch::new("parse");
    let rows = chain("core");
    dir.single_repo(&rows.iter().map(String::as_str).collect::<Vec<_>>());

    let out = run(
        dir.path(),
        &["--debug-render", "insights", "--debug-width", "abc"],
    );
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout_of(&out), "");
    assert_eq!(
        stderr_of(&out).trim_end_matches('\n'),
        "invalid argument \"abc\" for \"--debug-width\" flag: strconv.ParseInt: parsing \"abc\": invalid syntax"
    );

    let out = run(
        dir.path(),
        &["--debug-render", "insights", "--debug-height", "1zz"],
    );
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr_of(&out).trim_end_matches('\n'),
        "invalid argument \"1zz\" for \"--debug-height\" flag: strconv.ParseInt: parsing \"1zz\": invalid syntax"
    );
}

#[test]
fn dimensions_default_to_180x50() {
    // Go main.go:1631-1632. Omitting both flags must still render, and at the
    // registered size: 49 rows, because every branch reserves the last row
    // for the status bar (`height - 1`, pkg/ui/model.go:10575-10584).
    let dir = Scratch::new("defaults");
    let rows = chain("core");
    dir.single_repo(&rows.iter().map(String::as_str).collect::<Vec<_>>());

    let out = run(dir.path(), &["--debug-render", "insights"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));

    let stdout = stdout_of(&out);
    let body = stdout.trim_end_matches('\n');
    assert_eq!(
        body.lines().count(),
        49,
        "default height 50 minus the status-bar row"
    );
    for line in body.lines() {
        assert_eq!(
            line.chars().count(),
            180,
            "default width is 180 columns; got {} in {line:?}",
            line.chars().count()
        );
    }
}

#[test]
fn debug_width_requires_debug_render() {
    // Go main.go:1842-1843 registers both dimensions as modifiers of
    // `--debug-render`; a bare `--debug-width` is a usage error at exit 1.
    let dir = Scratch::new("modifier");
    let rows = chain("core");
    dir.single_repo(&rows.iter().map(String::as_str).collect::<Vec<_>>());

    let out = run(dir.path(), &["--debug-width", "100"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout_of(&out), "");
    let stderr = stderr_of(&out);
    assert!(
        stderr.contains("Error: --debug-width requires --debug-render"),
        "got {stderr:?}"
    );
    assert!(
        stderr.contains("Try: `bvr --debug-render triage --debug-width 120 --debug-height 40`."),
        "got {stderr:?}"
    );
}
