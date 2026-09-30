//! bv-tui: terminal UI for beads — ratatui-based, Elm-inspired event loop.
//! Phase 6 TUI-M1 slice: core journey (open→load→list→detail→filter→quit).

use crossterm::event::{self, Event as CEvent, KeyCode, KeyEventKind};
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    Frame,
};
use std::collections::BTreeMap;
use std::io;

/// Issue status (re-exported for view rendering).
use bv_core::model::Status;

/// Filter mode (Go: o/c/r/a keys).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterMode {
    All,
    Open,
    Closed,
    Ready,
}

impl FilterMode {
    pub fn label(self) -> &'static str {
        match self {
            FilterMode::All => "all",
            FilterMode::Open => "open",
            FilterMode::Closed => "closed",
            FilterMode::Ready => "ready",
        }
    }
}

/// Sort mode (5 modes cycling with `s`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortMode {
    Default,
    CreatedAsc,
    CreatedDesc,
    Priority,
    Updated,
}

impl SortMode {
    pub fn label(self) -> &'static str {
        match self {
            SortMode::Default => "Default",
            SortMode::CreatedAsc => "Created \u{2191}",
            SortMode::CreatedDesc => "Created \u{2193}",
            SortMode::Priority => "Priority",
            SortMode::Updated => "Updated",
        }
    }

    pub fn next(self) -> Self {
        match self {
            SortMode::Default => SortMode::CreatedAsc,
            SortMode::CreatedAsc => SortMode::CreatedDesc,
            SortMode::CreatedDesc => SortMode::Priority,
            SortMode::Priority => SortMode::Updated,
            SortMode::Updated => SortMode::Default,
        }
    }
}

/// Diff badge state for a row in Time-Travel mode.
///
/// Port of Go `IssueItem.DiffStatus` + `DiffStatus.Badge()` (new / closed /
/// modified). Populated from `time_travel_result` at render time, exactly
/// like Go's `getDiffStatus()` lookup over `newIssueIDs` /
/// `closedIssueIDs` / `modifiedIssueIDs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiffBadge {
    #[default]
    None,
    /// In current set but not at the diff ref (Go `DiffStatusNew`).
    Added,
    /// Closed since the diff ref (Go `DiffStatusClosed`).
    Closed,
    /// Present at ref but changed (Go `DiffStatusModified`).
    Modified,
}

impl DiffBadge {
    /// Go `DiffStatus.Badge()` exact strings.
    pub fn badge(self) -> &'static str {
        match self {
            DiffBadge::None => "",
            DiffBadge::Added => "🆕",
            DiffBadge::Closed => "✅",
            DiffBadge::Modified => "~",
        }
    }
}

/// A display row in the list (one per visible issue).
pub struct ListRow {
    pub id: String,
    pub title: String,
    pub status: Status,
    pub priority: i32,
    pub issue_type: String,
    pub labels: Vec<String>,
    pub created_at: Option<String>,
    pub description: String,
    pub notes: String,
    pub assignee: String,
    /// Workspace-mode repo key (Go `IssueItem.RepoPrefix`): derived from
    /// `source_repo`, falling back to the issue-ID prefix via the same rules
    /// as Go `normalizeRepoKey` (trim, strip trailing `-:_`, lowercase,
    /// reject empty/`.`/path-separator-containing). Rendered as `[API]`-style
    /// badge only when workspace mode is active. `None` = no badge.
    pub repo_prefix: Option<String>,
}

/// Go `normalizeRepoKey` (`pkg/ui/workspace_repos.go`): repo key from a raw
/// string. Returns `""` (here: `None`) when the input can't be a key.
pub fn normalize_repo_key(raw: &str) -> Option<String> {
    let key = raw.trim().trim_end_matches(['-', ':', '_']).to_lowercase();
    if key.is_empty() || key == "." || key.contains(['/', '\\']) {
        return None;
    }
    Some(key)
}

/// Go `issueRepoKey` (`pkg/ui/workspace_repos.go`): repo key for an issue —
/// `source_repo` first, then the issue-ID prefix via Go `ExtractRepoPrefix`
/// (first `-`/`:`/`_` separator, prefix part ≤ 10 ASCII-alphanumeric chars).
pub fn issue_repo_key(id: &str, source_repo: &str) -> Option<String> {
    if let Some(key) = normalize_repo_key(source_repo) {
        return Some(key);
    }
    for sep in ['-', ':', '_'] {
        if let Some(idx) = id.find(sep) {
            if idx > 0 {
                let prefix = &id[..idx];
                if prefix.len() <= 10
                    && !prefix.is_empty()
                    && prefix.chars().all(|c| c.is_ascii_alphanumeric())
                {
                    return Some(prefix.to_lowercase());
                }
            }
            break;
        }
    }
    None
}

/// Application state (Elm model).
/// Stores computed graph metrics for rendering views.
#[derive(Default, Clone)]
pub struct GraphMetrics {
    pub pagerank: BTreeMap<String, f64>,
    pub betweenness: BTreeMap<String, f64>,
    pub eigenvector: BTreeMap<String, f64>,
    pub hubs: BTreeMap<String, f64>,
    pub authorities: BTreeMap<String, f64>,
}

/// Drilldown overlay state: filtered issue list for one label (Go `d`
/// drilldown overlay — filterable, `Enter` jumps to List filtered by it).
pub struct LabelDrilldown {
    pub label: String,
    pub filter: String,
    pub cursor: usize,
}

/// One issue's semantic-search document, in the two forms semantic search
/// reads it.
///
/// Both are pure functions of the issue, so both are built once per `App`
/// rather than once per keystroke. See [`App::semantic_docs`].
#[derive(Clone)]
pub struct SemanticDoc {
    /// Exactly what `bv_search::query::issue_document` returns: the text the
    /// vector index content-hashes and embeds.
    pub text: String,
    /// The same text pre-lowercased, for the literal-match boost.
    pub lexical: bv_search::query::LexicalMatchDoc,
}

impl App {
    /// The per-issue semantic documents, built on first use.
    ///
    /// `OnceCell::get_or_init` keeps the "build once per `App`" guarantee
    /// while leaving `App::new` free of the 15 ms lowercasing pass, and works
    /// through `&self` for `fresh_text_scores`.
    pub fn semantic_docs(&self) -> &std::collections::HashMap<String, SemanticDoc> {
        self.semantic_docs.get_or_init(|| {
            self.issue_map
                .iter()
                .map(|(id, issue)| (id.clone(), SemanticDoc::new(issue)))
                .collect()
        })
    }
}

impl SemanticDoc {
    fn new(issue: &bv_core::model::Issue) -> Self {
        let text = bv_search::query::issue_document(
            &issue.id,
            &issue.title,
            &issue.labels,
            &issue.description,
        );
        Self {
            lexical: bv_search::query::LexicalMatchDoc::new(&text),
            text,
        }
    }
}

pub struct App {
    pub rows: Vec<ListRow>,
    /// Full issue data keyed by ID (for detail pane rendering)
    pub issue_map: std::collections::HashMap<String, bv_core::model::Issue>,
    pub filtered_indices: Vec<usize>,
    pub cursor: usize,
    pub filter_mode: FilterMode,
    pub sort_mode: SortMode,
    pub show_detail: bool,
    pub quit_requested: bool,
    pub width: u16,
    pub height: u16,
    /// True when terminal width > 100 (split view threshold).
    pub split_view: bool,
    pub status_msg: String,
    /// Current active view (list is default, toggled by b/E/G/i/etc).
    pub current_view: ViewMode,
    /// Search mode active (/ pressed).
    pub searching: bool,
    /// Current search query.
    pub search_query: String,
    /// Semantic search mode active (Ctrl+S pressed). Ranks by hybrid score
    /// (text cosine × preset weights × graph components — Go
    /// `hybridScorer.Score`) over the persistent `.bvvi` vector index when
    /// available, falling back to fresh hash-embedding when the index can't
    /// be loaded. Same engine family `--robot-search` uses.
    pub semantic_searching: bool,
    /// Current semantic query.
    pub semantic_query: String,
    /// Active hybrid preset for semantic search (Go `searchCfg` preset —
    /// Tab cycles; names match `--search-preset`: default / bug-hunting /
    /// sprint-planning / impact-first / text-only).
    pub semantic_preset: String,
    /// Persistent vector index backing semantic search (Go `VectorIndex`
    /// loaded from `<project>/.bv/semantic/index-hash-<dim>.bvvi`, synced
    /// incrementally by content hash). `None` until first Ctrl+S (lazy —
    /// same deferred-cost pattern as History/LabelDashboard), or when the
    /// beads dir can't be discovered (falls back to fresh embedding).
    pub semantic_index: Option<bv_search::vector_index::VectorIndex>,
    /// Data hash the semantic index was last synced against (staleness
    /// check — Go re-syncs when the issue set changes; we compare against
    /// the live `data_hash` the same way `watched_mtime` guards reloads).
    pub semantic_index_hash: Option<String>,
    /// Go `IssueDocument` text per issue, built once in [`App::new`].
    ///
    /// Go builds the same map once at model init (`pkg/ui/model.go:1863-1871`)
    /// and reads it per keystroke (`pkg/ui/semantic_search.go:475`), reusing
    /// the previous string for any issue the snapshot diff says is unchanged
    /// (`pkg/ui/snapshot.go:715-738`). Porting that lazy map was missed, so
    /// each character typed rebuilt the document, lowercased it and tokenized
    /// it into ~500k `String`s for every issue in the set.
    ///
    /// Valid for the life of the `App`: `issue_map` is only ever replaced
    /// wholesale by `App::new` (via `reload_from_disk`), never mutated in
    /// place, so this always describes exactly the data in `issue_map`.
    ///
    /// Populated on first Ctrl+S, not in `App::new`. Building it lowercases
    /// every document, which costs 15 ms and ~3.4 MB on the 171-issue
    /// ultraworkers dataset — real money for a session that never opens
    /// semantic search. `OnceCell` rather than `Option` because
    /// `fresh_text_scores` reads it through `&self`; the deferred-cost
    /// pattern is the same one `semantic_index` already documents.
    pub semantic_docs: std::cell::OnceCell<std::collections::HashMap<String, SemanticDoc>>,
    /// The slice of `filtered_indices` the list pane painted last frame.
    ///
    /// `handle_mouse` used to recompute this window from `self.height` while
    /// `render_list` computed it from the framebuffer's real height. Those
    /// two disagreed whenever the terminal was not exactly 120x40 — which is
    /// always, now that `handle_resize` works — so a click selected a
    /// different row than the one painted. One field, written by the
    /// renderer and read by the click handler: they cannot drift.
    pub list_window: std::ops::Range<usize>,
    /// `data_hash` of [`Self::issue_map`], computed on first use and kept.
    ///
    /// Same reasoning as `semantic_docs`: the input cannot change without a
    /// new `App`, so recomputing it per keystroke only ever re-hashed the
    /// same megabytes to reach the same answer.
    semantic_data_hash: Option<String>,
    /// Project directory the `.bvvi` index lives under, resolved once by
    /// [`Self::refresh_watch_target`]. `None` when datasource discovery
    /// failed, in which case semantic search stays index-less and falls back
    /// to fresh embedding.
    semantic_project_dir: Option<std::path::PathBuf>,
    pub show_sidebar: bool,
    /// Which panel has focus: false = list, true = detail
    pub focus_detail: bool,
    pub show_help: bool,
    pub label_filter: Option<String>,
    /// Scroll offset for the detail pane
    pub detail_scroll: u16,
    /// Graph metric maps from analysis (for insights rendering)
    pub graph_metrics: Option<GraphMetrics>,
    /// Active drift alerts (critical, warning, total) for footer badge
    pub alerts_critical: usize,
    pub alerts_warning: usize,
    pub alerts_total: usize,
    /// The actual alert messages backing the counts above, for the Alerts view.
    pub alerts: Vec<bv_analysis::drift::Alert>,
    /// Collapsed node ids in the Tree view (all nodes start expanded).
    pub tree_collapsed: std::collections::HashSet<String>,
    /// Cursor position within the Alerts view list.
    pub alerts_cursor: usize,
    /// Cross-label flow data for the FlowMatrix view.
    pub flow: Option<bv_analysis::label_health::CrossLabelFlow>,
    pub flow_cursor: usize,
    /// Attention scores for the Attention view.
    pub attention_labels: Vec<bv_analysis::label_health::LabelAttentionScore>,
    pub attention_cursor: usize,
    /// Graph view: selected issue index (sorted IDs).
    pub graph_cursor: usize,
    /// Graph view: scroll offset in the node list panel.
    pub graph_scroll: usize,
    /// Precomputed graph data for the Graph view.
    pub graph_data: Option<crate::views::graph::GraphData>,
    /// History/time-travel view state.
    pub history: Option<crate::views::history::HistoryState>,
    /// Label picker state.
    pub label_picker: Option<crate::views::pickers::LabelPicker>,
    /// Recipe picker state (built-in recipes only — see `default_recipes`).
    pub recipe_picker: Option<crate::views::pickers::RecipePicker>,
    /// Workspace repo picker state (only meaningful when `workspace_repos`
    /// is `Some`).
    pub repo_picker: Option<crate::views::pickers::RepoPicker>,
    /// True once `history` has been populated with real correlation data
    /// (lazy-loaded on first `h` press — Go bv-h305-style deferred cost).
    pub history_loaded: bool,
    /// Go `m.historyLoading` (model.go:9226-9245): a background
    /// `build_history_report` walk is in flight. The worker publishes onto
    /// `history_rx`; `poll_history_load` drains it on the run loop's tick.
    pub history_loading: bool,
    /// Go `m.historyLoadFailed` — selects the
    /// `"History unavailable; press h to retry"` placeholder
    /// (model.go:6357-6362) over `"Loading history…"`.
    pub history_load_failed: bool,
    /// Go's `(dataGeneration, requestGeneration)` pair (model.go:595-671).
    /// A completion carrying a generation other than the newest is dropped,
    /// so a slow walk can never overwrite a fresher dataset.
    pub history_generation: u64,
    /// Receiver half of the background history load.
    pub history_rx: Option<
        crossbeam_channel::Receiver<(u64, Result<bv_correlation::history::HistoryReport, String>)>,
    >,
    /// Revision-input buffer for Time-Travel (Go `focusTimeTravelInput`).
    /// `Some` = prompt open and capturing keys, `None` = closed.
    pub time_travel_prompt: Option<String>,
    /// Last computed revision diff (Go `SnapshotDiff`), via
    /// `bv_analysis::diff::diff_issues` over `GitLoader::load_at` output
    /// (the same plumbing `--robot-diff` uses — see plan Q4).
    pub time_travel_result: Option<bv_analysis::diff::DiffResult>,
    /// Error from the last Time-Travel diff attempt (bad ref, not a git
    /// repo). Shown in the view; mirrors the robot command's message.
    pub time_travel_error: Option<String>,
    /// Cursor over the flattened Time-Travel diff entry list
    /// (added, then removed, then changed).
    pub time_travel_cursor: usize,
    /// Per-label health dashboard (Go `focusLabelDashboard`), lazy-loaded
    /// on first `[` press like History (deferred analysis cost).
    pub label_health: Option<bv_analysis::label_health::LabelAnalysisResult>,
    pub label_health_loaded: bool,
    pub label_dashboard_cursor: usize,
    /// Detail modal for the cursor label (Go `h` health-detail modal).
    pub show_label_detail: bool,
    /// Drilldown overlay (Go `d`): filtered issue list for one label.
    pub label_drilldown: Option<LabelDrilldown>,
    /// Update-available modal visibility (Go `U` key / auto-show-once).
    pub show_update_modal: bool,
    /// True once the update modal has been auto-shown once this session,
    /// so it doesn't reappear every frame after the user dismisses it.
    pub update_modal_auto_shown: bool,
    /// Priority hints visibility (Go `p` key — `pkg/ui/model.go`
    /// `showPriorityHints`). Renders per-row up/down suggestion arrows in
    /// the list when true.
    pub show_priority_hints: bool,
    /// Lazily computed suggested-priority map (issue id -> suggested P),
    /// built once from `bv_analysis::impact::compute_impact_scores` (the
    /// same engine `--robot-priority` uses — Go `ComputeImpactScores` +
    /// `scoreToPriority`). `None` until first computed. Only entries where
    /// suggested != current are kept (that's the misalignment set).
    pub priority_hints: Option<std::collections::BTreeMap<String, i32>>,
    /// Live-reload watch target: the resolved `.beads/*.jsonl` path (Go
    /// watches this via `fsnotify`; bvr polls its mtime on the existing
    /// ~500ms terminal-event-poll tick instead of adding a background
    /// `notify` watcher thread — same user-visible effect, fewer moving
    /// parts. `None` if discovery failed (watching is then a no-op, not a
    /// startup error).
    pub watched_path: Option<std::path::PathBuf>,
    pub watched_mtime: Option<std::time::SystemTime>,
    /// Sprint dashboard state (loaded from .beads/sprints.jsonl).
    pub sprint: Option<crate::views::sprint::SprintState>,
    /// When the snapshot was loaded (freshness badge, Go bv-h305)
    pub loaded_at: std::time::Instant,
    /// PID of another live instance holding .beads/.bv.lock (Go bv-vrvn)
    pub instance_pid: Option<u32>,
    /// Large/huge dataset warning text (Go bv-9thm)
    pub dataset_warning: Option<String>,
    /// cass session count for selected bead (Go bv-y836)
    pub session_count: usize,
    pub update_tag: Option<String>,
    /// Release page for `update_tag` (Go `Model.updateURL`,
    /// `model.go:783`), carried from `UpdateInfo.release_url`. It is the link
    /// target of the "⭐ **Update Available:** [tag](url)" line the detail
    /// pane opens with (`model.go:8894-8895`).
    pub update_url: Option<String>,
    /// The self-update state machine (Go `Model.updateModal`,
    /// `model.go:957`). `None` until `U` or the once-per-session auto-notify
    /// opens it; retained after a close, exactly as Go keeps the value while
    /// only clearing `showUpdateModal`.
    pub update_modal: Option<crate::update_modal::UpdateModal>,
    /// Workspace mode: loaded repo names (Go workspaceMode)
    pub workspace_repos: Option<Vec<String>>,
    /// Active repo filter in workspace mode (None = all repos)
    pub active_repo: Option<String>,
    /// Per-row triage decorations (Go `IssueItem.IsQuickWin/IsBlocker/`
    /// `UnblocksCount` — from `triageResult.QuickWins/BlockersToClear/`
    /// `unblocksMap`). Computed once at load via the shared
    /// `bv_analysis::triage::compute_row_triage` (same formulas as
    /// `--robot-triage`'s builders) and read at render time. Empty until
    /// `App::new` fills it — render treats a missing entry as no decoration.
    pub row_triage: std::collections::HashMap<String, bv_analysis::triage::RowTriage>,
    /// Quit-confirmation overlay (Go `showQuitConfirm` / `focusQuitConfirm`):
    /// at the main list, first Esc clears filters, second Esc shows this
    /// confirm box; Esc/Y quits, any other key cancels back to the list.
    /// `q` keeps its immediate-quit behavior (Go `quitCommand`), matching Go
    /// where only the Esc path goes through the confirm step.
    pub show_quit_confirm: bool,
    /// Pending single-tap backtick timestamp (Go `CapsLockTracker.pending`):
    /// first backtick press records `Instant::now()` and waits up to 300ms
    /// for a second press. A second press within the window opens context
    /// help for the current view (Go `TriggerContextHelp`); if the window
    /// expires with no second press, the full Tutorial opens instead (Go
    /// `CapsLockTimerExpiredMsg` → `TriggerFullTutorial`). `None` = no tap
    /// pending. Checked at the top of `handle_key` (see `resolve_pending_tap`)
    /// so no background timer thread is needed — worst case the Tutorial
    /// opens on the *next* keypress after the window expires, documented in
    /// the method comment.
    pub tutorial_tap_pending: Option<std::time::Instant>,
    /// Context-help overlay visibility (Go `~` key / double-tap backtick →
    /// context help for the current view). Rendered from the existing
    /// `context_help::render_context_help` over `focus_for_view`; any key
    /// dismisses.
    pub show_context_help: bool,
    /// Cass session modal (Go `showCassModal` / `focusCassModal` /
    /// `CassSessionModal`): correlated coding sessions for the selected
    /// bead. `None` = closed. Opened with `V` (Go's binding, free in Rust:
    /// lowercase `v` is view-scoped to History/Sprint), dismissed with
    /// `V`/Esc/Enter/q (Go's exact dismiss set).
    pub cass_modal: Option<crate::cass::CassModalState>,
    /// cass CLI availability cache
    cass_available: bool,
    cass_cache: std::collections::HashMap<String, usize>,
    /// Actionable items state (lazy-built on first `F`, like History).
    pub actionable: Option<crate::actionable::ActionableState>,
    /// Tutorial state.
    pub tutorial: Option<crate::tutorial::TutorialState>,
    /// Board swimlane grouping (Go `s` cycle: Status → Priority → Type).
    pub board_mode: crate::views::board::SwimlaneMode,
    /// Board selected column (Go `h`/`l` move, `1`-`9` jump).
    pub board_column: usize,
    /// Agent blurb prompt visibility (Go auto-shows once at startup when the
    /// detected agent file needs the blurb; no manual keybinding).
    pub show_agent_prompt: bool,
    /// Pending blurb prompt state (Go `AgentPromptModal`).
    pub agent_prompt_modal: Option<crate::agent_prompt_modal::AgentPromptModal>,
    /// Velocity-comparison overlay inside Sprint view (Go `v` sub-toggle).
    pub show_velocity: bool,
    /// Theme for consistent styling. Resolved from `--theme` > `BV_THEME` >
    /// `~/.config/bv/config.yaml` > auto — call
    /// [`App::apply_theme_preference`] once after [`App::new`] to install it.
    pub theme: crate::theme::Theme,
    /// Key registry for help display.
    pub key_registry: crate::keybindings::KeyRegistry,
}

/// Which view is currently displayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    List,
    Board,
    Tree,
    Graph,
    /// Per-label health dashboard (Go `focusLabelDashboard`, `[` key).
    /// Badges computed by `bv_analysis::label_health` (same module backing
    /// Attention/FlowMatrix). See TUI_UX_PARITY_PLAN.md Phase C.
    LabelDashboard,
    FlowMatrix,
    Attention,
    Insights,
    Alerts,
    /// Bead↔commit correlation view (Go `pkg/ui/history.go`). Was
    /// previously aliased under `TimeTravel` — split out because Go treats
    /// History and Time-Travel as two distinct features with distinct keys
    /// (`h` vs `t`/`T`). Note that inside History `t` is *not* the global
    /// Time-Travel key: Go scopes it to the timeline-pane toggle
    /// (model.go:5680-5693) and the `handle_key` arms are ordered so that
    /// guard wins.
    History,
    /// Revision-diff mode (Go `focusTimeTravelInput` → `SnapshotDiff`).
    /// `t` opens the revision prompt, `T` diffs instantly vs HEAD~5;
    /// Enter submits. See TUI_UX_PARITY_PLAN.md Phase B.
    TimeTravel,
    Sprint,
    Tutorial,
    Actionable,
}

/// A built-in recipe definition (name + description shown in the picker).
/// Matches Go's embedded `defaults/recipes.yaml` names/descriptions exactly;
/// user/project YAML recipe loading is not ported (see `open_recipe_picker`
/// doc comment).
pub struct RecipeDef {
    pub name: &'static str,
    pub description: &'static str,
}

/// The 6 recipes Go ships as embedded defaults (`recipe.Loader::loadBuiltin`).
pub fn default_recipe_defs() -> &'static [RecipeDef] {
    &[
        RecipeDef {
            name: "triage",
            description: "Open/blocked issues, sorted by priority",
        },
        RecipeDef {
            name: "release-cut",
            description: "Recently closed issues for changelog",
        },
        RecipeDef {
            name: "blocked-review",
            description: "All blocked items with blocker info",
        },
        RecipeDef {
            name: "dependency-risk",
            description: "High betweenness/PageRank items",
        },
        RecipeDef {
            name: "quick-wins",
            description: "Low-priority, no-dependency items",
        },
        RecipeDef {
            name: "stale",
            description: "Items not updated in 14+ days",
        },
    ]
}

/// Large/huge dataset warning (Go largeDatasetWarning, bv-9thm).
fn dataset_warning_for(total: usize) -> Option<String> {
    let compact = |n: usize| {
        if n >= 1_000_000 {
            format!("{}m", n / 1_000_000)
        } else if n >= 1_000 {
            format!("{}k", n / 1_000)
        } else {
            format!("{n}")
        }
    };
    if total >= 20_000 {
        Some(format!("\u{26a0} huge {} issues", compact(total)))
    } else if total >= 5_000 {
        Some(format!("\u{26a0} large {} issues", compact(total)))
    } else {
        None
    }
}

/// Check whether the cass CLI is on PATH (Go cass.Detector).
fn cass_installed() -> bool {
    std::env::var("PATH")
        .map(|paths| {
            paths
                .split(':')
                .any(|dir| std::path::Path::new(dir).join("cass").is_file())
        })
        .unwrap_or(false)
}

/// Acquire the instance lock in .beads/.bv.lock (Go instance.NewLock, bv-vrvn).
/// Returns PID of another live instance if one holds the lock.
/// Best-effort cross-platform liveness check for a PID.
/// Unix: `kill -0 <pid>` (signal 0 — no-op, only checks permission/existence).
/// Windows: `kill` doesn't exist as a binary, so shell out to `tasklist` and
/// check whether the PID shows up in its filtered output.
#[cfg(unix)]
fn is_process_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(windows)]
fn is_process_alive(pid: u32) -> bool {
    std::process::Command::new("tasklist")
        .args(["/NH", "/FO", "CSV", "/FI", &format!("PID eq {pid}")])
        .output()
        .map(|out| {
            out.status.success()
                && String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
        })
        .unwrap_or(false)
}

#[cfg(not(any(unix, windows)))]
fn is_process_alive(_pid: u32) -> bool {
    // Unknown platform: assume dead so stale locks get reclaimed rather than
    // wedging the TUI forever.
    false
}

pub fn acquire_instance_lock(beads_dir: &std::path::Path) -> Option<u32> {
    let lock_path = beads_dir.join(".bv.lock");
    let my_pid = std::process::id();
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
    {
        Ok(mut f) => {
            use std::io::Write;
            let _ = writeln!(f, "{{\"pid\":{my_pid}}}");
            None // we are the first instance
        }
        Err(_) => {
            // Lock exists — read holder PID and check if alive.
            let holder = std::fs::read_to_string(&lock_path).ok().and_then(|s| {
                s.trim()
                    .trim_start_matches('{')
                    .trim_end_matches('}')
                    .split(':')
                    .nth(1)
                    .and_then(|p| p.trim().parse::<u32>().ok())
            });
            // If we already hold the lock (re-entrant acquire, or a stale write
            // from a previous run of this same process), it's trivially "alive".
            let alive = holder
                .map(|pid| pid == my_pid || is_process_alive(pid))
                .unwrap_or(false);
            if alive {
                holder
            } else {
                // Stale lock — take over.
                if let Ok(mut f) = std::fs::File::create(&lock_path) {
                    use std::io::Write;
                    let _ = writeln!(f, "{{\"pid\":{my_pid}}}");
                }
                None
            }
        }
    }
}

/// Remove the instance lock if we hold it.
pub fn release_instance_lock(beads_dir: &std::path::Path) {
    let lock_path = beads_dir.join(".bv.lock");
    if let Ok(content) = std::fs::read_to_string(&lock_path) {
        let content = content.trim();
        let mine = content
            .trim_start_matches('{')
            .trim_end_matches('}')
            .split(':')
            .nth(1)
            .and_then(|p| p.trim().parse::<u32>().ok())
            == Some(std::process::id());
        if mine {
            let _ = std::fs::remove_file(&lock_path);
        }
    }
}

/// Copy text to the system clipboard (Go copyIssueToClipboard).
/// Shared by issue-copy (`C`) and commit-SHA-copy (`y`, History view).
///
/// Primary path is `arboard` (cross-platform: Win32 on Windows, X11/Wayland
/// on Linux, pbcopy on macOS). When that fails — e.g. headless/SSH sessions
/// with no display server — fall back to shelling out to `pbcopy`/`wl-copy`/
/// `xclip`, preserving the pre-arboard behavior instead of hard-failing.
fn copy_to_clipboard(text: &str) -> Result<(), String> {
    if let Ok(mut cb) = arboard::Clipboard::new() {
        if cb.set_text(text.to_string()).is_ok() {
            return Ok(());
        }
    }
    shell_copy_fallback(text)
}

/// Go `correlation.CorrelatorOptions{Limit: 500}` for the TUI
/// (model.go:659-661, "Reasonable limit for TUI performance"). This is the
/// same cap `bv_correlation::history` applies as its own default when
/// `Limit == 0`, so it is named here only to make the Go constant explicit.
const HISTORY_COMMIT_LIMIT: i64 = 500;

/// Go `openBrowserURL` (model.go:5846-5864). Honours the same
/// `BV_NO_BROWSER` / `BV_TEST_MODE` suppression Go uses, so a test run never
/// launches a browser.
fn open_browser_url(url: &str) -> Result<(), String> {
    if std::env::var("BV_NO_BROWSER").is_ok_and(|v| !v.is_empty())
        || std::env::var("BV_TEST_MODE").is_ok_and(|v| !v.is_empty())
    {
        return Ok(());
    }
    let (cmd, args): (&str, &[&str]) = if cfg!(target_os = "macos") {
        ("open", &[url])
    } else if cfg!(target_os = "windows") {
        ("rundll32", &["url.dll,FileProtocolHandler", url])
    } else {
        ("xdg-open", &[url])
    };
    std::process::Command::new(cmd)
        .args(args)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("{e}"))
}

/// Go `gitRemoteToWebURL` (model.go:5794-5843) — SSH and HTTPS(S) remotes to
/// a browsable `https://host/owner/repo` URL, with `.git` and any query or
/// fragment stripped.
fn git_remote_to_web_url(remote: &str) -> Option<String> {
    let remote = remote.trim();
    if remote.is_empty() {
        return None;
    }
    if let Some(rest) = remote.strip_prefix("git@") {
        let rest = rest.strip_suffix(".git").unwrap_or(rest);
        let rest = rest.replacen(':', "/", 1);
        return normalize_git_remote_web_url(&format!("https://{rest}"));
    }
    if remote.starts_with("https://")
        || remote.starts_with("http://")
        || remote.starts_with("ssh://")
    {
        return normalize_git_remote_web_url(remote);
    }
    None
}

/// Go `normalizeGitRemoteWebURL` (model.go:5819-5843).
fn normalize_git_remote_web_url(remote: &str) -> Option<String> {
    let (scheme, rest) = remote.split_once("://")?;
    let scheme = match scheme {
        "https" | "http" => scheme,
        // `ssh://[user@]host/owner/repo` → `https://host/owner/repo`.
        "ssh" => "https",
        _ => return None,
    };
    // ssh://git@host/... — drop the userinfo.
    let authority = rest.split('/').next()?;
    let host = match authority.rsplit_once('@') {
        Some((user, host)) if scheme == "https" && !user.is_empty() => host,
        _ => authority,
    };
    if host.is_empty() {
        return None;
    }
    // Strip the query/fragment, then the trailing `/` and the `.git` suffix.
    let path = rest
        .split_once('/')
        .map(|(_, p)| p)
        .unwrap_or("")
        .split(['?', '#'])
        .next()
        .unwrap_or("");
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    if path.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{host}/{path}"))
}

/// Legacy shell-out clipboard path, kept as a headless fallback for
/// `copy_to_clipboard` when `arboard` has no display to talk to.
fn shell_copy_fallback(text: &str) -> Result<(), String> {
    let (cmd, args): (&str, &[&str]) = if cfg!(target_os = "macos") {
        ("pbcopy", &[])
    } else if std::env::var("WAYLAND_DISPLAY").is_ok() {
        ("wl-copy", &[])
    } else {
        ("xclip", &["-selection", "clipboard"])
    };
    let mut child = std::process::Command::new(cmd)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    {
        use std::io::Write;
        if let Some(stdin) = child.stdin.as_mut() {
            stdin
                .write_all(text.as_bytes())
                .map_err(|e| e.to_string())?;
        }
    }
    child.wait().map_err(|e| e.to_string())?;
    Ok(())
}
/// Detect the agent file for the repo being viewed (Go `CheckAgentFileCmd`):
/// cwd first (up to 3 parents up), then the repo root above `.beads/`
/// discovery when running from inside it.
fn detect_agent_file_for_tui(cwd: &std::path::Path) -> bv_core::agents::detect::AgentFileDetection {
    let detection = bv_core::agents::detect::detect_agent_file_in_parents(cwd, 3);
    if detection.found() {
        return detection;
    }
    bv_core::discovery::get_beads_dir(cwd)
        .ok()
        .and_then(|dir| dir.parent().map(bv_core::agents::detect::detect_agent_file))
        .unwrap_or_default()
}

