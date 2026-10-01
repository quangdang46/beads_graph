//! AGENTS.md integration — port of Go `pkg/agents`.
//!
//! Handles detection, content injection, and preference storage for
//! automatically adding beads_viewer usage instructions to agent
//! configuration files (AGENTS.md, CLAUDE.md, etc.).

pub mod detect;
pub mod file;
pub mod prefs;

/// Current blurb version. Increment when making breaking changes.
/// v4: rename Go `bv` references to Rust `bvr` + fix repo links
/// (quangdang46/beads_viewer_rust) so the injected instructions match this binary.
pub const BLURB_VERSION: i32 = 7;
pub const BLURB_START_MARKER: &str = "<!-- bv-agent-instructions-v7 -->";
pub const BLURB_END_MARKER: &str = "<!-- end-bv-agent-instructions -->";

pub const SUPPORTED_AGENT_FILES: &[&str] = &["AGENTS.md", "CLAUDE.md", "agents.md", "claude.md"];

/// Go `pkg/agents.AgentBlurb` v7, verbatim (blurb.go:29).
pub const AGENT_BLURB: &str = r#"<!-- bv-agent-instructions-v7 -->

---

## Beads Workflow Integration

This project uses a Beads tracker—either the Go `bd` CLI or the Rust `br` CLI—for issue tracking, plus [beads_viewer](https://github.com/Dicklesworthstone/beads_viewer) (`bv`) for graph-aware triage. Issues are stored in `.beads/`. `bv` auto-discovers supported JSONL exports, including `.beads/issues.jsonl` and legacy `.beads/beads.jsonl`.

**Choose the tracker CLI from this repository's instructions and configuration.** Use `bd` commands in a Go Beads workspace and `br` commands in a beads_rust workspace. Do not run both trackers against the same workspace or infer the tracker solely from the JSONL filename.

### Using bvr as an AI sidecar

bvr is a graph-aware triage engine for Beads projects. Instead of parsing .beads/issues.jsonl / .beads/beads.jsonl directly or hallucinating graph traversal, use robot flags for deterministic, dependency-aware outputs with precomputed metrics (PageRank, betweenness, critical path, cycles, HITS, eigenvector, k-core).

**Scope boundary:** bvr handles *what to work on* (triage, priority, planning). The selected tracker CLI (`bd` or `br`) handles creating, claiming, modifying, and closing beads.

**CRITICAL: Use ONLY --robot-* flags. Bare bvr launches an interactive TUI that blocks your session.**

#### The Workflow: Start With Triage

**`bvr --robot-triage` is your single entry point.** Its `triage` object contains:
- `quick_ref`: at-a-glance counts + top 3 picks
- `recommendations`: ranked actionable items with scores, reasons, unblock info
- `quick_wins`: low-effort high-impact items
- `blockers_to_clear`: items that unblock the most downstream work
- `project_health`: status/type/priority distributions, graph metrics
- `commands`: copy-paste shell commands for next steps

```bash
bvr --robot-triage        # THE MEGA-COMMAND: start here
bvr --robot-next          # Minimal: just the single top pick + claim command

# TOON output (--format toon): a compact tabular encoding. Measured on this
# repository it is 7% smaller than JSON for --robot-graph but 9-15% LARGER for
# nested payloads (--robot-triage, --robot-plan, --robot-insights,
# --robot-label-health); use --stats to see both sizes before adopting it.
# TOON encoding shells out to the tru binary. With no encoder installed,
# --format toon prints a fallback warning, emits JSON with output_format "json",
# and --stats prints no sizes at all.
bvr --robot-graph --format toon
bvr --robot-triage --format toon --stats
```

Recommendations can include blocked or assigned work; `triage.quick_ref.top_picks` reflects snapshot readiness. A suggested action records its original local ID, working directory, and tracker route. Use that route rather than a namespaced display ID or an unrelated current directory. Inspect current tracker state before execution: analysis does not reserve work or guarantee that a later claim succeeds.

#### Other bvr Commands

| Command | Returns |
|---------|---------|
| `--robot-plan` | Parallel execution tracks with unblocks lists |
| `--robot-priority` | Priority misalignment detection with confidence |
| `--robot-insights` | Full metrics: PageRank, betweenness, HITS, eigenvector, critical path, cycles, k-core |
| `--robot-alerts` | Stale issues, blocking cascades, priority mismatches |
| `--robot-suggest` | Hygiene: duplicates, missing deps, label suggestions, cycle breaks |
| `--robot-diff --diff-since <ref>` | Changes since ref: new/closed/modified issues |
| `--robot-graph [--graph-format=json\|dot\|mermaid]` | Dependency graph export |

Robot analysis commands default to JSON; `--format toon` selects TOON, and `--robot-help` defaults to text. In JSON mode, `--graph-format=dot` or `mermaid` puts diagram text in the `graph` field (`bvr --robot-graph --graph-format=dot | jq -r .graph`).

#### Scoping & Filtering

```bash
bvr --robot-plan --label backend              # Scope to label's subgraph
bvr --robot-insights --as-of HEAD~30          # Historical point-in-time
bvr --recipe actionable --robot-plan          # Pre-filter: ready to work (no blockers)
bvr --recipe high-impact --robot-triage       # Pre-filter: top PageRank scores
```

### Tracker Commands for Issue Management

Use exactly one command family, matching the tracker configured for the repository.

#### Rust beads_rust (`br`)

Use `br` 0.6.0 or newer when executing saved claim commands. It rechecks
deferred status and future `defer_until` values when the claim runs, so a
recommendation captured before a deferral cannot bypass it. This requirement
applies to executing tracker claims.

```bash
br ready --json                       # Show issues ready to work (no blockers)
br list --status=open --json          # All open issues
br show <id> --json                   # Full issue details with dependencies
br create --title="..." --type=task --priority=2 --json
br update <id> --claim --json         # Claim for the current actor and start work
br close <id> --reason="Completed" --json
br close <id1> <id2> --reason="Completed" --json
br sync --flush-only                  # Export DB to JSONL after Beads mutations
```

#### Go Beads (`bd`)

```bash
bd ready --json                       # Show issues ready to work
bd show <id> --json                   # Full issue details
bd create "..." -t task -p 2 --json
bd update <id> --claim --json         # Atomically claim work
bd close <id> --json
bd dep add <issue> <depends-on>
bd export -o .beads/issues.jsonl        # Refresh the compatibility export read by bv
```

### Workflow Pattern

1. **Triage**: Run `bvr --robot-triage` to find the highest-impact actionable work
2. **Verify**: Check the selected tracker's `show`/`ready` output before claiming
3. **Claim**: Use `br update <id> --claim --json` or `bd update <id> --claim --json`
4. **Work**: Implement the task
5. **Complete**: Use the selected tracker's `close` command
6. **Refresh for bv**: Run `br sync --flush-only` or the `bd export` command above so the JSONL export is current

### Key Concepts

- **Dependencies**: Issues can block other issues. `br ready --json` and `bd ready --json` show unblocked work.
- **Priority**: P0=critical, P1=high, P2=medium, P3=low, P4=backlog (use numbers 0-4, not words)
- **Types**: task, bug, feature, epic, chore, docs, question
- **Blocking**: Use `br dep add <issue> <depends-on>` or `bd dep add <issue> <depends-on>` to add dependencies

### Git Policy

Tracker commands do not grant permission to commit or push application code. Follow this repository's own git and tracker instructions before staging, committing, syncing, or pushing. If the repository says "commit only when asked," that rule overrides any generic workflow advice.

<!-- end-bv-agent-instructions -->"#;

/// Get the preferred agent file path for a new file.
/// Returns AGENTS.md in the project root.
pub fn get_preferred_agent_file_path(work_dir: &std::path::Path) -> std::path::PathBuf {
    work_dir.join("AGENTS.md")
}

/// Check if content contains a blurb (current, older versioned, or legacy).
/// Any `bv-agent-instructions-vN` marker counts — version mismatch is handled
/// separately by `get_blurb_version` + `needs_upgrade`, so old blurbs still
/// trigger the upgrade path instead of a duplicate append.
pub fn contains_any_blurb(content: &str) -> bool {
    contains_legacy_blurb(content)
        || content.contains(BLURB_START_MARKER)
        || content.contains("bv-agent-instructions-v")
}

/// Check if content contains a legacy blurb (pre-v4).
/// Requires ALL patterns: the v3 blurb has "### Using bvr as an AI sidecar"
/// but NOT "bvr already computes the hard parts" — that's the key
/// differentiator that makes the check reliable. Any v3 blurb (with `bv`
/// commands instead of `bvr`) is now treated as outdated: `needs_upgrade`
/// fires on version mismatch, and `update_blurb` strips the old section by
/// its markers.
pub fn contains_legacy_blurb(content: &str) -> bool {
    let patterns = [
        "### Using bvr as an AI sidecar",
        "--robot-insights",
        "--robot-plan",
        "bvr already computes the hard parts",
    ];
    let matches = patterns.iter().filter(|p| content.contains(*p)).count();
    matches == patterns.len()
}

/// Get the blurb version from content. Returns 0 if none found.
/// The marker format is `<!-- bv-agent-instructions-vN -->` — extract the
/// number after `v`.
pub fn get_blurb_version(content: &str) -> i32 {
    // Match pattern: bv-agent-instructions-vN
    if let Some(pos) = content.find("bv-agent-instructions-v") {
        let rest = &content[pos + "bv-agent-instructions-v".len()..];
        if let Some(end) = rest.find("-->") {
            let version_str = &rest[..end].trim();
            return version_str.parse().unwrap_or(0);
        }
    }
    0
}

/// Replace legacy blurb with current version.
/// Strips ANY versioned marker section (`bv-agent-instructions-vN`), not just
/// the current one, so older blurbs (e.g. v3) upgrade cleanly to v4.
pub fn update_blurb(content: &str) -> String {
    // Remove everything that looks like a blurb — any versioned markers plus
    // legacy content. The legacy blurb contains patterns not in v3+:
    // "bvr already computes the hard parts" is the key differentiator.
    let mut cleaned = content.to_string();

    // Remove any marker-wrapped section (any version, not just current).
    if let Some(start) = cleaned.find("bv-agent-instructions-v") {
        // Rewind to the opening "<!--" of the start marker.
        let section_start = cleaned[..start].rfind("<!--").unwrap_or(start);
        if let Some(end) = cleaned[start..].find(BLURB_END_MARKER) {
            let end_pos = start + end + BLURB_END_MARKER.len();
            let before = cleaned[..section_start].trim_end().to_string();
            let after = if end_pos < cleaned.len() {
                cleaned[end_pos..].trim_start().to_string()
            } else {
                String::new()
            };
            cleaned = if after.is_empty() {
                before
            } else {
                format!(
                    "{before}

{after}"
                )
            };
        }
    }

    // Remove any remaining legacy blurb content
    // (the "## Beads Workflow Integration" section before the markers)
    if let Some(legacy_start) = cleaned.find("## Beads Workflow Integration") {
        let mut legacy_end = cleaned.len();
        let search_from = legacy_start + 25;
        for line in cleaned[search_from..].lines() {
            if line.starts_with("## ") && !line.starts_with("### ") {
                if let Some(pos) = cleaned[search_from..].find(line) {
                    legacy_end = search_from + pos;
                }
                break;
            }
        }
        let before = cleaned[..legacy_start].trim_end().to_string();
        let after = if legacy_end < cleaned.len() {
            cleaned[legacy_end..].trim_start().to_string()
        } else {
            String::new()
        };
        cleaned = if after.is_empty() {
            before
        } else {
            format!(
                "{before}

{after}"
            )
        };
    }

    append_blurb(&cleaned)
}

/// Remove blurb from content (both legacy and current markers).
/// Order: remove marker-wrapped content first, then any legacy remnant.
pub fn remove_blurb(content: &str) -> String {
    // Step 1: Remove marker-wrapped content.
    if let Some(start) = content.find(BLURB_START_MARKER) {
        if let Some(end) = content[start..].find(BLURB_END_MARKER) {
            let end_pos = start + end + BLURB_END_MARKER.len();
            // `trim_end`/`trim_start` ate the file's trailing newline, so
            // removing the blurb from a one-line file left it without one,
            // where Go's rewrite keeps the original bytes either side.
            let before = content[..start].trim_end().to_string();
            let after = if end_pos < content.len() {
                content[end_pos..].trim_start().to_string()
            } else {
                String::new()
            };
            let mut result = before;
            if !after.is_empty() {
                result.push_str("\n\n");
                result.push_str(&after);
            }
            // Go keeps the file's trailing newline. Returning the trimmed
            // prefix alone left a one-line file without one, so
            // `--agents-remove` produced a file a byte short of the oracle's.
            if !result.ends_with('\n') {
                result.push('\n');
            }
            return result;
        }
    }

    // Step 2: Remove legacy blurb that's NOT between markers.
    if let Some(legacy_start) = content.find("## Beads Workflow Integration") {
        let mut legacy_end = content.len();
        let search_from = legacy_start + 25;
        for line in content[search_from..].lines() {
            if line.starts_with("## ") && !line.starts_with("### ") {
                if let Some(pos) = content[search_from..].find(line) {
                    legacy_end = search_from + pos;
                }
                break;
            }
        }
        let before = content[..legacy_start].trim_end().to_string();
        let after = if legacy_end < content.len() {
            content[legacy_end..].trim_start().to_string()
        } else {
            String::new()
        };
        if after.is_empty() {
            return before;
        }
        return format!("{before}\n\n{after}");
    }

    content.to_string()
}

/// Append blurb to content.
pub fn append_blurb(content: &str) -> String {
    let mut result = content.trim_end().to_string();
    result.push_str("\n\n");
    result.push_str(AGENT_BLURB);
    result.push('\n');
    result
}

/// Verify blurb is present in content.
pub fn verify_blurb_present(content: &str) -> bool {
    content.contains(BLURB_START_MARKER) && content.contains(BLURB_END_MARKER)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blurb_markers_present() {
        assert!(AGENT_BLURB.contains(BLURB_START_MARKER));
        assert!(AGENT_BLURB.contains(BLURB_END_MARKER));
    }

    #[test]
    fn version_is_7() {
        assert_eq!(BLURB_VERSION, 7);
    }

    #[test]
    fn append_and_verify() {
        let content = "# My Project\n\nSome text here.";
        let result = append_blurb(content);
        assert!(verify_blurb_present(&result));
        assert!(result.starts_with("# My Project"));
    }

    #[test]
    fn remove_blurb_cleans_up() {
        let content = "# Project\n\n";
        let with_blurb = append_blurb(content);
        let cleaned = remove_blurb(&with_blurb);
        assert!(!cleaned.contains(BLURB_START_MARKER));
        assert!(cleaned.contains("# Project"));
    }

    #[test]
    fn get_blurb_version_extracts_number() {
        let content = format!("{BLURB_START_MARKER}\nsome content\n{BLURB_END_MARKER}");
        assert_eq!(get_blurb_version(&content), BLURB_VERSION);
    }

    #[test]
    fn an_older_blurb_is_detected_and_upgraded() {
        // An older blurb must be detected as outdated so the TUI offers an
        // upgrade and `update_blurb` replaces it with the current one.
        let v3 = "<!-- bv-agent-instructions-v3 -->\nbv --robot-triage\n<!-- end-bv-agent-instructions -->";
        assert!(contains_any_blurb(v3));
        assert_eq!(get_blurb_version(v3), 3);
        assert!(get_blurb_version(v3) < BLURB_VERSION);
        let upgraded = update_blurb(v3);
        assert!(verify_blurb_present(&upgraded));
        assert!(upgraded.contains("bvr --robot-triage"));
        assert!(!upgraded.contains("bv-agent-instructions-v3"));
    }

    #[test]
    fn supported_files_includes_common() {
        assert!(SUPPORTED_AGENT_FILES.contains(&"AGENTS.md"));
        assert!(SUPPORTED_AGENT_FILES.contains(&"CLAUDE.md"));
    }
}