impl App {
    /// Install the palette for an already-resolved theme preference. Mirrors
    /// Go's `ui.SetThemeOverride` call as the first statement of the root
    /// command (cmd/bv/main.go:1766): the preference is process-wide, so it
    /// only has to be set once, before any view renders.
    ///
    /// Note that the current views draw with hardcoded ANSI colors rather than
    /// reading `self.theme`, so this records the resolved palette without
    /// changing what is painted. See `crates/bv-tui/src/theme.rs`.
    pub fn apply_theme_preference(&mut self, pref: Option<crate::theme::ThemePref>) {
        self.theme = crate::theme::Theme::for_preference(pref);
    }

    pub fn new(issues: Vec<bv_core::model::Issue>) -> Self {
        let issue_map: std::collections::HashMap<String, bv_core::model::Issue> =
            issues.iter().map(|i| (i.id.clone(), i.clone())).collect();
        // Built lazily on first semantic search — see `semantic_docs`. `App::new`
        // has already moved `issues` into `issue_map` by here, so the closure
        // reads the map rather than the local slice.
        let semantic_docs: std::cell::OnceCell<std::collections::HashMap<String, SemanticDoc>> =
            std::cell::OnceCell::new();
        // Compute proactive alerts (Go computeAlerts): cycles-driven drift
        let g_alerts = bv_analysis::build_graph(&issues);
        let has_cycle = bv_graph_core::algorithms::cycles::has_cycles(&g_alerts);
        let mut alerts: Vec<bv_analysis::drift::Alert> = Vec::new();
        if has_cycle {
            alerts.push(bv_analysis::drift::Alert {
                alert_type: bv_analysis::drift::AlertType::NewCycle,
                severity: bv_analysis::drift::Severity::Critical,
                message: "Dependency cycle detected in the issue graph".into(),
                baseline_val: None,
                current_val: None,
                delta: None,
                ..Default::default()
            });
        }
        // Staleness check: open issues untouched for a long time (Go drift engine check)
        let now = jiff::Timestamp::now();
        for i in &issues {
            if matches!(
                i.status,
                bv_core::model::Status::Open
                    | bv_core::model::Status::InProgress
                    | bv_core::model::Status::Blocked
            ) {
                if let Some(updated) = &i.updated_at {
                    if let Ok(t) = updated.parse::<jiff::Timestamp>() {
                        let age = now.since(t).map(|d| d.get_days()).unwrap_or(0);
                        if age > 30 {
                            alerts.push(bv_analysis::drift::Alert {
                                alert_type: bv_analysis::drift::AlertType::BlockedIncrease,
                                severity: bv_analysis::drift::Severity::Warning,
                                message: format!(
                                    "{} has been stale for {age} days ({:?})",
                                    i.id, i.status
                                ),
                                baseline_val: None,
                                current_val: Some(age as f64),
                                delta: None,
                                ..Default::default()
                            });
                        }
                    }
                }
            }
        }
        let a_crit = alerts
            .iter()
            .filter(|a| a.severity == bv_analysis::drift::Severity::Critical)
            .count();
        let a_warn = alerts
            .iter()
            .filter(|a| a.severity == bv_analysis::drift::Severity::Warning)
            .count();
        let alerts_total = alerts.len();
        let row_triage = bv_analysis::triage::compute_row_triage(&issues);
        let rows: Vec<ListRow> = issues
            .iter()
            .map(|i| ListRow {
                id: i.id.clone(),
                title: i.title.clone(),
                status: i.status,
                priority: i.priority,
                issue_type: i.issue_type.clone(),
                labels: i.labels.clone(),
                created_at: i.created_at.clone(),
                description: i.description.clone(),
                notes: i.notes.clone(),
                assignee: i.assignee.clone(),
                repo_prefix: issue_repo_key(&i.id, &i.source_repo),
            })
            .collect();
        let mut app = App {
            rows,
            issue_map,
            row_triage,
            alerts_critical: a_crit,
            alerts_warning: a_warn,
            alerts_total,
            alerts,
            tree_collapsed: std::collections::HashSet::new(),
            alerts_cursor: 0,
            flow: None,
            flow_cursor: 0,
            attention_labels: Vec::new(),
            attention_cursor: 0,
            graph_cursor: 0,
            label_health: None,
            label_health_loaded: false,
            label_dashboard_cursor: 0,
            show_label_detail: false,
            label_drilldown: None,
            graph_scroll: 0,
            graph_data: None,
            history: None,
            label_picker: None,
            recipe_picker: None,
            repo_picker: None,
            history_loaded: false,
            history_loading: false,
            history_load_failed: false,
            history_generation: 0,
            history_rx: None,
            time_travel_prompt: None,
            time_travel_result: None,
            time_travel_error: None,
            time_travel_cursor: 0,
            show_update_modal: false,
            update_modal_auto_shown: false,
            show_priority_hints: false,
            priority_hints: None,
            watched_path: None,
            watched_mtime: None,
            sprint: None,
            filtered_indices: Vec::new(),
            cursor: 0,
            filter_mode: FilterMode::All,
            sort_mode: SortMode::Default,
            show_detail: false,
            quit_requested: false,
            width: 120,
            height: 40,
            split_view: true,
            status_msg: String::new(),
            current_view: ViewMode::List,
            searching: false,
            search_query: String::new(),
            semantic_searching: false,
            semantic_query: String::new(),
            semantic_preset: "default".to_string(),
            semantic_index: None,
            semantic_index_hash: None,
            semantic_docs,
            list_window: 0..0,
            semantic_data_hash: None,
            semantic_project_dir: None,
            show_sidebar: false,
            focus_detail: false,
            show_help: false,
            show_quit_confirm: false,
            tutorial_tap_pending: None,
            show_context_help: false,
            label_filter: None,
            detail_scroll: 0,
            graph_metrics: None,
            loaded_at: std::time::Instant::now(),
            instance_pid: None,
            dataset_warning: dataset_warning_for(issues.len()),
            session_count: 0,
            update_tag: None,
            update_url: None,
            update_modal: None,
            workspace_repos: None,
            active_repo: None,
            cass_modal: None,
            cass_available: cass_installed(),
            cass_cache: std::collections::HashMap::new(),
            actionable: None,
            tutorial: Some(crate::tutorial::TutorialState::new()),
            board_mode: crate::views::board::SwimlaneMode::Status,
            board_column: 0,
            // Auto-show happens in `tui_event_loop`, not here, so unit
            // tests (`App::new` directly) start with a clean slate.
            show_agent_prompt: false,
            agent_prompt_modal: None,
            show_velocity: false,
            theme: crate::theme::Theme::default(),
            key_registry: crate::keybindings::build_default_registry(),
        };
        // Pre-compute flow-matrix and attention data for the TUI views
        // (backed by the same data the robot-label-flow / robot-label-attention
        // commands use, so there's no separate code path — just computed once
        // at startup instead of on demand).
        let cfg = bv_analysis::label_health::LabelHealthConfig::default();
        let now = jiff::Timestamp::now();
        let flow = bv_analysis::label_health::compute_cross_label_flow(&issues, &cfg);
        app.flow = Some(flow);
        let attention =
            bv_analysis::label_health::compute_label_attention_scores(&issues, &cfg, now);
        app.attention_labels = attention.labels;
        // Build graph data for the Graph view (blocker/dependent maps).
        app.graph_data = Some(crate::views::graph::GraphData::build(
            app.issue_map.values().cloned().collect(),
            app.graph_metrics.clone(),
        ));
        // Initialize history view (empty for now — populated when user presses h).
        app.history = Some(crate::views::history::HistoryState::new());
        app.refresh_watch_target();
        app.apply_filter();
        app
    }

    /// (Re)discover the datasource: the `.beads/*.jsonl` path to watch for
    /// live reload (Go `fsnotify`, `p` — see `check_for_reload` below and
    /// TUI_UX_PARITY_PLAN.md Phase F), and the project directory the
    /// `.bvvi` semantic index lives under (Go `DefaultIndexPath`).
    ///
    /// One discovery pass feeds both, because it is not cheap:
    /// `get_beads_dir` shells out to `git rev-parse` up to three times when
    /// the working directory is not itself a Beads root, which measured at
    /// ~200 ms per call on Windows — and semantic search used to redo it on
    /// every keystroke. Nothing in the process changes the working directory
    /// or `BEADS_DIR`, so one pass per `App` is enough; `reload_from_disk`
    /// replaces the whole `App` and redoes it.
    ///
    /// Best-effort: if discovery fails (e.g. no `.beads` dir, or the active
    /// datasource is SQLite — out of scope here, matching Go's own
    /// `.beads/issues.jsonl`-specific watch target), watching is silently
    /// disabled rather than surfaced as a startup error.
    fn refresh_watch_target(&mut self) {
        let cwd = std::env::current_dir().unwrap_or_default();
        self.watched_path = None;
        self.semantic_project_dir = None;
        if let Ok(dir) = bv_core::discovery::get_beads_dir(&cwd) {
            self.semantic_project_dir = Some(dir.parent().unwrap_or(&cwd).to_path_buf());
            self.watched_path = bv_core::discovery::find_jsonl_path_with_warnings(&dir, |_| {})
                .ok()
                .flatten();
        }
        self.watched_mtime = self
            .watched_path
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .and_then(|m| m.modified().ok());
    }

    /// Poll the watched file's mtime and reload if it changed externally.
    /// Called from the terminal event loop's existing ~500ms poll tick
    /// (`tui_event_loop`) — no background thread, no `notify` watcher
    /// dependency; same user-visible effect ("editing issues.jsonl updates
    /// the TUI without a manual keypress") with fewer moving parts. A no-op
    /// when `watched_path` is `None` (discovery failed or unsupported
    /// datasource). Returns true if a reload happened, for the caller to
    /// avoid double-work in the same tick.
    pub fn check_for_reload(&mut self) -> bool {
        let Some(path) = &self.watched_path else {
            return false;
        };
        let Some(current_mtime) = std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
        else {
            return false; // file briefly missing mid-write — try again next tick
        };
        if self.watched_mtime == Some(current_mtime) {
            return false;
        }
        self.reload_from_disk();
        true
    }
    pub fn apply_filter(&mut self) {
        let label_filter = self.label_filter.clone();
        let active_repo = self.active_repo.clone();
        self.filtered_indices = (0..self.rows.len())
            .filter(|&i| {
                let r = &self.rows[i];
                let mode_ok = match self.filter_mode {
                    FilterMode::All => true,
                    FilterMode::Open => matches!(r.status, Status::Open | Status::InProgress),
                    FilterMode::Closed => r.status.is_closed(),
                    FilterMode::Ready => matches!(r.status, Status::Open),
                };
                mode_ok
                    && label_filter
                        .as_ref()
                        .is_none_or(|label| r.labels.iter().any(|l| l == label))
                    && active_repo.as_ref().is_none_or(|repo| {
                        self.issue_map
                            .get(&r.id)
                            .map(|i| &i.source_repo == repo)
                            .unwrap_or(false)
                    })
            })
            .collect();
        // Apply sort
        if self.sort_mode == SortMode::Default {
            self.filtered_indices.sort_by_key(|&i| {
                (
                    self.rows[i].priority,
                    std::cmp::Reverse(self.rows[i].id.clone()),
                )
            });
        }
        if self.cursor >= self.filtered_indices.len() {
            self.cursor = self.filtered_indices.len().saturating_sub(1);
        }
    }
    /// Update cass session count for the selected bead (Go getCassSessionCount).
    fn update_session_count(&mut self) {
        self.session_count = 0;
        if !self.cass_available {
            return;
        }
        let Some(row) = self.selected() else { return };
        let id = row.id.clone();
        if let Some(n) = self.cass_cache.get(&id) {
            self.session_count = *n;
            return;
        }
        if let Ok(out) = std::process::Command::new("cass")
            .args(["search", &id, "--robot", "--limit", "10"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output()
        {
            let count = serde_json::from_slice::<serde_json::Value>(&out.stdout)
                .ok()
                .and_then(|v| v.get("results").and_then(|r| r.as_array()).map(|a| a.len()))
                .unwrap_or(0);
            self.cass_cache.insert(id, count);
            self.session_count = count;
        }
    }

    fn handle_search_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Esc => {
                self.searching = false;
                self.search_query.clear();
                self.apply_filter();
                true
            }
            KeyCode::Enter => {
                self.searching = false;
                self.apply_filter();
                true
            }
            KeyCode::Backspace => {
                self.search_query.pop();
                self.apply_search();
                true
            }
            KeyCode::Char(c) => {
                self.search_query.push(c);
                self.apply_search();
                true
            }
            _ => true,
        }
    }

    /// Fuzzy-ranked search (Go `/` fuzzy entry): nucleo-matcher scores each
    /// `id + title` haystack; matches sort by score desc, id asc tiebreak.
    /// Empty query restores the natural row order. Case-insensitive like
    /// the old substring check, so every previous contains-match still
    /// surfaces (possibly reordered by score).
    fn apply_search(&mut self) {
        if self.search_query.is_empty() {
            self.filtered_indices = (0..self.rows.len()).collect();
        } else {
            // Lowercase the needle only: `Config::DEFAULT` already folds
            // haystack case, but folding must not be left to nucleo on the
            // needle side — an uppercase needle char that can only match via
            // folding (e.g. "Is" vs "issue") passes the prefilter yet trips
            // a `debug_assert!`-style reject in the optimal matcher
            // (nucleo-matcher 0.3.1 `fuzzy_optimal.rs`: "should have been
            // caught by prefilter"). Pre-folding keeps matching
            // case-insensitive without touching that path.
            let mut matcher = nucleo_matcher::Matcher::new(nucleo_matcher::Config::DEFAULT);
            let mut haystack_buf = Vec::new();
            let mut needle_buf = Vec::new();
            let folded = self.search_query.to_lowercase();
            let needle = nucleo_matcher::Utf32Str::new(&folded, &mut needle_buf);
            let mut scored: Vec<(u16, usize)> = Vec::new();
            for (i, r) in self.rows.iter().enumerate() {
                let haystack = format!("{} {}", r.id, r.title);
                haystack_buf.clear();
                let hay = nucleo_matcher::Utf32Str::new(&haystack, &mut haystack_buf);
                if let Some(score) = matcher.fuzzy_match(hay, needle) {
                    scored.push((score, i));
                }
            }
            scored.sort_by(|a, b| {
                b.0.cmp(&a.0)
                    .then_with(|| self.rows[a.1].id.cmp(&self.rows[b.1].id))
            });
            let idx: Vec<usize> = scored.into_iter().map(|(_, i)| i).collect();
            self.filtered_indices = idx;
        }
        self.cursor = 0;
    }
    /// Semantic search input (Ctrl+S mode): Esc clears and exits, Enter
    /// accepts (back to the standard filter order), typing re-ranks, Tab
    /// cycles the hybrid preset (Go `searchCfg` preset names, same list as
    /// `--search-preset`).
    fn handle_semantic_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Esc => {
                self.semantic_searching = false;
                self.semantic_query.clear();
                self.apply_filter();
                true
            }
            KeyCode::Enter => {
                self.semantic_searching = false;
                self.apply_filter();
                true
            }
            KeyCode::Tab => {
                const PRESETS: &[&str] = &[
                    "default",
                    "bug-hunting",
                    "sprint-planning",
                    "impact-first",
                    "text-only",
                ];
                let next = PRESETS
                    .iter()
                    .position(|p| *p == self.semantic_preset)
                    .map(|i| PRESETS[(i + 1) % PRESETS.len()])
                    .unwrap_or(PRESETS[0]);
                self.semantic_preset = next.to_string();
                self.status_msg = format!("Semantic preset: {next}");
                self.apply_semantic();
                true
            }
            KeyCode::Backspace => {
                self.semantic_query.pop();
                self.apply_semantic();
                true
            }
            KeyCode::Char(c) => {
                self.semantic_query.push(c);
                self.apply_semantic();
                true
            }
            _ => true,
        }
    }

    /// Hybrid semantic ranking (Go `hybridScorer.Score` over the persistent
    /// `.bvvi` index): text cosine × preset weights × graph components
    /// (pagerank from `graph_metrics`, impact from blocker fan-out, status /
    /// priority / recency via Go-exact normalizers). Short queries get Go's
    /// text-floor (0.55) + literal-match boost (+0.35). Zero-score rows are
    /// hidden; ties break by id asc. Empty query restores order.
    ///
    /// The vector side reads from the persistent index (`ensure_semantic_index`,
    /// synced incrementally by content hash — Go `SyncVectorIndex`) and falls
    /// back to fresh hash-embedding when no index is available, so results
    /// are identical either way: the index is a pure cache.
    fn apply_semantic(&mut self) {
        if self.semantic_query.is_empty() {
            self.filtered_indices = (0..self.rows.len()).collect();
            self.cursor = 0;
            return;
        }
        self.ensure_semantic_index();
        let dim = bv_search::embedder::DEFAULT_DIM;
        let query_vec = bv_search::embedder::hash_embed(&self.semantic_query, dim);
        let weights = bv_search::hybrid::get_preset(&self.semantic_preset)
            .or_else(|| bv_search::hybrid::get_preset("default"))
            .expect("default preset exists");
        let weights = bv_search::query::adjust_weights_for_query(weights, &self.semantic_query);
        let short_boost_query = self.semantic_query.clone();

        // Per-issue graph components (Go `AnalyzerMetricsLoader`: pagerank
        // from analysis, blocker fan-out as impact input, updated_at recency).
        let now = jiff::Timestamp::now();
        let max_blockers = self
            .graph_data
            .as_ref()
            .map(|g| g.dependents.values().map(Vec::len).max().unwrap_or(0))
            .unwrap_or(0);
        let days_since = |row: &ListRow| -> f64 {
            self.issue_map
                .get(&row.id)
                .and_then(|i| i.updated_at.as_deref())
                .and_then(|s| s.parse::<jiff::Timestamp>().ok())
                .map(|t| now.since(t).map(|d| d.get_days()).unwrap_or(0) as f64)
                .unwrap_or(0.0)
        };

        // Text scores: index lookup when available, fresh embed otherwise.
        // Both paths produce cosine similarities over the same hash-embed
        // vectors, so ranking is identical either way.
        let text_scores: Vec<(usize, f64)> = if let Some(idx) = &self.semantic_index {
            match idx.search_top_k(&query_vec, self.rows.len()) {
                Ok(hits) => {
                    let by_id: std::collections::HashMap<&str, f64> = hits
                        .iter()
                        .map(|h| (h.issue_id.as_str(), h.score))
                        .collect();
                    self.rows
                        .iter()
                        .enumerate()
                        .filter_map(|(i, r)| by_id.get(r.id.as_str()).map(|&s| (i, s)))
                        .collect()
                }
                Err(_) => self.fresh_text_scores(&query_vec, dim),
            }
        } else {
            self.fresh_text_scores(&query_vec, dim)
        };

        let mut scored: Vec<(i64, usize)> = Vec::new();
        for (i, text_score) in text_scores {
            let row = &self.rows[i];
            let pr = self
                .graph_metrics
                .as_ref()
                .and_then(|gm| gm.pagerank.get(&row.id).copied())
                .unwrap_or(0.0);
            let blockers = self
                .graph_data
                .as_ref()
                .and_then(|g| g.dependents.get(&row.id))
                .map(Vec::len)
                .unwrap_or(0);
            let components = bv_search::hybrid::ComponentScores {
                pagerank: pr.clamp(0.0, 1.0),
                status: bv_search::hybrid::ComponentScores::normalize_status(row.status.as_str()),
                impact: bv_search::hybrid::ComponentScores::normalize_impact(
                    blockers,
                    max_blockers,
                ),
                priority: bv_search::hybrid::ComponentScores::normalize_priority(row.priority),
                recency: bv_search::hybrid::ComponentScores::normalize_recency_days(days_since(
                    row,
                )),
            };
            let mut score = bv_search::hybrid::hybrid_score(text_score, &weights, &components);
            // Go short-query literal boost (added post-threshold, pre-top-k).
            // The document is the one `App::new` built; a row with no
            // matching issue scored 0.0 before and still does.
            score += self
                .semantic_docs()
                .get(&row.id)
                .map(|doc| doc.lexical.boost(&short_boost_query))
                .unwrap_or(0.0);
            if score > 0.0 {
                // Fixed-point rank key: deterministic across platforms.
                scored.push(((score * 1_000_000.0) as i64, i));
            }
        }
        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| self.rows[a.1].id.cmp(&self.rows[b.1].id))
        });
        self.filtered_indices = scored.into_iter().map(|(_, i)| i).collect();
        self.cursor = 0;
    }

    /// Fresh hash-embed text scores for every row (fallback when no
    /// persistent index is available — same vectors the index stores).
    fn fresh_text_scores(&self, query_vec: &[f32], dim: usize) -> Vec<(usize, f64)> {
        let _ = dim;
        self.rows
            .iter()
            .enumerate()
            .filter_map(|(i, r)| {
                // Index and fallback must agree: both embed the Go
                // `IssueDocument` text (ID×3/title×2/labels/desc), not the
                // old title+description pair.
                let text: std::borrow::Cow<'_, str> = match self.semantic_docs().get(&r.id) {
                    Some(doc) => std::borrow::Cow::Borrowed(doc.text.as_str()),
                    None => std::borrow::Cow::Owned(format!("{} {}", r.title, r.description)),
                };
                let issue_vec = bv_search::embedder::hash_embed(&text, dim);
                let score = bv_search::embedder::cosine_similarity(query_vec, &issue_vec);
                (score > 0.0).then_some((i, score))
            })
            .collect()
    }

    /// `data_hash` of `issue_map`, hashed once and memoized.
    ///
    /// The only writer of `issue_map` is [`App::new`] (whole-value
    /// replacement, reached from `reload_from_disk`), so within one `App` the
    /// answer cannot change. Hashing it per keystroke meant deep-cloning
    /// every issue and re-running SHA-256 over the whole corpus to recompute
    /// a digest we already had.
    fn live_data_hash(&mut self) -> String {
        if let Some(h) = &self.semantic_data_hash {
            return h.clone();
        }
        let issues: Vec<&bv_core::model::Issue> = self.issue_map.values().collect();
        let hash = bv_core::data_hash::compute_data_hash_refs(&issues);
        self.semantic_data_hash = Some(hash.clone());
        hash
    }

    /// Load-or-build the persistent `.bvvi` semantic index (Go
    /// `LoadOrNewVectorIndex` + `SyncVectorIndex`, batch 32), syncing
    /// incrementally when the issue set changed (compared via `data_hash`).
    /// Silent no-op when the beads dir can't be discovered (falls back to
    /// fresh embedding in `apply_semantic`).
    fn ensure_semantic_index(&mut self) {
        // Staleness check first, before anything touches the filesystem.
        // Resolving the Beads directory costs three `git rev-parse`
        // subprocesses, which is far more than the rest of this function and
        // used to run on every keystroke. The hash is memoized and the issue
        // set cannot change within one `App`, so a match means there is
        // genuinely nothing to do.
        let live_hash = self.live_data_hash();
        if self.semantic_index.is_some()
            && self.semantic_index_hash.as_deref() == Some(live_hash.as_str())
        {
            return;
        }
        // Project dir = parent of `.beads` (Go `DefaultIndexPath` takes the
        // project dir and appends `.bv/semantic/...`). `None` when discovery
        // failed — stay index-less and let `apply_semantic` embed afresh.
        let Some(project_dir) = self.semantic_project_dir.as_deref() else {
            return;
        };
        let dim = bv_search::embedder::DEFAULT_DIM;
        let path = bv_search::index_sync::default_index_path(project_dir, dim);
        let (mut idx, _loaded) = bv_search::index_sync::load_or_new(&path, dim);
        let docs: std::collections::BTreeMap<String, String> = self
            .semantic_docs()
            .iter()
            .map(|(id, doc)| (id.clone(), doc.text.clone()))
            .collect();
        match bv_search::index_sync::sync_index(&mut idx, &docs, |texts| {
            texts
                .iter()
                .map(|t| bv_search::embedder::hash_embed(t, dim))
                .collect()
        }) {
            Ok(stats) => {
                if stats.changed() || !_loaded {
                    let _ = idx.save(&path);
                }
                self.semantic_index = Some(idx);
                self.semantic_index_hash = Some(live_hash);
            }
            Err(_) => {
                // Sync failure (e.g. bad dim) → stay index-less; the fresh
                // fallback in `apply_semantic` keeps search working.
                self.semantic_index = None;
                self.semantic_index_hash = None;
            }
        }
    }

    /// Handle mouse events (wheel scroll + click select).
    pub fn handle_mouse(&mut self, mouse: crossterm::event::MouseEvent) -> bool {
        use crossterm::event::{MouseButton, MouseEventKind};
        match mouse.kind {
            MouseEventKind::ScrollDown => {
                if self.focus_detail {
                    self.detail_scroll = self.detail_scroll.saturating_add(1);
                } else if self.cursor + 1 < self.filtered_indices.len() {
                    self.cursor += 1;
                }
                true
            }
            MouseEventKind::ScrollUp => {
                if self.focus_detail {
                    self.detail_scroll = self.detail_scroll.saturating_sub(1);
                } else {
                    self.cursor = self.cursor.saturating_sub(1);
                }
                true
            }
            MouseEventKind::Down(MouseButton::Left) => {
                // Screen rows 0 and 1 are chrome: the outer frame, then the
                // list block's own top border. The first item is on row 2.
                //
                // This was off by one before (it subtracted 3, not 2), so
                // clicking the first visible row did nothing at all and every
                // other click selected the row *above* the one under the
                // pointer. The offset has to be read off the actual layout:
                // `render_list` puts its header at `area.y + 1` and the list
                // body at `area.y + 1` too, so the list block's border takes
                // row 1 and the body starts at row 2.
                // Go ignores clicks entirely while any overlay is up
                // (`handleLeftClick`, model.go:8793-8799): the list is not
                // drawn behind a modal, so there is nothing to select, and a
                // click that moved the cursor behind the modal would surface
                // as the list coming back to a different row.
                if self.show_quit_confirm
                    || self.show_agent_prompt
                    || self.cass_modal.is_some()
                    || self.show_update_modal
                    || self.show_label_detail
                    || self.label_drilldown.is_some()
                    // Alerts and the tutorial are full-screen views rather
                    // than overlay flags here, so they are reached through
                    // `current_view`.
                    || self.current_view == ViewMode::Alerts
                    || self.current_view == ViewMode::Tutorial
                    || self.time_travel_prompt.is_some()
                    || self.recipe_picker.as_ref().is_some_and(|p| p.visible)
                    || self.repo_picker.as_ref().is_some_and(|p| p.visible)
                    || self.label_picker.as_ref().is_some_and(|p| p.visible)
                    || self.show_help
                {
                    return true;
                }
                const LIST_TOP: u16 = 2;
                if mouse.row < LIST_TOP {
                    return true;
                }
                // The list is only drawn beside a detail pane when split view
                // is actually laid out. `render` uses the same predicate
                // (`app.split_view && app.width > 100`) to choose a
                // horizontal split; below that it stacks the panes, and there
                // is no right-hand pane to focus — so the left/right test must
                // not run, or every click past 40% of a full-width list row
                // silently does nothing but steal focus.
                let detail_pane_visible = self.split_view && self.width > 100;
                if detail_pane_visible && mouse.column >= (self.width as f64 * 0.4) as u16 {
                    self.focus_detail = true;
                    return true;
                }
                // Row index within the window the renderer actually painted.
                // Read `list_window` rather than recomputing it — a second
                // implementation of the same arithmetic is exactly how this
                // drifted out of sync with the screen in the first place.
                let screen_row = usize::from(mouse.row - LIST_TOP);
                if let Some(idx) = self.list_window.start.checked_add(screen_row) {
                    if idx < self.filtered_indices.len() {
                        self.cursor = idx;
                        self.focus_detail = false;
                    }
                }
                true
            }
            _ => false,
        }
    }

    /// Go `tea.WindowSizeMsg` arm (bubbletea delivers a `WindowSizeMsg`
    /// before the first `View` and again on every resize; model.go:2353 and
    /// :2452 re-`SetSize` the list and re-lay-out the panels against
    /// `m.width`/`m.height`).
    ///
    /// Rust used to drop `CEvent::Resize` on the floor, so `app.width` /
    /// `app.height` kept their 120x40 constructor values for the life of the
    /// process. That is load-bearing: the split-view guard
    /// `app.split_view && app.width > 100` and every modal-centering
    /// computation in `render_overlays` read those fields, so after a resize
    /// the panes were laid out for the wrong terminal.
    ///
    /// `crossterm::terminal::size()` is the authoritative source here rather
    /// than the event payload, because the event is emitted before the
    /// backend has necessarily settled; it is only called on an actual
    /// resize, so its cost does not land on the frame path. Returns whether
    /// anything changed.
    pub fn handle_resize(&mut self) -> bool {
        match crossterm::terminal::size() {
            // `size()` already yields `u16` column/row pairs.
            Ok((w, h)) => {
                if self.width == w && self.height == h {
                    return false;
                }
                self.width = w;
                self.height = h;
                true
            }
            // If the query fails, keep the last known geometry; a stale
            // layout beats panicking mid-session.
            Err(_) => false,
        }
    }

    pub fn cycle_filter(&mut self) {
        self.filter_mode = match self.filter_mode {
            FilterMode::All => FilterMode::Open,
            FilterMode::Open => FilterMode::Closed,
            FilterMode::Closed => FilterMode::Ready,
            FilterMode::Ready => FilterMode::All,
        };
        self.apply_filter();
    }

    pub fn selected(&self) -> Option<&ListRow> {
        self.filtered_indices
            .get(self.cursor)
            .map(|&i| &self.rows[i])
    }

    /// Resolve a pending single-tap backtick (Go `CapsLockTracker` +
    /// `CapsLockTimerExpiredMsg` → `TriggerFullTutorial`): if the 300ms
    /// double-tap window has expired with no second tap, the user meant a
    /// single tap → open the full Tutorial (page 0). Called at the top of
    /// `handle_key` so the only infrastructure is the event loop's own
    /// keypress cadence — no timer thread, no message queue. Documented
    /// latency: the Tutorial opens on the first keypress *after* the window
    /// expires (or immediately if the next keypress IS the second tap —
    /// that path never touches this method).
    fn resolve_pending_tutorial_tap(&mut self) {
        const TAP_WINDOW: std::time::Duration = std::time::Duration::from_millis(300);
        if self
            .tutorial_tap_pending
            .is_some_and(|t| t.elapsed() >= TAP_WINDOW)
        {
            self.tutorial_tap_pending = None;
            if self.current_view != ViewMode::Tutorial {
                if let Some(t) = &mut self.tutorial {
                    t.current_page = 0;
                }
                self.current_view = ViewMode::Tutorial;
            }
        }
    }

    /// Open context help for the current view (shared by the `~` key and
    /// double-tap backtick paths).
    fn open_context_help(&mut self) {
        self.tutorial_tap_pending = None;
        self.show_context_help = true;
    }

    /// True when any list filter deviates from the default (Go
    /// `hasActiveFilters`: status filter != "all", label/repo filter set, or
    /// a fuzzy/semantic search query active).
    fn has_active_filters(&self) -> bool {
        self.filter_mode != FilterMode::All
            || self.label_filter.is_some()
            || self.active_repo.is_some()
            || self.searching
            || self.semantic_searching
    }

    /// Reset all list filters to default (Go `clearAllFilters`: status back
    /// to "all", label/repo cleared, search queries cleared) and re-apply.
    fn clear_all_filters(&mut self) {
        self.filter_mode = FilterMode::All;
        self.label_filter = None;
        self.active_repo = None;
        self.searching = false;
        self.search_query.clear();
        self.semantic_searching = false;
        self.semantic_query.clear();
        self.apply_filter();
    }

    /// Go `showSelfUpdateModal` (`model.go:9653-9667`): open the self-update
    /// modal for the pending release, or report that the binary is current.
    /// `U` is not a toggle in Go — it always (re)opens, because the modal
    /// itself consumes keys while it is up.
    pub fn show_self_update_modal(&mut self) {
        // Go tests both `!m.updateAvailable || m.updateTag == ""`; bvr has no
        // separate `updateAvailable` flag, so the tag alone is the gate.
        let Some(tag) = self.update_tag.as_deref().filter(|t| !t.is_empty()) else {
            self.status_msg = "No update available - you're running the latest version".to_string();
            return;
        };
        // `bv_update::current_version()` (not `env!("CARGO_PKG_VERSION")`) so the
        // modal's "Current version:" row matches the `v`-prefixed rendering
        // every other update surface uses — Go stamps `version.Version` with
        // the prefix (`pkg/version/version.go` normalizeVersion) and
        // `NewUpdateModal` reads it directly (`update_modal.go:80`).
        let mut modal = crate::update_modal::UpdateModal::new(
            &bv_update::current_version(),
            tag,
            self.update_url.as_deref().unwrap_or_default(),
        );
        modal.set_size(self.width, self.height);
        self.update_modal = Some(modal);
        self.show_update_modal = true;
    }

    /// Go's `if m.showUpdateModal` key block (`model.go:3788-3821`): hand the
    /// key to the modal first, then apply the parent's close policy. Every
    /// path returns consumed — a key can never fall through to the list while
    /// the overlay is up.
    fn handle_update_modal_key(&mut self, code: KeyCode) -> bool {
        let Some(modal) = self.update_modal.as_mut() else {
            // Nothing to drive; drop the flag rather than swallow keys.
            self.show_update_modal = false;
            return true;
        };
        let action = modal.handle_key(code);
        if action == crate::update_modal::UpdateModalAction::Dismiss || modal.close_policy(&code) {
            self.show_update_modal = false;
        }
        true
    }

    /// Go's `UpdateProgressMsg` / `UpdateCompleteMsg` arms
    /// (`model.go:2304-2333`): drain the install thread on the event-loop
    /// tick, and — the protection Go spells out at `model.go:2306-2314` —
    /// retire the "update available" notice once the tag it advertises has
    /// actually been installed. Without this the user could run the update a
    /// second time and overwrite the useful pre-update backup with a backup of
    /// the already-updated binary.
    ///
    /// The retire check is idempotent (Go gets to run it once per message),
    /// so it is re-evaluated every tick. Returns true when the modal had new
    /// messages and needs a redraw.
    pub fn poll_update_modal(&mut self) -> bool {
        let Some(modal) = self.update_modal.as_mut() else {
            return false;
        };
        let changed = modal.poll();
        if modal.state == crate::update_modal::UpdateState::Success
            && self.update_tag.as_deref() == Some(modal.completed_version.as_str())
        {
            self.update_tag = None;
            self.update_url = None;
        }
        changed
    }

    /// Handle a key event; returns true if the event was consumed.
    pub fn handle_key(&mut self, code: KeyCode) -> bool {
        // Quit-confirmation overlay (Go `showQuitConfirm` block at the top of
        // Update): Esc/Y quits, any other key cancels back to the list.
        if self.show_quit_confirm {
            match code {
                KeyCode::Esc | KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.quit_requested = true;
                }
                _ => {
                    self.show_quit_confirm = false;
                }
            }
            return true;
        }
        if self.semantic_searching {
            return self.handle_semantic_key(code);
        }
        if self.searching {
            return self.handle_search_key(code);
        }
        if self.show_help {
            self.show_help = false;
            return true;
        }
        if self.show_update_modal {
            return self.handle_update_modal_key(code);
        }
        if matches!(self.label_picker, Some(ref p) if p.visible) {
            return self.handle_label_picker_key(code);
        }
        if matches!(self.recipe_picker, Some(ref p) if p.visible) {
            return self.handle_recipe_picker_key(code);
        }
        if matches!(self.repo_picker, Some(ref p) if p.visible) {
            return self.handle_repo_picker_key(code);
        }
        if self.time_travel_prompt.is_some() {
            return self.handle_time_travel_prompt_key(code);
        }
        if self.label_drilldown.is_some() {
            return self.handle_label_drilldown_key(code);
        }
        if self.show_agent_prompt {
            return self.handle_agent_prompt_key(code);
        }
        if self.cass_modal.is_some() {
            return self.handle_cass_modal_key(code);
        }
        if self.show_context_help {
            // Go `~` / double-tap context help: any key dismisses (same
            // "press any key to close" contract as `render_context_help`).
            self.show_context_help = false;
            return true;
        }
        // Resolve a pending single-tap backtick (Go `CapsLockTracker` +
        // `CapsLockTimerExpiredMsg` → `TriggerFullTutorial`): if >300ms
        // elapsed since the first tap with no second tap, the user meant a
        // single tap → open the full Tutorial now. Runs at the top of every
        // keypress so no background timer is needed; worst case the Tutorial
        // opens on the next keypress after the window (documented latency,
        // same tradeoff as Go's message-queue tick).
        self.resolve_pending_tutorial_tap();
        // Go `handleHistoryKeys` is a whole function (model.go:5483-5753) with
        // three ordered regions: the search input, the file tree, then the
        // general switch. Handle the first two here so the main `match` only
        // sees keys the History view has not already claimed.
        if self.current_view == ViewMode::History && self.handle_history_modal_key(code) {
            return true;
        }
        match code {
            KeyCode::Tab => {
                self.focus_detail = !self.focus_detail;
                true
            }
            KeyCode::Char('q') => {
                if self.focus_detail {
                    self.focus_detail = false;
                } else {
                    self.quit_requested = true;
                }
                true
            }
            KeyCode::Esc => {
                if self.searching {
                    self.searching = false;
                    self.search_query.clear();
                } else if self.current_view == ViewMode::List && self.has_active_filters() {
                    // Go: first Esc at the main list clears filters instead
                    // of quitting (`hasActiveFilters` → `clearAllFilters`).
                    self.clear_all_filters();
                    self.status_msg = "Filters cleared".to_string();
                } else if self.show_detail {
                    self.show_detail = false;
                } else if self.label_drilldown.is_some() || self.show_label_detail {
                    self.label_drilldown = None;
                    self.show_label_detail = false;
                } else if self.current_view == ViewMode::Sprint && self.show_velocity {
                    self.show_velocity = false;
                } else if matches!(
                    self.current_view,
                    ViewMode::TimeTravel
                        | ViewMode::LabelDashboard
                        | ViewMode::Board
                        | ViewMode::Tree
                        | ViewMode::Insights
                        | ViewMode::Alerts
                        | ViewMode::FlowMatrix
                        | ViewMode::Attention
                        | ViewMode::Tutorial
                        | ViewMode::Actionable
                        | ViewMode::Sprint
                ) {
                    self.current_view = ViewMode::List;
                } else {
                    // Go: no filters active at the main list — second Esc
                    // shows the quit confirmation (`showQuitConfirm` +
                    // `focusQuitConfirm`); the top-of-`handle_key` block
                    // above resolves Esc/Y → quit, anything else → cancel.
                    self.show_quit_confirm = true;
                }
                true
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if self.focus_detail {
                    self.detail_scroll = self.detail_scroll.saturating_add(1);
                } else if self.current_view == ViewMode::FlowMatrix {
                    let max = self.flow.as_ref().map(|f| f.labels.len()).unwrap_or(0);
                    if self.flow_cursor + 1 < max {
                        self.flow_cursor += 1;
                    }
                } else if self.current_view == ViewMode::Attention {
                    if self.attention_cursor + 1 < self.attention_labels.len() {
                        self.attention_cursor += 1;
                    }
                } else if self.current_view == ViewMode::Graph {
                    let max = self
                        .graph_data
                        .as_ref()
                        .map(|g| g.sorted_ids.len())
                        .unwrap_or(0);
                    if max > 0 && self.graph_cursor + 1 < max {
                        self.graph_cursor += 1;
                        // Auto-scroll the node list panel
                        let visible = self.height.saturating_sub(4) as usize;
                        if self.graph_cursor >= self.graph_scroll + visible {
                            self.graph_scroll = self.graph_cursor.saturating_sub(visible - 1);
                        }
                    }
                } else if self.current_view == ViewMode::History {
                    // Go `MoveDown` / `MoveDownGit` (history.go:793-832,
                    // :1311-1333): the cursor belongs to the *focused* pane —
                    // beads on the list, commits in the middle/detail pane,
                    // timeline scroll on the timeline pane.
                    if let Some(ref mut h) = self.history {
                        h.move_down();
                    }
                } else if self.current_view == ViewMode::Alerts {
                    if self.alerts_cursor + 1 < self.alerts.len() {
                        self.alerts_cursor += 1;
                    }
                } else if self.current_view == ViewMode::TimeTravel {
                    let max = self.time_travel_entry_count();
                    if max > 0 && self.time_travel_cursor + 1 < max {
                        self.time_travel_cursor += 1;
                    }
                } else if self.current_view == ViewMode::LabelDashboard {
                    let max = self.label_dashboard_count();
                    if max > 0 && self.label_dashboard_cursor + 1 < max {
                        self.label_dashboard_cursor += 1;
                    }
                } else if self.current_view == ViewMode::Sprint {
                    if let Some(s) = &mut self.sprint {
                        s.move_down();
                    }
                } else if self.current_view == ViewMode::Actionable {
                    if let Some(a) = &mut self.actionable {
                        a.move_down();
                    }
                } else if self.current_view == ViewMode::Tutorial {
                    if let Some(t) = &mut self.tutorial {
                        t.next_page();
                    }
                } else if self.cursor + 1 < self.filtered_indices.len() {
                    self.cursor += 1;
                    self.update_session_count();
                }
                true
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.focus_detail {
                    self.detail_scroll = self.detail_scroll.saturating_sub(1);
                } else if self.current_view == ViewMode::FlowMatrix {
                    self.flow_cursor = self.flow_cursor.saturating_sub(1);
                } else if self.current_view == ViewMode::Attention {
                    self.attention_cursor = self.attention_cursor.saturating_sub(1);
                } else if self.current_view == ViewMode::Graph {
                    self.graph_cursor = self.graph_cursor.saturating_sub(1);
                    // Auto-scroll up if cursor goes above visible area
                    if self.graph_cursor < self.graph_scroll {
                        self.graph_scroll = self.graph_cursor;
                    }
                } else if self.current_view == ViewMode::History {
                    // Go `MoveUp` / `MoveUpGit` (history.go:768-790,
                    // :1287-1308) — focus-aware, same as `j` above.
                    if let Some(ref mut h) = self.history {
                        h.move_up();
                    }
                } else if self.current_view == ViewMode::Alerts {
                    self.alerts_cursor = self.alerts_cursor.saturating_sub(1);
                } else if self.current_view == ViewMode::TimeTravel {
                    self.time_travel_cursor = self.time_travel_cursor.saturating_sub(1);
                } else if self.current_view == ViewMode::LabelDashboard {
                    self.label_dashboard_cursor = self.label_dashboard_cursor.saturating_sub(1);
                } else if self.current_view == ViewMode::Sprint {
                    if let Some(s) = &mut self.sprint {
                        s.move_up();
                    }
                } else if self.current_view == ViewMode::Actionable {
                    if let Some(a) = &mut self.actionable {
                        a.move_up();
                    }
                } else if self.current_view == ViewMode::Tutorial {
                    if let Some(t) = &mut self.tutorial {
                        t.prev_page();
                    }
                } else {
                    self.cursor = self.cursor.saturating_sub(1);
                    self.update_session_count();
                }
                true
            }
            KeyCode::Char('o') => {
                self.filter_mode = FilterMode::Open;
                self.apply_filter();
                true
            }
            KeyCode::Char('c') => {
                if self.current_view == ViewMode::History {
                    // Go only cycles the confidence filter in bead mode
                    // (model.go:5660-5669), and the ladder is the four
                    // thresholds {0, 0.5, 0.75, 0.9} (history.go:894).
                    let conf = if let Some(ref mut h) = self.history {
                        if h.is_git_mode() {
                            0.0
                        } else {
                            h.cycle_confidence()
                        }
                    } else {
                        0.0
                    };
                    // Go model.go:5661-5668.
                    self.status_msg = if conf == 0.0 {
                        "\u{1f50d} Showing all commits".to_string()
                    } else {
                        format!("\u{1f50d} Confidence filter: \u{2265}{:.0}%", conf * 100.0)
                    };
                } else {
                    self.filter_mode = FilterMode::Closed;
                    self.apply_filter();
                }
                true
            }
            KeyCode::Char('r') => {
                self.filter_mode = FilterMode::Ready;
                self.apply_filter();
                true
            }
            // ---- History-scoped keys -------------------------------------
            // Go binds `f`, `t`, `/`, `tab`, `enter`, `esc`, `o` and `g`
            // *inside* `handleHistoryKeys` (model.go:5483-5753). These arms
            // sit ahead of the global `f` / `t` / `/` arms below, which would
            // otherwise shadow them: the global `f` opens Flow-Matrix, the
            // global `t` opens the Time-Travel revision prompt, and the global
            // `/` opens the main-list fuzzy search. Go has no global `t` at
            // all, so the guard below is what makes the timeline toggle
            // reachable (the TUI_UX_PARITY_PLAN.md G5 collision).
            KeyCode::Char('/') if self.current_view == ViewMode::History => {
                if let Some(ref mut h) = self.history {
                    h.start_search();
                    self.status_msg =
                        "\u{1f50d} Type to search commits, beads, authors...".to_string();
                }
                true
            }
            KeyCode::Char('f') | KeyCode::Char('F') if self.current_view == ViewMode::History => {
                if let Some(ref mut h) = self.history {
                    h.toggle_file_tree();
                    self.status_msg = if h.is_file_tree_visible() {
                        "\u{1f4c1} File tree: j/k navigate, Enter select, Esc close".to_string()
                    } else {
                        "\u{1f4c1} File tree hidden".to_string()
                    };
                }
                true
            }
            KeyCode::Char('t') if self.current_view == ViewMode::History => {
                let Some(ref mut h) = self.history else {
                    return true;
                };
                // Go's refusal message when the pane cannot be shown
                // (model.go:5683-5686).
                // Go's exact strings (model.go:5683-5692), emoji included.
                if !h.timeline_available() {
                    self.status_msg =
                        "\u{1f552} Timeline needs bead mode and \u{2265}100 columns".to_string();
                } else if h.toggle_timeline() {
                    self.status_msg = "\u{1f552} Timeline pane shown (t to hide)".to_string();
                } else {
                    self.status_msg = "\u{1f552} Timeline pane hidden (t to show)".to_string();
                }
                true
            }
            KeyCode::Enter if self.current_view == ViewMode::History => {
                self.jump_from_history();
                true
            }
            KeyCode::Char('g') if self.current_view == ViewMode::History => {
                self.goto_history_graph();
                true
            }
            KeyCode::Char('/') => {
                self.searching = true;
                self.search_query.clear();
                true
            }
            KeyCode::Char('b') => {
                self.current_view = if self.current_view == ViewMode::Board {
                    ViewMode::List
                } else {
                    ViewMode::Board
                };
                true
            }
            KeyCode::Char('i') => {
                self.current_view = if self.current_view == ViewMode::Insights {
                    ViewMode::List
                } else {
                    ViewMode::Insights
                };
                true
            }
            KeyCode::Char('`') => {
                // Go `CapsLockTracker.HandlePress` (capslock.go): a second
                // tap within 300ms means context help for the current view
                // (`TriggerContextHelp`); otherwise start/keep the single-tap
                // timer (`TriggerNone` + tick command → resolved by
                // `resolve_pending_tutorial_tap` at the next keypress).
                const TAP_WINDOW: std::time::Duration = std::time::Duration::from_millis(300);
                let double_tap = self
                    .tutorial_tap_pending
                    .is_some_and(|t| t.elapsed() < TAP_WINDOW);
                self.tutorial_tap_pending = None;
                if double_tap {
                    self.open_context_help();
                } else if self.current_view == ViewMode::Tutorial {
                    self.current_view = ViewMode::List;
                } else {
                    self.tutorial_tap_pending = Some(std::time::Instant::now());
                }
                true
            }
            KeyCode::Char('~') => {
                // Go `IsContextHelpTrigger` (capslock.go): direct context
                // help for the current view, no tap dance needed.
                self.open_context_help();
                true
            }
            KeyCode::Char('t') => {
                // Go Time-Travel (`focusTimeTravelInput` → `SnapshotDiff`):
                // open the revision-input prompt; Enter submits a
                // `diff_issues` diff of the loaded set vs `git show <rev>`.
                // Re-opens over an existing result to run another revision.
                self.current_view = ViewMode::TimeTravel;
                if self.time_travel_prompt.is_none() {
                    self.time_travel_prompt = Some(String::new());
                }
                true
            }
            KeyCode::Char('T') => {
                // Go instant variant: diff against HEAD~5 with no prompt.
                self.current_view = ViewMode::TimeTravel;
                self.time_travel_prompt = None;
                self.run_time_travel("HEAD~5");
                true
            }
            KeyCode::Char('[') => {
                // Go Label Dashboard (`focusLabelDashboard`).
                if self.current_view == ViewMode::LabelDashboard {
                    self.current_view = ViewMode::List;
                } else {
                    self.load_label_health_if_needed();
                    self.current_view = ViewMode::LabelDashboard;
                }
                true
            }
            // In the dashboard, `h` opens the health-detail modal for the
            // cursor label (Go `h`) instead of toggling History — this
            // guarded arm must precede the plain `h` arm below.
            KeyCode::Char('h') if self.current_view == ViewMode::LabelDashboard => {
                self.show_label_detail = !self.show_label_detail;
                self.label_drilldown = None;
                true
            }
            // Go `d` drilldown overlay: filtered issue list for the cursor
            // label. `d` is unbound elsewhere, so no fallback behavior lost.
            KeyCode::Char('d') if self.current_view == ViewMode::LabelDashboard => {
                self.open_label_drilldown();
                true
            }
            // Board column navigation (Go `board.go`): guarded arms must
            // precede the global `h`/`l`/`s` meanings (History toggle, label
            // picker, sort cycle), same pattern as the dashboard `h` above.
            KeyCode::Char('h') if self.current_view == ViewMode::Board => {
                self.board_column = self.board_column.saturating_sub(1);
                true
            }
            KeyCode::Char('l') if self.current_view == ViewMode::Board => {
                let max = crate::views::board::column_labels(self, self.board_mode).len();
                if max > 0 && self.board_column + 1 < max {
                    self.board_column += 1;
                }
                true
            }
            KeyCode::Char('s') if self.current_view == ViewMode::Board => {
                self.board_mode = self.board_mode.next();
                self.board_column = 0;
                self.status_msg = format!("Board: grouped by {}", self.board_mode.label());
                true
            }
            KeyCode::Char(c)
                if self.current_view == ViewMode::Board && ('1'..='9').contains(&c) =>
            {
                let idx = (c as u8 - b'1') as usize;
                let max = crate::views::board::column_labels(self, self.board_mode).len();
                if idx < max {
                    self.board_column = idx;
                }
                true
            }
            KeyCode::Char('h') => {
                if self.current_view == ViewMode::History {
                    self.current_view = ViewMode::List;
                } else {
                    self.load_history_if_needed();
                    self.current_view = ViewMode::History;
                }
                true
            }
            KeyCode::Char('J') if self.current_view == ViewMode::History => {
                if let Some(ref mut h) = self.history {
                    h.next_commit();
                }
                true
            }
            KeyCode::Char('K') if self.current_view == ViewMode::History => {
                if let Some(ref mut h) = self.history {
                    h.prev_commit();
                }
                true
            }
            KeyCode::Char('v') if self.current_view == ViewMode::History => {
                if let Some(ref mut h) = self.history {
                    h.toggle_mode();
                    // Go model.go:5558-5566.
                    self.status_msg = if h.is_git_mode() {
                        "\u{1f500} Git Mode: commits on left, related beads on right".to_string()
                    } else {
                        "\u{1f4e6} Bead Mode: beads on left, commits on right".to_string()
                    };
                }
                true
            }
            KeyCode::Char('y') if self.current_view == ViewMode::History => {
                self.copy_history_commit_sha();
                true
            }
            KeyCode::Char('!') => {
                self.current_view = if self.current_view == ViewMode::Alerts {
                    ViewMode::List
                } else {
                    ViewMode::Alerts
                };
                true
            }
            KeyCode::Char('f') => {
                self.current_view = if self.current_view == ViewMode::FlowMatrix {
                    ViewMode::List
                } else {
                    ViewMode::FlowMatrix
                };
                true
            }
            KeyCode::Char('A') => {
                self.current_view = if self.current_view == ViewMode::Attention {
                    ViewMode::List
                } else {
                    ViewMode::Attention
                };
                true
            }
            KeyCode::Char('E') => {
                self.current_view = if self.current_view == ViewMode::Tree {
                    ViewMode::List
                } else {
                    ViewMode::Tree
                };
                true
            }
            KeyCode::Char('G') => {
                self.current_view = if self.current_view == ViewMode::Graph {
                    ViewMode::List
                } else {
                    // Build graph data when entering graph view
                    if self.graph_data.is_none() {
                        let issues: Vec<bv_core::model::Issue> =
                            self.issue_map.values().cloned().collect();
                        let metrics = self.graph_metrics.clone();
                        self.graph_data =
                            Some(crate::views::graph::GraphData::build(issues, metrics));
                    }
                    ViewMode::Graph
                };
                true
            }
            KeyCode::Char('P') => {
                self.current_view = if self.current_view == ViewMode::Sprint {
                    ViewMode::List
                } else {
                    self.load_sprint_if_needed();
                    ViewMode::Sprint
                };
                true
            }
            KeyCode::Char('F') => {
                self.current_view = if self.current_view == ViewMode::Actionable {
                    ViewMode::List
                } else {
                    self.load_actionable_if_needed();
                    ViewMode::Actionable
                };
                true
            }
            // Sprint velocity comparison (Go `velocity_comparison.go`
            // sub-toggle). `v` is History-scoped elsewhere, free here.
            KeyCode::Char('v') if self.current_view == ViewMode::Sprint => {
                self.show_velocity = !self.show_velocity;
                true
            }
            KeyCode::Char('a') => {
                self.filter_mode = FilterMode::All;
                self.apply_filter();
                true
            }
            KeyCode::Char('s') => {
                self.sort_mode = self.sort_mode.next();
                self.apply_filter();
                true
            }
            KeyCode::Enter => {
                self.show_detail = !self.show_detail;
                // When opening detail panel, auto-focus it so j/k scroll
                // the detail content immediately (UX parity with Go bv).
                if self.show_detail {
                    self.focus_detail = true;
                }
                true
            }
            KeyCode::Char('x') => {
                self.export_markdown();
                true
            }
            KeyCode::Char('C') => {
                self.copy_issue_to_clipboard();
                true
            }
            KeyCode::Char('O') => {
                self.open_in_editor();
                true
            }
            KeyCode::Char('?') => {
                self.show_help = !self.show_help;
                true
            }
            KeyCode::Char('S') => {
                self.sort_mode = SortMode::Priority;
                self.apply_filter();
                self.status_msg = "Sorted by triage score".to_string();
                true
            }
            KeyCode::Char('L') => {
                self.cycle_label_filter();
                true
            }
            KeyCode::Char('l') => {
                self.open_label_picker();
                true
            }
            KeyCode::Char('\'') => {
                self.open_recipe_picker();
                true
            }
            KeyCode::Char('w') if self.workspace_repos.is_some() => {
                self.open_repo_picker();
                true
            }
            KeyCode::Char('U') => {
                self.show_self_update_modal();
                true
            }
            KeyCode::Char('p') => {
                self.toggle_priority_hints();
                true
            }
            KeyCode::Char('V') => {
                // Cass session preview modal (Go `V` → `showCassSessionModal`,
                // bv-5bqh). Uppercase is free: lowercase `v` is view-scoped
                // (History bead/git toggle, Sprint velocity overlay).
                self.open_cass_modal();
                true
            }
            _ => false,
        }
    }

    /// Toggle Priority Hints (Go `p` — `pkg/ui/model.go`
    /// `showPriorityHints`). Lazily computes the suggestion set on first
    /// use via `bv_analysis::impact::compute_impact_scores` +
    /// `score_to_priority` — the same engine `--robot-priority` already
    /// uses, so this is wiring, not a new scoring algorithm. Unlike the
    /// existing `--robot-priority` CLI path (which only surfaces
    /// under-prioritized issues, `suggested < current`), this counts both
    /// directions to match Go's bidirectional ↑/↓ hint UI.
    fn toggle_priority_hints(&mut self) {
        self.show_priority_hints = !self.show_priority_hints;
        if !self.show_priority_hints {
            self.status_msg.clear();
            return;
        }
        if self.priority_hints.is_none() {
            self.priority_hints = Some(self.compute_priority_hints());
        }
        let n = self.priority_hints.as_ref().map(|m| m.len()).unwrap_or(0);
        self.status_msg = if n > 0 {
            format!("Priority hints: \u{2191} increase \u{2193} decrease ({n} suggestions)")
        } else {
            "Priority hints: No misalignments detected".to_string()
        };
    }

    /// Compute the suggested-priority map (Go `ComputeImpactScores` +
    /// `GenerateRecommendations` — `pkg/analysis/priority.go`). Only open
    /// issues are scored (Go parity — closed/tombstone are skipped inside
    /// `compute_impact_scores` itself); only entries where the suggestion
    /// differs from the current priority are kept, matching "misalignment."
    fn compute_priority_hints(&self) -> std::collections::BTreeMap<String, i32> {
        let issues: Vec<bv_core::model::Issue> = self.issue_map.values().cloned().collect();
        let g = bv_analysis::build_graph(&issues);
        let empty = BTreeMap::new();
        let (pagerank, betweenness) = match &self.graph_metrics {
            Some(gm) => (&gm.pagerank, &gm.betweenness),
            None => (&empty, &empty),
        };
        let inputs = bv_analysis::impact::ImpactInputs {
            issues: &issues,
            pagerank,
            betweenness,
            critical_path: None,
            g: &g,
            now: jiff::Timestamp::now(),
        };
        bv_analysis::impact::compute_impact_scores(&inputs)
            .into_iter()
            .filter_map(|r| {
                let suggested = bv_analysis::scoring::score_to_priority(r.score);
                (suggested != r.priority).then_some((r.id, suggested))
            })
            .collect()
    }

    /// Build and show the label-filter picker overlay (Go `label_picker.go`
    /// — real picker, replacing/augmenting the `L` one-key cycle).
    fn open_label_picker(&mut self) {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for row in &self.rows {
            for label in &row.labels {
                *counts.entry(label.clone()).or_insert(0) += 1;
            }
        }
        let options: Vec<crate::views::pickers::LabelOption> = counts
            .into_iter()
            .map(|(name, count)| crate::views::pickers::LabelOption { name, count })
            .collect();
        let mut picker = crate::views::pickers::LabelPicker::new(options);
        picker.toggle(); // sets visible = true
        self.label_picker = Some(picker);
    }

    fn handle_label_picker_key(&mut self, code: KeyCode) -> bool {
        let Some(picker) = self.label_picker.as_mut() else {
            return false;
        };
        match code {
            KeyCode::Esc => picker.visible = false,
            KeyCode::Up | KeyCode::Char('k') => picker.move_up(),
            KeyCode::Down | KeyCode::Char('j') => picker.move_down(),
            KeyCode::Backspace => {
                let mut t = picker.filter_text.clone();
                t.pop();
                picker.update_filter(&t);
            }
            KeyCode::Char(c) => {
                let mut t = picker.filter_text.clone();
                t.push(c);
                picker.update_filter(&t);
            }
            KeyCode::Enter => {
                let chosen = picker.selected_label().map(|s| s.to_string());
                picker.visible = false;
                if let Some(label) = chosen {
                    self.label_filter = Some(label);
                    self.apply_filter();
                }
            }
            _ => {}
        }
        true
    }

    /// Build and show the workspace repo picker overlay (Go
    /// `repo_picker.go` — real picker, replacing the old `w` one-key
    /// cycle-through per TUI_UX_PARITY_PLAN.md Q1).
    fn open_repo_picker(&mut self) {
        let Some(repos) = &self.workspace_repos else {
            return;
        };
        let counts: BTreeMap<&str, usize> =
            self.issue_map
                .values()
                .fold(BTreeMap::new(), |mut acc, issue| {
                    *acc.entry(issue.source_repo.as_str()).or_insert(0) += 1;
                    acc
                });
        let options: Vec<crate::views::pickers::RepoOption> = repos
            .iter()
            .map(|name| crate::views::pickers::RepoOption {
                name: name.clone(),
                path: name.clone(),
                issue_count: counts.get(name.as_str()).copied().unwrap_or(0),
            })
            .collect();
        let mut picker = crate::views::pickers::RepoPicker::new(options);
        picker.toggle();
        self.repo_picker = Some(picker);
    }

    fn handle_repo_picker_key(&mut self, code: KeyCode) -> bool {
        let Some(picker) = self.repo_picker.as_mut() else {
            return false;
        };
        match code {
            KeyCode::Esc => picker.visible = false,
            KeyCode::Up | KeyCode::Char('k') => picker.move_up(),
            KeyCode::Down | KeyCode::Char('j') => picker.move_down(),
            KeyCode::Enter => {
                let chosen = picker.selected_repo().map(|r| r.name.clone());
                picker.visible = false;
                self.active_repo = chosen;
                self.status_msg = match &self.active_repo {
                    Some(r) => format!("Repos: {r}"),
                    None => "Repos: all".to_string(),
                };
                self.apply_filter();
            }
            _ => {}
        }
        true
    }

    /// Build and show the recipe picker overlay (Go `recipe_picker.go`).
    /// Recipe *definitions* (name/filter/sort) come from
    /// `crate::views::pickers::default_recipes` — the same 6 built-in
    /// recipes Go embeds in `defaults/recipes.yaml`
    /// (triage/release-cut/blocked-review/dependency-risk/quick-wins/stale).
    /// User/project YAML recipe loading is a documented scope cut (same
    /// convention as the rest of this port — see `pickers.rs`'s own scope
    /// note); only the built-in set is real.
    fn open_recipe_picker(&mut self) {
        let options: Vec<crate::views::pickers::RecipeOption> = default_recipe_defs()
            .iter()
            .map(|r| crate::views::pickers::RecipeOption {
                name: r.name.to_string(),
                description: r.description.to_string(),
                labels: vec![],
            })
            .collect();
        let mut picker = crate::views::pickers::RecipePicker::new(options);
        picker.toggle();
        self.recipe_picker = Some(picker);
    }

    fn handle_recipe_picker_key(&mut self, code: KeyCode) -> bool {
        let Some(picker) = self.recipe_picker.as_mut() else {
            return false;
        };
        let mut apply: Option<usize> = None;
        match code {
            KeyCode::Esc => picker.visible = false,
            KeyCode::Up | KeyCode::Char('k') => picker.move_up(),
            KeyCode::Down | KeyCode::Char('j') => picker.move_down(),
            KeyCode::Enter => {
                apply = Some(picker.selected);
                picker.visible = false;
            }
            _ => {}
        }
        if let Some(idx) = apply {
            self.apply_recipe(idx);
        }
        true
    }

    /// Apply built-in recipe `idx` (from `default_recipe_defs`) by setting
    /// filter/sort state and, where the recipe needs a predicate the
    /// FilterMode enum can't express (blocked/dependency-risk/quick-wins/
    /// stale), computing `filtered_indices` directly — same pattern
    /// `apply_search` already uses to bypass FilterMode.
    fn apply_recipe(&mut self, idx: usize) {
        let defs = default_recipe_defs();
        let Some(def) = defs.get(idx) else { return };
        self.label_filter = None;
        self.active_repo = None;
        match def.name {
            "triage" => {
                self.filter_mode = FilterMode::All;
                self.apply_filter();
                let by_id: std::collections::HashMap<&str, &bv_core::model::Issue> = self
                    .issue_map
                    .iter()
                    .map(|(k, v)| (k.as_str(), v))
                    .collect();
                self.filtered_indices.retain(|&i| {
                    bv_analysis::blocker_chain::is_actionable(&by_id, &self.rows[i].id)
                });
                self.sort_mode = SortMode::Priority;
                self.filtered_indices
                    .sort_by_key(|&i| self.rows[i].priority);
            }
            "release-cut" => {
                self.filter_mode = FilterMode::Closed;
                self.sort_mode = SortMode::Updated;
                self.apply_filter();
                self.filtered_indices
                    .sort_by(|&a, &b| self.rows[b].created_at.cmp(&self.rows[a].created_at));
            }
            "blocked-review" => {
                self.filter_mode = FilterMode::All;
                self.apply_filter();
                let by_id: std::collections::HashMap<&str, &bv_core::model::Issue> = self
                    .issue_map
                    .iter()
                    .map(|(k, v)| (k.as_str(), v))
                    .collect();
                self.filtered_indices.retain(|&i| {
                    !bv_analysis::blocker_chain::open_blockers(&by_id, &self.rows[i].id).is_empty()
                });
            }
            "dependency-risk" => {
                self.filter_mode = FilterMode::All;
                self.apply_filter();
                if let Some(gm) = &self.graph_metrics {
                    let bw = gm.betweenness.clone();
                    self.filtered_indices.sort_by(|&a, &b| {
                        let sa = bw.get(&self.rows[a].id).copied().unwrap_or(0.0);
                        let sb = bw.get(&self.rows[b].id).copied().unwrap_or(0.0);
                        sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
                    });
                    self.filtered_indices.truncate(20);
                }
            }
            "quick-wins" => {
                self.filter_mode = FilterMode::Open;
                self.apply_filter();
                self.filtered_indices.retain(|&i| {
                    self.rows[i].priority >= 2
                        && self
                            .issue_map
                            .get(&self.rows[i].id)
                            .map(|iss| iss.dependencies.is_empty())
                            .unwrap_or(false)
                });
            }
            "stale" => {
                self.filter_mode = FilterMode::Open;
                self.apply_filter();
                let now = jiff::Timestamp::now();
                self.filtered_indices.retain(|&i| {
                    self.rows[i]
                        .created_at
                        .as_ref()
                        .and_then(|_| self.issue_map.get(&self.rows[i].id))
                        .and_then(|iss| iss.updated_at.as_ref())
                        .and_then(|u| u.parse::<jiff::Timestamp>().ok())
                        .map(|t| now.since(t).map(|d| d.get_days()).unwrap_or(0) >= 14)
                        .unwrap_or(false)
                });
            }
            _ => {}
        }
        self.cursor = 0;
        self.status_msg = format!("Recipe: {}", def.name);
    }

    /// Handle a Ctrl-modified key event; returns true if consumed.
    pub fn handle_ctrl_key(&mut self, code: KeyCode) -> bool {
        if self.semantic_searching {
            return self.handle_semantic_key(code);
        }
        match code {
            KeyCode::Char('r') | KeyCode::Char('R') => {
                self.reload_from_disk();
                true
            }
            // Go semantic search entry (Ctrl+S): cosine-similarity ranking
            // via the same `bv_search` text engine `--robot-search` uses.
            KeyCode::Char('s') | KeyCode::Char('S') => {
                self.semantic_searching = true;
                self.semantic_query.clear();
                self.status_msg =
                    "Semantic search (bv-search cosine) — type to rank, Enter accepts, Esc clears"
                        .to_string();
                true
            }
            _ => false,
        }
    }

    /// Export all issues to beads_report_<project>_<date>.md (Go exportToMarkdown).
    fn export_markdown(&mut self) {
        let issues: Vec<bv_core::model::Issue> = self.issue_map.values().cloned().collect();

        let project = std::env::current_dir()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .unwrap_or_else(|| "beads".to_string());
        let sanitized: String = project
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let today = jiff::Timestamp::now().strftime("%Y-%m-%d").to_string();
        let filename = format!("beads_report_{sanitized}_{today}.md");

        let md = bv_export::mermaid::generate_markdown(&issues, "Beads Report");
        match std::fs::write(&filename, md) {
            Ok(_) => {
                self.status_msg = format!("Exported {} issues to {filename}", issues.len());
            }
            Err(e) => {
                self.status_msg = format!("Export failed: {e}");
            }
        }
    }

    /// Copy selected issue details to clipboard (Go copyIssueToClipboard).
    fn copy_issue_to_clipboard(&mut self) {
        let Some(row) = self.selected() else { return };
        let text = format!(
            "{}: {}\nStatus: {} | Priority: P{} | Type: {}\n{}",
            row.id,
            row.title,
            row.status.as_str(),
            row.priority,
            row.issue_type,
            row.description,
        );
        let id = row.id.clone();
        match copy_to_clipboard(&text) {
            Ok(()) => self.status_msg = format!("Copied {id} to clipboard"),
            Err(e) => self.status_msg = format!("Clipboard failed: {e}"),
        }
    }

    /// Copy the selected commit's SHA in the History view (Go `y`) — moved to
    /// `copy_history_commit_sha` next to the loader, because the git-mode half
    /// needs the same bead-or-commit selection Go's handler uses
    /// (model.go:5636-5658).
    /// Lazily build the Actionable view state on first `F` (same deferred
    /// pattern as History/LabelDashboard — see G15). Rebuilt from scratch on
    /// `reload_from_disk` because that reconstructs the whole `App`.
    fn load_actionable_if_needed(&mut self) {
        if self.actionable.is_some() {
            return;
        }
        let issues: Vec<bv_core::model::Issue> = self.issue_map.values().cloned().collect();
        self.actionable = Some(crate::actionable::ActionableState::new(&issues));
    }

    /// Lazily load sprints on first `P` (`.beads/sprints.jsonl` — same
    /// deferred pattern as History; previously `sprint` stayed `None`
    /// forever so the view could never show data). Errors resolve to an
    /// empty state so the view renders "No sprints defined" instead of
    /// retrying the filesystem on every toggle.
    fn load_sprint_if_needed(&mut self) {
        if self.sprint.is_some() {
            return;
        }
        let cwd = std::env::current_dir().unwrap_or_default();
        let sprints = bv_core::sprint::load_sprints(&cwd).unwrap_or_default();
        self.sprint = Some(crate::views::sprint::SprintState {
            sprints,
            selected_idx: 0,
        });
    }

    /// Keys while the agent blurb prompt is open (Go `agent_prompt_modal.go`:
    /// `←/→,h/l,Tab` move, `Enter/Space` confirm, `y/n/d` quick actions,
    /// `Esc/q` decline).
    fn handle_agent_prompt_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.decline_agent_prompt(false);
                true
            }
            KeyCode::Left | KeyCode::Char('h') => {
                if let Some(m) = self.agent_prompt_modal.as_mut() {
                    m.move_left();
                }
                true
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => {
                if let Some(m) = self.agent_prompt_modal.as_mut() {
                    m.move_right();
                }
                true
            }
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.accept_agent_prompt();
                true
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                self.decline_agent_prompt(false);
                true
            }
            KeyCode::Char('d') | KeyCode::Char('D') => {
                self.decline_agent_prompt(true);
                true
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let selected = self
                    .agent_prompt_modal
                    .as_ref()
                    .map(|m| m.selected)
                    .unwrap_or(1);
                match selected {
                    0 => self.accept_agent_prompt(),
                    2 => self.decline_agent_prompt(true),
                    _ => self.decline_agent_prompt(false),
                }
                true
            }
            _ => true, // modal captures all keys; anything else just holds it open
        }
    }

    /// Blurb prompt accept (Go `AgentPromptAccept`): inject the blurb into the
    /// detected file, record the acceptance, close the modal.
    fn accept_agent_prompt(&mut self) {
        let Some(modal) = self.agent_prompt_modal.clone() else {
            self.show_agent_prompt = false;
            return;
        };
        let path = std::path::PathBuf::from(&modal.file_path);
        let result = if modal.needs_upgrade && !self.agent_file_needs_blurb(&path) {
            bv_core::agents::file::update_blurb_in_file(&path)
        } else {
            bv_core::agents::file::append_blurb_to_file(&path)
        };
        let project_dir = std::path::PathBuf::from(&modal.project_dir);
        match result {
            Ok(()) => {
                let _ = bv_core::agents::prefs::record_accept(
                    &project_dir,
                    bv_core::agents::BLURB_VERSION,
                );
                self.status_msg = format!("Added bv instructions to {}", modal.file_type);
            }
            Err(e) => {
                self.status_msg = format!("Could not update {}: {e}", modal.file_type);
            }
        }
        self.show_agent_prompt = false;
        self.agent_prompt_modal = None;
    }

    /// Blurb prompt decline (Go `AgentPromptDecline` / `AgentPromptNeverAsk`).
    /// A plain decline only dismisses; never-ask persists via prefs.
    fn decline_agent_prompt(&mut self, never: bool) {
        if never {
            if let Some(modal) = self.agent_prompt_modal.clone() {
                let project_dir = std::path::PathBuf::from(&modal.project_dir);
                let _ = bv_core::agents::prefs::record_decline(
                    &project_dir,
                    true,
                    bv_core::agents::BLURB_VERSION,
                );
            }
            self.status_msg = "Won't ask about agent instructions again".to_string();
        }
        self.show_agent_prompt = false;
        self.agent_prompt_modal = None;
    }

    /// True when the file at `path` still lacks any blurb (used to pick
    /// append vs update on accept).
    fn agent_file_needs_blurb(&self, path: &std::path::Path) -> bool {
        std::fs::read_to_string(path)
            .map(|c| !bv_core::agents::contains_any_blurb(&c))
            .unwrap_or(true)
    }

    /// Open the cass session modal for the selected issue (Go
    /// `showCassSessionModal`): requires a healthy cass CLI, correlates
    /// with the 3s-timeout backend, shows a status message (not the modal)
    /// when cass is missing or no sessions correlate — Go's exact strings.
    fn open_cass_modal(&mut self) {
        if !self.cass_available {
            self.status_msg =
                "⚠️ cass not available (install it for session correlation)".to_string();
            return;
        }
        let Some(row) = self.selected() else {
            return;
        };
        let bead_id = row.id.clone();
        let (title, description, created_at, updated_at, closed_at) = self
            .issue_map
            .get(&bead_id)
            .map(|i| {
                (
                    i.title.clone(),
                    i.description.clone(),
                    i.created_at.clone(),
                    i.updated_at.clone(),
                    i.closed_at.clone(),
                )
            })
            .unwrap_or_default();
        let (sessions, keywords) = crate::cass::correlate(
            &bead_id,
            &title,
            &description,
            created_at.as_deref(),
            updated_at.as_deref(),
            closed_at.as_deref(),
        );
        if sessions.is_empty() {
            self.status_msg = format!("No correlated sessions found for {bead_id}");
            return;
        }
        let search_cmd = if keywords.is_empty() {
            format!("cass search \"{bead_id}\"")
        } else {
            format!("cass search \"{}\"", keywords.join(" "))
        };
        self.cass_modal = Some(crate::cass::CassModalState {
            bead_id,
            sessions,
            keywords,
            search_cmd,
            selected: 0,
            copied_at: None,
        });
    }

    /// Key handling while the cass modal is open (Go
    /// `CassSessionModal.Update` + the dismiss arm: `V`/Esc/Enter/q close,
    /// j/k navigate, `y` copies `searchCmd` with a 2s flash).
    fn handle_cass_modal_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Char('V') | KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
                self.cass_modal = None;
                true
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if let Some(m) = &mut self.cass_modal {
                    m.move_down();
                }
                true
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Some(m) = &mut self.cass_modal {
                    m.move_up();
                }
                true
            }
            KeyCode::Char('y') => {
                let cmd = self.cass_modal.as_ref().map(|m| m.search_cmd.clone());
                if let Some(cmd) = cmd {
                    match copy_to_clipboard(&cmd) {
                        Ok(()) => {
                            if let Some(m) = &mut self.cass_modal {
                                m.copied_at = Some(std::time::Instant::now());
                            }
                            self.status_msg = "Copied cass search command".to_string();
                        }
                        Err(e) => self.status_msg = format!("Clipboard failed: {e}"),
                    }
                }
                true
            }
            _ => true, // modal captures all keys; anything else holds it open
        }
    }

    /// Go's `velocityComparison` model (model.go:770, :1736, :1923). Go
    /// computes it from `analysis.ComputeAllHistoricalVelocity(issues, 4, now)`
    /// and holds it on the model, refreshing it in `SetData`; Rust rebuilds it
    /// from the live issue set each time the view is opened, which is the same
    /// data without a stale cache to invalidate.
    fn velocity_comparison(&self) -> crate::views::velocity_comparison::VelocityComparisonModel {
        let mut model =
            crate::views::velocity_comparison::VelocityComparisonModel::new(self.theme.clone());
        let issues: Vec<bv_core::model::Issue> = self.issue_map.values().cloned().collect();
        model.set_size(self.width as usize, self.height as usize);
        model.set_data(&issues);
        model
    }

    /// The two modal regions of Go's `handleHistoryKeys` — the search input
    /// (model.go:5485-5503) and the file tree (model.go:5506-5548) — plus
    /// the `esc` that closes the view (model.go:5756-5759). Returns `true`
    /// when the key was consumed, mirroring Go's early `return m, nil`.
    ///
    /// Anything not claimed here falls through to the guarded History arms in
    /// `handle_key`'s main `match`.
    fn handle_history_modal_key(&mut self, code: KeyCode) -> bool {
        // `h` and `esc` close the view whatever the state is (model.go:5756),
        // so they are checked before the `history` borrow — otherwise an
        // unpopulated History would trap the user.
        if matches!(code, KeyCode::Esc) {
            self.current_view = ViewMode::List;
            return true;
        }
        let Some(ref mut h) = self.history else {
            return false;
        };

        // --- search input ---
        if h.is_search_active() {
            match code {
                KeyCode::Esc => {
                    h.cancel_search();
                    self.status_msg = "\u{1f50d} Search cancelled".to_string();
                }
                KeyCode::Enter => {
                    // Blur but keep the query and the filtered result
                    // (Go `FinishSearch`).
                    h.finish_search();
                }
                KeyCode::Backspace => h.backspace_search(),
                KeyCode::Char(c) => h.push_search_char(c),
                _ => {}
            }
            return true;
        }

        // --- file tree ---
        if h.file_tree_has_focus() {
            match code {
                KeyCode::Char('j') | KeyCode::Down => h.move_file_tree_down(),
                KeyCode::Char('k') | KeyCode::Up => h.move_file_tree_up(),
                KeyCode::Enter | KeyCode::Char('l') => {
                    // A directory expands, a file becomes the filter
                    // (model.go:5516-5531).
                    if h.selected_file_is_dir() {
                        h.toggle_expand_file();
                    } else {
                        h.select_file();
                        self.status_msg =
                            format!("\u{1f4c1} Filtering by: {}", h.selected_file_name());
                    }
                }
                KeyCode::Char('h') => h.collapse_file_node(),
                KeyCode::Esc => {
                    if !h.file_filter().is_empty() {
                        h.clear_file_filter();
                        self.status_msg = "\u{1f4c1} File filter cleared".to_string();
                    } else {
                        h.set_file_tree_focus(false);
                        self.status_msg =
                            "\u{1f4c1} File tree: press Tab to return focus".to_string();
                    }
                }
                KeyCode::Tab => h.set_file_tree_focus(false),
                _ => {}
            }
            return true;
        }

        // `tab` cycles pane focus (model.go:5593-5608). It lives here because
        // the global `Tab` arm above toggles the list/detail split and would
        // otherwise shadow it. With the file tree marked visible Go routes Tab
        // through List→Detail→FileTree→List instead of `ToggleFocus`; the
        // file-tree panel is never rendered by Go's own layout (see
        // `render_file_tree_panel`), so this only moves the focus highlight.
        if let KeyCode::Tab = code {
            if h.is_file_tree_visible() {
                if h.file_tree_has_focus() {
                    h.set_file_tree_focus(false);
                } else if h.is_detail_focused() {
                    h.set_file_tree_focus(true);
                } else {
                    h.toggle_focus();
                }
            } else {
                h.toggle_focus();
            }
            return true;
        }

        // `o` opens the selected commit in a browser (model.go:5694-5728). Like
        // `tab`, it must beat the global `o` = "show open issues".
        if let KeyCode::Char('o') = code {
            self.open_history_commit_in_browser();
            return true;
        }

        false
    }

    /// Go `enterHistoryView` (model.go:9222-9245) + `LoadHistoryCmd`
    /// (model.go:595-671): build the *full* `correlation.HistoryReport` on a
    /// background thread with `CorrelatorOptions{Limit: 500}` ("Reasonable
    /// limit for TUI performance", model.go:659-661), then hand it to
    /// `HistoryState` via the generation-fenced channel.
    ///
    /// The narrower `correlator::correlate` path is deliberately not used: its
    /// `CorrelatedCommit` has `files: Vec<String>` (no action, no line counts)
    /// and no `method`, so it cannot back the per-file change lines, the
    /// method label, the lifecycle section or the cycle-time summary that Go
    /// renders. The report the TUI shows is therefore the same one
    /// `--robot-history` and the pages export already build.
    ///
    /// Re-entering the view after a failure retries: `history_loaded` is only
    /// set once a report actually lands.
    fn load_history_if_needed(&mut self) {
        if self.history_loaded || self.history_loading {
            return;
        }
        let Ok(cwd) = std::env::current_dir() else {
            self.history_load_failed = true;
            self.status_msg = "History: could not determine working directory".to_string();
            return;
        };
        let beads: Vec<bv_correlation::history::BeadInfo> = self
            .issue_map
            .values()
            .map(|i| bv_correlation::history::BeadInfo {
                id: i.id.clone(),
                title: i.title.clone(),
                status: i.status.as_str().to_string(),
            })
            .collect();
        if beads.is_empty() {
            // Go: "No issue history available" (model.go:9241-9244).
            self.status_msg = "No issue history available".to_string();
            return;
        }
        let opts = bv_correlation::history::HistoryOptions {
            limit: HISTORY_COMMIT_LIMIT,
            ..Default::default()
        };
        let generated_at = jiff::Timestamp::now().to_string();
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.history_generation += 1;
        let generation = self.history_generation;
        self.history_rx = Some(rx);
        self.history_loading = true;
        self.history_load_failed = false;
        self.status_msg = "Loading history…".to_string();
        // std::thread + a bounded channel; the project bans tokio (AGENTS.md).
        std::thread::spawn(move || {
            let out = bv_correlation::history::build_history_report(
                &cwd,
                &beads,
                &opts,
                None,
                generated_at,
            );
            let _ = tx.send((generation, out));
        });
    }

    /// Drain a completed background history load. Called from the run loop's
    /// ~500ms tick, so a slow `git log -p` walk never blocks a keypress.
    /// A result whose generation is stale is discarded, mirroring Go's
    /// `(dataGeneration, requestGeneration)` fencing (model.go:665-670).
    ///
    /// Returns whether the panel's visible state changed, so the event loop
    /// can skip a redraw on the ticks where the background walk is still in
    /// flight (Go: `View()` is not called until the `HistoryLoadedMsg`
    /// arrives, because the placeholder text never changes in between).
    pub fn poll_history_load(&mut self) -> bool {
        if !self.history_loading {
            return false;
        }
        let Some(rx) = &self.history_rx else {
            self.history_loading = false;
            return false;
        };
        let Ok((generation, result)) = rx.try_recv() else {
            return false;
        };
        self.history_loading = false;
        self.history_rx = None;
        if generation != self.history_generation {
            // A newer request superseded this one, so this result is stale.
            // `load_history_if_needed` will not start a replacement while a
            // load is in flight, so nothing is pending: surface the retry
            // placeholder rather than spinning on "Loading history…".
            self.history_load_failed = true;
            return true;
        }
        match result {
            Ok(report) => {
                let beads_with_commits = report.stats.beads_with_commits;
                match self.history.as_mut() {
                    Some(h) if h.has_report() => h.set_report(report),
                    _ => {
                        self.history =
                            Some(crate::views::history::HistoryState::from_report(report))
                    }
                }
                self.history_loaded = true;
                self.history_load_failed = false;
                self.status_msg =
                    format!("Loaded history: {beads_with_commits} beads with commits");
            }
            Err(e) => {
                self.history_load_failed = true;
                self.status_msg = format!("History unavailable: {e}");
            }
        }
        true
    }

    /// Go `y` — copy the selected commit's SHA, in either view mode
    /// (model.go:5636-5658).
    fn copy_history_commit_sha(&mut self) {
        let Some(ref history) = self.history else {
            return;
        };
        let (sha, short_sha) = if history.is_git_mode() {
            match history.selected_git_commit() {
                Some(c) => (c.sha.clone(), c.short_sha.clone()),
                None => {
                    self.status_msg = "\u{274c} No commit selected".to_string();
                    return;
                }
            }
        } else {
            match history.selected_commit() {
                Some(c) => (c.sha.clone(), c.short_sha.clone()),
                None => {
                    self.status_msg = "\u{274c} No commit selected".to_string();
                    return;
                }
            }
        };
        match copy_to_clipboard(&sha) {
            Ok(()) => self.status_msg = format!("\u{1f4cb} Copied {short_sha} to clipboard"),
            Err(e) => self.status_msg = format!("Clipboard failed: {e}"),
        }
    }

    /// Go `o` — open the selected commit in a browser (model.go:5694-5728).
    /// Resolves `git remote get-url origin` → web URL → `<web>/commit/<sha>`
    /// (model.go:575-582).
    fn open_history_commit_in_browser(&mut self) {
        let Some(ref history) = self.history else {
            return;
        };
        let sha = if history.is_git_mode() {
            history.selected_git_commit().map(|c| c.sha.clone())
        } else {
            history.selected_commit().map(|c| c.sha.clone())
        };
        let Some(sha) = sha else {
            self.status_msg = "\u{274c} No commit selected".to_string();
            return;
        };
        let cwd = std::env::current_dir().unwrap_or_default();
        let output = std::process::Command::new("git")
            .args(["remote", "get-url", "origin"])
            .current_dir(&cwd)
            .output();
        let web_url = match output {
            Ok(o) if o.status.success() => {
                git_remote_to_web_url(&String::from_utf8_lossy(&o.stdout))
            }
            _ => None,
        };
        let Some(web_url) = web_url else {
            self.status_msg = "\u{274c} No git remote configured".to_string();
            return;
        };
        let url = format!("{web_url}/commit/{sha}");
        match open_browser_url(&url) {
            Ok(()) => {
                let short: String = sha.chars().take(7).collect();
                self.status_msg = format!("\u{1f310} Opened {short} in browser");
            }
            Err(e) => self.status_msg = format!("\u{274c} Could not open browser: {e}"),
        }
    }

    /// Go `g` — reveal the selected bead and jump to the Graph view
    /// (model.go:5729-5755).
    fn goto_history_graph(&mut self) {
        let Some(ref history) = self.history else {
            return;
        };
        let bead_id = if history.is_git_mode() {
            history.selected_related_bead_id().to_string()
        } else {
            history.selected_bead_id().to_string()
        };
        if bead_id.is_empty() {
            self.status_msg = "\u{274c} No bead selected".to_string();
            return;
        }
        self.reveal_issue(&bead_id);
        self.select_issue_in_list(&bead_id);
        self.current_view = ViewMode::Graph;
        self.status_msg = format!("\u{1f4ca} Graph view: {bead_id}");
    }

    /// Go `enter` — reveal the selected bead in the main list and leave
    /// History (model.go:5609-5635).
    fn jump_from_history(&mut self) {
        let Some(ref history) = self.history else {
            return;
        };
        let bead_id = if history.is_git_mode() {
            history.selected_related_bead_id().to_string()
        } else {
            history.selected_bead_id().to_string()
        };
        if bead_id.is_empty() {
            return;
        }
        self.reveal_issue(&bead_id);
        self.select_issue_in_list(&bead_id);
        self.current_view = ViewMode::List;
        self.show_detail = true;
        self.focus_detail = true;
    }

    /// Select `id` in the filtered list if it is present (Go's `m.list.Select`
    /// loop inside the `enter` and `g` handlers). `cursor` indexes
    /// `filtered_indices`.
    fn select_issue_in_list(&mut self, id: &str) {
        if let Some(pos) = self
            .filtered_indices
            .iter()
            .position(|&i| self.rows[i].id == id)
        {
            self.cursor = pos;
        }
    }

    /// Go `revealRecipeIssue` — clear filters that would hide the target so
    /// the bead is actually visible in the list.
    fn reveal_issue(&mut self, id: &str) {
        if self.filtered_indices.iter().any(|&i| self.rows[i].id == id) {
            return;
        }
        if self.issue_map.contains_key(id) {
            self.filter_mode = FilterMode::All;
            self.label_filter = None;
            self.search_query.clear();
            self.apply_filter();
        }
    }

    /// Number of flattened entries in the current Time-Travel diff
    /// (added + removed + changed), for cursor clamping.
    fn time_travel_entry_count(&self) -> usize {
        self.time_travel_result
            .as_ref()
            .map_or(0, |r| r.added.len() + r.removed.len() + r.changed.len())
    }

    /// Pure compute+store half of a Time-Travel diff (testable without git):
    /// diff `previous` (issues at `git_ref`) against the loaded issue set.
    fn apply_time_travel_result(&mut self, previous: Vec<bv_core::model::Issue>, git_ref: &str) {
        let current: Vec<bv_core::model::Issue> = self.issue_map.values().cloned().collect();
        let n_prev = previous.len();
        let result = bv_analysis::diff::diff_issues(&current, &previous, git_ref);
        self.time_travel_cursor = 0;
        self.time_travel_error = None;
        self.status_msg = format!(
            "Time-Travel vs {git_ref}: +{} -{} ~{} (from {n_prev} issues at ref)",
            result.added_count, result.removed_count, result.changed_count
        );
        self.time_travel_result = Some(result);
    }

    /// Fetch issues at `git_ref` via the shared `GitLoader` plumbing (the
    /// same path `--robot-diff` uses — see plan Q4) and store the diff.
    /// Error text mirrors the robot command's
    /// (`could not read issues at ref <ref>`).
    fn run_time_travel(&mut self, git_ref: &str) {
        let cwd = std::env::current_dir().unwrap_or_default();
        match bv_core::discovery::GitLoader::new(&cwd).load_at(git_ref) {
            Ok(previous) => self.apply_time_travel_result(previous, git_ref),
            Err(_) => {
                self.time_travel_result = None;
                self.time_travel_error = Some(format!("could not read issues at ref {git_ref}"));
                self.status_msg = format!("Time-Travel: could not read issues at ref {git_ref}");
            }
        }
    }

    /// Key handling while the Time-Travel revision prompt is open: typing
    /// edits the buffer, Enter submits, Esc cancels (Go `focusTimeTravelInput`).
    fn handle_time_travel_prompt_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Esc => {
                self.time_travel_prompt = None;
                if self.time_travel_result.is_none() {
                    self.current_view = ViewMode::List;
                }
                true
            }
            KeyCode::Enter => {
                let rev = self
                    .time_travel_prompt
                    .clone()
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if rev.is_empty() {
                    return true; // keep prompt open; nothing to diff
                }
                self.time_travel_prompt = None;
                self.run_time_travel(&rev);
                true
            }
            KeyCode::Backspace => {
                if let Some(buf) = &mut self.time_travel_prompt {
                    buf.pop();
                }
                true
            }
            KeyCode::Char(c) => {
                if let Some(buf) = &mut self.time_travel_prompt {
                    buf.push(c);
                }
                true
            }
            _ => true,
        }
    }
    /// Lazily compute per-label health (Go `focusLabelDashboard` data).
    /// Deferred to first `[` press like History — same analysis the
    /// `--robot-label-health` path uses, computed once per session.
    fn load_label_health_if_needed(&mut self) {
        if self.label_health_loaded {
            return;
        }
        self.label_health_loaded = true; // don't retry every keypress on failure
        let issues: Vec<bv_core::model::Issue> = self.issue_map.values().cloned().collect();
        let cfg = bv_analysis::label_health::LabelHealthConfig::default();
        let now = jiff::Timestamp::now();
        let result = bv_analysis::label_health::compute_all_label_health(&issues, &cfg, now);
        self.status_msg = format!("Label dashboard: {} labels", result.total_labels);
        self.label_health = Some(result);
    }

    fn label_dashboard_count(&self) -> usize {
        self.label_health.as_ref().map_or(0, |r| r.labels.len())
    }

    /// Display order: worst health first, label asc tiebreak (so the
    /// labels needing attention sit at the top, Go dashboard style).
    fn label_dashboard_order(&self) -> Vec<usize> {
        let mut idx: Vec<usize> = (0..self.label_dashboard_count()).collect();
        if let Some(r) = &self.label_health {
            idx.sort_by(|&a, &b| {
                r.labels[a]
                    .health
                    .cmp(&r.labels[b].health)
                    .then_with(|| r.labels[a].label.cmp(&r.labels[b].label))
            });
        }
        idx
    }

    fn selected_dashboard_label(&self) -> Option<&bv_analysis::label_health::LabelHealth> {
        let order = self.label_dashboard_order();
        order
            .get(self.label_dashboard_cursor)
            .and_then(|&i| self.label_health.as_ref().map(|r| &r.labels[i]))
    }

    fn open_label_drilldown(&mut self) {
        if let Some(label) = self.selected_dashboard_label().map(|l| l.label.clone()) {
            // One overlay at a time (same convention as render_overlays).
            self.show_label_detail = false;
            self.label_drilldown = Some(LabelDrilldown {
                label,
                filter: String::new(),
                cursor: 0,
            });
        }
    }

    /// (id, title, status) rows for the open drilldown, substring-filtered
    /// on id+title and sorted by id (same filter semantics as `/` search).
    fn drilldown_rows(&self) -> Vec<(String, String, bv_core::model::Status)> {
        let Some(dd) = &self.label_drilldown else {
            return Vec::new();
        };
        let q = dd.filter.to_lowercase();
        let mut rows: Vec<(String, String, bv_core::model::Status)> = self
            .issue_map
            .values()
            .filter(|i| i.labels.iter().any(|l| l == &dd.label))
            .filter(|i| {
                q.is_empty()
                    || i.id.to_lowercase().contains(&q)
                    || i.title.to_lowercase().contains(&q)
            })
            .map(|i| (i.id.clone(), i.title.clone(), i.status))
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        rows
    }

    /// Key handling while the drilldown overlay is open: picker convention
    /// (`j`/`k` navigate, other chars filter, `Enter` jumps, `Esc` closes).
    fn handle_label_drilldown_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Esc => {
                self.label_drilldown = None;
                true
            }
            KeyCode::Enter => {
                // Jump to List filtered by this label (Go drilldown Enter).
                if let Some(dd) = self.label_drilldown.take() {
                    self.label_filter = Some(dd.label.clone());
                    self.current_view = ViewMode::List;
                    self.apply_filter();
                    self.status_msg = format!("Filtered by label: {}", dd.label);
                }
                true
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(dd) = &mut self.label_drilldown {
                    dd.cursor = dd.cursor.saturating_sub(1);
                }
                true
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let max = self.drilldown_rows().len();
                if let Some(dd) = &mut self.label_drilldown {
                    if max > 0 && dd.cursor + 1 < max {
                        dd.cursor += 1;
                    }
                }
                true
            }
            KeyCode::Backspace => {
                if let Some(dd) = &mut self.label_drilldown {
                    dd.filter.pop();
                    dd.cursor = 0;
                }
                true
            }
            KeyCode::Char(c) => {
                if let Some(dd) = &mut self.label_drilldown {
                    dd.filter.push(c);
                    dd.cursor = 0;
                }
                true
            }
            _ => true,
        }
    }

    /// Open selected issue in $EDITOR (Go "O" edit).
    fn open_in_editor(&mut self) {
        let Some(row) = self.selected() else { return };
        let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
        let path = std::env::temp_dir().join(format!("bead_{}.md", row.id));
        let body = format!(
            "# {}: {}\n\n## Description\n\n{}\n\n## Notes\n\n{}\n",
            row.id, row.title, row.description, row.notes
        );
        if std::fs::write(&path, &body).is_ok() {
            let _ = std::process::Command::new(&editor).arg(&path).status();
            self.status_msg = format!("Opened {} in {editor}", row.id);
        }
    }

    /// Cycle label filter through available labels (Go label picker, simplified).
    fn cycle_label_filter(&mut self) {
        let mut labels: Vec<String> = self
            .rows
            .iter()
            .flat_map(|r| r.labels.iter().cloned())
            .collect();
        labels.sort();
        labels.dedup();
        if labels.is_empty() {
            self.status_msg = "No labels".to_string();
            return;
        }
        match &self.label_filter {
            None => {
                self.label_filter = Some(labels[0].clone());
                self.status_msg = format!("Filter: label={}", labels[0]);
            }
            Some(current) => match labels.iter().position(|l| l == current) {
                Some(i) if i + 1 < labels.len() => {
                    self.label_filter = Some(labels[i + 1].clone());
                    self.status_msg = format!("Filter: label={}", labels[i + 1]);
                }
                _ => {
                    self.label_filter = None;
                    self.status_msg = "Filter: all labels".to_string();
                }
            },
        }
        self.apply_filter();
    }

    /// Reload issues from disk (Go Ctrl+R refresh).
    pub fn reload_from_disk(&mut self) {
        let cwd = std::env::current_dir().unwrap_or_default();
        match bv_core::discovery::load_issues_from_repo(&cwd) {
            Ok((issues, _)) => {
                let fresh = App::new(issues);
                *self = fresh;
                self.status_msg = "Refreshed".to_string();
            }
            Err(e) => {
                self.status_msg = format!("Refresh failed: {e}");
            }
        }
    }
}

fn type_icon(issue_type: &str) -> (&'static str, Color) {
    match issue_type {
        "bug" => ("🐛", Color::Red),
        "feature" => ("✨", Color::Green),
        "task" => ("📋", Color::Blue),
        "epic" => ("🚀", Color::Magenta),
        "chore" => ("🧹", Color::Gray),
        _ => ("•", Color::DarkGray),
    }
}

fn prio_badge_style(priority: i32) -> (Color, Color) {
    match priority {
        0 => (Color::White, Color::Red),
        1 => (Color::White, Color::LightRed),
        2 => (Color::Black, Color::Yellow),
        3 => (Color::White, Color::Blue),
        _ => (Color::Gray, Color::DarkGray),
    }
}

fn status_badge(status: &str) -> (&'static str, Color) {
    match status {
        "open" => ("OPEN", Color::Green),
        "in_progress" => ("PROG", Color::Yellow),
        "blocked" => ("BLKD", Color::Red),
        "deferred" => ("DEFR", Color::Cyan),
        "draft" => ("DRFT", Color::Cyan),
        "pinned" => ("PIN", Color::Magenta),
        "hooked" => ("HOOK", Color::Cyan),
        "review" => ("REVW", Color::Blue),
        "closed" => ("DONE", Color::DarkGray),
        "tombstone" => ("TOMB", Color::DarkGray),
        _ => ("????", Color::Gray),
    }
}

fn age_str(created_at: &Option<String>) -> String {
    if let Some(ca) = created_at {
        if let Ok(t) = ca.parse::<jiff::Timestamp>() {
            let now = jiff::Timestamp::now();
            let days = (now - t).total(jiff::Unit::Second).unwrap_or(0.0) / 86400.0;
            if days < 1.0 {
                return "today".into();
            }
            if days < 30.0 {
                return format!("{days:.0}d");
            }
            if days < 365.0 {
                return format!("{:.0}w", days / 7.0);
            }
            return format!("{:.0}y", days / 365.0);
        }
    }
    String::new()
}

pub fn render(f: &mut Frame, app: &mut App) {
    match app.current_view {
        ViewMode::Tree => {
            // Borrow, don't clone: the tree walks `id`/`title`/`dependencies`
            // only, and `.values().cloned().collect()` was deep-copying every
            // large text field (~3 MB on ultraworkers) on every frame to do it.
            let issues: Vec<&bv_core::model::Issue> = app.issue_map.values().collect();
            let nodes = crate::views::tree::build_tree_nodes(&issues, &app.tree_collapsed);
            // The tree has no cursor, so the window is always anchored at the
            // top: build only the rows that fit, which is all a
            // non-scrolled `Paragraph` would paint anyway.
            let height = usize::from(f.area().height.saturating_sub(2));
            let window = crate::views::tree::visible_range(nodes.len(), 0, height);
            let lines = crate::views::tree::render_tree_lines(&nodes, window);
            let block = ratatui::widgets::Block::default()
                .borders(ratatui::widgets::Borders::ALL)
                .title(" TREE VIEW ");
            f.render_widget(
                ratatui::widgets::Paragraph::new(lines).block(block),
                f.area(),
            );
            render_status_bar(f, app);
            render_overlays(f, app);
            return;
        }
        ViewMode::Insights => {
            let empty = BTreeMap::new();
            let (pr, bw, hub, auth) = if let Some(ref gm) = app.graph_metrics {
                (&gm.pagerank, &gm.betweenness, &gm.hubs, &gm.authorities)
            } else {
                (&empty, &empty, &empty, &empty)
            };
            crate::views::insights::render_insights(f, pr, bw, hub, auth);
            render_status_bar(f, app);
            render_overlays(f, app);
            return;
        }
        ViewMode::Alerts => {
            crate::views::alerts::render_alerts(f, &app.alerts, app.alerts_cursor, f.area());
            render_status_bar(f, app);
            render_overlays(f, app);
            return;
        }
        ViewMode::FlowMatrix => {
            if let Some(ref flow) = app.flow {
                crate::views::flow_matrix::render_flow_matrix(f, flow, app.flow_cursor, f.area());
            } else {
                let msg = ratatui::widgets::Paragraph::new("No flow data available");
                f.render_widget(msg, f.area());
            }
            render_status_bar(f, app);
            render_overlays(f, app);
            return;
        }
        ViewMode::Attention => {
            crate::views::attention::render_attention(
                f,
                &app.attention_labels,
                app.attention_cursor,
                f.area(),
            );
            render_status_bar(f, app);
            render_overlays(f, app);
            return;
        }
        ViewMode::Tutorial => {
            if let Some(ref t) = app.tutorial {
                crate::tutorial::render_tutorial(f, t, f.area());
            } else {
                let msg = ratatui::widgets::Paragraph::new("No tutorial content");
                f.render_widget(msg, f.area());
            }
            render_status_bar(f, app);
            render_overlays(f, app);
            return;
        }
        ViewMode::Actionable => {
            if let Some(ref state) = app.actionable {
                crate::actionable::render_actionable(f, &state.items, state.selected, f.area());
            } else {
                let msg = ratatui::widgets::Paragraph::new("No actionable items");
                f.render_widget(msg, f.area());
            }
            render_status_bar(f, app);
            render_overlays(f, app);
            return;
        }
        ViewMode::Board => {
            crate::views::board::render_board(f, app, f.area(), app.board_mode, app.board_column);
            render_status_bar(f, app);
            render_overlays(f, app);
            return;
        }
        ViewMode::Graph => {
            if let Some(ref graph) = app.graph_data {
                crate::views::graph::render_graph(
                    f,
                    graph,
                    app.graph_cursor,
                    app.graph_scroll,
                    f.area(),
                );
            } else {
                // Build graph data on-the-fly if not cached
                let issues: Vec<bv_core::model::Issue> = app.issue_map.values().cloned().collect();
                let graph =
                    crate::views::graph::GraphData::build(issues, app.graph_metrics.clone());
                crate::views::graph::render_graph(
                    f,
                    &graph,
                    app.graph_cursor,
                    app.graph_scroll,
                    f.area(),
                );
            }
            render_status_bar(f, app);
            render_overlays(f, app);
            return;
        }
        ViewMode::Sprint => {
            if let Some(ref sprint_state) = app.sprint {
                // Borrow, don't clone — see `render_sprint`'s signature note.
                let issues: Vec<&bv_core::model::Issue> = app.issue_map.values().collect();
                crate::views::sprint::render_sprint(f, sprint_state, &issues, f.area());
            } else {
                let msg = ratatui::widgets::Paragraph::new("No sprint data available");
                f.render_widget(msg, f.area());
            }
            if app.show_velocity {
                let model = app.velocity_comparison();
                crate::views::velocity_comparison::render_velocity_comparison(f, &model, f.area());
            }
            render_status_bar(f, app);
            render_overlays(f, app);
            return;
        }
        ViewMode::History => {
            // Go model.go:6353-6364: while the background correlation walk is
            // in flight (or after it failed) the whole view is replaced by a
            // centered placeholder rather than the panels.
            if !app.history_loaded || app.history_load_failed {
                let message = if app.history_load_failed {
                    "History unavailable; press h to retry"
                } else {
                    "Loading history…"
                };
                let block = ratatui::widgets::Paragraph::new(message)
                    .alignment(ratatui::layout::Alignment::Center)
                    .style(ratatui::style::Style::default().fg(app.theme.secondary));
                let rows = ratatui::layout::Layout::vertical([
                    ratatui::layout::Constraint::Fill(1),
                    ratatui::layout::Constraint::Length(1),
                    ratatui::layout::Constraint::Fill(1),
                ])
                .split(f.area());
                f.render_widget(block, rows[1]);
            } else if let Some(ref mut history) = app.history {
                crate::views::history::render_history(f, history, &app.theme, f.area());
            } else {
                crate::views::history::render_empty(
                    f,
                    &app.theme,
                    f.area(),
                    "No history data loaded",
                );
            }
            render_status_bar(f, app);
            render_overlays(f, app);
            return;
        }
        ViewMode::TimeTravel => {
            render_time_travel(f, app);
            render_status_bar(f, app);
            render_overlays(f, app);
            return;
        }
        ViewMode::LabelDashboard => {
            render_label_dashboard(f, app);
            render_status_bar(f, app);
            render_overlays(f, app);
            return;
        }
        _ => {}
    }

    let chunks = if app.split_view && app.width > 100 {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(f.area())
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(100)])
            .split(f.area())
    };

    render_list(f, app, chunks[0]);
    if app.split_view && app.width > 100 && chunks.len() > 1 {
        render_detail(f, app, chunks[1]);
    }

    if app.show_sidebar {
        render_sidebar(f, app);
    }

    render_status_bar(f, app);
    render_overlays(f, app);
}

/// Label Dashboard view (Go `focusLabelDashboard`): per-label health
/// badges computed by `bv_analysis::label_health` (thresholds
/// Healthy ≥ 70 / Warning ≥ 40 / else Critical live in that crate, shared
/// with `--robot-label-health`), worst-first order, `h` detail modal and
/// `d` drilldown overlay for the cursor label.
fn render_label_dashboard(f: &mut Frame, app: &App) {
    let area = f.area();
    let order = app.label_dashboard_order();
    let mut lines: Vec<Line> = vec![Line::from(Span::styled(
        " Label Dashboard ",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    ))];
    let Some(result) = &app.label_health else {
        lines.push(Line::from(""));
        lines.push(Line::from("No label data (press [ to load)."));
        let msg = ratatui::widgets::Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(" LABELS "));
        f.render_widget(msg, area);
        return;
    };
    lines.push(Line::from(format!(
        "{} labels: {} healthy  {} warning  {} critical   (j/k move, h detail, d issues, [ close)",
        result.total_labels, result.healthy_count, result.warning_count, result.critical_count
    )));
    lines.push(Line::from(""));
    if order.is_empty() {
        lines.push(Line::from("No labeled issues."));
    }
    for (row, &i) in order.iter().enumerate() {
        let l = &result.labels[i];
        let (badge, color) = match l.health_level {
            "healthy" => ("●", Color::Green),
            "warning" => ("●", Color::Yellow),
            _ => ("●", Color::Red),
        };
        let blocked_ratio = if l.issue_count > 0 {
            l.blocked as f64 / l.issue_count as f64 * 100.0
        } else {
            0.0
        };
        let text = format!(
            "{} {:<24} {:>3}  open {:>3}/{:<3}  blocked {:>4.0}%  {}",
            badge, l.label, l.health, l.open_count, l.issue_count, blocked_ratio, l.health_level
        );
        if row == app.label_dashboard_cursor {
            lines.push(Line::from(vec![
                Span::styled(
                    "> ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    text,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!(" {badge}"), Style::default().fg(color)),
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(format!("{badge} "), Style::default().fg(color)),
                Span::raw(text),
            ]));
        }
    }
    let msg = ratatui::widgets::Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(" LABELS "));
    f.render_widget(msg, area);
    if app.show_label_detail {
        render_label_detail_modal(f, app);
    }
    if app.label_drilldown.is_some() {
        render_label_drilldown(f, app);
    }
}

/// Health-detail modal for the cursor label: velocity / staleness /
/// blocked-ratio / work-distribution breakdown (Go `h` modal).
fn render_label_detail_modal(f: &mut Frame, app: &App) {
    let Some(l) = app.selected_dashboard_label() else {
        return;
    };
    let blocked_ratio = if l.issue_count > 0 {
        l.blocked as f64 / l.issue_count as f64 * 100.0
    } else {
        0.0
    };
    let body: Vec<Line> = vec![
        Line::from(Span::styled(
            format!(" {}  ({} {})", l.label, l.health, l.health_level),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(format!(
            "Work: {} open / {} closed / {} total, {} blocked ({:.0}%)",
            l.open_count, l.closed_count, l.issue_count, l.blocked, blocked_ratio
        )),
        Line::from(format!(
            "Velocity: {} closed/7d, {} closed/30d, avg {:.1}d to close (score {}) — {} {:.0}%",
            l.velocity.closed_last_7_days,
            l.velocity.closed_last_30_days,
            l.velocity.avg_days_to_close,
            l.velocity.velocity_score,
            l.velocity.trend_direction,
            l.velocity.trend_percent
        )),
        Line::from(format!(
            "Freshness: {} stale, avg {:.1}d since update (score {})",
            l.freshness.stale_count, l.freshness.avg_days_since_update, l.freshness.freshness_score
        )),
        Line::from(format!(
            "Flow: {} in / {} out deps, {} blocked-by-external, {} blocking-external (score {})",
            l.flow.incoming_deps,
            l.flow.outgoing_deps,
            l.flow.blocked_by_external,
            l.flow.blocking_external,
            l.flow.flow_score
        )),
        Line::from(""),
        Line::from(Span::styled(
            " h / esc = close ",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    let w = 72.min(app.width.saturating_sub(4));
    let h = (body.len() as u16 + 2).min(app.height.saturating_sub(2));
    let popup = ratatui::layout::Rect {
        x: app.width.saturating_sub(w) / 2,
        y: app.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    };
    f.render_widget(ratatui::widgets::Clear, popup);
    f.render_widget(
        ratatui::widgets::Paragraph::new(body).block(
            ratatui::widgets::Block::default()
                .borders(ratatui::widgets::Borders::ALL)
                .title(format!(" LABEL: {} ", l.label))
                .border_style(Style::default().fg(Color::Yellow)),
        ),
        popup,
    );
}

/// Drilldown overlay: filtered issue list for one label; `Enter` jumps to
/// List filtered by that label (Go `d` drilldown).
fn render_label_drilldown(f: &mut Frame, app: &App) {
    let Some(dd) = &app.label_drilldown else {
        return;
    };
    let rows = app.drilldown_rows();
    let mut body: Vec<Line> = vec![Line::from(vec![
        Span::raw("Filter: "),
        Span::raw(dd.filter.clone()),
        Span::styled("█", Style::default().fg(Color::Yellow)),
    ])];
    let visible = app.height.saturating_sub(10).max(1) as usize;
    let start = dd.cursor.saturating_sub(visible.saturating_sub(1));
    for (row, (id, title, _)) in rows.iter().enumerate().skip(start).take(visible) {
        let text = format!("{id}  {title}");
        if row == dd.cursor {
            body.push(Line::from(vec![
                Span::styled(
                    "> ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    text,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
        } else {
            body.push(Line::from(format!("  {text}")));
        }
    }
    if rows.is_empty() {
        body.push(Line::from("(no issues match)"));
    }
    body.push(Line::from(""));
    body.push(Line::from(Span::styled(
        " Enter = list filtered by label   Esc = close ",
        Style::default().fg(Color::DarkGray),
    )));
    let w = 72.min(app.width.saturating_sub(4));
    let h = (body.len() as u16 + 2).min(app.height.saturating_sub(2));
    let popup = ratatui::layout::Rect {
        x: app.width.saturating_sub(w) / 2,
        y: app.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    };
    f.render_widget(ratatui::widgets::Clear, popup);
    f.render_widget(
        ratatui::widgets::Paragraph::new(body).block(
            ratatui::widgets::Block::default()
                .borders(ratatui::widgets::Borders::ALL)
                .title(format!(" LABEL: {} ({} issues) ", dd.label, rows.len()))
                .border_style(Style::default().fg(Color::Cyan)),
        ),
        popup,
    );
}

/// Time-Travel view (Go `focusTimeTravelInput` → `SnapshotDiff`): revision
/// prompt plus the added/removed/changed diff vs that git revision, backed
/// by `bv_analysis::diff::diff_issues` over `GitLoader::load_at` output.
fn render_time_travel(f: &mut Frame, app: &App) {
    let area = f.area();
    let mut head: Vec<Line> = vec![Line::from(Span::styled(
        " Time-Travel ",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    ))];
    if let Some(buf) = &app.time_travel_prompt {
        head.push(Line::from(""));
        head.push(Line::from("Revision (branch / tag / SHA / HEAD~N):"));
        head.push(Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::Yellow)),
            Span::raw(buf.clone()),
            Span::styled("█", Style::default().fg(Color::Yellow)),
        ]));
        head.push(Line::from(Span::styled(
            " Enter = diff   Esc = cancel",
            Style::default().fg(Color::DarkGray),
        )));
    }
    // Flattened navigable entries: added, then removed, then changed.
    let mut entries: Vec<Line> = Vec::new();
    let mut title = " TIME-TRAVEL ".to_string();
    match (&app.time_travel_result, &app.time_travel_error) {
        (Some(r), _) => {
            title = format!(" TIME-TRAVEL vs {} ", r.diff_ref);
            head.push(Line::from(""));
            head.push(Line::from(format!(
                "+{} added   -{} removed   ~{} changed   (j/k navigate, t new revision, T HEAD~5, esc back)",
                r.added_count, r.removed_count, r.changed_count
            )));
            if r.added_count == 0 && r.removed_count == 0 && r.changed_count == 0 {
                head.push(Line::from(
                    "No differences — working set matches this revision.",
                ));
            }
            let mut idx = 0;
            let mut push_entry = |idx: &mut usize, text: String| {
                let (marker, style) = if *idx == app.time_travel_cursor {
                    (
                        "> ",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    ("  ", Style::default())
                };
                entries.push(Line::from(vec![
                    Span::styled(marker, style),
                    Span::raw(text),
                ]));
                *idx += 1;
            };
            for id in &r.added {
                push_entry(&mut idx, format!("+ {id}"));
            }
            for id in &r.removed {
                push_entry(&mut idx, format!("- {id}"));
            }
            for c in &r.changed {
                let mut detail = String::new();
                if let Some(sc) = &c.status_change {
                    detail.push_str(&format!(" [{sc}]"));
                }
                if c.title_changed {
                    detail.push_str(" [title]");
                }
                if c.priority_changed {
                    detail.push_str(" [priority]");
                }
                push_entry(&mut idx, format!("~ {}{}", c.id, detail));
            }
        }
        (None, Some(e)) => {
            head.push(Line::from(""));
            head.push(Line::from(Span::styled(
                format!("Error: {e}"),
                Style::default().fg(Color::Red),
            )));
            head.push(Line::from("Press t for another revision, T for HEAD~5."));
        }
        (None, None) => {
            if app.time_travel_prompt.is_none() {
                head.push(Line::from(""));
                head.push(Line::from(
                    "Press t to enter a revision, T for instant HEAD~5.",
                ));
            }
        }
    }
    // Window the entry list around the cursor so huge diffs fit the view.
    let chrome = head.len() + 5; // borders + status bar + overflow line
    let visible = area
        .height
        .saturating_sub(chrome.min(u16::MAX as usize) as u16)
        .max(1) as usize;
    let start = app
        .time_travel_cursor
        .saturating_sub(visible.saturating_sub(1));
    let mut lines = head;
    lines.extend(entries.into_iter().skip(start).take(visible));
    let total = app.time_travel_entry_count();
    if total > visible {
        lines.push(Line::from(Span::styled(
            format!(
                "… {} of {total} entries (j/k to scroll)",
                visible.min(total)
            ),
            Style::default().fg(Color::DarkGray),
        )));
    }
    let msg = ratatui::widgets::Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(msg, area);
}

/// Modal/popup overlays shown on top of whichever view is active: help,
/// update-available modal, and the label/recipe/repo pickers.
///
/// Previously the `?` help overlay (and nothing else) was drawn only in the
/// code path reached after the big `match app.current_view` in `render()`
/// fell through — which every non-List view `return`s before reaching, so
/// `?` silently did nothing while inside Board/Tree/Graph/Insights/Alerts/
/// FlowMatrix/Attention/Actionable/Sprint/History/TimeTravel (see
/// TUI_UX_PARITY_PLAN.md G8). Centralizing overlay rendering here and
/// calling it from every early-return branch fixes that for all of them at
/// once.
fn render_overlays(f: &mut Frame, app: &mut App) {
    // Help overlay (Go "?" help) — now generated from `key_registry`
    // instead of a separately hand-maintained literal list, so it can't
    // drift from the registry the way the old hardcoded text had (e.g. it
    // used to say "g Toggle graph view" while the real binding was `G`).
    if app.show_help {
        let focus = focus_for_view(app.current_view);
        let bindings = app.key_registry.bindings_for(focus);
        let mut help_lines: Vec<Line> = vec![
            Line::from(Span::styled(
                " Keyboard Shortcuts ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
        ];
        if bindings.is_empty() {
            help_lines.push(Line::from("  (no bindings documented for this view yet)"));
        } else {
            for b in bindings {
                help_lines.push(Line::from(format!("  {:<12} {}", b.key, b.desc)));
            }
        }
        help_lines.push(Line::from(""));
        help_lines.push(Line::from(Span::styled(
            " Press any key to close ",
            Style::default().fg(Color::DarkGray),
        )));
        let w = 56.min(app.width.saturating_sub(4));
        let h = (help_lines.len() as u16 + 2).min(app.height.saturating_sub(2));
        let x = (app.width.saturating_sub(w)) / 2;
        let y = (app.height.saturating_sub(h)) / 2;
        let popup = ratatui::layout::Rect {
            x,
            y,
            width: w,
            height: h,
        };
        f.render_widget(ratatui::widgets::Clear, popup);
        f.render_widget(
            ratatui::widgets::Paragraph::new(help_lines).block(
                ratatui::widgets::Block::default()
                    .borders(ratatui::widgets::Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            ),
            popup,
        );
        return; // one overlay at a time, matching prior behavior
    }

    if app.show_update_modal {
        if let Some(modal) = app.update_modal.as_mut() {
            // Go sizes from the live terminal (`m.updateModal.SetSize(m.width,
            // m.height)`, model.go:9664). `App::width`/`height` are only
            // written by `--debug-render`, so take the real size from the
            // frame; re-applying it each frame also survives a resize.
            let area = f.area();
            modal.set_size(area.width, area.height);
            modal.render(f, area);
        }
        return;
    }

    if app.show_agent_prompt {
        if let Some(modal) = &app.agent_prompt_modal {
            crate::agent_prompt_modal::render_agent_prompt(f, modal, f.area());
        }
        return;
    }

    if let Some(picker) = &app.label_picker {
        if picker.visible {
            picker.render(f, f.area());
            return;
        }
    }
    if let Some(picker) = &app.recipe_picker {
        if picker.visible {
            picker.render(f, f.area());
            return;
        }
    }
    if let Some(picker) = &app.repo_picker {
        if picker.visible {
            picker.render(f, f.area());
        }
    }

    // Cass session modal (Go `CassSessionModal.View` centered via
    // `CenterModal`): header with bead ID, up to 3 session cards (agent •
    // relative time, match reason, snippet), "(N more — run: <cmd>)" line,
    // footer with bindings or the 2s "✓ Copied!" flash.
    if let Some(modal) = &app.cass_modal {
        let now = jiff::Timestamp::now();
        let mut lines: Vec<Line> = vec![
            Line::from(vec![
                Span::styled(
                    "📎 Related Coding Sessions  ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(modal.bead_id.clone(), Style::default().fg(Color::DarkGray)),
            ]),
            Line::from(""),
        ];
        let shown = modal.display_count();
        for (i, s) in modal.sessions.iter().take(shown).enumerate() {
            let agent = if s.session.agent.is_empty() {
                "Unknown"
            } else {
                s.session.agent.as_str()
            };
            let when = crate::cass::format_relative_time(&s.session.timestamp, now);
            let header = format!("[{}] {agent} • {when}", i + 1);
            lines.push(Line::from(if i == modal.selected {
                Span::styled(
                    header,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::raw(header)
            }));
            lines.push(Line::from(Span::styled(
                format!(
                    "    {}",
                    crate::cass::format_match_reason(s, &modal.bead_id)
                ),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )));
            let snippet = if s.session.snippet.trim().is_empty() {
                "(no preview available)".to_string()
            } else {
                s.session
                    .snippet
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .take(3)
                    .collect::<Vec<_>>()
                    .join(" / ")
                    .chars()
                    .take(120)
                    .collect::<String>()
            };
            lines.push(Line::from(format!("    {snippet}")));
            lines.push(Line::from(""));
        }
        if modal.sessions.len() > shown {
            let extra = modal.sessions.len() - shown;
            let plural = if extra > 1 { "s" } else { "" };
            let more = format!("({extra} more session{plural} - run: {})", modal.search_cmd);
            lines.push(Line::from(Span::styled(
                more,
                Style::default().fg(Color::DarkGray),
            )));
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(
            if modal.show_copied() {
                "[j/k] Navigate    ✓ Copied!              [V/Esc] Close"
            } else {
                "[j/k] Navigate    [y] Copy search cmd    [V/Esc] Close"
            },
            Style::default().fg(Color::DarkGray),
        )));
        let w = 70.min(app.width.saturating_sub(4));
        let h = (lines.len() as u16 + 2).min(app.height.saturating_sub(2));
        let popup = ratatui::layout::Rect {
            x: app.width.saturating_sub(w) / 2,
            y: app.height.saturating_sub(h) / 2,
            width: w,
            height: h,
        };
        f.render_widget(ratatui::widgets::Clear, popup);
        f.render_widget(
            ratatui::widgets::Paragraph::new(lines).block(
                ratatui::widgets::Block::default()
                    .borders(ratatui::widgets::Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            ),
            popup,
        );
        return;
    }

    // Context help overlay (Go `~` / double-tap backtick): per-view
    // bindings rendered from the registry, any key dismisses (handled at
    // the top of `handle_key`).
    if app.show_context_help {
        crate::context_help::render_context_help(f, focus_for_view(app.current_view), f.area());
        return;
    }

    // Quit confirmation (Go `renderQuitConfirm`): centered bordered box —
    // "Quit bv?" / "Press Esc or Y to quit" / "Press any other key to cancel".
    if app.show_quit_confirm {
        let lines = vec![
            Line::from(Span::styled(
                "Quit bv?",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(vec![
                Span::raw("Press "),
                Span::styled(
                    "Esc",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" or "),
                Span::styled(
                    "Y",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" to quit"),
            ]),
            Line::from("Press any other key to cancel"),
        ];
        let w = 40.min(app.width.saturating_sub(4));
        let h = (lines.len() as u16 + 2).min(app.height.saturating_sub(2));
        let popup = ratatui::layout::Rect {
            x: app.width.saturating_sub(w) / 2,
            y: app.height.saturating_sub(h) / 2,
            width: w,
            height: h,
        };
        f.render_widget(ratatui::widgets::Clear, popup);
        f.render_widget(
            ratatui::widgets::Paragraph::new(lines)
                .alignment(ratatui::layout::Alignment::Center)
                .block(
                    ratatui::widgets::Block::default()
                        .borders(ratatui::widgets::Borders::ALL)
                        .border_style(Style::default().fg(Color::Red)),
                ),
            popup,
        );
    }
}

/// Map the active `ViewMode` to the `keybindings::Focus` bucket that
/// documents its bindings (used by the dynamic help overlay above).
fn focus_for_view(view: ViewMode) -> crate::keybindings::Focus {
    use crate::keybindings::Focus;
    match view {
        ViewMode::List => Focus::List,
        ViewMode::Board => Focus::Board,
        ViewMode::Tree => Focus::Tree,
        ViewMode::Graph => Focus::Graph,
        ViewMode::FlowMatrix => Focus::FlowMatrix,
        ViewMode::Attention => Focus::Attention,
        ViewMode::Insights => Focus::Insights,
        ViewMode::Alerts => Focus::Alerts,
        ViewMode::History => Focus::History,
        ViewMode::TimeTravel => Focus::TimeTravel,
        ViewMode::LabelDashboard => Focus::LabelDashboard,
        ViewMode::Sprint => Focus::Sprint,
        ViewMode::Tutorial => Focus::Tutorial,
        ViewMode::Actionable => Focus::Actionable,
    }
}

fn render_list(f: &mut Frame, app: &mut App, area: ratatui::layout::Rect) {
    let inner_width = area.width.saturating_sub(2) as usize;

    // Header row matching Go: "  TYPE PRI STATUS      ID                     TITLE"
    let header = ratatui::widgets::Paragraph::new(Line::from(Span::styled(
        "  TYPE PRI STATUS      ID                     TITLE",
        Style::default()
            .bg(Color::Cyan)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD),
    )));
    let header_area = ratatui::layout::Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: 1,
    };
    f.render_widget(header, header_area);

    // The list body starts one row down (the header) and takes the rest of
    // `area`. Computed before the items so the visible window is known first.
    let list_area = ratatui::layout::Rect {
        x: area.x,
        y: area.y + 1,
        width: area.width,
        height: area.height.saturating_sub(1),
    };

    // Build `ListItem`s only for the rows that can reach the screen.
    //
    // Every `ListItem` is one `Line` of ~15 `Span`s, and each row spends six
    // `format!` calls on it (priority, status, id, title truncation, age, and
    // the search-rank badge). Constructing all of them cost real time on a
    // 169-row set even though `List::render` then draws only the window
    // (`widgets/list/rendering.rs:72-78` does `.skip(offset).take(...)`).
    //
    // Go never pays this: `m.list` is a bubbles `list.Model`, which holds
    // prebuilt `list.Item`s and virtualizes its own rendering per page
    // (`m.list.View()`, model.go:6509).
    //
    // The window is chosen to match what ratatui's `List` would have scrolled
    // to. `ListState` below is always fresh, so its offset is 0 and
    // `get_items_bounds` (rendering.rs:129-200) walks `first_visible_index`
    // forward until the selected row is in view. With every item exactly one
    // line tall, that resolves to: the last N rows ending at the cursor,
    // clamped to the list.
    //
    // N is the block's *inner* height: `List` renders into
    // `block.inner_if_some(area)` (rendering.rs:41-50), so the all-borders
    // block eats two rows.
    let row_capacity = usize::from(list_area.height).saturating_sub(2);
    let visible =
        crate::views::tree::visible_range(app.filtered_indices.len(), app.cursor, row_capacity);
    // Publish it so `handle_mouse` maps a click onto the same row the user
    // actually saw, instead of re-deriving the arithmetic.
    app.list_window = visible.clone();
    let window_start = visible.start;

    // List items
    let items: Vec<ListItem> = app.filtered_indices[visible]
        .iter()
        .enumerate()
        .map(|(local_idx, &row_idx)| {
            // Rank within the *whole* filtered set, not the window — the
            // `#n` search badge and the selection test both mean "row N of
            // the list", not "row N of what happens to be on screen".
            let vis_idx = window_start + local_idx;
            let row = &app.rows[row_idx];
            let selected = vis_idx == app.cursor;
            let (icon, icon_color) = type_icon(&row.issue_type);

            // Priority badge
            let (pfg, pbg) = prio_badge_style(row.priority);
            let prio_label = format!("P{}", row.priority);

            // Status badge
            let status_str = row.status.as_str();
            let (slabel, scolor) = status_badge(status_str);

            // Age
            let age = age_str(&row.created_at);

            let mut spans = vec![];

            // Selection indicator
            if selected {
                spans.push(Span::styled(
                    "▸ ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ));
            } else {
                spans.push(Span::raw("  "));
            }

            // Type icon
            spans.push(Span::styled(icon, Style::default().fg(icon_color)));
            spans.push(Span::raw(" "));

            // Repo badge in workspace mode (Go `RenderRepoBadge`: "[API]",
            // uppercase, capped at 4 chars — `normalizeRepoKey` already
            // computed at load into `row.repo_prefix`).
            if app.workspace_repos.is_some() {
                if let Some(prefix) = row.repo_prefix.as_deref() {
                    let display: String = prefix.to_uppercase().chars().take(4).collect();
                    spans.push(Span::styled(
                        format!("[{display}]"),
                        Style::default()
                            .fg(Color::Magenta)
                            .add_modifier(Modifier::BOLD),
                    ));
                    spans.push(Span::raw(" "));
                }
            }

            // Priority badge
            spans.push(Span::styled(
                format!("{:<3}", prio_label),
                Style::default()
                    .fg(pfg)
                    .bg(pbg)
                    .add_modifier(Modifier::BOLD),
            ));

            // Priority hint arrow (Go `p` — delegate.go): shown only while
            // hints are toggled on. suggested < current numeric priority
            // means the issue is under-prioritized (should be more urgent)
            // -> up-arrow "increase"; suggested > current -> down-arrow.
            if app.show_priority_hints {
                if let Some(hints) = &app.priority_hints {
                    match hints.get(&row.id) {
                        Some(&suggested) if suggested < row.priority => {
                            spans.push(Span::styled(
                                "\u{2191}",
                                Style::default()
                                    .fg(Color::Green)
                                    .add_modifier(Modifier::BOLD),
                            ));
                        }
                        Some(&suggested) if suggested > row.priority => {
                            spans.push(Span::styled(
                                "\u{2193}",
                                Style::default()
                                    .fg(Color::Yellow)
                                    .add_modifier(Modifier::BOLD),
                            ));
                        }
                        _ => spans.push(Span::raw(" ")),
                    }
                } else {
                    spans.push(Span::raw(" "));
                }
            }
            spans.push(Span::raw(" "));

            // Triage indicator (Go `delegate.go` bv-151): quick-win ⭐,
            // else blocker-with-unblocks 🔓N, else any-unblocks ↪N.
            // Go order: IsQuickWin first, then IsBlocker && UnblocksCount>0,
            // then UnblocksCount>0.
            let triage_indicator: Option<String> = app.row_triage.get(&row.id).and_then(|t| {
                if t.is_quick_win {
                    Some("⭐".to_string())
                } else if t.is_blocker && t.unblocks_count > 0 {
                    Some(format!("🔓{}", t.unblocks_count))
                } else if t.unblocks_count > 0 {
                    Some(format!("↪{}", t.unblocks_count))
                } else {
                    None
                }
            });
            if let Some(ind) = triage_indicator {
                spans.push(Span::styled(ind, Style::default().fg(Color::Yellow)));
                spans.push(Span::raw(" "));
            }

            // Status badge
            spans.push(Span::styled(
                format!("{:<4}", slabel),
                Style::default().fg(scolor),
            ));
            spans.push(Span::raw(" "));

            // Search score badge while a search is active (Go `delegate.go`:
            // `d.ShowSearchScores && i.SearchScoreSet` renders `[0.42]`).
            // Rust has no per-row score store; the score IS the ranking that
            // produced `filtered_indices`, so the badge shows the row's rank
            // position when a fuzzy (/) or semantic (Ctrl+S) filter is active.
            // Scope note: Go shows the raw float score; we show rank — same
            // slot, same "search is active" signal, without inventing scores.
            if app.searching || app.semantic_searching {
                spans.push(Span::styled(
                    format!("#{}", vis_idx + 1),
                    Style::default().fg(Color::Cyan),
                ));
                spans.push(Span::raw(" "));
            }

            // ID
            spans.push(Span::styled(
                format!("{:<20}", row.id),
                Style::default().fg(Color::Cyan),
            ));
            spans.push(Span::raw(" "));

            // Diff badge in Time-Travel mode (Go `IssueItem.DiffStatus.Badge()`
            // via `getDiffStatus` over newIssueIDs/closedIssueIDs/
            // modifiedIssueIDs). Rust's `DiffResult` names the same sets
            // `added` (in current, not at ref → 🆕) / `removed` (was at ref,
            // gone now — never rendered as a live row, kept for symmetry) /
            // `changed` (status/title/priority drift → ~). An issue that the
            // diff reports as *closed* relative to the ref keeps Go's ✅ —
            // detected here by comparing the live row's status against the
            // changed entry: if the row is currently closed-like and the ref
            // predates the closure, it's a closure, not a generic edit.
            if app.current_view == ViewMode::TimeTravel {
                if let Some(result) = &app.time_travel_result {
                    let badge = if result.added.iter().any(|id| id == &row.id) {
                        DiffBadge::Added
                    } else if result.changed.iter().any(|c| c.id == row.id) {
                        let closed_now = matches!(row.status, Status::Closed | Status::Tombstone);
                        if closed_now {
                            DiffBadge::Closed
                        } else {
                            DiffBadge::Modified
                        }
                    } else {
                        DiffBadge::None
                    };
                    if badge != DiffBadge::None {
                        spans.push(Span::raw(badge.badge()));
                        spans.push(Span::raw(" "));
                    }
                }
            }

            // Title (truncated to fit)
            let title_width = inner_width.saturating_sub(45);
            let title = if row.title.chars().count() > title_width {
                let truncated: String = row
                    .title
                    .chars()
                    .take(title_width.saturating_sub(1))
                    .collect();
                format!("{}…", truncated)
            } else {
                format!("{:<width$}", row.title, width = title_width)
            };
            spans.push(Span::styled(
                title,
                Style::default().add_modifier(if selected {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
            ));

            // Age (right-aligned)
            if !age.is_empty() {
                spans.push(Span::raw(" "));
                spans.push(Span::styled(age, Style::default().fg(Color::DarkGray)));
            }

            ListItem::new(Line::from(spans))
        })
        .collect();

    let title = format!(
        " ISSUES ({}) [{}] ",
        app.filtered_indices.len(),
        app.sort_mode.label()
    );
    let list_border_color = if !app.focus_detail {
        Color::Cyan
    } else {
        Color::DarkGray
    };
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(Style::default().fg(list_border_color)),
        )
        .highlight_style(Style::default().bg(Color::DarkGray))
        .highlight_symbol(">");

    // The window is already scrolled, so the state's offset is 0 and its
    // selected index is relative to the window. `app.cursor` is the absolute
    // row; `window_start` is the first row handed to `List`.
    let mut state = ListState::default();
    state.select(app.cursor.checked_sub(window_start));
    f.render_stateful_widget(list, list_area, &mut state);
}

fn render_detail(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let mut content: Vec<Line> = match app.selected() {
        Some(row) => {
            let graph_scores = app.graph_metrics.as_ref().and_then(|gm| {
                gm.pagerank
                    .get(&row.id)
                    .map(|&pr| crate::detail::GraphScores {
                        pagerank: pr,
                        betweenness: gm.betweenness.get(&row.id).copied().unwrap_or(0.0),
                        eigenvector: gm.eigenvector.get(&row.id).copied().unwrap_or(0.0),
                        hubs: gm.hubs.get(&row.id).copied().unwrap_or(0.0),
                        authorities: gm.authorities.get(&row.id).copied().unwrap_or(0.0),
                        critical_path: 0.0,
                    })
            });

            // Use full issue data from issue_map for detail rendering
            if let Some(full_issue) = app.issue_map.get(&row.id) {
                crate::detail::build_detail_lines(
                    full_issue,
                    graph_scores.as_ref(),
                    Some(&app.issue_map),
                )
            } else {
                vec![Line::from(""), Line::from("Issue not found in map")]
            }
        }
        None => vec![
            Line::from(""),
            Line::from(Span::styled(
                "No issue selected",
                Style::default().fg(Color::DarkGray),
            )),
        ],
    };
    // Go's `updateViewportContent` (`model.go:8894-8895`) opens with the
    // update notice, ahead of the title block, and closes with a blank line.
    // The raw `[tag](url)` is deliberate: Go writes the same literal string
    // into a plain viewport, so the link syntax is shown, not resolved.
    if let Some(tag) = &app.update_tag {
        let url = app.update_url.as_deref().unwrap_or_default();
        content.insert(
            0,
            Line::from(Span::styled(
                format!("\u{2b50} **Update Available:** [{tag}]({url})"),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )),
        );
        content.insert(1, Line::from(""));
    }
    let focused_border = if app.focus_detail {
        Color::Cyan
    } else {
        Color::DarkGray
    };
    let title = if app.focus_detail {
        " DETAIL ◄ FOCUSED "
    } else {
        " DETAIL "
    };
    let para = ratatui::widgets::Paragraph::new(content)
        .wrap(ratatui::widgets::Wrap { trim: false })
        .scroll((app.detail_scroll, 0))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(Style::default().fg(focused_border)),
        );
    f.render_widget(para, area);
}

fn render_sidebar(f: &mut Frame, _app: &App) {
    let entries = crate::chrome::default_help_entries();
    let area = ratatui::layout::Rect {
        x: f.area().width.saturating_sub(34),
        y: 0,
        width: 34.min(f.area().width),
        height: f.area().height,
    };
    crate::chrome::render_sidebar(f, area, &[("Navigation", entries)]);
}

fn render_status_bar(f: &mut Frame, app: &App) {
    let area = ratatui::layout::Rect {
        x: 0,
        y: f.area().height - 1,
        width: f.area().width,
        height: 1,
    };

    // Search mode
    if app.searching {
        let bar = Paragraph::new(Line::from(vec![
            Span::styled("/", Style::default().fg(Color::Yellow)),
            Span::styled(&app.search_query, Style::default().fg(Color::White)),
            Span::styled("_", Style::default().fg(Color::Yellow)),
        ]));
        f.render_widget(bar, area);
        return;
    }
    // Semantic search mode (Ctrl+S): same bar shape, distinct prefix.
    if app.semantic_searching {
        let bar = Paragraph::new(Line::from(vec![
            Span::styled("s/", Style::default().fg(Color::Magenta)),
            Span::styled(&app.semantic_query, Style::default().fg(Color::White)),
            Span::styled("_", Style::default().fg(Color::Magenta)),
        ]));
        f.render_widget(bar, area);
        return;
    }

    let mut spans: Vec<Span> = Vec::new();

    // Filter badge (colored bg like Go)
    let (filter_icon, filter_txt) = match app.filter_mode {
        FilterMode::All => ("📋", "ALL"),
        FilterMode::Open => ("📂", "OPEN"),
        FilterMode::Closed => ("✅", "CLOSED"),
        FilterMode::Ready => ("🚀", "READY"),
    };
    spans.push(Span::styled(
        format!(" {filter_icon} {filter_txt} "),
        Style::default()
            .bg(Color::Cyan)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD),
    ));

    // Label filter badge (Go searchBadge area)
    if let Some(label) = &app.label_filter {
        spans.push(Span::styled(
            format!(" #{label} "),
            Style::default().bg(Color::DarkGray).fg(Color::Cyan),
        ));
    }

    // Sort badge (only when not default)
    if app.sort_mode != SortMode::Default {
        spans.push(Span::styled(
            format!(" ↕ {} ", app.sort_mode.label()),
            Style::default().bg(Color::DarkGray).fg(Color::Cyan),
        ));
    }

    // Stats section with colored indicators
    let open_count = app
        .rows
        .iter()
        .filter(|r| matches!(r.status, Status::Open))
        .count();
    let ready_count = app.filtered_indices.len();
    let blocked_count = app
        .rows
        .iter()
        .filter(|r| matches!(r.status, Status::Blocked))
        .count();
    let closed_count = app.rows.iter().filter(|r| r.status.is_closed()).count();

    spans.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));
    spans.push(Span::styled(
        format!("○{open_count} "),
        Style::default().fg(Color::Green),
    ));
    spans.push(Span::styled(
        format!("◉{ready_count} "),
        Style::default().fg(Color::Cyan),
    ));
    spans.push(Span::styled(
        format!("◈{blocked_count} "),
        Style::default().fg(Color::Yellow),
    ));
    spans.push(Span::styled(
        format!("●{closed_count}"),
        Style::default().fg(Color::DarkGray),
    ));

    // Alerts badge (Go alertsSection, bv-168)
    if app.alerts_total > 0 {
        let (bg, fg) = if app.alerts_critical > 0 {
            (Color::Red, Color::White)
        } else {
            (Color::DarkGray, Color::Yellow)
        };
        spans.push(Span::styled(
            format!(" \u{26a0} {} alerts (!) ", app.alerts_total),
            Style::default().bg(bg).fg(fg).add_modifier(Modifier::BOLD),
        ));
    }

    // Freshness badge (Go bv-h305: warn 30s, stale 2min since snapshot)
    {
        let elapsed = app.loaded_at.elapsed();
        let stale_s = std::env::var("BV_FRESHNESS_STALE_S")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(120);
        let warn_s = std::env::var("BV_FRESHNESS_WARN_S")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(30);
        let secs = elapsed.as_secs();
        let fmt_age = |s: u64| {
            if s < 60 {
                "<1m ago".to_string()
            } else if s < 3600 {
                format!("{}m ago", s / 60)
            } else {
                format!("{}h ago", s / 3600)
            }
        };
        if secs >= stale_s {
            spans.push(Span::styled(
                format!(" \u{26a0} STALE: {} ", fmt_age(secs)),
                Style::default()
                    .bg(Color::Red)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ));
        } else if secs >= warn_s {
            spans.push(Span::styled(
                format!(" \u{26a0} {} ", fmt_age(secs)),
                Style::default().bg(Color::DarkGray).fg(Color::Yellow),
            ));
        }
    }

    // Phase 2 metrics badge (Go bv-tspo: ◌ metrics... until ready)
    if app.graph_metrics.is_none() {
        spans.push(Span::styled(
            " \u{25d0} metrics... ",
            Style::default().bg(Color::DarkGray).fg(Color::Cyan),
        ));
    }

    // Instance warning (Go bv-vrvn: ⚠ PID of other live instance)
    if let Some(pid) = app.instance_pid {
        spans.push(Span::styled(
            format!(" \u{26a0} PID {pid} "),
            Style::default()
                .bg(Color::Yellow)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        ));
    }

    // Session indicator (Go bv-y836: cass sessions for selected bead)
    if app.session_count > 0 {
        let count_str = if app.session_count > 9 {
            "9+".to_string()
        } else {
            app.session_count.to_string()
        };
        spans.push(Span::styled(
            format!(" \u{1f4bc} {count_str} sessions "),
            Style::default().bg(Color::DarkGray).fg(Color::Cyan),
        ));
    }

    // Update badge — Go `updateSection` (`model.go:7649-7655`): U+2B50 WHITE
    // MEDIUM STAR, then " Update <tag>", padded one space each side.
    if let Some(tag) = &app.update_tag {
        spans.push(Span::styled(
            format!(" \u{2b50} Update {tag} "),
            Style::default()
                .bg(Color::Green)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        ));
    }

    // Workspace repo filter badge (Go repoFilterSection 🗂)
    if app.workspace_repos.is_some() {
        let label = match &app.active_repo {
            Some(r) => r.clone(),
            None => {
                let repos = app.workspace_repos.as_deref().unwrap_or_default();
                let shown: Vec<&str> = repos.iter().take(3).map(|s| s.as_str()).collect();
                if repos.len() > 3 {
                    format!("{},+{}", shown.join(","), repos.len() - 3)
                } else {
                    shown.join(",")
                }
            }
        };
        spans.push(Span::styled(
            format!(" \u{1f5c2} {label} "),
            Style::default()
                .bg(Color::DarkGray)
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    }

    // Large dataset warning (Go bv-9thm)
    if let Some(warn) = &app.dataset_warning {
        let huge = warn.contains("huge");
        spans.push(Span::styled(
            format!(" {warn} "),
            Style::default()
                .bg(if huge { Color::Red } else { Color::DarkGray })
                .fg(if huge { Color::White } else { Color::Yellow })
                .add_modifier(Modifier::BOLD),
        ));
    }

    // Label hint (Go labelHint: "L:labels * h:detail")
    if app.current_view != ViewMode::Board {
        spans.push(Span::styled(
            " \u{2502} ",
            Style::default().fg(Color::DarkGray),
        ));
        spans.push(Span::styled(
            "L:labels * h:detail",
            Style::default().fg(Color::DarkGray),
        ));
    }

    // Keyboard hints (context-aware, matching Go footer)
    spans.push(Span::styled(
        " \u{2502} ",
        Style::default().fg(Color::DarkGray),
    ));

    let key_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let sep_style = Style::default().fg(Color::DarkGray);
    let mut hints: Vec<Span> = Vec::new();
    let push_hint = |hints: &mut Vec<Span>, key: &str, desc: &str| {
        if !hints.is_empty() {
            hints.push(Span::styled(" \u{2502} ", sep_style));
        }
        hints.push(Span::styled(key.to_string(), key_style));
        hints.push(Span::raw(format!(" {desc}")));
    };

    if app.focus_detail {
        push_hint(&mut hints, "esc", "back");
        push_hint(&mut hints, "C", "copy");
        push_hint(&mut hints, "O", "edit");
        push_hint(&mut hints, "Ctrl+R", "refresh");
        push_hint(&mut hints, "?", "help");
    } else if app.show_detail {
        push_hint(&mut hints, "tab", "focus");
        push_hint(&mut hints, "C", "copy");
        push_hint(&mut hints, "x", "export");
        push_hint(&mut hints, "Ctrl+R", "refresh");
        push_hint(&mut hints, "?", "help");
    } else if app.current_view == ViewMode::Board {
        push_hint(&mut hints, "h/l", "col");
        push_hint(&mut hints, "j/k", "move");
        push_hint(&mut hints, "s", "group");
        push_hint(&mut hints, "b", "list");
        push_hint(&mut hints, "?", "help");
    } else {
        push_hint(&mut hints, "\u{23ce}", "details");
        push_hint(&mut hints, "t", "diff");
        push_hint(&mut hints, "S", "triage");
        push_hint(&mut hints, "l", "labels");
        push_hint(&mut hints, "Ctrl+R", "refresh");
        push_hint(&mut hints, "?", "help");
    }
    spans.extend(hints);

    // Count badge (right side, padded like Go countBadge)
    let count_text = format!(" {} issues ", app.filtered_indices.len());
    let count_width = count_text.len() as u16;
    let used: u16 = spans.iter().map(|s| s.width() as u16).sum::<u16>() + count_width;
    let filler_width = area.width.saturating_sub(used);
    if filler_width > 0 {
        spans.push(Span::raw(" ".repeat(filler_width as usize)));
    }
    spans.push(Span::styled(
        count_text,
        Style::default().fg(Color::DarkGray),
    ));

    let bar = Paragraph::new(Line::from(spans));
    f.render_widget(bar, area);
}

/// Render one view into an off-screen buffer and return it as text, so
/// `--debug-render` can capture TUI output without entering raw mode or the
/// alternate screen. Port of Go `(*ui.Model).RenderDebugView`
/// (pkg/ui/model.go:10570-10586).
///
/// Only the panel is rendered — Go never draws the status bar here, it just
/// reserves the last row by rendering at `height - 1`. `"history"` is
/// accepted even though the `--debug-render` help text omits it, because Go's
/// switch does. Any other name returns Go's `Unknown view: <name>` string
/// verbatim, without touching the buffer.
pub fn render_debug_view(app: &mut App, view: &str, width: u16, height: u16) -> String {
    // Go sets m.width/m.height/m.ready before the switch; m.ready has no
    // Rust counterpart (nothing reads it) and the two size fields are set for
    // parity with the model's own bookkeeping.
    app.width = width;
    app.height = height;

    if !matches!(view, "insights" | "board" | "history") {
        return format!("Unknown view: {view}");
    }

    // `height - 1` reserves the status-bar row. saturating_sub keeps a
    // zero/one-row request from wrapping, which Go's `int` arithmetic would
    // happily hand to the panel as a negative height.
    let backend = ratatui::backend::TestBackend::new(width, height.saturating_sub(1));
    let mut terminal = match ratatui::Terminal::new(backend) {
        Ok(t) => t,
        // TestBackend::size never fails and a zero-area buffer is legal, so
        // this is unreachable in practice; degrade to empty output rather
        // than panicking inside `--debug-render`.
        Err(_) => return String::new(),
    };

    let empty = BTreeMap::new();
    // A failed draw leaves the backend partially written; TestBackend only
    // fails if its own size query does, so keep whatever it managed to draw.
    let _ = terminal.draw(|f| match view {
        "insights" => {
            let (pr, bw, hub, auth) = if let Some(ref gm) = app.graph_metrics {
                (&gm.pagerank, &gm.betweenness, &gm.hubs, &gm.authorities)
            } else {
                (&empty, &empty, &empty, &empty)
            };
            crate::views::insights::render_insights(f, pr, bw, hub, auth)
        }
        "board" => {
            crate::views::board::render_board(f, app, f.area(), app.board_mode, app.board_column)
        }
        _ => match app.history {
            Some(ref mut history) => {
                crate::views::history::render_history(f, history, &app.theme, f.area())
            }
            None => crate::views::history::render_empty(
                f,
                &app.theme,
                f.area(),
                "No history data available",
            ),
        },
    });

    buffer_to_lines(terminal.backend().buffer())
}

/// Flatten a ratatui buffer to one string per row, newline-joined.
///
/// `TestBackend`'s own `buffer_view` (ratatui-core-0.1.2/src/backend/test.rs:45)
/// is private and `Buffer` only implements `Debug`, so the traversal is
/// repeated here. No wide-glyph bookkeeping is needed: `Buffer::set_stringn`
/// resets the cells a multi-column grapheme covers
/// (ratatui-core-0.1.2/src/buffer/buffer.rs:363-367), and `Cell::symbol`
/// renders a reset cell as a single space, so concatenating every cell in
/// reading order lays the text out exactly as it would appear on screen.
fn buffer_to_lines(buf: &ratatui::buffer::Buffer) -> String {
    let width = buf.area.width as usize;
    if width == 0 {
        return String::new();
    }
    let mut out = String::with_capacity(buf.content.len() + buf.area.height as usize);
    for (i, row) in buf.content.chunks(width).enumerate() {
        if i > 0 {
            out.push('\n');
        }
        for cell in row {
            out.push_str(cell.symbol());
        }
    }
    out
}

/// Go `updater.StartupCheckEnabled()` (`updater.go:160-162`), the gate `Init`
/// uses before appending `CheckUpdateCmd` (`model.go:2076`).
///
/// `BV_NO_UPDATE_CHECK` is read for *truthiness*, not presence: Go's
/// `envTruthy` (`updater.go:87-93`) treats `0` / `false` / `no` / `off` and
/// the empty string as "not an opt-out", so those values leave the check
/// running — `pkg/updater/network_test.go:398-404` pins exactly that case.
/// The previous gate here was `std::env::var("BV_NO_UPDATE_CHECK").is_err()`,
/// which opted out for every value including `0`. `config.yaml`'s
/// `updates.check` is honoured by the same call.
fn startup_update_check_enabled() -> bool {
    bv_update::prefs::startup_check_enabled()
}

/// Run the TUI event loop. Returns when user quits.
pub fn run_tui(app: &mut App) -> io::Result<()> {
    // Instance lock (Go bv-vrvn)
    let beads_dir = std::env::current_dir()
        .ok()
        .map(|d| d.join(".beads"))
        .unwrap_or_default();
    app.instance_pid = acquire_instance_lock(&beads_dir);

    // Update check (Go `CheckUpdateCmd`, `model.go:497-506`, gated by
    // `updater.StartupCheckEnabled()` at `model.go:2076`): background thread
    // through bv-update (proper semver compare incl. dev-build rule).
    let (update_tx, update_rx) = std::sync::mpsc::channel::<bv_update::github::UpdateInfo>();
    if startup_update_check_enabled() {
        std::thread::spawn(move || {
            if let Ok(Some(info)) = bv_update::github::check_for_updates() {
                let _ = update_tx.send(info);
            }
        });
    }

    let mut stdout = io::stdout();
    crossterm::terminal::enable_raw_mode()?;
    // Mouse reporting stays on crossterm's EnableMouseCapture
    // (`?1000h ?1002h ?1003h ?1015h ?1006h`).
    //
    // An earlier revision replaced it with a hand-rolled
    // `?1000h ?1002h ?1006h` — 1002 but not 1003 — reasoning that `?1003h`
    // ("report *all* motion events") is pure overhead, since `handle_mouse`
    // ignores motion and so those events cannot change state. **That was
    // wrong, and it broke clicking.** The per-motion redraw cost it was
    // meant to remove was already gone: it was the draw-first loop paying
    // for it, and the loop now redraws only when a handler reports a
    // change. Re-emitting a different set of DECSET codes bought nothing
    // measurable on top of that, while changing the terminal's reporting
    // mode — which is observable to the user and to anything else reading
    // the tty. Do not narrow this again without a measurement that it
    // matters, and without verifying a click still lands on the row under
    // the pointer.
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    )?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let terminal = ratatui::Terminal::new(backend)?;

    // `tui_event_loop` owns the restore path: even if the event loop fails
    // on an I/O error, the alternate screen is left and raw mode is dropped.
    // A `?` that returned straight out of the old body skipped both, leaving
    // the user in a broken terminal.
    let result = tui_event_loop(terminal, app, &update_rx);

    release_instance_lock(&beads_dir);
    result
}

/// Whether a time-animated widget is on screen and needs sub-second repaints.
fn self_update_modal_animating(app: &App) -> bool {
    app.show_update_modal && app.update_modal.is_some()
}

/// Apply one terminal event to `app`. Returns whether the user asked to quit.
///
/// `Ctrl+C` is handled here rather than by the caller so the burst-drain
/// loop can stop on it without unwinding the whole event loop.
fn handle_one_event(app: &mut App, ev: CEvent) -> bool {
    match ev {
        CEvent::Key(key) => {
            if key.kind != KeyEventKind::Press {
                return false;
            }
            if key
                .modifiers
                .contains(crossterm::event::KeyModifiers::CONTROL)
                && key.code == KeyCode::Char('c')
            {
                return true;
            }
            // `handle_*` return `true` when they changed state. Go's
            // `Update` has the same shape: every arm that mutates the Model
            // is what triggers the repaint.
            if key
                .modifiers
                .contains(crossterm::event::KeyModifiers::CONTROL)
            {
                app.handle_ctrl_key(key.code);
            } else {
                app.handle_key(key.code);
            }
        }
        CEvent::Mouse(mouse) => {
            // Motion events over a region that handles no drag change
            // nothing, and the burst loop above already collapsed a motion
            // storm into a single repaint.
            app.handle_mouse(mouse);
        }
        CEvent::Resize(..) => {
            // Go handles `tea.WindowSizeMsg` and re-sizes its list and every
            // panel against it (model.go:2353, :2452). Rust previously
            // dropped this event outright, leaving `app.width`/`app.height`
            // pinned to the 120x40 defaults for the life of the process.
            app.handle_resize();
        }
        _ => {}
    }
    false
}

fn tui_event_loop(
    mut terminal: ratatui::Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>,
    app: &mut App,
    update_rx: &std::sync::mpsc::Receiver<bv_update::github::UpdateInfo>,
) -> io::Result<()> {
    // Go auto-shows the blurb prompt once at startup when the detected
    // agent file needs the blurb and prefs don't suppress it.
    let cwd = std::env::current_dir().unwrap_or_default();
    let detection = detect_agent_file_for_tui(&cwd);
    if bv_core::agents::prefs::should_prompt_for_agent_file(&cwd, &detection) {
        app.agent_prompt_modal = Some(crate::agent_prompt_modal::AgentPromptModal {
            file_path: detection.file_path.clone(),
            file_type: detection.file_type.clone(),
            needs_upgrade: !detection.needs_blurb() && detection.needs_upgrade(),
            selected: 0,
            project_dir: cwd.to_string_lossy().to_string(),
        });
        app.show_agent_prompt = true;
    }
    // Redraw policy. Go's bubbletea runtime calls `View()` only when a
    // `tea.Msg` actually arrives (model.go:2206 `Update` returns a Model; the
    // renderer repaints on change). This loop used to be draw-FIRST: an
    // unconditional `terminal.draw()` at the top of every iteration, so the
    // full view was rebuilt on every idle ~500ms tick AND on every single
    // input event — including every `MouseMoved`, because crossterm's
    // `EnableMouseCapture` turns on SGR 1003 (report *all* motion) and
    // `handle_mouse` returns `false` for motion events that changed nothing
    // (lib.rs `MouseEventKind::Moved` falls through to `_ => false`).
    //
    // `dirty` is the Go "did anything change?" signal. It is set when a
    // handler reports a state change, when an async source lands, and on the
    // periodic tick — but only when a whole second has elapsed, so the
    // wall-clock-dependent widgets (freshness badge, relative "Nm ago" ages)
    // still advance at the same 1Hz they did when the loop drew twice a
    // second. Everything else stops redrawing.
    let mut dirty = true;
    let mut last_tick_millis = jiff::Timestamp::now().as_millisecond();
    // Seed the geometry from the real terminal before the first frame.
    // Go's bubbletea sends an initial `WindowSizeMsg` before `View` runs
    // (tea.go:317-319); without the equivalent, `app.width`/`app.height`
    // kept their 120x40 constructor defaults until the user happened to
    // resize, so the split-view threshold, the modal centring and the
    // click-to-row mapping were all computed for the wrong terminal.
    app.handle_resize();

    // The event loop is the fallible part; the restore is not. Running it in
    // a closure means a failed `event::read` or a `draw` error returns out
    // of here and still falls through to LeaveAlternateScreen +
    // disable_raw_mode, instead of `?`-ing out of the function with the user
    // stuck in the alternate screen and raw mode on.
    let mut run = || -> io::Result<()> {
        loop {
            if dirty {
                terminal.draw(|f| render(f, app))?;
                dirty = false;
            }
            if app.quit_requested {
                break;
            }
            // Drain update-check channel (non-blocking). The tag was already
            // vetted by bv-update semver compare (incl. dev-build rule);
            // re-verify here in case the binary version changed mid-session.
            // Go's `UpdateMsg` arm (`model.go:2290-2302`) stores the tag and the
            // release URL together and drops the notice when the tag is not newer.
            if let Ok(info) = update_rx.try_recv() {
                if bv_update::is_newer_than_current(&info.new_version) {
                    app.update_tag = Some(info.new_version.clone());
                    app.update_url = Some(info.release_url.clone());
                    // Auto-notify once per session (Go: update badge + prompt);
                    // `U` reopens afterwards.
                    if !app.update_modal_auto_shown {
                        app.update_modal_auto_shown = true;
                        app.show_self_update_modal();
                    }
                    dirty = true;
                }
            }
            // Drain the self-update install thread (Go's `UpdateProgressMsg` /
            // `UpdateCompleteMsg` arms, `model.go:2304-2333`).
            dirty |= app.poll_update_modal();
            // Live reload (Go fsnotify; TUI_UX_PARITY_PLAN.md Phase F): piggyback
            // on this same ~500ms tick instead of a background notify watcher.
            dirty |= app.check_for_reload();
            // Same trick for the background History correlation walk (Go's
            // `HistoryLoadedMsg`): drain the channel here so a slow `git log -p`
            // never blocks a keypress.
            dirty |= app.poll_history_load();
            // Wall-clock widgets. The previous loop redrew twice a second, so a
            // badge showing "45s ago" refreshed every 500ms; redrawing once per
            // elapsed second is visually identical and costs 2x less.
            // The self-update spinner advances one frame per 100 ms over a
            // 10-frame cycle (`update_modal::render_spinner`), and
            // 1000 ms % 1000 == 0 lands on frame 0 every single time — a frozen
            // glyph where Go animates, because Go re-ticks at 100 ms. So while
            // that modal is up, keep the old half-second cadence; otherwise the
            // once-a-second tick is enough for the age and freshness badges.
            let tick_ms = if self_update_modal_animating(app) {
                500
            } else {
                1000
            };
            let now_millis = jiff::Timestamp::now().as_millisecond();
            if now_millis - last_tick_millis >= tick_ms {
                last_tick_millis = now_millis;
                dirty = true;
            }
            // Poll events with timeout so freshness badge stays live.
            //
            // Drain the whole pending burst before painting, not one event per
            // frame. A wheel gesture and a held arrow key both deliver a stream
            // of events, and one frame per event is what made scrolling crawl:
            // a scrolled detail pane already rewrites ~34% of the screen
            // (3394 of 10000 cells, measured), so ten queued wheel events meant
            // ten full-screen repaints to produce a ten-line scroll. Applying
            // every queued event first and then painting once collapses that to
            // a single repaint, and it drops no input — the events are all
            // handled, just before the pixels move rather than between each one.
            if event::poll(std::time::Duration::from_millis(500))? {
                let mut quit = false;
                loop {
                    quit |= handle_one_event(app, event::read()?);
                    if quit || !event::poll(std::time::Duration::ZERO)? {
                        break;
                    }
                }
                if quit {
                    // Route through `quit_requested`, which the top of this loop
                    // already tests. Breaking out of the drain loop directly would
                    // only end the drain, and the outer loop would spin on
                    // forever with Ctrl+C swallowed.
                    app.quit_requested = true;
                }
                // Force the repaint: a burst that only scrolled the list still
                // has to reach the screen.
                dirty = true;
            }
        }
        Ok(())
    };

    // Restore first, report the loop's error after. A restore that itself
    // fails must not mask why the loop stopped.
    crossterm::execute!(
        io::stdout(),
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture
    )?;
    crossterm::terminal::disable_raw_mode()?;
    run()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_app(n: usize) -> App {
        let issues: Vec<bv_core::model::Issue> = (0..n)
            .map(|i| bv_core::model::Issue {
                id: format!("T-{i}"),
                content_hash: String::new(),
                title: format!("Issue {i}"),
                description: String::new(),
                design: String::new(),
                acceptance_criteria: String::new(),
                notes: String::new(),
                status: if i % 3 == 0 {
                    Status::Closed
                } else {
                    Status::Open
                },
                priority: (i % 4) as i32,
                issue_type: "task".into(),
                assignee: String::new(),
                estimated_minutes: None,
                created_at: None,
                updated_at: None,
                due_date: None,
                defer_until: None,
                closed_at: None,
                external_ref: None,
                compaction_level: 0,
                compacted_at: None,
                compacted_at_commit: None,
                original_size: 0,
                labels: vec![],
                dependencies: vec![],
                comments: vec![],
                source_repo: String::new(),
            })
            .collect();
        App::new(issues)
    }

    #[test]
    fn filter_open_shows_only_non_closed() {
        let mut app = make_app(9); // every 3rd closed → 6 non-closed
        app.filter_mode = FilterMode::Open;
        app.apply_filter();
        assert_eq!(app.filtered_indices.len(), 6);
    }

    #[test]
    fn j_k_navigation_bounds() {
        let mut app = make_app(3);
        app.handle_key(KeyCode::Char('j'));
        assert_eq!(app.cursor, 1);
        app.handle_key(KeyCode::Char('j'));
        app.handle_key(KeyCode::Char('j')); // clamped at last
        assert_eq!(app.cursor, 2);
        app.handle_key(KeyCode::Char('k'));
        assert_eq!(app.cursor, 1);
    }

    #[test]
    fn sort_mode_cycles_through_five() {
        let mut app = make_app(3);
        let start = app.sort_mode;
        app.handle_key(KeyCode::Char('s'));
        assert_ne!(start, app.sort_mode);
    }

    /// Go's `LoadHistoryCmd` (model.go:595-671) is a background `tea.Cmd`, so
    /// `h` *starts* the correlation walk and renders the `"Loading history…"`
    /// placeholder; `history_loaded` is only set once `poll_history_load`
    /// drains the result. Both assertions are deterministic here because
    /// `poll_history_load` only runs from the run loop's tick, which this test
    /// never calls.
    #[test]
    fn history_toggle_loads_lazily_and_switches_view() {
        let mut app = make_app(3);
        assert!(!app.history_loaded);
        app.handle_key(KeyCode::Char('h'));
        assert_eq!(app.current_view, ViewMode::History);
        assert!(app.history_loading, "h should start the background walk");
        assert!(
            !app.history_loaded,
            "history_loaded only flips once the walk returns"
        );
        // Toggling back to List must not restart or reset the in-flight walk.
        app.handle_key(KeyCode::Char('h'));
        assert_eq!(app.current_view, ViewMode::List);
        assert!(app.history_loading, "the in-flight walk is left alone");
        assert!(!app.history_loaded);
    }

    /// Go fences a load with a `(dataGeneration, requestGeneration)` pair so a
    /// slow walk can never overwrite a newer dataset (model.go:595-671).
    /// Drives the channel directly, so the test needs no git walk.
    #[test]
    fn history_load_drops_stale_generations_and_retries() {
        let mut app = make_app(3);
        app.handle_key(KeyCode::Char('h'));
        assert!(app.history_loading);
        let generation = app.history_generation;

        // A completion from an older request is discarded. Nothing is pending
        // by then, so it must surface the retry placeholder rather than spin.
        let (tx, rx) = crossbeam_channel::bounded(1);
        app.history_rx = Some(rx);
        tx.send((generation - 1, Err("stale".to_string()))).unwrap();
        app.poll_history_load();
        assert!(!app.history_loading);
        assert!(!app.history_loaded, "a stale result must not be applied");
        assert!(app.history_load_failed);

        // Re-entering retries, and a current-generation report lands.
        app.handle_key(KeyCode::Char('h')); // back to the List
        app.handle_key(KeyCode::Char('h')); // retry
        assert_eq!(app.current_view, ViewMode::History);
        assert!(app.history_loading);
        let generation = app.history_generation;
        let (tx, rx) = crossbeam_channel::bounded(1);
        app.history_rx = Some(rx);
        let report = bv_correlation::history::HistoryReport {
            generated_at: "2026-01-01T00:00:00Z".into(),
            data_hash: "hash".into(),
            git_range: "all history".into(),
            latest_commit_sha: String::new(),
            window: bv_correlation::history::HistoryWindow {
                revision: String::new(),
                limit: 500,
                since: None,
                until: None,
                commits: 0,
            },
            stats: bv_correlation::history::HistoryStats {
                total_beads: 0,
                beads_with_commits: 0,
                total_commits: 0,
                unique_authors: 0,
                avg_commits_per_bead: 0.0,
                avg_cycle_time_days: None,
                method_distribution: BTreeMap::new(),
                strategies: None,
                feedback_applied: None,
            },
            histories: BTreeMap::new(),
            commit_index: BTreeMap::new(),
            causal_history: None,
        };
        tx.send((generation, Ok(report))).unwrap();
        app.poll_history_load();
        assert!(!app.history_loading);
        assert!(app.history_loaded);
        assert!(!app.history_load_failed);
        assert!(
            app.history.as_ref().is_some_and(|h| h.has_report()),
            "the report must reach the view"
        );
        assert_eq!(
            app.status_msg, "Loaded history: 0 beads with commits",
            "Go model.go:9231"
        );
    }

    /// Regression test for TUI_UX_PARITY_PLAN.md G11: j/k inside the
    /// History view used to be dead code (misplaced inside the Graph arm's
    /// else-branch), so pressing j/k while in History silently moved the
    /// *main list's* cursor instead of doing nothing meaningful in that
    /// view. Confirm the main list cursor is now left untouched.
    #[test]
    fn history_j_k_does_not_move_main_list_cursor() {
        let mut app = make_app(5);
        app.handle_key(KeyCode::Char('h')); // enter History
        let cursor_before = app.cursor;
        app.handle_key(KeyCode::Char('j'));
        app.handle_key(KeyCode::Char('j'));
        app.handle_key(KeyCode::Char('k'));
        assert_eq!(
            app.cursor, cursor_before,
            "History view's j/k must not move the List view's cursor"
        );
    }

    #[test]
    fn normalize_repo_key_matches_go_rules() {
        // Go `normalizeRepoKey`: trim, strip trailing -:_, lowercase,
        // reject empty/./slash-containing.
        assert_eq!(normalize_repo_key("api"), Some("api".into()));
        assert_eq!(normalize_repo_key("  API--"), Some("api".into()));
        assert_eq!(normalize_repo_key(""), None);
        assert_eq!(normalize_repo_key("."), None);
        assert_eq!(normalize_repo_key("a/b"), None);
        assert_eq!(normalize_repo_key("a\\b"), None);
    }

    #[test]
    fn issue_repo_key_prefers_source_repo_then_id_prefix() {
        // Go `issueRepoKey`: source_repo first, then ID prefix (Go
        // `ExtractRepoPrefix`: first -/:/_ separator, prefix ≤ 10 ASCII
        // alphanumerics).
        assert_eq!(
            issue_repo_key("T-1", "web"),
            Some("web".into()),
            "source_repo wins over ID prefix"
        );
        assert_eq!(
            issue_repo_key("api-AUTH-1", ""),
            Some("api".into()),
            "ID prefix fallback"
        );
        assert_eq!(
            issue_repo_key("averylongprefixname-X-1", ""),
            None,
            "prefix > 10 chars is not a repo key"
        );
        assert_eq!(
            issue_repo_key("T-1", ""),
            Some("t".into()),
            "short ID prefix still counts (Go has no minimum length)"
        );
        assert_eq!(issue_repo_key("noprefix", ""), None);
    }

    #[test]
    fn diff_badge_strings_match_go_exactly() {
        assert_eq!(DiffBadge::None.badge(), "");
        assert_eq!(DiffBadge::Added.badge(), "🆕");
        assert_eq!(DiffBadge::Closed.badge(), "✅");
        assert_eq!(DiffBadge::Modified.badge(), "~");
    }

    #[test]
    fn row_triage_marks_blocker_and_quick_win_on_chain() {
        // A blocks B blocks C (all open): A unblocks B's sole blocker... B
        // has exactly one open blocker (A), C has exactly one (B). A and B
        // both have unblocks > 0 → blockers; quick-win ranking picks among
        // the claimable (open, non-epic, unassigned, zero open blockers).
        use bv_core::model::{Dependency, DependencyType};
        let dep = |on: &str| Dependency {
            issue_id: String::new(),
            depends_on_id: on.into(),
            depends_on_legacy: String::new(),
            target_id_legacy: String::new(),
            r#type: DependencyType::Blocks,
            created_at: None,
            created_by: String::new(),
        };
        let issue = |id: &str, deps: Vec<Dependency>| bv_core::model::Issue {
            id: id.into(),
            content_hash: String::new(),
            title: format!("{id} title"),
            description: String::new(),
            design: String::new(),
            acceptance_criteria: String::new(),
            notes: String::new(),
            status: Status::Open,
            priority: 2,
            issue_type: "task".into(),
            assignee: String::new(),
            estimated_minutes: None,
            created_at: None,
            updated_at: None,
            due_date: None,
            defer_until: None,
            closed_at: None,
            external_ref: None,
            compaction_level: 0,
            compacted_at: None,
            compacted_at_commit: None,
            original_size: 0,
            labels: vec![],
            dependencies: deps,
            comments: vec![],
            source_repo: String::new(),
        };
        let issues = vec![
            issue("A", vec![]),
            issue("B", vec![dep("A")]),
            issue("C", vec![dep("B")]),
        ];
        let triage = bv_analysis::triage::compute_row_triage(&issues);
        assert_eq!(triage["A"].unblocks_count, 1, "A is B's sole open blocker");
        assert_eq!(triage["B"].unblocks_count, 1, "B is C's sole open blocker");
        assert_eq!(triage["C"].unblocks_count, 0);
        assert!(triage["A"].is_blocker);
        assert!(triage["B"].is_blocker);
        assert!(!triage["C"].is_blocker);
        // Only A is claimable (B and C each have an open blocker), so A
        // must be the quick win.
        assert!(triage["A"].is_quick_win);
        assert!(!triage["B"].is_quick_win);
    }

    #[test]
    fn time_travel_t_opens_revision_prompt_not_history() {
        let mut app = make_app(3);
        app.handle_key(KeyCode::Char('t'));
        assert_eq!(
            app.current_view,
            ViewMode::TimeTravel,
            "t must open the TimeTravel view, not silently alias to History"
        );
        assert_eq!(
            app.time_travel_prompt,
            Some(String::new()),
            "t must open the revision-input prompt (Go focusTimeTravelInput)"
        );
    }

    #[test]
    fn time_travel_prompt_typing_backspace_and_cancel() {
        let mut app = make_app(3);
        app.handle_key(KeyCode::Char('t'));
        for c in "HEAD~".chars() {
            app.handle_key(KeyCode::Char(c));
        }
        app.handle_key(KeyCode::Char('5'));
        assert_eq!(app.time_travel_prompt.as_deref(), Some("HEAD~5"));
        app.handle_key(KeyCode::Backspace);
        assert_eq!(app.time_travel_prompt.as_deref(), Some("HEAD~"));
        app.handle_key(KeyCode::Esc);
        assert_eq!(app.time_travel_prompt, None);
        assert_eq!(
            app.current_view,
            ViewMode::List,
            "cancelling with no result returns to list"
        );
    }

    #[test]
    fn time_travel_prompt_enter_empty_keeps_prompt_open() {
        let mut app = make_app(3);
        app.handle_key(KeyCode::Char('t'));
        app.handle_key(KeyCode::Enter);
        assert_eq!(
            app.time_travel_prompt,
            Some(String::new()),
            "empty revision must not run git or close the prompt"
        );
        assert!(app.time_travel_result.is_none());
        assert!(app.time_travel_error.is_none());
    }

    /// Synthetic "previous revision" with one added, one removed, one
    /// changed issue relative to `make_app(3)` (T-0 closed, T-1/T-2 open).
    fn seed_time_travel_previous(app: &App) -> Vec<bv_core::model::Issue> {
        let mut previous: Vec<bv_core::model::Issue> = app.issue_map.values().cloned().collect();
        previous.retain(|i| i.id != "T-1"); // → added now
        previous
            .iter_mut()
            .find(|i| i.id == "T-0")
            .expect("T-0 in map")
            .status = Status::Open; // → changed (now Closed)
        let mut extra = app.issue_map["T-2"].clone();
        extra.id = "X-9".to_string();
        previous.push(extra); // → removed now
        previous
    }

    #[test]
    fn time_travel_apply_computes_added_removed_changed() {
        let mut app = make_app(3);
        let previous = seed_time_travel_previous(&app);
        app.apply_time_travel_result(previous, "HEAD~5");
        let r = app.time_travel_result.as_ref().expect("result stored");
        assert_eq!(r.diff_ref, "HEAD~5");
        assert_eq!(r.added, vec!["T-1".to_string()]);
        assert_eq!(r.removed, vec!["X-9".to_string()]);
        assert_eq!(r.changed.len(), 1);
        assert_eq!(r.changed[0].id, "T-0");
        assert!(r.changed[0].status_change.is_some());
        assert!(app.time_travel_error.is_none());
        assert_eq!(app.time_travel_cursor, 0);
        assert!(
            app.status_msg.starts_with("Time-Travel vs HEAD~5"),
            "unexpected status: {}",
            app.status_msg
        );
    }

    #[test]
    fn time_travel_j_k_moves_diff_cursor_not_list() {
        let mut app = make_app(3);
        let previous = seed_time_travel_previous(&app);
        app.apply_time_travel_result(previous, "HEAD~5");
        app.current_view = ViewMode::TimeTravel;
        assert_eq!(app.time_travel_entry_count(), 3);
        let cursor_before = app.cursor;
        app.handle_key(KeyCode::Char('j'));
        assert_eq!(app.time_travel_cursor, 1);
        assert_eq!(app.cursor, cursor_before);
        app.handle_key(KeyCode::Char('j'));
        app.handle_key(KeyCode::Char('j')); // clamped at last
        assert_eq!(app.time_travel_cursor, 2);
        app.handle_key(KeyCode::Char('k'));
        assert_eq!(app.time_travel_cursor, 1);
    }

    #[test]
    fn time_travel_esc_in_view_returns_to_list() {
        let mut app = make_app(3);
        app.current_view = ViewMode::TimeTravel;
        app.handle_key(KeyCode::Esc);
        assert_eq!(app.current_view, ViewMode::List);
        assert!(!app.quit_requested, "esc in a view must not quit");
    }

    #[test]
    fn time_travel_upper_t_runs_instant_head5_diff() {
        let mut app = make_app(3);
        app.handle_key(KeyCode::Char('T'));
        assert_eq!(app.current_view, ViewMode::TimeTravel);
        assert_eq!(app.time_travel_prompt, None);
        assert!(
            app.status_msg.starts_with("Time-Travel"),
            "T must run the diff (or report the error) in the status line, got: {}",
            app.status_msg
        );
    }

    /// App with two real labels: backend on T-1/T-2, frontend on T-4/T-5
    /// (make_app statuses: every 3rd id closed, so all four are open).
    fn make_labeled_app() -> App {
        let mut app = make_app(6);
        for id in ["T-1", "T-2"] {
            app.issue_map.get_mut(id).unwrap().labels = vec!["backend".to_string()];
        }
        for id in ["T-4", "T-5"] {
            app.issue_map.get_mut(id).unwrap().labels = vec!["frontend".to_string()];
        }
        app
    }

    #[test]
    fn label_dashboard_opens_and_loads_lazily() {
        let mut app = make_labeled_app();
        assert!(!app.label_health_loaded);
        app.handle_key(KeyCode::Char('['));
        assert_eq!(app.current_view, ViewMode::LabelDashboard);
        assert!(app.label_health_loaded, "[ should trigger lazy health load");
        assert_eq!(app.label_dashboard_count(), 2);
        // Toggling back must not reload.
        app.handle_key(KeyCode::Char('['));
        assert_eq!(app.current_view, ViewMode::List);
        assert!(app.label_health_loaded);
    }

    #[test]
    fn label_dashboard_j_k_moves_dashboard_cursor_not_list() {
        let mut app = make_labeled_app();
        app.handle_key(KeyCode::Char('['));
        assert_eq!(app.label_dashboard_count(), 2);
        let cursor_before = app.cursor;
        app.handle_key(KeyCode::Char('j'));
        assert_eq!(app.label_dashboard_cursor, 1);
        assert_eq!(app.cursor, cursor_before);
        app.handle_key(KeyCode::Char('j')); // clamped at last
        assert_eq!(app.label_dashboard_cursor, 1);
        app.handle_key(KeyCode::Char('k'));
        assert_eq!(app.label_dashboard_cursor, 0);
    }

    #[test]
    fn label_dashboard_h_toggles_detail_not_history() {
        let mut app = make_labeled_app();
        app.handle_key(KeyCode::Char('['));
        app.handle_key(KeyCode::Char('h'));
        assert!(
            app.show_label_detail,
            "h in dashboard must open the detail modal"
        );
        assert_eq!(
            app.current_view,
            ViewMode::LabelDashboard,
            "h in dashboard must not switch to History"
        );
        app.handle_key(KeyCode::Char('h'));
        assert!(!app.show_label_detail);
    }

    #[test]
    fn label_dashboard_d_drilldown_enter_applies_label_filter() {
        let mut app = make_labeled_app();
        app.handle_key(KeyCode::Char('['));
        let first = app
            .selected_dashboard_label()
            .expect("a label is selected")
            .label
            .clone();
        app.handle_key(KeyCode::Char('d'));
        assert_eq!(
            app.label_drilldown.as_ref().map(|d| d.label.as_str()),
            Some(first.as_str())
        );
        // Typing narrows the drilldown rows; Enter jumps to List filtered.
        app.handle_key(KeyCode::Char('T'));
        assert!(
            app.drilldown_rows()
                .iter()
                .all(|(id, title, _)| id.contains('T') || title.contains('T')),
            "filter must narrow rows to the typed substring"
        );
        app.handle_key(KeyCode::Enter);
        assert_eq!(app.label_filter, Some(first));
        assert_eq!(app.current_view, ViewMode::List);
        assert!(app.label_drilldown.is_none());
    }

    #[test]
    fn label_dashboard_drilldown_esc_closes_stays_in_view() {
        let mut app = make_labeled_app();
        app.handle_key(KeyCode::Char('['));
        app.handle_key(KeyCode::Char('d'));
        assert!(app.label_drilldown.is_some());
        app.handle_key(KeyCode::Esc);
        assert!(app.label_drilldown.is_none());
        assert_eq!(app.current_view, ViewMode::LabelDashboard);
    }

    #[test]
    fn label_dashboard_esc_returns_to_list() {
        let mut app = make_labeled_app();
        app.handle_key(KeyCode::Char('['));
        app.handle_key(KeyCode::Esc);
        assert_eq!(app.current_view, ViewMode::List);
        assert!(!app.quit_requested, "esc in a view must not quit");
    }

    #[test]
    fn label_picker_opens_with_real_label_counts() {
        let mut app = make_app(4);
        app.rows[0].labels = vec!["backend".to_string()];
        app.rows[1].labels = vec!["backend".to_string(), "urgent".to_string()];
        app.handle_key(KeyCode::Char('l'));
        let picker = app.label_picker.as_ref().expect("picker should be built");
        assert!(picker.visible);
        let backend = picker.labels.iter().find(|l| l.name == "backend").unwrap();
        assert_eq!(backend.count, 2);
        let urgent = picker.labels.iter().find(|l| l.name == "urgent").unwrap();
        assert_eq!(urgent.count, 1);
    }

    #[test]
    fn label_picker_enter_applies_filter_and_closes() {
        let mut app = make_app(4);
        app.rows[0].labels = vec!["backend".to_string()];
        app.handle_key(KeyCode::Char('l'));
        app.handle_key(KeyCode::Enter);
        assert_eq!(app.label_filter, Some("backend".to_string()));
        assert!(!app.label_picker.as_ref().unwrap().visible);
    }

    #[test]
    fn recipe_picker_opens_with_six_builtin_recipes() {
        let mut app = make_app(3);
        app.handle_key(KeyCode::Char('\''));
        let picker = app
            .recipe_picker
            .as_ref()
            .expect("recipe picker should be built");
        assert!(picker.visible);
        assert_eq!(picker.recipes.len(), 6);
        assert_eq!(picker.recipes[0].name, "triage");
    }

    #[test]
    fn quick_wins_recipe_filters_by_priority_and_no_dependencies() {
        // make_app: status closes every 3rd id (i%3==0), priority = i%4.
        // With n=8: T-2 (open, p2) and T-7 (open, p3) are the only
        // open+priority>=2 issues. Give T-2 a dependency so it should be
        // excluded by quick-wins, leaving T-7 as the sole quick win.
        let mut app = make_app(8);
        let dep = bv_core::model::Dependency {
            issue_id: "T-2".into(),
            depends_on_id: "T-0".into(),
            depends_on_legacy: String::new(),
            target_id_legacy: String::new(),
            r#type: Default::default(),
            created_at: None,
            created_by: String::new(),
        };
        app.issue_map.get_mut("T-2").unwrap().dependencies = vec![dep];
        app.apply_recipe(4); // index 4 == "quick-wins" in default_recipe_defs()
        let ids: Vec<&str> = app
            .filtered_indices
            .iter()
            .map(|&i| app.rows[i].id.as_str())
            .collect();
        assert!(
            ids.contains(&"T-7"),
            "T-7 (priority 3, no deps, open) should be a quick win"
        );
        assert!(
            !ids.contains(&"T-2"),
            "T-2 has a dependency — quick-wins requires none"
        );
    }

    #[test]
    fn update_modal_only_shows_when_tag_present() {
        let mut app = make_app(2);
        app.handle_key(KeyCode::Char('U'));
        assert!(
            !app.show_update_modal,
            "U with no update_tag must not open the modal"
        );
        app.update_tag = Some("v9.9.9".to_string());
        app.handle_key(KeyCode::Char('U'));
        assert!(app.show_update_modal);
    }

    // -- Go pkg/ui/update_keys_test.go + model.go:9653-9667 ---------------

    /// Go's status string, byte for byte (`model.go:9656`).
    const GO_NO_UPDATE: &str = "No update available - you're running the latest version";

    #[test]
    fn u_without_a_release_reports_the_go_status_string() {
        // Go `showSelfUpdateModal` returns early on
        // `!m.updateAvailable || m.updateTag == ""`, and Rust's
        // `update_tag.is_some()` alone is not enough: an empty tag is the
        // same condition.
        for tag in [None, Some(String::new())] {
            let mut app = make_app(2);
            app.update_tag = tag;
            app.handle_key(KeyCode::Char('U'));
            assert!(!app.show_update_modal, "modal must not open");
            assert_eq!(app.status_msg, GO_NO_UPDATE);
        }
    }

    #[test]
    fn u_opens_a_confirm_modal_carrying_the_release_url() {
        // Go `NewUpdateModal(m.updateTag, m.updateURL, m.theme)`
        // (`model.go:9663`) — the URL is constructor input, not something the
        // modal fetches.
        let mut app = make_app(2);
        app.update_tag = Some("v9.9.9".to_string());
        app.update_url = Some("https://example/releases/tag/v9.9.9".to_string());
        app.handle_key(KeyCode::Char('U'));
        assert!(app.show_update_modal);
        let modal = app.update_modal.as_ref().expect("modal built");
        assert_eq!(modal.state, crate::update_modal::UpdateState::Confirm);
        assert_eq!(modal.new_version, "v9.9.9");
        assert_eq!(modal.release_url, "https://example/releases/tag/v9.9.9");
        // Go `SetSize` on open (`model.go:9664`), which clamps to [50, 70]
        // after subtracting 10. `make_app` is 120 columns wide.
        assert_eq!(modal.width, 70);
    }

    #[test]
    fn modal_current_version_carries_the_v_prefix() {
        // Go stamps `version.Version` with a leading "v" (pkg/version
        // normalizeVersion) and `NewUpdateModal` copies it straight into
        // `currentVersion` (`update_modal.go:80`). Passing
        // env!("CARGO_PKG_VERSION") here rendered "0.2.0" while every other
        // surface showed "v0.2.0".
        let mut app = make_app(2);
        app.update_tag = Some("v9.9.9".to_string());
        app.handle_key(KeyCode::Char('U'));
        let modal = app.update_modal.as_ref().expect("modal built");
        assert_eq!(modal.current_version, bv_update::current_version());
        assert!(
            modal.current_version.starts_with('v'),
            "modal showed {:?}",
            modal.current_version
        );
    }

    #[test]
    fn modal_keys_drive_the_state_machine_and_close_policy() {
        // Go `model.go:3788-3821`: the modal sees the key first, then the
        // parent applies its close policy; the key never reaches the list.
        let mut app = make_app(2);
        app.update_tag = Some("v9.9.9".to_string());
        app.handle_key(KeyCode::Char('U'));
        assert!(app.show_update_modal);

        // `h`/`l` move the button focus without closing.
        assert!(app.handle_key(KeyCode::Char('l')));
        assert_eq!(app.update_modal.as_ref().unwrap().confirm_focus, 1);
        assert!(app.show_update_modal);

        // `n` cancels and closes.
        assert!(app.handle_key(KeyCode::Char('n')));
        assert!(!app.show_update_modal);
    }

    #[test]
    fn successful_install_retires_the_notice_only_for_the_advertised_tag() {
        // Go `UpdateCompleteMsg` arm (`model.go:2306-2314`): clear only when
        // the install succeeded AND wrote the tag the notice advertises — a
        // failed or stale completion must leave the retry notice in place.
        let cases = [
            ("successful install", true, "v9.9.9", true),
            ("failed install", false, "v9.9.9", false),
            ("stale successful install", true, "v9.9.8", false),
        ];
        for (name, success, version, want_clear) in cases {
            let mut app = make_app(2);
            app.update_tag = Some("v9.9.9".to_string());
            app.update_url = Some("https://example/releases/tag/v9.9.9".to_string());
            app.update_modal = Some(crate::update_modal::UpdateModal::new(
                "v0.2.0", "v9.9.9", "",
            ));
            let modal = app.update_modal.as_mut().unwrap();
            modal.state = if success {
                crate::update_modal::UpdateState::Success
            } else {
                crate::update_modal::UpdateState::Error
            };
            modal.completed_version = version.to_string();
            app.poll_update_modal();
            if want_clear {
                assert!(app.update_tag.is_none(), "{name}: notice not retired");
                assert!(app.update_url.is_none(), "{name}: url not retired");
            } else {
                assert_eq!(app.update_tag.as_deref(), Some("v9.9.9"), "{name}");
                assert_eq!(
                    app.update_url.as_deref(),
                    Some("https://example/releases/tag/v9.9.9"),
                    "{name}"
                );
            }
        }
    }

    #[test]
    fn poll_update_modal_is_a_no_op_with_no_modal() {
        let mut app = make_app(2);
        app.update_tag = Some("v9.9.9".to_string());
        assert!(!app.poll_update_modal());
        assert_eq!(app.update_tag.as_deref(), Some("v9.9.9"));
    }

    // -- Go model.go:7649-7655 / 8894-8895 --------------------------------

    /// The TUI's startup update check must be gated on Go's
    /// `updater.StartupCheckEnabled()` (`model.go:2076`), not on whether
    /// `BV_NO_UPDATE_CHECK` is *set*. Go's `envTruthy`
    /// (`updater.go:87-93`) reads only a non-empty value that is not one of
    /// `0/false/no/off`, and `pkg/updater/network_test.go:398-404` pins the
    /// case that used to break: "BV_NO_UPDATE_CHECK=0 must not count as an
    /// opt-out". The old gate, `std::env::var(...).is_err()`, opted out for
    /// every value including `0`.
    #[test]
    fn startup_update_check_gate_is_truthiness_based_not_presence_based() {
        use std::sync::{Mutex, MutexGuard, OnceLock};
        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        // The environment is process-global; every mutation here is paired
        // with a restore, and the lock keeps concurrent tests from reading a
        // half-applied set.
        let _guard: MutexGuard<'_, ()> = ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let keys = [
            "BV_NO_UPDATE_CHECK",
            "BV_NO_SAVED_CONFIG",
            "XDG_CONFIG_HOME",
        ];
        let saved: Vec<Option<String>> = keys.iter().map(|k| std::env::var(k).ok()).collect();
        // Point at an empty config dir so a developer's real config.yaml
        // cannot decide the result.
        let empty_cfg = std::env::temp_dir().join("bvr_tui_update_gate_absent");
        std::env::set_var("XDG_CONFIG_HOME", &empty_cfg);
        std::env::set_var("BV_NO_SAVED_CONFIG", "");

        let restore = || {
            for (k, v) in keys.iter().zip(&saved) {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        };

        for (value, want_check) in [
            (None, true),
            (Some("1"), false),
            (Some("true"), false),
            // Go's explicit non-opt-outs: the check still runs.
            (Some("0"), true),
            (Some("false"), true),
            (Some("no"), true),
            (Some("off"), true),
            (Some(""), true),
        ] {
            match value {
                Some(v) => std::env::set_var("BV_NO_UPDATE_CHECK", v),
                None => std::env::remove_var("BV_NO_UPDATE_CHECK"),
            }
            assert_eq!(
                startup_update_check_enabled(),
                want_check,
                "BV_NO_UPDATE_CHECK={value:?}"
            );
            // The old gate would have disabled the check for every Some case.
            if value.is_some() {
                assert!(
                    std::env::var("BV_NO_UPDATE_CHECK").is_ok(),
                    "sanity: the variable is actually set for {value:?}"
                );
            }
        }
        restore();
    }

    #[test]
    fn update_modal_overlay_renders_the_confirm_body() {
        let mut app = make_app(3);
        app.update_tag = Some("v9.9.9".to_string());
        app.handle_key(KeyCode::Char('U'));
        let out = render_with(&mut app, 120, 30);
        assert!(out.contains("Update Available"), "modal not drawn:\n{out}");
        assert!(out.contains("v9.9.9"), "modal lacks the new tag:\n{out}");
        assert!(out.contains("Current version: v"), "modal row:\n{out}");
        assert!(
            out.contains("[Y] Update"),
            "confirm affordance missing:\n{out}"
        );
    }

    fn render_with(app: &mut App, width: u16, height: u16) -> String {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        app.width = width;
        app.height = height;
        terminal.draw(|f| render(f, app)).unwrap();
        buffer_to_lines(terminal.backend().buffer())
    }

    #[test]
    fn update_badge_uses_the_go_glyph_and_text() {
        // Go `updateSection` renders "⭐ Update %s" (`model.go:7655`),
        // U+2B50 WHITE MEDIUM STAR. bvr shipped U+2B06 BLACK FOUR POINTED
        // STAR instead.
        //
        // The star also decorates P0/P1 rows, so assertions are scoped to the
        // status-bar row that carries the tag. A double-width cell renders
        // with a filler space, so the glyph is matched as "immediately
        // before the text" rather than as a literal substring.
        let mut app = make_app(3);
        let clean = render_with(&mut app, 120, 20);
        assert!(
            !clean.contains("Update v"),
            "badge appeared with no pending update:\n{clean}"
        );

        app.update_tag = Some("v9.9.9".to_string());
        let with = render_with(&mut app, 120, 20);
        let badge = with
            .lines()
            .find(|l| l.contains("Update v9.9.9"))
            .unwrap_or_else(|| panic!("badge row missing:\n{with}"));
        let before = badge.split("Update v9.9.9").next().unwrap().trim_end();
        assert!(
            before.ends_with('\u{2b50}'),
            "Go badge must read \"⭐ Update v9.9.9\": {badge:?}"
        );
        assert!(
            !with.contains('\u{2b06}'),
            "wrong star glyph still rendered:\n{with}"
        );
    }

    #[test]
    fn detail_pane_opens_with_the_go_update_line() {
        // Go `updateViewportContent` (`model.go:8894-8895`) writes
        // "⭐ **Update Available:** [%s](%s)\n\n" before the title block, raw
        // link syntax included.
        let mut app = make_app(3);
        app.split_view = true;
        app.show_detail = true;
        let clean = render_with(&mut app, 140, 30);
        assert!(!clean.contains("Update Available"), "unexpected:\n{clean}");

        app.update_tag = Some("v9.9.9".to_string());
        app.update_url = Some("https://example/releases/tag/v9.9.9".to_string());
        let with = render_with(&mut app, 140, 30);
        let notice = with
            .lines()
            .find(|l| l.contains("**Update Available:**"))
            .unwrap_or_else(|| panic!("detail notice missing:\n{with}"));
        assert!(
            notice.contains("**Update Available:** [v9.9.9](https://example/releases/tag/v9.9.9)"),
            "Go detail link malformed: {notice:?}"
        );
        // The star is the notice's first character; the split-view border
        // sits in front of it and the double-width cell leaves a filler space,
        // so match on "the glyph immediately precedes the bold text".
        let before = notice
            .split("**Update Available:**")
            .next()
            .unwrap()
            .trim_end();
        assert!(
            before.ends_with('\u{2b50}'),
            "detail notice must open with U+2B50: {notice:?}"
        );
    }

    #[test]
    fn priority_hints_toggle_computes_once_and_sets_status() {
        let mut app = make_app(6);
        assert!(app.priority_hints.is_none());
        app.handle_key(KeyCode::Char('p'));
        assert!(app.show_priority_hints);
        assert!(
            app.priority_hints.is_some(),
            "toggling on should trigger the lazy compute"
        );
        assert!(
            app.status_msg.starts_with("Priority hints:"),
            "unexpected status message: {}",
            app.status_msg
        );
        let computed_once = app.priority_hints.clone();
        // Toggling off then on again must not recompute (same Some(map)),
        // matching the lazy-compute-once pattern used by History/LabelDashboard.
        app.handle_key(KeyCode::Char('p'));
        assert!(!app.show_priority_hints);
        assert!(app.status_msg.is_empty());
        app.handle_key(KeyCode::Char('p'));
        assert_eq!(
            app.priority_hints.as_ref().map(|m| m.len()),
            computed_once.map(|m| m.len())
        );
    }

    #[test]
    fn check_for_reload_detects_mtime_change() {
        let mut app = make_app(2);
        let dir = std::env::temp_dir().join(format!("bvr_watch_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("issues.jsonl");
        std::fs::write(&path, "").unwrap();
        let mtime1 = std::fs::metadata(&path).unwrap().modified().unwrap();
        app.watched_path = Some(path.clone());
        app.watched_mtime = Some(mtime1);
        assert!(
            !app.check_for_reload(),
            "unchanged mtime must not trigger a reload"
        );
        // Some filesystems have coarse mtime resolution — sleep briefly to
        // guarantee a strictly-later timestamp on the rewrite below.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&path, "{}\n").unwrap();
        assert!(
            app.check_for_reload(),
            "changed mtime must trigger reload_from_disk"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_for_reload_is_noop_without_a_watched_path() {
        let mut app = make_app(2);
        app.watched_path = None;
        assert!(!app.check_for_reload());
    }

    #[test]
    fn quit_sets_flag() {
        let mut app = make_app(3);
        app.handle_key(KeyCode::Char('q'));
        assert!(app.quit_requested);
    }

    fn make_cass_modal() -> crate::cass::CassModalState {
        let session = |agent: &str| crate::cass::ScoredSession {
            session: crate::cass::CassSession {
                source_path: "s.md".into(),
                line_number: 1,
                agent: agent.into(),
                title: "t".into(),
                score: 0.9,
                snippet: "snippet here".into(),
                timestamp: "2026-09-09T10:00:00Z".into(),
                match_type: "exact".into(),
            },
            strategy: crate::cass::CorrelationStrategy::IdMention,
            keywords: Vec::new(),
        };
        crate::cass::CassModalState {
            bead_id: "T-0".into(),
            sessions: vec![session("claude"), session("cursor")],
            keywords: Vec::new(),
            search_cmd: "cass search \"T-0\"".into(),
            selected: 0,
            copied_at: None,
        }
    }

    #[test]
    fn cass_modal_v_opens_only_when_sessions_found() {
        // No cass on PATH in test env (or no sessions) → status message,
        // never a modal. This pins the fail-soft contract: V must never
        // crash or open an empty modal.
        let mut app = make_app(3);
        app.cass_available = false;
        app.handle_key(KeyCode::Char('V'));
        assert!(app.cass_modal.is_none());
        assert!(
            app.status_msg.contains("cass not available"),
            "Go's exact missing-cass message, got: {}",
            app.status_msg
        );
    }

    #[test]
    fn cass_modal_jk_navigates_and_dismiss_keys_close() {
        let mut app = make_app(3);
        app.cass_modal = Some(make_cass_modal());
        app.handle_key(KeyCode::Char('j'));
        assert_eq!(app.cass_modal.as_ref().unwrap().selected, 1);
        app.handle_key(KeyCode::Char('j')); // clamped at last
        assert_eq!(app.cass_modal.as_ref().unwrap().selected, 1);
        app.handle_key(KeyCode::Char('k'));
        assert_eq!(app.cass_modal.as_ref().unwrap().selected, 0);
        // Go's exact dismiss set: V / Esc / Enter / q.
        for key in [
            KeyCode::Char('V'),
            KeyCode::Esc,
            KeyCode::Enter,
            KeyCode::Char('q'),
        ] {
            app.cass_modal = Some(make_cass_modal());
            app.handle_key(key);
            assert!(
                app.cass_modal.is_none(),
                "dismiss key {key:?} must close the modal"
            );
        }
    }

    #[test]
    fn cass_modal_y_copies_search_cmd_without_closing() {
        let mut app = make_app(3);
        app.cass_modal = Some(make_cass_modal());
        // y copies (clipboard may fail headless — either way the modal
        // stays open; only success sets the flash timestamp).
        app.handle_key(KeyCode::Char('y'));
        assert!(app.cass_modal.is_some(), "y must not close the modal");
    }

    #[test]
    fn esc_first_clears_filters_then_confirms() {
        // Go: first Esc at the main list clears active filters
        // (`hasActiveFilters` → `clearAllFilters`); only the second Esc
        // shows the quit confirmation.
        let mut app = make_app(9);
        app.filter_mode = FilterMode::Open;
        app.apply_filter();
        assert_ne!(app.filtered_indices.len(), 9);
        app.handle_key(KeyCode::Esc);
        assert!(!app.quit_requested, "first Esc must clear, not quit");
        assert!(!app.show_quit_confirm, "no confirm while clearing");
        assert_eq!(app.filter_mode, FilterMode::All);
        assert_eq!(app.filtered_indices.len(), 9);
    }

    #[test]
    fn esc_twice_without_filters_shows_confirm_then_quits() {
        let mut app = make_app(3);
        app.handle_key(KeyCode::Esc);
        assert!(
            app.show_quit_confirm,
            "second Esc (no filters) must show confirm"
        );
        assert!(!app.quit_requested, "confirm must not quit by itself");
        app.handle_key(KeyCode::Char('y'));
        assert!(app.quit_requested, "y confirms the quit");
    }

    #[test]
    fn quit_confirm_other_key_cancels() {
        // Go: any key other than Esc/Y cancels back to the list.
        let mut app = make_app(3);
        app.handle_key(KeyCode::Esc);
        assert!(app.show_quit_confirm);
        app.handle_key(KeyCode::Char('n'));
        assert!(!app.show_quit_confirm, "other key must cancel confirm");
        assert!(!app.quit_requested);
        assert_eq!(app.current_view, ViewMode::List);
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            app.handle_key(KeyCode::Char(c));
        }
    }

    #[test]
    fn fuzzy_search_surfaces_substring_match_ranked() {
        let mut app = make_app(9);
        app.handle_key(KeyCode::Char('/'));
        // Lowercase on purpose: matching must be case-insensitive, and the
        // old contains-match ("Issue 1") must still surface.
        type_text(&mut app, "issue 1");
        let t1 = app.rows.iter().position(|r| r.id == "T-1").unwrap();
        assert_eq!(
            app.filtered_indices,
            vec![t1],
            "only T-1 contains '1'; fuzzy must not invent matches"
        );
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn fuzzy_search_empty_query_restores_all_rows() {
        let mut app = make_app(4);
        app.handle_key(KeyCode::Char('/'));
        type_text(&mut app, "Issue");
        assert!(!app.filtered_indices.is_empty());
        for _ in 0..5 {
            app.handle_key(KeyCode::Backspace);
        }
        assert_eq!(app.filtered_indices.len(), app.rows.len());
    }

    #[test]
    fn semantic_search_ranks_by_embedding_overlap() {
        let mut app = make_app(3);
        app.rows[0].title = "database migration".to_string();
        app.rows[1].title = "database backup".to_string();
        app.rows[2].title = "button color".to_string();
        assert!(app.handle_ctrl_key(KeyCode::Char('s')));
        assert!(app.semantic_searching);
        type_text(&mut app, "database");
        let mut ids: Vec<&str> = app
            .filtered_indices
            .iter()
            .map(|&i| app.rows[i].id.as_str())
            .collect();
        ids.sort_unstable();
        // Hybrid scoring (Go `hybridScorer.Score`): every row gets status /
        // priority / recency components even with zero text overlap, so the
        // disjoint row now ranks (low) instead of being hidden. The old
        // text-only assertion (disjoint hidden) belongs to the pre-hybrid
        // engine. What matters: token-overlapping rows rank FIRST, in order.
        assert!(
            ids.starts_with(&["T-0", "T-1"]) || ids.starts_with(&["T-1", "T-0"]),
            "token-overlapping rows must rank first, got {ids:?}"
        );
    }

    #[test]
    fn semantic_tab_cycles_hybrid_preset_and_reranks() {
        let mut app = make_app(3);
        app.handle_ctrl_key(KeyCode::Char('s'));
        type_text(&mut app, "database");
        assert_eq!(app.semantic_preset, "default");
        app.handle_key(KeyCode::Tab);
        assert_eq!(app.semantic_preset, "bug-hunting");
        // Full cycle wraps back to default.
        for _ in 0..4 {
            app.handle_key(KeyCode::Tab);
        }
        assert_eq!(app.semantic_preset, "default");
    }

    #[test]
    fn semantic_index_and_fallback_agree_on_ranking() {
        // The persistent index is a pure cache: ranking through it must
        // match fresh embedding. Build an in-memory index over the same
        // IssueDocument texts `apply_semantic` uses, install it, and
        // compare against the fallback path (index cleared).
        let mut app = make_app(3);
        app.rows[0].title = "database migration".to_string();
        app.rows[1].title = "database backup".to_string();
        app.rows[2].title = "button color".to_string();
        let dim = bv_search::embedder::DEFAULT_DIM;
        let mut idx = bv_search::vector_index::VectorIndex::new(dim);
        for r in &app.rows {
            let doc = bv_search::query::issue_document(&r.id, &r.title, &r.labels, &r.description);
            idx.upsert(
                &r.id,
                bv_search::vector_index::compute_content_hash(&doc),
                bv_search::embedder::hash_embed(&doc, dim),
            )
            .unwrap();
        }
        app.semantic_query = "database".to_string();
        app.semantic_index = Some(idx);
        app.apply_semantic();
        let via_index = app.filtered_indices.clone();
        app.semantic_index = None;
        app.apply_semantic();
        assert_eq!(
            via_index, app.filtered_indices,
            "index-backed and fresh ranking must agree"
        );
    }

    #[test]
    fn semantic_esc_clears_and_exits() {
        let mut app = make_app(3);
        app.handle_ctrl_key(KeyCode::Char('s'));
        type_text(&mut app, "x");
        assert!(app.semantic_searching);
        app.handle_key(KeyCode::Esc);
        assert!(!app.semantic_searching);
        assert!(app.semantic_query.is_empty());
        assert_eq!(app.filtered_indices.len(), app.rows.len());
    }

    #[test]
    fn semantic_enter_accepts_back_to_standard_order() {
        let mut app = make_app(3);
        app.handle_ctrl_key(KeyCode::Char('S'));
        type_text(&mut app, "Issue");
        app.handle_key(KeyCode::Enter);
        assert!(!app.semantic_searching);
        assert_eq!(app.filtered_indices.len(), app.rows.len());
    }
    /// Regression: mixed-case needles once panicked inside nucleo-matcher
    /// 0.3.1 (uppercase char matchable only via case folding passed the
    /// prefilter but tripped the optimal-matcher's reject assert). The
    /// needle is pre-folded now, so every ASCII prefix must rank without
    /// panicking and the full word must still find its row.
    #[test]
    fn fuzzy_search_mixed_case_prefixes_never_panic() {
        for q in [
            "I", "Is", "Iss", "Issu", "Issue", "i", "is", "issue", "issue 1", "1", "T-", "0",
        ] {
            let mut app = make_app(4);
            app.search_query = q.to_string();
            app.apply_search();
        }
        let mut app = make_app(4);
        app.search_query = "ISSUE 2".to_string();
        app.apply_search();
        let t2 = app.rows.iter().position(|r| r.id == "T-2").unwrap();
        assert_eq!(app.filtered_indices, vec![t2]);
    }
    #[cfg(test)]
    mod cleanup_tests {
        use super::*;

        #[test]
        fn board_group_cycle_resets_column() {
            use crate::views::board::SwimlaneMode;
            let mut app = make_app(6);
            app.handle_key(KeyCode::Char('b'));
            assert_eq!(app.current_view, ViewMode::Board);
            app.handle_key(KeyCode::Char('s'));
            assert_eq!(app.board_mode, SwimlaneMode::Priority);
            assert_eq!(app.board_column, 0);
            assert!(app.status_msg.contains("Priority"));
            app.handle_key(KeyCode::Char('s'));
            assert_eq!(app.board_mode, SwimlaneMode::Type);
            app.handle_key(KeyCode::Char('s'));
            assert_eq!(app.board_mode, SwimlaneMode::Status);
        }

        #[test]
        fn board_column_nav_and_digit_jump() {
            let mut app = make_app(6);
            app.handle_key(KeyCode::Char('b'));
            let cursor_before = app.cursor;
            // Status mode has 4 columns; `l` advances, `h` retreats, clamped.
            app.handle_key(KeyCode::Char('l'));
            assert_eq!(app.board_column, 1);
            app.handle_key(KeyCode::Char('h'));
            assert_eq!(app.board_column, 0);
            app.handle_key(KeyCode::Char('h')); // clamped at first
            assert_eq!(app.board_column, 0);
            // Priority mode has exactly 5 columns (P0-P4).
            app.handle_key(KeyCode::Char('s'));
            app.handle_key(KeyCode::Char('5'));
            assert_eq!(app.board_column, 4);
            app.handle_key(KeyCode::Char('6')); // no-op, clamped
            assert_eq!(app.board_column, 4);
            app.handle_key(KeyCode::Char('3'));
            assert_eq!(app.board_column, 2);
            assert_eq!(
                app.cursor, cursor_before,
                "board nav must not move the list cursor"
            );
        }

        #[test]
        fn board_s_does_not_sort() {
            let mut app = make_app(3);
            app.handle_key(KeyCode::Char('b'));
            let start = app.sort_mode;
            app.handle_key(KeyCode::Char('s'));
            assert_eq!(app.sort_mode, start, "s in Board cycles grouping, not sort");
        }

        #[test]
        fn actionable_lazy_loads_and_jk_routes_to_items() {
            let mut app = make_app(6);
            assert!(app.actionable.is_none());
            app.handle_key(KeyCode::Char('F'));
            assert_eq!(app.current_view, ViewMode::Actionable);
            assert!(
                app.actionable.is_some(),
                "F must lazy-build actionable state"
            );
            let cursor_before = app.cursor;
            app.handle_key(KeyCode::Char('j'));
            app.handle_key(KeyCode::Char('k'));
            assert_eq!(
                app.cursor, cursor_before,
                "actionable j/k must not move the list cursor"
            );
        }

        #[test]
        fn tutorial_jk_turns_pages_and_backtick_resets() {
            let mut app = make_app(3);
            // Single tap arms the 300ms timer; force-expire it (no real
            // waiting in tests) by backdating, then resolve on next key.
            app.handle_key(KeyCode::Char('`'));
            assert!(
                app.tutorial_tap_pending.is_some(),
                "first tap must arm the double-tap window, not open Tutorial yet"
            );
            app.tutorial_tap_pending =
                Some(std::time::Instant::now() - std::time::Duration::from_millis(500));
            // Resolve via a neutral key: `q` would quit, `j` would turn a
            // page once Tutorial opens, `l` opens the label picker. Any
            // non-backtick key resolves the pending tap first (top of
            // `handle_key`), then executes normally — use `;` (inert,
            // unbound in List).
            app.handle_key(KeyCode::Char(';')); // resolves pending → Tutorial
            assert_eq!(app.current_view, ViewMode::Tutorial);
            let cursor_before = app.cursor;
            app.handle_key(KeyCode::Char('j'));
            assert_eq!(app.tutorial.as_ref().unwrap().current_page, 1);
            assert_eq!(app.cursor, cursor_before);
            app.handle_key(KeyCode::Char('k'));
            assert_eq!(app.tutorial.as_ref().unwrap().current_page, 0);
            // Backtick while IN Tutorial still toggles straight back to List
            // (no tap dance when already there).
            app.handle_key(KeyCode::Char('`'));
            assert_eq!(app.current_view, ViewMode::List);
        }

        #[test]
        fn double_tap_backtick_opens_context_help_not_tutorial() {
            // Go `CapsLockTracker`: two taps within 300ms → context help
            // for the current view (`TriggerContextHelp`).
            let mut app = make_app(3);
            app.handle_key(KeyCode::Char('`'));
            app.handle_key(KeyCode::Char('`')); // immediate second tap
            assert!(app.show_context_help, "double tap must open context help");
            assert_ne!(
                app.current_view,
                ViewMode::Tutorial,
                "double tap must NOT open the full Tutorial"
            );
            // Any key dismisses the overlay.
            app.handle_key(KeyCode::Char('x'));
            assert!(!app.show_context_help);
        }

        #[test]
        fn tilde_opens_context_help_directly() {
            // Go `IsContextHelpTrigger` (`~`): direct context help, and it
            // cancels any pending single-tap timer.
            let mut app = make_app(3);
            app.handle_key(KeyCode::Char('`')); // arm the tap timer
            assert!(app.tutorial_tap_pending.is_some());
            app.handle_key(KeyCode::Char('~'));
            assert!(app.show_context_help);
            assert!(app.tutorial_tap_pending.is_none());
        }

        #[test]
        fn sprint_lazy_loads_jk_and_velocity_toggle() {
            let mut app = make_app(3);
            assert!(app.sprint.is_none());
            app.handle_key(KeyCode::Char('P'));
            assert_eq!(app.current_view, ViewMode::Sprint);
            assert!(app.sprint.is_some(), "P must lazy-load sprints");
            let cursor_before = app.cursor;
            app.handle_key(KeyCode::Char('j'));
            app.handle_key(KeyCode::Char('k'));
            assert_eq!(
                app.cursor, cursor_before,
                "sprint j/k must not move the list cursor"
            );
            app.handle_key(KeyCode::Char('v'));
            assert!(app.show_velocity);
            app.handle_key(KeyCode::Esc);
            assert!(!app.show_velocity, "esc closes the overlay first");
            assert_eq!(app.current_view, ViewMode::Sprint);
            app.handle_key(KeyCode::Esc);
            assert_eq!(app.current_view, ViewMode::List);
            assert!(!app.quit_requested, "esc in a view must not quit");
        }

        /// The velocity overlay is Go's per-LABEL table, not the per-sprint
        /// one it replaced. `v` must therefore yield rows keyed by label, with
        /// the four weekly columns and the trend word Go's
        /// `GetVelocityTrend` returns.
        #[test]
        fn velocity_overlay_renders_per_label_rows() {
            // A minimal closed issue per (label, weeks-ago) pair. Built by
            // hand because `make_app` produces unlabeled issues and this test
            // needs the labels the view groups by.
            let mut issues: Vec<bv_core::model::Issue> = Vec::new();
            // Two labels so the sort is observable, and closed_at values so
            // the weekly buckets are not all empty.
            let base = jiff::Timestamp::now();
            for (i, (label, days_ago)) in [("alpha", 1i64), ("alpha", 8), ("beta", 3), ("beta", 20)]
                .iter()
                .enumerate()
            {
                let mut iss = make_app(1).issue_map.remove("T-0").unwrap();
                iss.id = format!("V-{i}");
                iss.labels = vec![label.to_string()];
                iss.status = Status::Closed;
                // Hours, not days: jiff will not subtract calendar units from
                // a Timestamp, and a week is 168h either way.
                iss.closed_at = Some((base - jiff::Span::new().hours(days_ago * 24)).to_string());
                issues.push(iss);
            }

            let mut app = App::new(issues);
            app.handle_key(KeyCode::Char('P'));
            app.handle_key(KeyCode::Char('v'));
            assert!(app.show_velocity);

            let model = app.velocity_comparison();
            assert!(
                model.data_count() >= 2,
                "expected one row per label, got {}",
                model.data_count()
            );

            let text: String = model.view().iter().map(|l| l.to_string()).collect();
            for header in ["Label", "W-4", "W-3", "W-2", "W-1", "Avg", "Trend", "Spark"] {
                assert!(text.contains(header), "missing column {header}");
            }
            assert!(
                text.contains("alpha") && text.contains("beta"),
                "rows must be per label:\n{text}"
            );
            // The sprint table's columns must be gone.
            assert!(
                !text.contains("Planned"),
                "sprint view still present:\n{text}"
            );
        }

        #[test]
        fn agent_prompt_modal_navigate_select_close() {
            use crate::agent_prompt_modal::AgentPromptModal;
            let mut app = make_app(3);
            // Auto-show happens in `tui_event_loop`, not `App::new`.
            assert!(!app.show_agent_prompt);
            app.agent_prompt_modal = Some(AgentPromptModal {
                file_path: "AGENTS.md".into(),
                file_type: "AGENTS.md".into(),
                needs_upgrade: false,
                selected: 0,
                project_dir: String::new(),
            });
            app.show_agent_prompt = true;
            app.handle_key(KeyCode::Right);
            assert_eq!(app.agent_prompt_modal.as_ref().unwrap().selected, 1);
            app.handle_key(KeyCode::Left);
            assert_eq!(app.agent_prompt_modal.as_ref().unwrap().selected, 0);
            app.handle_key(KeyCode::Char('d'));
            assert!(!app.show_agent_prompt);
            assert!(app.agent_prompt_modal.is_none());
        }

        #[test]
        fn agent_prompt_modal_accept_appends_blurb() {
            use crate::agent_prompt_modal::AgentPromptModal;
            let dir = std::env::temp_dir().join("bvr_tui_agent_accept");
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            // Isolate prefs + cwd-dependent detection from the real user config.
            let cfg = std::env::temp_dir().join("bvr_tui_agent_accept_cfg");
            let _ = std::fs::remove_dir_all(&cfg);
            std::env::set_var("XDG_CONFIG_HOME", &cfg);
            let file = dir.join("AGENTS.md");
            std::fs::write(&file, "# Project\n\n").unwrap();
            let mut app = make_app(3);
            app.agent_prompt_modal = Some(AgentPromptModal {
                file_path: file.to_string_lossy().to_string(),
                file_type: "AGENTS.md".into(),
                needs_upgrade: false,
                selected: 0,
                project_dir: dir.to_string_lossy().to_string(),
            });
            app.show_agent_prompt = true;
            app.handle_key(KeyCode::Char('y'));
            assert!(!app.show_agent_prompt);
            let content = std::fs::read_to_string(&file).unwrap();
            assert!(bv_core::agents::verify_blurb_present(&content));
            let _ = std::fs::remove_dir_all(&cfg);
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn esc_returns_toggle_views_to_list() {
            // NOTE: backtick is excluded — since TUI-3 it arms a 300ms
            // double-tap timer (Go `CapsLockTracker`) instead of opening
            // Tutorial synchronously; the view change now resolves on the
            // next keypress after the window expires (covered by
            // `tutorial_jk_turns_pages_and_backtick_resets`).
            for key in ['b', 'E', 'i', 'F', 'P', '!', 'f', 'A'] {
                let mut app = make_app(3);
                app.show_agent_prompt = false;
                app.handle_key(KeyCode::Char(key));
                assert_ne!(
                    app.current_view,
                    ViewMode::List,
                    "key {key} should leave List"
                );
                app.handle_key(KeyCode::Esc);
                assert_eq!(
                    app.current_view,
                    ViewMode::List,
                    "esc should close the {key} view"
                );
                assert!(!app.quit_requested, "esc in a view must not quit");
            }
        }
    }

    // ── --debug-render (Go pkg/ui/model.go:10570-10586) ──────────────

    #[test]
    fn debug_render_rejects_an_unknown_view_with_go_wording() {
        let mut app = make_app(3);
        assert_eq!(
            render_debug_view(&mut app, "triage", 40, 10),
            "Unknown view: triage"
        );
        assert_eq!(render_debug_view(&mut app, "", 40, 10), "Unknown view: ");
    }

    #[test]
    fn debug_render_accepts_the_three_go_view_names() {
        let mut app = make_app(3);
        // "history" is missing from the --debug-render help text but present
        // in Go's switch (pkg/ui/model.go:10582).
        for view in ["insights", "board", "history"] {
            let out = render_debug_view(&mut app, view, 60, 12);
            assert!(
                !out.starts_with("Unknown view:"),
                "{view} should be a known view"
            );
            assert!(!out.is_empty(), "{view} rendered nothing");
        }
    }

    #[test]
    fn debug_render_is_case_sensitive_like_go() {
        let mut app = make_app(1);
        assert_eq!(
            render_debug_view(&mut app, "Insights", 40, 10),
            "Unknown view: Insights"
        );
    }

    #[test]
    fn debug_render_reserves_the_last_row_for_the_status_bar() {
        let mut app = make_app(3);
        let out = render_debug_view(&mut app, "insights", 50, 11);
        // Go renders every view at height-1, so the dump is (height-1) rows
        // regardless of the requested height.
        assert_eq!(out.lines().count(), 10, "{out:?}");
    }

    #[test]
    fn debug_render_rows_are_exactly_the_requested_width() {
        let mut app = make_app(3);
        for width in [1u16, 17, 80] {
            let out = render_debug_view(&mut app, "board", width, 6);
            for line in out.lines() {
                assert_eq!(line.chars().count(), width as usize, "width {width}");
            }
        }
    }

    #[test]
    fn debug_render_records_the_requested_size_on_the_app() {
        let mut app = make_app(1);
        render_debug_view(&mut app, "insights", 123, 45);
        assert_eq!(app.width, 123);
        assert_eq!(app.height, 45);
    }

    #[test]
    fn debug_render_survives_a_degenerate_height() {
        let mut app = make_app(1);
        // Go hands height-1 straight to the panel; a u16 subtraction would
        // wrap, so height<=1 must not panic.
        for height in [0u16, 1] {
            render_debug_view(&mut app, "insights", 20, height);
        }
    }

    #[test]
    fn debug_render_survives_a_zero_width() {
        let mut app = make_app(1);
        assert_eq!(render_debug_view(&mut app, "insights", 0, 10), "");
    }

    #[test]
    fn debug_render_renders_real_insight_content() {
        let mut app = make_app(6);
        app.graph_metrics = Some(GraphMetrics {
            pagerank: [("T-1".to_string(), 0.9)].into_iter().collect(),
            betweenness: [("T-2".to_string(), 0.8)].into_iter().collect(),
            eigenvector: BTreeMap::new(),
            hubs: BTreeMap::new(),
            authorities: BTreeMap::new(),
        });
        let out = render_debug_view(&mut app, "insights", 100, 20);
        assert!(out.contains("Bottlenecks"), "{out}");
        assert!(out.contains("T-2"), "{out}");
    }

    #[test]
    fn debug_render_history_without_data_says_so() {
        let mut app = make_app(1);
        app.history = None;
        let out = render_debug_view(&mut app, "history", 60, 12);
        assert!(out.contains("No history data available"), "{out}");
    }

    #[test]
    fn debug_render_does_not_emit_the_status_bar() {
        // Go renders only the panel for --debug-render and never calls
        // renderStatusBar, so the footer row must be absent.
        let mut app = make_app(5);
        app.current_view = ViewMode::Insights;
        app.filter_mode = FilterMode::All; // leading status-bar badge: " 📋 ALL "
        let out = render_debug_view(&mut app, "insights", 100, 20);
        assert!(!out.contains("\u{1f4cb}"), "status bar leaked:\n{out}");

        // Positive control: the same view through the normal `render` path
        // paints that badge, and paints it in the very last row — the row
        // Go's `height-1` reserves. Without this the assertion above could
        // pass merely because the marker never appears at all.
        let backend = ratatui::backend::TestBackend::new(100, 20);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, &mut app)).unwrap();
        let full = buffer_to_lines(terminal.backend().buffer());
        let last = full.lines().next_back().unwrap();
        assert!(last.contains("\u{1f4cb}"), "control last row:\n{last}");
        // ...and the panel is one row taller without the reservation.
        assert_eq!(full.lines().count(), out.lines().count() + 1);
    }

    #[test]
    fn buffer_to_lines_reflows_a_hand_built_buffer() {
        let area = ratatui::layout::Rect::new(0, 0, 3, 2);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        buf[(0, 0)].set_symbol("a");
        buf[(1, 0)].set_symbol("b");
        buf[(2, 0)].set_symbol("c");
        buf[(0, 1)].set_symbol("d");
        // Untouched cells render as a single space (Cell::symbol).
        assert_eq!(buffer_to_lines(&buf), "abc\nd  ");
    }

    #[test]
    fn buffer_to_lines_prints_a_wide_grapheme_once() {
        // Each CJK glyph is two columns wide, so set_stringn stores it in one
        // cell and resets the cell it covers
        // (ratatui-core-0.1.2/src/buffer/buffer.rs:363-367). Concatenating
        // every cell therefore yields one char per column — which a terminal
        // re-flows to "日本" — instead of printing the glyph twice.
        let area = ratatui::layout::Rect::new(0, 0, 4, 1);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        buf.set_stringn(
            0,
            0,
            "\u{65e5}\u{672c}",
            4,
            ratatui::style::Style::default(),
        );
        assert_eq!(buffer_to_lines(&buf), "\u{65e5} \u{672c} ");
        assert_eq!(buffer_to_lines(&buf).chars().count(), 4);
    }

    // ── --theme (Go cmd/bv/main.go:1766, 4639-4704) ─────────────────

    #[test]
    fn apply_theme_preference_installs_the_light_palette() {
        let mut app = make_app(1);
        app.apply_theme_preference(Some(crate::theme::ThemePref::Light));
        assert_eq!(app.theme.bg, crate::theme::Theme::light().bg);
    }

    #[test]
    fn apply_theme_preference_defaults_to_the_dark_palette() {
        let mut app = make_app(1);
        app.apply_theme_preference(None);
        assert_eq!(app.theme.bg, crate::theme::Theme::default().bg);
    }
}

pub mod actionable;
pub mod agent_prompt_modal;
pub mod cass;

pub mod chrome;
pub mod context;
pub mod context_help;
pub mod detail;
pub mod helpers;
pub mod keybindings;
pub mod markdown;
pub mod theme;
pub mod tutorial;
pub mod update_modal;
pub mod views;
pub mod visuals;
pub mod worker;

#[cfg(test)]
mod footer_state_tests {
    use super::*;

    #[test]
    fn dataset_warning_thresholds_match_go() {
        assert_eq!(dataset_warning_for(999), None);
        assert_eq!(dataset_warning_for(4_999), None); // medium tier: no warning
        assert!(dataset_warning_for(5_000).unwrap().contains("large"));
        assert!(dataset_warning_for(20_000).unwrap().contains("huge"));
        assert_eq!(
            dataset_warning_for(24_000).unwrap(),
            "\u{26a0} huge 24k issues"
        );
    }

    #[test]
    fn instance_lock_takeover_on_stale() {
        let dir = std::env::temp_dir().join(format!("bvr_lock_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lock = dir.join(".bv.lock");
        // Stale holder: PID that (almost certainly) doesn't exist
        std::fs::write(&lock, "{\"pid\":999999999}").unwrap();
        let holder = acquire_instance_lock(&dir);
        assert_eq!(holder, None, "stale lock should be taken over");
        // Now we hold it — second acquirer sees our PID
        let second = acquire_instance_lock(&dir);
        assert_eq!(second, Some(std::process::id()));
        release_instance_lock(&dir);
        assert!(!lock.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Go `gitRemoteToWebURL` (model.go:5794-5817) + `normalizeGitRemoteWebURL`
    /// (:5819-5843), which the History view's `o` binding depends on to build
    /// `<web>/commit/<sha>`. A wrong rewrite sends the user to a 404.
    #[test]
    fn git_remote_to_web_url_matches_go() {
        // SSH scp-style: git@host:owner/repo.git
        assert_eq!(
            git_remote_to_web_url("git@github.com:Dicklesworthstone/beads_viewer.git"),
            Some("https://github.com/Dicklesworthstone/beads_viewer".to_string())
        );
        // HTTPS: keep the host and path, drop `.git`.
        assert_eq!(
            git_remote_to_web_url("https://github.com/owner/repo.git"),
            Some("https://github.com/owner/repo".to_string())
        );
        // ssh:// URL: scheme becomes https and the userinfo is dropped.
        assert_eq!(
            git_remote_to_web_url("ssh://git@github.com/owner/repo.git"),
            Some("https://github.com/owner/repo".to_string())
        );
        // Trailing slash, query and fragment are all stripped.
        assert_eq!(
            git_remote_to_web_url("https://github.com/owner/repo/?tab=readme#top"),
            Some("https://github.com/owner/repo".to_string())
        );
        // A remote with no repository path is not browsable.
        assert_eq!(git_remote_to_web_url("https://github.com"), None);
        // Unrecognised forms, and empty input, are rejected.
        assert_eq!(git_remote_to_web_url("file:///tmp/repo"), None);
        assert_eq!(git_remote_to_web_url("   "), None);
    }
}
