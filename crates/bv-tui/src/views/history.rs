//! History view — bead↔commit correlations. Port of Go `pkg/ui/history.go`.
//!
//! Reached with lowercase `h` (Go `model.go:9224` sets `m.isHistoryView`).
//! Go's file is 3,671 lines; both halves of it are reproduced here — the model
//! (enums, filtering, file tree, search) and the render half — against the
//! *same* data model Go renders against:
//! `bv_correlation::history::HistoryReport`, which already carries `Events`,
//! `Milestones`, `CycleTime`, `Stats`, `CommitIndex` and per-commit `Method` /
//! `AuthorEmail` / `FileChange`.
//!
//! The report is built by `App::load_history_if_needed` through
//! `history::build_history_report` with `HistoryOptions { limit: 500 }`,
//! matching Go's `LoadHistoryCmd` (`model.go:659-661`, "Reasonable limit for TUI
//! performance"). The narrower `correlator::CorrelatedCommit` path is
//! deliberately NOT used: its `files: Vec<String>` has no action or line
//! counts and it has no `method`, so it cannot back `render_commit_detail`.
//!
//! Structure mirrors Go 1:1 — the four-line header, the 2/3/4-pane responsive
//! layout with breakpoints at 100 and 150 columns, the four-target focus model,
//! the timeline pane, the file tree, the six search modes and both view modes.
//! Where a Go construct has no ratatui equivalent the *content* is preserved
//! and only the mechanism differs: a `bubbles/textinput` becomes a plain
//! `String` plus a block cursor, `lipgloss.JoinHorizontal` becomes a `Layout`,
//! and `truncateRunesHelper`'s `runewidth.StringWidth` becomes
//! [`str_width`], which ratatui computes with the same unicode-width tables.

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::theme::Theme;

// The report types are the contract. Re-exported so the rest of the crate
// names them the way the Go file names `correlation.BeadHistory` & co.
// `BeadEvent` and `EventType` live in bv-correlation's `extractor` (Go puts
// both in `correlation/types.go`) and are re-exported through `history` only
// privately, so they are named from their defining module here.
pub use bv_correlation::extractor::{BeadEvent, EventType};
pub use bv_correlation::history::{
    BeadHistory, BeadMilestones, CycleTime, FileChange, HistoryCommit, HistoryOptions,
    HistoryReport, HistoryStats,
};

/// Go `layoutBreakpointStandard` (history.go:38) — width at which the view
/// stops being two-pane.
pub const LAYOUT_BREAKPOINT_STANDARD: u16 = 100;
/// Go `layoutBreakpointWide` (history.go:39) — width at which the timeline
/// pane is on by default.
pub const LAYOUT_BREAKPOINT_WIDE: u16 = 150;

/// Go `historyLayout` (history.go:28-34).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryLayout {
    /// `< 100` columns: two panes.
    Narrow,
    /// `100..149`: three panes.
    Standard,
    /// `>= 150`: three panes with the timeline unless `t` pinned it off.
    Wide,
}

/// Go `historyFocus` (history.go:18-25).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryFocus {
    /// Left pane (beads, or commits in git mode).
    List,
    /// Timeline pane, four-pane layout only.
    Timeline,
    /// Middle pane, three- and four-pane layouts.
    Middle,
    /// Right pane.
    Detail,
}

/// Go `historyViewMode` (history.go:43-48).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryViewMode {
    /// Beads left, commits for the selected bead right.
    Bead,
    /// Commits left, related beads for the selected commit right.
    Git,
}

/// Go `historySearchMode` (history.go:64-71). All six exist so the matching
/// functions are translatable; only `All` is reachable from the shipped TUI
/// (Go's `StartSearchWithMode` is never called from a non-test file either).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistorySearchMode {
    Off,
    All,
    Commit,
    Sha,
    Bead,
    Author,
}

/// Go `timelineEntryType` (history.go:76-80).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TimelineEntryType {
    Event,
    Commit,
    Session,
}

/// Go `CommitListEntry` (history.go:51-59).
#[derive(Debug, Clone)]
pub struct CommitListEntry {
    pub sha: String,
    pub short_sha: String,
    pub message: String,
    pub author: String,
    /// Go formats `commit.Timestamp` with `"2006-01-02 15:04"` and then sorts
    /// the *string*, so the formatted form is the sort key.
    pub timestamp: String,
    pub file_count: usize,
    pub bead_ids: Vec<String>,
}

/// Go `TimelineEntry` (history.go:83-96). `Session*` fields are carried for
/// shape parity; Go's session cache (`cass.ScoredResult`) has no Rust source
/// in this crate, so no session entries are ever constructed.
#[derive(Debug, Clone)]
pub struct TimelineEntry {
    pub timestamp: String,
    pub entry_type: TimelineEntryType,
    pub label: String,
    pub detail: String,
    pub confidence: f64,
    pub event_type: String,
    pub session_agent: String,
    pub session_message_count: i64,
    pub session_path: String,
    pub session_score: f64,
}

/// Go `FileTreeNode` (history.go:99-107), held in an arena so children are
/// indices rather than Go's `[]*FileTreeNode`.
#[derive(Debug, Clone, Default)]
pub struct FileTreeNode {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    /// Arena indices, sorted directories-first then by name (Go's
    /// `sortTreeNode`, history.go:630-643).
    pub children: Vec<usize>,
    pub change_count: usize,
    pub expanded: bool,
    pub level: usize,
}

/// Go `conventionalCommit` (history.go:2924-2931).
#[derive(Debug, Clone, Default)]
pub struct ConventionalCommit {
    pub commit_type: String,
    pub scope: String,
    pub breaking: bool,
    pub subject: String,
    pub body: String,
    pub is_conventional: bool,
}

/// The History view's state. Field-for-field the Go `HistoryModel`
/// (history.go:110-173) minus the cass session cache.
pub struct HistoryState {
    // Data.
    report: Option<HistoryReport>,
    /// Filtered + sorted bead list (Go `histories`).
    histories: Vec<BeadHistory>,
    /// `bead_ids[i]` is the ID of `histories[i]`, kept parallel as in Go.
    bead_ids: Vec<String>,

    // Navigation state.
    selected_bead: usize,
    selected_commit: usize,
    scroll_offset: usize,
    focused: HistoryFocus,

    // Git-centric mode.
    view_mode: HistoryViewMode,
    commit_list: Vec<CommitListEntry>,
    selected_git_commit: usize,
    selected_related_bead: usize,
    git_scroll_offset: usize,

    // Middle-pane scroll (three- and four-pane layouts).
    middle_scroll_offset: usize,
    // Timeline-pane scroll.
    timeline_scroll_offset: usize,

    // Filters.
    author_filter: String,
    min_confidence: f64,

    // Search.
    search_query: String,
    search_mode: HistorySearchMode,
    search_active: bool,
    last_search_query: String,
    /// `None` is Go's "no active query" sentinel; `Some(vec![])` is an active
    /// query that matched nothing (history.go:1025-1031).
    filtered_commits: Option<Vec<CommitListEntry>>,

    // Display state.
    width: u16,
    height: u16,

    // File tree.
    show_file_tree: bool,
    file_tree: Vec<FileTreeNode>,
    file_tree_roots: Vec<usize>,
    flat_file_list: Vec<usize>,
    selected_file_idx: usize,
    file_tree_scroll: usize,
    file_filter: String,
    file_tree_focus: bool,

    // Go `bv-kvlx`: 150ms flash on the mode indicator after a `v` toggle.
    mode_changed_at: Option<Instant>,

    // Go `bv-1x6o`: the pane defaults to on at >= 150 columns; `t` pins it.
    timeline_pinned: bool,
    timeline_visible: bool,
}

impl Default for HistoryState {
    fn default() -> Self {
        Self {
            report: None,
            histories: Vec::new(),
            bead_ids: Vec::new(),
            selected_bead: 0,
            selected_commit: 0,
            scroll_offset: 0,
            focused: HistoryFocus::List,
            view_mode: HistoryViewMode::Bead,
            commit_list: Vec::new(),
            selected_git_commit: 0,
            selected_related_bead: 0,
            git_scroll_offset: 0,
            middle_scroll_offset: 0,
            timeline_scroll_offset: 0,
            author_filter: String::new(),
            min_confidence: 0.0,
            search_query: String::new(),
            search_mode: HistorySearchMode::Off,
            search_active: false,
            last_search_query: String::new(),
            filtered_commits: None,
            width: 0,
            height: 0,
            show_file_tree: false,
            file_tree: Vec::new(),
            file_tree_roots: Vec::new(),
            flat_file_list: Vec::new(),
            selected_file_idx: 0,
            file_tree_scroll: 0,
            file_filter: String::new(),
            file_tree_focus: false,
            mode_changed_at: None,
            timeline_pinned: false,
            timeline_visible: false,
        }
    }
}

impl HistoryState {
    /// Go `NewHistoryModel` (history.go:209-228) with a nil report.
    pub fn new() -> Self {
        Self::default()
    }

    /// Build from a freshly generated report (Go `NewHistoryModel` +
    /// `rebuildFilteredList`).
    pub fn from_report(report: HistoryReport) -> Self {
        let mut h = Self {
            report: Some(report),
            ..Self::default()
        };
        h.rebuild_filtered_list();
        h
    }

    // -----------------------------------------------------------------
    // Data accessors
    // -----------------------------------------------------------------

    pub fn has_report(&self) -> bool {
        self.report.is_some()
    }

    pub fn report(&self) -> Option<&HistoryReport> {
        self.report.as_ref()
    }

    pub fn histories(&self) -> &[BeadHistory] {
        &self.histories
    }

    pub fn mode(&self) -> HistoryViewMode {
        self.view_mode
    }

    pub fn is_git_mode(&self) -> bool {
        self.view_mode == HistoryViewMode::Git
    }

    pub fn focused(&self) -> HistoryFocus {
        self.focused
    }

    pub fn set_focused(&mut self, f: HistoryFocus) {
        self.focused = f;
    }

    pub fn min_confidence(&self) -> f64 {
        self.min_confidence
    }

    pub fn author_filter(&self) -> &str {
        &self.author_filter
    }

    pub fn file_filter(&self) -> &str {
        &self.file_filter
    }

    pub fn is_file_tree_visible(&self) -> bool {
        self.show_file_tree
    }

    pub fn file_tree_has_focus(&self) -> bool {
        self.file_tree_focus
    }

    pub fn set_file_tree_focus(&mut self, focus: bool) {
        self.file_tree_focus = focus;
    }

    pub fn is_detail_focused(&self) -> bool {
        self.focused == HistoryFocus::Detail
    }

    pub fn is_search_active(&self) -> bool {
        self.search_active
    }

    pub fn search_query(&self) -> &str {
        &self.search_query
    }

    /// Go `SelectedBeadID` (history.go:1431-1435).
    pub fn selected_bead_id(&self) -> &str {
        self.bead_ids
            .get(self.selected_bead)
            .map_or("", String::as_str)
    }

    /// Go `SelectedHistory` (history.go:1439-1453).
    pub fn selected_history(&self) -> Option<&BeadHistory> {
        self.histories.get(self.selected_bead)
    }

    /// Go `SelectedCommit` (history.go:1447-1453). `BeadHistory.commits` is
    /// `Option<Vec<_>>` on the Rust side (Go's nil slice), hence the
    /// `unwrap_or_default`.
    pub fn selected_commit(&self) -> Option<&HistoryCommit> {
        self.selected_history()
            .and_then(|h| h.commits.as_deref())
            .and_then(|c| c.get(self.selected_commit))
    }

    /// Go `SelectedGitCommit` (history.go:1355-1361).
    pub fn selected_git_commit(&self) -> Option<&CommitListEntry> {
        self.filtered_commit_list().get(self.selected_git_commit)
    }

    /// Go `SelectedRelatedBeadID` (history.go:1364-1370).
    pub fn selected_related_bead_id(&self) -> &str {
        self.selected_git_commit()
            .and_then(|c| c.bead_ids.get(self.selected_related_bead))
            .map_or("", String::as_str)
    }

    /// Go `GetFilteredCommitList` (history.go:1177-1182).
    pub fn filtered_commit_list(&self) -> &[CommitListEntry] {
        match &self.filtered_commits {
            Some(v) => v,
            None => &self.commit_list,
        }
    }

    /// Length of the *unfiltered* commit list, the denominator in
    /// `renderFilterLine` (history.go:2227).
    pub fn commit_list_len(&self) -> usize {
        self.commit_list.len()
    }

    /// Timeline-pane scroll offset, so the key handler can clamp it.
    pub fn timeline_scroll_offset(&self) -> usize {
        self.timeline_scroll_offset
    }

    /// Go `GetSearchModeName` (history.go:1185-1198).
    pub fn search_mode_name(&self) -> &'static str {
        match self.search_mode {
            HistorySearchMode::Commit => "msg",
            HistorySearchMode::Sha => "sha",
            HistorySearchMode::Bead => "bead",
            HistorySearchMode::Author => "author",
            _ => "all",
        }
    }

    /// Go `GetHistoryForBead` (history.go:1456-1465).
    pub fn history_for_bead(&self, bead_id: &str) -> Option<&BeadHistory> {
        self.report.as_ref().and_then(|r| r.histories.get(bead_id))
    }

    // -----------------------------------------------------------------
    // Layout / sizing (G6, G7)
    // -----------------------------------------------------------------

    /// Go `SetSize` (history.go:489-501) — stores the dimensions and demotes
    /// focus to the list when a resize removed the focused pane.
    pub fn set_size(&mut self, width: u16, height: u16) {
        self.width = width;
        self.height = height;
        let panes = self.pane_count();
        if panes < 4 && self.focused == HistoryFocus::Timeline {
            self.focused = HistoryFocus::List;
        }
        if panes < 3 && self.focused == HistoryFocus::Middle {
            self.focused = HistoryFocus::List;
        }
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn height(&self) -> u16 {
        self.height
    }

    /// Go `determineLayout` (history.go:1473-1480).
    pub fn determine_layout(&self) -> HistoryLayout {
        if self.width < LAYOUT_BREAKPOINT_STANDARD {
            HistoryLayout::Narrow
        } else if self.width < LAYOUT_BREAKPOINT_WIDE {
            HistoryLayout::Standard
        } else {
            HistoryLayout::Wide
        }
    }

    /// Go `paneCount` (history.go:1483-1496).
    pub fn pane_count(&self) -> usize {
        if self.timeline_visible() {
            return 4;
        }
        match self.determine_layout() {
            HistoryLayout::Narrow => 2,
            HistoryLayout::Standard | HistoryLayout::Wide => 3,
        }
    }

    /// Go `listHeight` (history.go:1425-1428) — the scroll bound, which
    /// reserves three rows where the per-renderer `visibleItems` reserves five.
    fn list_height(&self) -> usize {
        self.height.saturating_sub(3).max(1) as usize
    }

    // -----------------------------------------------------------------
    // Timeline pane availability (G13)
    // -----------------------------------------------------------------

    /// Go `TimelineAvailable` (history.go:178-180): bead mode only (git mode
    /// has no per-bead timeline) and wide enough for three or more panes.
    pub fn timeline_available(&self) -> bool {
        self.view_mode != HistoryViewMode::Git && self.determine_layout() != HistoryLayout::Narrow
    }

    /// Go `TimelineVisible` (history.go:184-192).
    pub fn timeline_visible(&self) -> bool {
        if !self.timeline_available() {
            return false;
        }
        if self.timeline_pinned {
            return self.timeline_visible;
        }
        self.determine_layout() == HistoryLayout::Wide
    }

    /// Go `ToggleTimeline` (history.go:196-206) — returns the new visibility.
    pub fn toggle_timeline(&mut self) -> bool {
        if !self.timeline_available() {
            return false;
        }
        self.timeline_visible = !self.timeline_visible();
        self.timeline_pinned = true;
        if !self.timeline_visible && self.focused == HistoryFocus::Timeline {
            self.focused = HistoryFocus::List;
        }
        self.timeline_visible
    }

    // -----------------------------------------------------------------
    // Filtering (G2, G10 file filter)
    // -----------------------------------------------------------------

    /// Go `SetAuthorFilter` (history.go:504-507).
    pub fn set_author_filter(&mut self, author: &str) {
        self.author_filter = author.to_string();
        self.rebuild_filtered_list();
    }

    /// Go `SetMinConfidence` (history.go:510-513).
    pub fn set_min_confidence(&mut self, conf: f64) {
        self.min_confidence = conf;
        self.rebuild_filtered_list();
    }

    /// Go `CycleConfidence` (history.go:893-906) — `{0, 0.5, 0.75, 0.9}` with
    /// the same ±0.01 match and modulo wrap.
    pub fn cycle_confidence(&mut self) -> f64 {
        const THRESHOLDS: [f64; 4] = [0.0, 0.5, 0.75, 0.9];
        let mut current_idx = 0;
        for (i, t) in THRESHOLDS.iter().enumerate() {
            if self.min_confidence >= t - 0.01 && self.min_confidence <= t + 0.01 {
                current_idx = i;
                break;
            }
        }
        let next = THRESHOLDS[(current_idx + 1) % THRESHOLDS.len()];
        self.set_min_confidence(next);
        next
    }

    /// Go `rebuildFilteredList` (history.go:373-486). Author filter, then
    /// confidence, then file filter; a bead whose commit list empties is
    /// dropped entirely. Sorted by commit count descending, tie-broken by bead
    /// ID ascending. The previously selected bead is restored by ID.
    pub fn rebuild_filtered_list(&mut self) {
        let selected_id = self.selected_bead_id().to_string();

        self.histories.clear();
        self.bead_ids.clear();

        let Some(report) = self.report.as_ref() else {
            // Empty list — nothing to scroll to.
            self.scroll_offset = 0;
            return;
        };

        for (bead_id, history) in &report.histories {
            // Skip beads with no commits (history.go:390-392).
            let mut history = history.clone();
            let commits = history.commits.take().unwrap_or_default();
            if commits.is_empty() {
                continue;
            }

            // Author filter (history.go:394-407).
            if !self.author_filter.is_empty() {
                let needle = self.author_filter.to_lowercase();
                let matched = commits.iter().any(|c| {
                    c.author.to_lowercase().contains(&needle)
                        || c.author_email.to_lowercase().contains(&needle)
                });
                if !matched {
                    continue;
                }
            }

            // Confidence filter (history.go:409-421).
            let mut commits = commits;
            if self.min_confidence > 0.0 {
                commits.retain(|c| c.confidence >= self.min_confidence);
                if commits.is_empty() {
                    continue;
                }
            }

            // File filter (history.go:423-439).
            if !self.file_filter.is_empty() {
                let filter = self.file_filter.clone();
                commits.retain(|c| {
                    c.files
                        .iter()
                        .any(|f| f.path == filter || f.path.starts_with(&format!("{filter}/")))
                });
                if commits.is_empty() {
                    continue;
                }
            }

            history.commits = Some(commits);
            self.histories.push(history);
            self.bead_ids.push(bead_id.clone());
        }

        // Sort by most commits first, then bead ID (history.go:446-451).
        let mut order: Vec<usize> = (0..self.histories.len()).collect();
        order.sort_by(|&a, &b| {
            let ca = self.histories[a].commits.as_ref().map_or(0, Vec::len);
            let cb = self.histories[b].commits.as_ref().map_or(0, Vec::len);
            cb.cmp(&ca)
                .then_with(|| self.histories[a].bead_id.cmp(&self.histories[b].bead_id))
        });
        self.histories = order.iter().map(|&i| self.histories[i].clone()).collect();
        self.bead_ids = order.iter().map(|&i| self.bead_ids[i].clone()).collect();

        // The rebuild can shrink or re-sort the list; a stale `scroll_offset`
        // past the end renders a blank pane (`start..end` is empty). Clamp it
        // into the valid window — `filter_bead_list` resets to 0 because a
        // search query is a fresh context, whereas an author/confidence
        // filter should keep the user's place where possible.
        let max_scroll = self.histories.len().saturating_sub(self.list_height());
        self.scroll_offset = self.scroll_offset.min(max_scroll);

        // Restore selection, clamping the commit index (history.go:459-485).
        if !selected_id.is_empty() {
            if let Some(pos) = self.bead_ids.iter().position(|id| *id == selected_id) {
                self.selected_bead = pos;
                let num_commits = self.histories[pos].commits.as_ref().map_or(0, Vec::len);
                self.selected_commit = if self.selected_commit >= num_commits {
                    num_commits.saturating_sub(1)
                } else {
                    self.selected_commit
                };
                self.ensure_bead_visible();
                return;
            }
        }
        self.selected_bead = 0;
        self.selected_commit = 0;
        self.ensure_bead_visible();
    }

    // -----------------------------------------------------------------
    // Search (G15)
    // -----------------------------------------------------------------

    /// Go `StartSearch` (history.go:924-928).
    pub fn start_search(&mut self) {
        self.search_active = true;
        self.search_mode = HistorySearchMode::All;
    }

    /// Go `CancelSearch` (history.go:953-960) — clears the query too.
    pub fn cancel_search(&mut self) {
        self.search_active = false;
        self.search_query.clear();
        self.search_mode = HistorySearchMode::Off;
        self.last_search_query.clear();
        self.apply_search_filter();
    }

    /// Go `FinishSearch` (history.go:963-966) — blurs but keeps the query.
    pub fn finish_search(&mut self) {
        self.search_active = false;
    }

    /// Go `ClearSearch` (history.go:969-973).
    pub fn clear_search(&mut self) {
        self.search_query.clear();
        self.last_search_query.clear();
        self.apply_search_filter();
    }

    /// One keystroke into the search box (Go `UpdateSearchInput`, :987-998).
    pub fn push_search_char(&mut self, c: char) {
        // Go's `textinput.CharLimit` is 100 (history.go:213).
        if self.search_query.chars().count() < 100 {
            self.search_query.push(c);
        }
        self.sync_search();
    }

    pub fn backspace_search(&mut self) {
        self.search_query.pop();
        self.sync_search();
    }

    /// Re-run the filter when the query changed, as `UpdateSearchInput` does.
    fn sync_search(&mut self) {
        if self.search_query != self.last_search_query {
            self.last_search_query = self.search_query.clone();
            self.apply_search_filter();
        }
    }

    /// Go `applySearchFilter` (history.go:1001-1022). Always rebuilds the
    /// unfiltered list first, so backspacing to relax the filter works.
    pub fn apply_search_filter(&mut self) {
        self.rebuild_filtered_list();
        if self.view_mode == HistoryViewMode::Git {
            self.build_commit_list();
        }
        let query = self.search_query.trim().to_string();
        if query.is_empty() {
            self.filtered_commits = None;
            return;
        }
        if self.view_mode == HistoryViewMode::Git {
            self.filter_commit_list(&query);
        } else {
            self.filter_bead_list(&query);
        }
    }

    /// Go `filterCommitList` (history.go:1025-1053).
    fn filter_commit_list(&mut self, query: &str) {
        if self.commit_list.is_empty() {
            // Non-nil-but-empty: an active query that matches nothing must
            // not fall through to the full list.
            self.filtered_commits = Some(Vec::new());
            return;
        }
        let q = query.to_lowercase();
        let matched: Vec<CommitListEntry> = self
            .commit_list
            .iter()
            .filter(|c| self.commit_matches_query(c, &q))
            .cloned()
            .collect();
        self.filtered_commits = Some(matched);
        if self.selected_git_commit >= self.filtered_commits.as_ref().map_or(0, Vec::len) {
            self.selected_git_commit = 0;
            self.selected_related_bead = 0;
        }
        self.git_scroll_offset = 0;
    }

    /// Go `commitMatchesQuery` (history.go:1056-1098).
    fn commit_matches_query(&self, commit: &CommitListEntry, query: &str) -> bool {
        match self.search_mode {
            HistorySearchMode::Sha => {
                commit.sha.to_lowercase().starts_with(query)
                    || commit.short_sha.to_lowercase().starts_with(query)
            }
            HistorySearchMode::Commit => commit.message.to_lowercase().contains(query),
            HistorySearchMode::Author => commit.author.to_lowercase().contains(query),
            HistorySearchMode::Bead => commit.bead_ids.iter().any(|id| {
                id.to_lowercase().contains(query)
                    || self
                        .history_for_bead(id)
                        .is_some_and(|h| h.title.to_lowercase().contains(query))
            }),
            // searchModeAll / Off: OR across every field.
            _ => {
                if commit.sha.to_lowercase().starts_with(query)
                    || commit.short_sha.to_lowercase().starts_with(query)
                {
                    return true;
                }
                if commit.message.to_lowercase().contains(query) {
                    return true;
                }
                if commit.author.to_lowercase().contains(query) {
                    return true;
                }
                commit
                    .bead_ids
                    .iter()
                    .any(|id| id.to_lowercase().contains(query))
            }
        }
    }

    /// Go `filterBeadList` (history.go:1101-1128).
    fn filter_bead_list(&mut self, query: &str) {
        let q = query.to_lowercase();
        let mut kept: Vec<BeadHistory> = Vec::new();
        let mut kept_ids: Vec<String> = Vec::new();
        for (i, hist) in self.histories.iter().enumerate() {
            let bead_id = &self.bead_ids[i];
            if self.bead_matches_query(bead_id, hist, &q) {
                kept.push(hist.clone());
                kept_ids.push(bead_id.clone());
            }
        }
        self.histories = kept;
        self.bead_ids = kept_ids;
        if self.selected_bead >= self.bead_ids.len() {
            self.selected_bead = 0;
            self.selected_commit = 0;
        }
        self.scroll_offset = 0;
    }

    /// Go `beadMatchesQuery` (history.go:1131-1174).
    fn bead_matches_query(&self, bead_id: &str, hist: &BeadHistory, query: &str) -> bool {
        let commits = hist.commits.as_deref().unwrap_or(&[]);
        match self.search_mode {
            HistorySearchMode::Bead => {
                bead_id.to_lowercase().contains(query) || hist.title.to_lowercase().contains(query)
            }
            HistorySearchMode::Commit => commits
                .iter()
                .any(|c| c.message.to_lowercase().contains(query)),
            HistorySearchMode::Sha => commits.iter().any(|c| {
                c.sha.to_lowercase().starts_with(query)
                    || c.short_sha.to_lowercase().starts_with(query)
            }),
            HistorySearchMode::Author => commits
                .iter()
                .any(|c| c.author.to_lowercase().contains(query)),
            _ => {
                if bead_id.to_lowercase().contains(query)
                    || hist.title.to_lowercase().contains(query)
                {
                    return true;
                }
                commits.iter().any(|c| {
                    c.message.to_lowercase().contains(query)
                        || c.author.to_lowercase().contains(query)
                        || c.short_sha.to_lowercase().starts_with(query)
                })
            }
        }
    }

    // -----------------------------------------------------------------
    // Git-centric mode (G14)
    // -----------------------------------------------------------------

    /// Go `ToggleViewMode` (history.go:1203-1224).
    pub fn toggle_mode(&mut self) {
        self.mode_changed_at = Some(Instant::now());
        if self.view_mode == HistoryViewMode::Bead {
            self.view_mode = HistoryViewMode::Git;
            self.build_commit_list();
            self.selected_git_commit = 0;
            self.selected_related_bead = 0;
            self.git_scroll_offset = 0;
        } else {
            self.view_mode = HistoryViewMode::Bead;
            self.selected_bead = 0;
            self.selected_commit = 0;
            self.scroll_offset = 0;
        }
        // Re-apply a preserved query across the mode switch (history.go:1221-1223).
        if !self.search_query.trim().is_empty() {
            self.apply_search_filter();
        }
    }

    /// Go `buildCommitList` (history.go:1232-1284) — de-duplicate by SHA across
    /// every bead, format the timestamp, sort descending with a descending-SHA
    /// tiebreak.
    pub fn build_commit_list(&mut self) {
        self.commit_list.clear();
        let Some(report) = self.report.as_ref() else {
            return;
        };
        let mut index_by_sha: HashMap<String, usize> = HashMap::new();
        for (bead_id, hist) in &report.histories {
            for commit in hist.commits.iter().flatten() {
                if let Some(&i) = index_by_sha.get(&commit.sha) {
                    let entry = &mut self.commit_list[i];
                    if !entry.bead_ids.iter().any(|b| b == bead_id) {
                        entry.bead_ids.push(bead_id.clone());
                    }
                    continue;
                }
                index_by_sha.insert(commit.sha.clone(), self.commit_list.len());
                self.commit_list.push(CommitListEntry {
                    sha: commit.sha.clone(),
                    short_sha: commit.short_sha.clone(),
                    message: commit.message.clone(),
                    author: commit.author.clone(),
                    timestamp: format_absolute_datetime(&commit.timestamp),
                    file_count: commit.files.len(),
                    bead_ids: vec![bead_id.clone()],
                });
            }
        }
        for entry in &mut self.commit_list {
            entry.bead_ids.sort();
        }
        self.commit_list.sort_by(|a, b| {
            b.timestamp
                .cmp(&a.timestamp)
                .then_with(|| b.sha.cmp(&a.sha))
        });
    }

    // -----------------------------------------------------------------
    // Navigation (G7 focus-aware j/k)
    // -----------------------------------------------------------------

    /// Go `MoveUp` (history.go:768-790) / `MoveUpGit` (history.go:1287-1308).
    pub fn move_up(&mut self) {
        if self.view_mode == HistoryViewMode::Git {
            self.move_up_git();
            return;
        }
        match self.focused {
            HistoryFocus::List => {
                if self.selected_bead > 0 {
                    self.selected_bead -= 1;
                    self.selected_commit = 0;
                    self.middle_scroll_offset = 0;
                    self.ensure_bead_visible();
                }
            }
            HistoryFocus::Timeline => {
                self.timeline_scroll_offset = self.timeline_scroll_offset.saturating_sub(1);
            }
            _ => {
                if self.selected_commit > 0 {
                    self.selected_commit -= 1;
                    if self.focused == HistoryFocus::Middle {
                        self.ensure_middle_scroll_visible();
                    }
                }
            }
        }
    }

    /// Go `MoveDown` (history.go:793-832) / `MoveDownGit` (history.go:1311-1333).
    pub fn move_down(&mut self) {
        if self.view_mode == HistoryViewMode::Git {
            self.move_down_git();
            return;
        }
        match self.focused {
            HistoryFocus::List => {
                if self.selected_bead + 1 < self.histories.len() {
                    self.selected_bead += 1;
                    self.selected_commit = 0;
                    self.middle_scroll_offset = 0;
                    self.ensure_bead_visible();
                }
            }
            HistoryFocus::Timeline => {
                // Go clamps against `height - 8` floored to 3 (:806-809).
                let max_visible = self.height.saturating_sub(8).max(3) as usize;
                let total = self.timeline_entries().len();
                let max_scroll = total.saturating_sub(max_visible);
                if self.timeline_scroll_offset < max_scroll {
                    self.timeline_scroll_offset += 1;
                }
            }
            _ => {
                let n = self
                    .selected_history()
                    .and_then(|h| h.commits.as_ref())
                    .map_or(0, Vec::len);
                if self.selected_commit + 1 < n {
                    self.selected_commit += 1;
                    if self.focused == HistoryFocus::Middle {
                        self.ensure_middle_scroll_visible();
                    }
                }
            }
        }
    }

    fn move_up_git(&mut self) {
        if self.focused == HistoryFocus::List {
            if self.selected_git_commit > 0 {
                self.selected_git_commit -= 1;
                self.selected_related_bead = 0;
                self.middle_scroll_offset = 0;
                self.ensure_git_commit_visible();
            }
        } else if self.selected_related_bead > 0 {
            self.selected_related_bead -= 1;
            if self.focused == HistoryFocus::Middle {
                self.ensure_middle_scroll_visible();
            }
        }
    }

    fn move_down_git(&mut self) {
        let n = self.filtered_commit_list().len();
        if self.focused == HistoryFocus::List {
            if self.selected_git_commit + 1 < n {
                self.selected_git_commit += 1;
                self.selected_related_bead = 0;
                self.middle_scroll_offset = 0;
                self.ensure_git_commit_visible();
            }
        } else if self.selected_git_commit < n {
            let bead_count = self.selected_git_commit().map_or(0, |c| c.bead_ids.len());
            if self.selected_related_bead + 1 < bead_count {
                self.selected_related_bead += 1;
                if self.focused == HistoryFocus::Middle {
                    self.ensure_middle_scroll_visible();
                }
            }
        }
    }

    /// Go `ToggleFocus` (history.go:835-867) — the 4/3/2-pane cycles.
    pub fn toggle_focus(&mut self) {
        match self.pane_count() {
            4 => {
                self.focused = match self.focused {
                    HistoryFocus::List => HistoryFocus::Timeline,
                    HistoryFocus::Timeline => HistoryFocus::Middle,
                    HistoryFocus::Middle => HistoryFocus::Detail,
                    HistoryFocus::Detail => HistoryFocus::List,
                };
            }
            3 => {
                self.focused = match self.focused {
                    HistoryFocus::List => HistoryFocus::Middle,
                    HistoryFocus::Middle => HistoryFocus::Detail,
                    _ => HistoryFocus::List,
                };
            }
            _ => {
                self.focused = if self.focused == HistoryFocus::List {
                    HistoryFocus::Detail
                } else {
                    HistoryFocus::List
                };
            }
        }
    }

    /// Go `NextCommit` / `NextRelatedBead` (history.go:875-883, :1336-1345).
    pub fn next_commit(&mut self) {
        if self.view_mode == HistoryViewMode::Git {
            let bead_count = self.selected_git_commit().map_or(0, |c| c.bead_ids.len());
            if self.selected_related_bead + 1 < bead_count {
                self.selected_related_bead += 1;
            }
            return;
        }
        let n = self
            .selected_history()
            .and_then(|h| h.commits.as_ref())
            .map_or(0, Vec::len);
        if self.selected_commit + 1 < n {
            self.selected_commit += 1;
        }
    }

    /// Go `PrevCommit` / `PrevRelatedBead` (history.go:886-890, :1348-1352).
    pub fn prev_commit(&mut self) {
        if self.selected_related_bead > 0 && self.view_mode == HistoryViewMode::Git {
            self.selected_related_bead -= 1;
            return;
        }
        self.selected_commit = self.selected_commit.saturating_sub(1);
    }

    /// Go `ensureBeadVisible` (history.go:1387-1398).
    fn ensure_bead_visible(&mut self) {
        let visible = self.list_height();
        if self.selected_bead < self.scroll_offset {
            self.scroll_offset = self.selected_bead;
        } else if self.selected_bead >= self.scroll_offset + visible {
            self.scroll_offset = self.selected_bead + 1 - visible;
        }
    }

    /// Go `ensureGitCommitVisible` (history.go:1373-1384).
    fn ensure_git_commit_visible(&mut self) {
        let visible = self.list_height();
        if self.selected_git_commit < self.git_scroll_offset {
            self.git_scroll_offset = self.selected_git_commit;
        } else if self.selected_git_commit >= self.git_scroll_offset + visible {
            self.git_scroll_offset = self.selected_git_commit + 1 - visible;
        }
    }

    /// Go `ensureMiddleScrollVisible` (history.go:1401-1422), specialised to
    /// the current selection — the item count is the same value Go passes.
    fn ensure_middle_scroll_visible(&mut self) {
        let visible = self.height.saturating_sub(7).max(1) as usize;
        if self.selected_commit < self.middle_scroll_offset {
            self.middle_scroll_offset = self.selected_commit;
        } else if self.selected_commit >= self.middle_scroll_offset + visible {
            self.middle_scroll_offset = self.selected_commit + 1 - visible;
        }
        let item_count = if self.view_mode == HistoryViewMode::Git {
            self.selected_git_commit().map_or(0, |c| c.bead_ids.len())
        } else {
            self.selected_history()
                .and_then(|h| h.commits.as_ref())
                .map_or(0, Vec::len)
        };
        let max_scroll = item_count.saturating_sub(visible);
        if self.middle_scroll_offset > max_scroll {
            self.middle_scroll_offset = max_scroll;
        }
    }

    // -----------------------------------------------------------------
    // File tree (G10)
    // -----------------------------------------------------------------

    /// Go `ToggleFileTree` (history.go:664-669).
    pub fn toggle_file_tree(&mut self) {
        self.show_file_tree = !self.show_file_tree;
        if self.show_file_tree && self.file_tree.is_empty() {
            self.build_file_tree();
        }
    }

    /// Go `buildFileTree` (history.go:518-603). A commit that touches two
    /// files in the same directory increments that directory once, so the
    /// per-commit prefix set is deduplicated (history.go:530-541).
    pub fn build_file_tree(&mut self) {
        self.file_tree.clear();
        self.file_tree_roots.clear();
        self.flat_file_list.clear();
        let Some(report) = self.report.as_ref() else {
            return;
        };

        let mut changes: BTreeMap<String, usize> = BTreeMap::new();
        for hist in report.histories.values() {
            for commit in hist.commits.iter().flatten() {
                let mut in_commit: HashSet<String> = HashSet::new();
                for file in &commit.files {
                    for prefix in Self::file_tree_path_prefixes_of(&file.path) {
                        in_commit.insert(prefix);
                    }
                }
                for path in in_commit {
                    *changes.entry(path).or_insert(0) += 1;
                }
            }
        }

        // One arena node per distinct path, then link children to parents.
        let mut by_path: HashMap<String, usize> = HashMap::new();
        for (path, count) in &changes {
            let parts: Vec<&str> = path.split('/').collect();
            for (i, part) in parts.iter().enumerate() {
                let is_last = i == parts.len() - 1;
                let full_path = parts[..=i].join("/");
                let idx = *by_path.entry(full_path.clone()).or_insert_with(|| {
                    self.file_tree.push(FileTreeNode {
                        name: (*part).to_string(),
                        path: full_path.clone(),
                        is_dir: !is_last,
                        children: Vec::new(),
                        change_count: 0,
                        expanded: false,
                        level: i,
                    });
                    self.file_tree.len() - 1
                });
                if !is_last {
                    self.file_tree[idx].is_dir = true;
                } else {
                    self.file_tree[idx].change_count = *count;
                }
            }
        }
        let paths: Vec<String> = self.file_tree.iter().map(|n| n.path.clone()).collect();
        for (idx, path) in paths.iter().enumerate() {
            if self.file_tree[idx].level == 0 {
                continue;
            }
            let parent_path = path
                .split('/')
                .take(self.file_tree[idx].level)
                .collect::<Vec<_>>()
                .join("/");
            if let Some(&parent) = by_path.get(&parent_path) {
                self.file_tree[parent].children.push(idx);
            }
        }
        let roots: Vec<usize> = (0..self.file_tree.len())
            .filter(|&i| self.file_tree[i].level == 0)
            .collect();
        for &root in &roots {
            self.sort_tree_node(root);
        }
        self.file_tree_roots = roots;
        self.sort_roots();
        self.rebuild_flat_file_list();
    }

    fn sort_roots(&mut self) {
        self.file_tree_roots.sort_by(|&a, &b| {
            self.file_tree[a]
                .is_dir
                .cmp(&self.file_tree[b].is_dir)
                .then_with(|| self.file_tree[a].name.cmp(&self.file_tree[b].name))
        });
    }

    /// Go `sortTreeNode` (history.go:630-643).
    fn sort_tree_node(&mut self, idx: usize) {
        if self.file_tree[idx].children.is_empty() {
            return;
        }
        let children = std::mem::take(&mut self.file_tree[idx].children);
        for c in &children {
            self.sort_tree_node(*c);
        }
        let mut sorted = children;
        sorted.sort_by(|&a, &b| {
            self.file_tree[a]
                .is_dir
                .cmp(&self.file_tree[b].is_dir)
                .then_with(|| self.file_tree[a].name.cmp(&self.file_tree[b].name))
        });
        self.file_tree[idx].children = sorted;
    }

    /// Go `fileTreePathPrefixes` (history.go:605-627).
    fn file_tree_path_prefixes_of(path: &str) -> Vec<String> {
        let trimmed = path.trim_matches('/');
        if trimmed.is_empty() {
            return Vec::new();
        }
        let parts: Vec<&str> = trimmed.split('/').filter(|p| !p.is_empty()).collect();
        if parts.is_empty() {
            return Vec::new();
        }
        (1..=parts.len()).map(|i| parts[..i].join("/")).collect()
    }

    /// Go `rebuildFlatFileList` + `addToFlatList` (history.go:646-661).
    fn rebuild_flat_file_list(&mut self) {
        self.flat_file_list.clear();
        let roots = self.file_tree_roots.clone();
        for r in roots {
            self.add_to_flat_list(r);
        }
    }

    fn add_to_flat_list(&mut self, idx: usize) {
        self.flat_file_list.push(idx);
        if self.file_tree[idx].is_dir && self.file_tree[idx].expanded {
            let children = self.file_tree[idx].children.clone();
            for c in children {
                self.add_to_flat_list(c);
            }
        }
    }

    /// Go `MoveUpFileTree` / `MoveDownFileTree` (history.go:687-695).
    pub fn move_file_tree_up(&mut self) {
        if self.selected_file_idx > 0 {
            self.selected_file_idx -= 1;
        }
    }

    pub fn move_file_tree_down(&mut self) {
        if self.selected_file_idx + 1 < self.flat_file_list.len() {
            self.selected_file_idx += 1;
        }
    }

    /// Go `ToggleExpandFile` (history.go:701-710).
    pub fn toggle_expand_file(&mut self) {
        let Some(&idx) = self.flat_file_list.get(self.selected_file_idx) else {
            return;
        };
        if self.file_tree[idx].is_dir {
            self.file_tree[idx].expanded = !self.file_tree[idx].expanded;
            self.rebuild_flat_file_list();
        }
    }

    /// Go `CollapseFileNode` (history.go:754-763).
    pub fn collapse_file_node(&mut self) {
        let Some(&idx) = self.flat_file_list.get(self.selected_file_idx) else {
            return;
        };
        if self.file_tree[idx].is_dir && self.file_tree[idx].expanded {
            self.file_tree[idx].expanded = false;
            self.rebuild_flat_file_list();
        }
    }

    /// Go `SelectFile` (history.go:713-724) — toggles the filter.
    pub fn select_file(&mut self) {
        let Some(&idx) = self.flat_file_list.get(self.selected_file_idx) else {
            return;
        };
        let path = self.file_tree[idx].path.clone();
        self.file_filter = if self.file_filter == path {
            String::new()
        } else {
            path
        };
        self.rebuild_filtered_list();
    }

    /// Go `ClearFileFilter` (history.go:727-730).
    pub fn clear_file_filter(&mut self) {
        self.file_filter.clear();
        self.rebuild_filtered_list();
    }

    /// Go `SelectedFileName` (history.go:738-743).
    pub fn selected_file_name(&self) -> &str {
        self.flat_file_list
            .get(self.selected_file_idx)
            .map_or("", |&i| self.file_tree[i].name.as_str())
    }

    /// Whether the file-tree cursor sits on a directory, so the key handler
    /// can pick between expanding it and filtering by it (model.go:5519-5528).
    pub fn selected_file_is_dir(&self) -> bool {
        self.flat_file_list
            .get(self.selected_file_idx)
            .is_some_and(|&i| self.file_tree[i].is_dir)
    }

    /// Node under the file-tree cursor, for the renderers.
    pub fn file_tree_rows(&self) -> Vec<&FileTreeNode> {
        self.flat_file_list
            .iter()
            .map(|&i| &self.file_tree[i])
            .collect()
    }

    // -----------------------------------------------------------------
    // Report refresh (G16)
    // -----------------------------------------------------------------

    /// Go `SetReport` (history.go:231-339) — applies a refreshed report while
    /// keeping the selected bead (by ID), the selected commit (by SHA), the
    /// selected git commit and related bead, every expanded file-tree path and
    /// the file filter. Timeline rows have no stable identity across refreshes
    /// so that offset resets.
    pub fn set_report(&mut self, report: HistoryReport) {
        let selected_bead_id = self.selected_bead_id().to_string();
        let selected_bead_sha = self
            .selected_commit()
            .map(|c| c.sha.clone())
            .unwrap_or_default();
        let (selected_git_sha, selected_related) = match self.selected_git_commit() {
            Some(c) => (c.sha.clone(), self.selected_related_bead_id().to_string()),
            None => (String::new(), String::new()),
        };
        let selected_file_path = self
            .flat_file_list
            .get(self.selected_file_idx)
            .map(|&i| self.file_tree[i].path.clone())
            .unwrap_or_default();
        let mut expanded_paths: HashSet<String> = HashSet::new();
        for n in &self.file_tree {
            if n.expanded {
                expanded_paths.insert(n.path.clone());
            }
        }
        let had_file_tree = !self.file_tree.is_empty();

        self.report = Some(report);
        // Re-apply the query and mode, not just the base list, so an active
        // search survives the refresh (history.go:261-264).
        self.apply_search_filter();

        if self.view_mode == HistoryViewMode::Bead {
            self.selected_bead = 0;
            self.selected_commit = 0;
            if let Some(i) = self.bead_ids.iter().position(|id| *id == selected_bead_id) {
                self.selected_bead = i;
                if let Some(commits) = self.histories[i].commits.as_ref() {
                    if let Some(j) = commits.iter().position(|c| c.sha == selected_bead_sha) {
                        self.selected_commit = j;
                    }
                }
            }
            self.ensure_bead_visible();
            if self.selected_bead < self.histories.len() {
                self.ensure_middle_scroll_visible();
            } else {
                self.middle_scroll_offset = 0;
            }
        } else {
            let commits = self.filtered_commit_list().to_vec();
            self.selected_git_commit = 0;
            self.selected_related_bead = 0;
            if let Some(i) = commits.iter().position(|c| c.sha == selected_git_sha) {
                self.selected_git_commit = i;
                if let Some(j) = commits[i]
                    .bead_ids
                    .iter()
                    .position(|b| *b == selected_related)
                {
                    self.selected_related_bead = j;
                }
            }
            self.ensure_git_commit_visible();
            if self.selected_git_commit < commits.len() {
                self.ensure_middle_scroll_visible();
            } else {
                self.middle_scroll_offset = 0;
            }
        }
        self.timeline_scroll_offset = 0;

        if self.show_file_tree || had_file_tree {
            self.build_file_tree();
            for n in &mut self.file_tree {
                n.expanded = expanded_paths.contains(&n.path);
            }
            self.rebuild_flat_file_list();
            // The cursor is an index into the *flat* list, not the arena.
            self.selected_file_idx = self
                .flat_file_list
                .iter()
                .position(|&i| self.file_tree[i].path == selected_file_path)
                .unwrap_or(0);
            self.file_tree_scroll = if self.flat_file_list.is_empty() {
                0
            } else {
                self.file_tree_scroll.min(self.flat_file_list.len() - 1)
            };
        }
    }
}

// ===========================================================================
// Time helpers
//
// The report carries RFC3339 strings (`%aI`), not instants. Go's `time.Time`
// renders with `t.Format(layout)` in the *commit's own* UTC offset, so every
// wall-clock field is read straight out of the string; the instant is only
// needed for the relative-time arithmetic, which uses `jiff::Timestamp`.
// ===========================================================================

const MONTH_ABBREV: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const DOW_ABBREV: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// The wall-clock fields of an RFC3339 timestamp, in the string's own offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WallClock {
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
}

/// Parse the `YYYY-MM-DDThh:mm` prefix every RFC3339 timestamp starts with.
fn parse_wall_clock(s: &str) -> Option<WallClock> {
    let b = s.as_bytes();
    if b.len() < 16 || b[4] != b'-' || b[7] != b'-' || b[13] != b':' {
        return None;
    }
    if b[10] != b'T' && b[10] != b' ' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> { s.get(r)?.parse::<i64>().ok() };
    Some(WallClock {
        year: num(0..4)?,
        month: num(5..7)? as u32,
        day: num(8..10)? as u32,
        hour: num(11..13)? as u32,
        minute: num(14..16)? as u32,
    })
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`), so the weekday can be derived without a tz database.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = ((m as i64) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// 0 = Sunday, matching Go's `time.Weekday`.
fn weekday(s: &str) -> Option<usize> {
    let w = parse_wall_clock(s)?;
    // 1970-01-01 (day 0) was a Thursday, index 4.
    Some((days_from_civil(w.year, w.month, w.day).rem_euclid(7) as usize + 4) % 7)
}

fn h12(hour: u32) -> u32 {
    match hour % 12 {
        0 => 12,
        h => h,
    }
}

fn meridiem(hour: u32) -> &'static str {
    if hour < 12 {
        "AM"
    } else {
        "PM"
    }
}

/// Go `t.Format("2006-01-02 15:04")` — the absolute date on the author line
/// (history.go:2705) and on `CommitListEntry.Timestamp` (:1261).
pub fn format_absolute_datetime(timestamp: &str) -> String {
    match parse_wall_clock(timestamp) {
        Some(w) => format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            w.year, w.month, w.day, w.hour, w.minute
        ),
        None => timestamp.to_string(),
    }
}

/// Go `formatTimelineTimestamp` (history.go:1704-1718) — `3:04 PM` under 24h,
/// `Mon 3PM` under 7d, `Jan 2` under 365d, else `Jan '06`.
pub fn format_timeline_timestamp(timestamp: &str, now: jiff::Timestamp) -> String {
    let Some(w) = parse_wall_clock(timestamp) else {
        return timestamp.to_string();
    };
    let Some(t) = timestamp.parse::<jiff::Timestamp>().ok() else {
        return timestamp.to_string();
    };
    let secs = now.since(t).map(|s| s.get_seconds()).unwrap_or(0).max(0);
    if secs < 24 * 3600 {
        return format!("{}:{:02} {}", h12(w.hour), w.minute, meridiem(w.hour));
    }
    if secs < 7 * 24 * 3600 {
        return format!(
            "{} {}{}",
            DOW_ABBREV[weekday(timestamp).unwrap_or(0)],
            h12(w.hour),
            meridiem(w.hour)
        );
    }
    if secs < 365 * 24 * 3600 {
        return format!(
            "{} {}",
            MONTH_ABBREV[(w.month as usize).clamp(1, 12) - 1],
            w.day
        );
    }
    format!(
        "{} '{:02}",
        MONTH_ABBREV[(w.month as usize).clamp(1, 12) - 1],
        w.year.rem_euclid(100)
    )
}

/// Go `relativeTime` (history.go:2891-2921) — the ladder
/// `in future` / `just now` / `Nm ago` / `Nh ago` / `Nd ago` / `Nw ago` /
/// `Nmo ago` / `Ny ago`. Unparsable input yields `"unknown"`, matching the
/// `CassSessionModal` convention elsewhere in this crate.
pub fn relative_time(timestamp: &str, now: jiff::Timestamp) -> String {
    let Ok(t) = timestamp.parse::<jiff::Timestamp>() else {
        return "unknown".to_string();
    };
    let Ok(span) = now.since(t) else {
        return "in future".to_string();
    };
    let secs = span.get_seconds();
    if secs < 0 {
        return "in future".to_string();
    }
    let hours = secs / 3600;
    if secs < 60 {
        return "just now".to_string();
    }
    if secs < 3600 {
        return format!("{}m ago", secs / 60);
    }
    if secs < 24 * 3600 {
        return format!("{hours}h ago");
    }
    let days = hours / 24;
    if secs < 7 * 24 * 3600 {
        return format!("{days}d ago");
    }
    if secs < 30 * 24 * 3600 {
        return format!("{}w ago", days / 7);
    }
    if secs < 365 * 24 * 3600 {
        return format!("{}mo ago", days / 30);
    }
    format!("{}y ago", days / 365)
}

/// Go `formatDuration` (history.go:1929-1941) over nanoseconds.
pub fn format_duration(nanos: i64) -> String {
    if nanos < 3600 * 1_000_000_000 {
        return format!("{}m", nanos / 60 / 1_000_000_000);
    }
    if nanos < 24 * 3600 * 1_000_000_000 {
        return format!("{}h", nanos / 3600 / 1_000_000_000);
    }
    let days = nanos / 3600 / 1_000_000_000 / 24;
    if days == 1 {
        return "1d".to_string();
    }
    format!("{days}d")
}

/// Go `formatCycleTime` (history.go:2249-2262) — days to a compact string.
pub fn format_cycle_time(days: f64) -> String {
    if days < 1.0 {
        let hours = days * 24.0;
        if hours < 1.0 {
            return format!("{:.0}m", hours * 60.0);
        }
        return format!("{hours:.1}h");
    }
    if days < 7.0 {
        return format!("{days:.1}d");
    }
    format!("{:.1}w", days / 7.0)
}

// ===========================================================================
// Width + truncation — Go's `truncateRunesHelper` (helpers.go:42-60) and
// `truncate` (:74-76)
// ===========================================================================

/// Terminal cell width of `s`, measured with ratatui's unicode-width tables
/// — the same measurement Go's `runewidth.StringWidth` performs.
pub fn str_width(s: &str) -> usize {
    Line::from(s).width()
}

/// Widest prefix of `s` that fits `max` cells.
fn take_width(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut w = 0usize;
    for c in s.chars() {
        let cw = str_width(c.encode_utf8(&mut [0u8; 4]));
        if w + cw > max {
            break;
        }
        out.push(c);
        w += cw;
    }
    out
}

/// Go `truncateRunesHelper` (helpers.go:42-60). `max_width <= 0` returns the
/// empty string; a suffix wider than the budget is itself truncated.
pub fn truncate_width(s: &str, max_width: i64, suffix: &str) -> String {
    if max_width <= 0 {
        return String::new();
    }
    let max = max_width as usize;
    if str_width(s) <= max {
        return s.to_string();
    }
    let sw = str_width(suffix);
    if sw > max {
        return take_width(suffix, max);
    }
    format!("{}{}", take_width(s, max - sw), suffix)
}

/// Go `truncate` (helpers.go:74-76) — `truncateRunesHelper` with `…`.
pub fn truncate_ellipsis(s: &str, max: i64) -> String {
    truncate_width(s, max, "…")
}

/// `strings.Repeat("─", n)` with a floor of one, as every Go panel does.
fn rule(n: i64) -> String {
    "─".repeat(n.max(1) as usize)
}

// ===========================================================================
// Commit formatting helpers
// ===========================================================================

/// Go `authorInitials` (history.go:2868-2888).
pub fn author_initials(name: &str) -> String {
    if name.is_empty() {
        return "??".to_string();
    }
    let parts: Vec<&str> = name.split_whitespace().collect();
    match parts.len() {
        0 => "??".to_string(),
        1 => {
            let runes: Vec<char> = parts[0].chars().collect();
            let take = runes.len().min(2);
            runes[..take].iter().collect::<String>().to_uppercase()
        }
        _ => {
            let first = parts[0].chars().next().unwrap_or('?');
            let last = parts[parts.len() - 1].chars().next().unwrap_or('?');
            format!("{first}{last}").to_uppercase()
        }
    }
}

/// Go `methodLabel` (history.go:2852-2863).
pub fn method_label(method: &str) -> &'static str {
    match method {
        "co_committed" => "(co-committed)",
        "explicit_id" => "(explicit ID)",
        "temporal_author" => "(temporal)",
        _ => "",
    }
}

/// Go `parseConventionalCommit` (history.go:2934-2982).
pub fn parse_conventional_commit(msg: &str) -> ConventionalCommit {
    let mut result = ConventionalCommit::default();
    let mut parts = msg.splitn(2, '\n');
    let first_line = parts.next().unwrap_or("").trim().to_string();
    result.body = parts.next().unwrap_or("").trim().to_string();

    const PATTERNS: [&str; 12] = [
        "feat", "fix", "docs", "style", "refactor", "perf", "test", "build", "ci", "chore",
        "revert", "wip",
    ];
    let lower = first_line.to_lowercase();
    for prefix in PATTERNS {
        if !lower.starts_with(prefix) {
            continue;
        }
        // Safe: every pattern is ASCII, so the byte index is a char boundary.
        let mut rest = &first_line[prefix.len()..];
        if rest.starts_with('(') {
            if let Some(end) = rest.find(')') {
                if end > 0 {
                    result.scope = rest[1..end].to_string();
                    rest = &rest[end + 1..];
                }
            }
        }
        if rest.starts_with('!') {
            result.breaking = true;
            rest = &rest[1..];
        }
        if let Some(subject) = rest.strip_prefix(':') {
            result.commit_type = prefix.to_string();
            result.subject = subject.trim().to_string();
            result.is_conventional = true;
            return result;
        }
    }
    result.subject = first_line;
    result
}

/// Go `commitTypeIndicator` (history.go:2985-3026).
pub fn commit_type_indicator(msg: &str) -> &'static str {
    let lower = msg.to_lowercase();
    if lower.starts_with("merge ") {
        return "⊕";
    }
    if lower.starts_with("revert ") {
        return "↩";
    }
    let cc = parse_conventional_commit(msg);
    if cc.is_conventional {
        return match cc.commit_type.as_str() {
            "feat" => "✨",
            "fix" => "🐛",
            "docs" => "📝",
            "refactor" => "♻",
            "perf" => "⚡",
            "test" => "🧪",
            "chore" => "🔧",
            "ci" => "🔄",
            "build" => "📦",
            "style" => "💄",
            _ => "",
        };
    }
    ""
}

/// Go `groupFilesByDirectory` (history.go:3034-3059) — first-seen directory
/// order, files in commit order within each directory.
pub fn group_files_by_directory(files: &[FileChange]) -> Vec<(String, Vec<&FileChange>)> {
    let mut groups: Vec<(String, Vec<&FileChange>)> = Vec::new();
    for f in files {
        let dir = match f.path.rfind('/') {
            Some(i) => f.path[..i].to_string(),
            None => ".".to_string(),
        };
        match groups.iter_mut().find(|(d, _)| *d == dir) {
            Some((_, v)) => v.push(f),
            None => groups.push((dir, vec![f])),
        }
    }
    groups
}

/// Go `fileActionIcon` (history.go:3078-3091).
pub fn file_action_icon(action: &str) -> &'static str {
    match action {
        "A" => "+",
        "D" => "-",
        "M" => "~",
        "R" => "→",
        _ => "?",
    }
}

/// Go `fileActionColor` (history.go:3062-3075).
pub fn file_action_color(action: &str, theme: &Theme) -> ratatui::style::Color {
    match action {
        "A" => theme.open,
        "D" => theme.closed,
        "M" => theme.in_progress,
        "R" => theme.secondary,
        _ => theme.muted,
    }
}

/// Go `eventTypeIcon` (history.go:3096-3111).
pub fn event_type_icon(et: &str) -> &'static str {
    match et {
        "created" => "🆕",
        "claimed" => "👤",
        "closed" => "✓",
        "reopened" => "↺",
        "modified" => "✎",
        _ => "•",
    }
}

/// Go `eventTypeColor` (history.go:3114-3129).
pub fn event_type_color(et: &str, theme: &Theme) -> ratatui::style::Color {
    match et {
        "created" => theme.primary,
        "claimed" => theme.in_progress,
        "closed" => theme.open,
        "reopened" => theme.secondary,
        _ => theme.muted,
    }
}

/// Go `eventTypeLabel` (history.go:3132-3147).
pub fn event_type_label(et: &str) -> String {
    match et {
        "created" => "Created".to_string(),
        "claimed" => "Claimed".to_string(),
        "closed" => "Closed".to_string(),
        "reopened" => "Reopened".to_string(),
        "modified" => "Modified".to_string(),
        other => other.to_string(),
    }
}

/// Go's per-status icon: `○` open, `✓` closed, `●` in progress
/// (history.go:2329-2335, :2549-2555, :3393-3404, :3625-3636).
pub fn status_icon(status: &str) -> &'static str {
    match status {
        "closed" => "✓",
        "in_progress" => "●",
        _ => "○",
    }
}

/// Go's selection indicator: `▸ ` selected, `  ` unselected.
fn indicator(selected: bool) -> &'static str {
    if selected {
        "▸ "
    } else {
        "  "
    }
}

/// Go `padRight` (helpers.go:64-70) — pad on the right to a cell width.
pub fn pad_right(s: &str, width: usize) -> String {
    let w = str_width(s);
    if w >= width {
        return s.to_string();
    }
    format!("{}{}", s, " ".repeat(width - w))
}

/// Right-align a string in a fixed cell width, as Go's `Align(Right)` does
/// for the timeline timestamp column and the bead commit count.
fn pad_left(s: &str, width: usize) -> String {
    let w = str_width(s);
    if w >= width {
        return s.to_string();
    }
    format!("{}{}", " ".repeat(width - w), s)
}

/// Go's `len()` is a *byte* count, and several truncation constants subtract
/// glyph byte lengths (`len(statusIcon)` is 3 for `○`). Reproduced verbatim so
/// the arithmetic matches, even though it mixes with the cell-width measure
/// inside `truncate_width`.
fn byte_len(s: &str) -> i64 {
    s.len() as i64
}

// ===========================================================================
// Timeline (G13)
// ===========================================================================

impl HistoryState {
    /// Go `buildTimeline` (history.go:1613-1691) — milestones first (Go
    /// prefers them over `Events` as more reliable), then commits, then any
    /// cached cass sessions. Sorted chronologically with a tie-break on entry
    /// type so events precede commits precede sessions.
    ///
    /// No session entries are produced: Go's `cass.ScoredResult` cache has no
    /// Rust source in this crate, and Go itself only fills it from an
    /// asynchronous loader the History view never starts on its own.
    pub fn timeline_entries(&self) -> Vec<TimelineEntry> {
        let Some(hist) = self.selected_history() else {
            return Vec::new();
        };
        let mut entries: Vec<TimelineEntry> = Vec::new();
        let m = &hist.milestones;
        if let Some(e) = &m.created {
            entries.push(TimelineEntry {
                label: "○ Created".into(),
                detail: hist.title.clone(),
                event_type: "created".into(),
                ..blank_with(&e.timestamp)
            });
        }
        if let Some(e) = &m.claimed {
            entries.push(TimelineEntry {
                label: "● Claimed".into(),
                detail: format!("by {}", e.author),
                event_type: "claimed".into(),
                ..blank_with(&e.timestamp)
            });
        }
        if let Some(e) = &m.reopened {
            entries.push(TimelineEntry {
                label: "↻ Reopened".into(),
                event_type: "reopened".into(),
                ..blank_with(&e.timestamp)
            });
        }
        if let Some(e) = &m.closed {
            entries.push(TimelineEntry {
                label: "✓ Closed".into(),
                event_type: "closed".into(),
                ..blank_with(&e.timestamp)
            });
        }
        for commit in hist.commits.iter().flatten() {
            entries.push(TimelineEntry {
                timestamp: commit.timestamp.clone(),
                entry_type: TimelineEntryType::Commit,
                label: commit.short_sha.clone(),
                detail: commit.message.clone(),
                confidence: commit.confidence,
                event_type: String::new(),
                session_agent: String::new(),
                session_message_count: 0,
                session_path: String::new(),
                session_score: 0.0,
            });
        }
        entries.sort_by(|a, b| {
            let order = match (
                a.timestamp.parse::<jiff::Timestamp>(),
                b.timestamp.parse::<jiff::Timestamp>(),
            ) {
                (Ok(x), Ok(y)) => x.cmp(&y),
                _ => a.timestamp.cmp(&b.timestamp),
            };
            order.then_with(|| a.entry_type.cmp(&b.entry_type))
        });
        entries
    }
}

fn blank_with(timestamp: &str) -> TimelineEntry {
    TimelineEntry {
        timestamp: timestamp.to_string(),
        entry_type: TimelineEntryType::Event,
        label: String::new(),
        detail: String::new(),
        confidence: 0.0,
        event_type: String::new(),
        session_agent: String::new(),
        session_message_count: 0,
        session_path: String::new(),
        session_score: 0.0,
    }
}

// ===========================================================================
// Rendering
// ===========================================================================

/// Go's header is four rendered lines: title, stats, filter, rule
/// (history.go:2137).
const HEADER_HEIGHT: u16 = 4;

/// The rounded border every Go panel uses (`lipgloss.RoundedBorder()`), in
/// `theme.muted` when unfocused and `theme.primary` when focused. The timeline
/// pane's *unfocused* border is `theme.border`, not `theme.muted`
/// (history.go:1726-1729) — hence the separate flag.
fn panel_block(
    theme: &Theme,
    focused: bool,
    title: &str,
    border_color: ratatui::style::Color,
) -> Block<'static> {
    let color = if focused { theme.primary } else { border_color };
    let mut b = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(color));
    if !title.is_empty() {
        b = b.title(Span::styled(
            title.to_string(),
            Style::default()
                .fg(theme.primary)
                .add_modifier(Modifier::BOLD),
        ));
    }
    b
}

/// Go `renderEmpty` (history.go:2028-2037) — the message plus the literal
/// `"Press H to close"` suffix, centered in `theme.secondary`.
///
/// The capital `H` is Go's own text, copied verbatim. Go's actual close
/// handler is lowercase `h`/`esc` (model.go:5756) — uppercase `H` is bound to
/// board and graph scroll — so Go's own footer hint at model.go:7825 is wrong
/// too. Copying the text keeps the strings byte-identical; the key binding
/// here matches Go's handler (`h` and `esc` both close), so the two agree in
/// this port even though they do not in Go.
pub fn render_empty(f: &mut Frame, theme: &Theme, area: Rect, msg: &str) {
    let text = format!("{msg}\n\nPress H to close");
    let lines: Vec<Line> = text.split('\n').map(Line::from).collect();
    let mid_h = lines.len() as u16;
    let rows = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(mid_h),
        Constraint::Fill(1),
    ])
    .split(area);
    let block = if rows.len() == 3 { rows[1] } else { area };
    f.render_widget(
        Paragraph::new(lines)
            .style(Style::default().fg(theme.secondary))
            .alignment(ratatui::layout::Alignment::Center),
        block,
    );
}

/// Go `View` (history.go:1499-1523) plus the layout dispatch
/// (history.go:1526-1610). `state.set_size` is called first so focus demotion
/// and the pane count see this frame's dimensions.
pub fn render_history(f: &mut Frame, state: &mut HistoryState, theme: &Theme, area: Rect) {
    let panel_h = area.height.saturating_sub(HEADER_HEIGHT);
    state.set_size(area.width, panel_h);

    // Go's three empty cases, verbatim.
    if !state.has_report() {
        render_empty(f, theme, area, "No history data loaded");
        return;
    }
    if state.is_git_mode() {
        // Go checks the *unfiltered* `commitList` here (history.go:1506) even
        // though the panel below renders the filtered one; keep the asymmetry.
        if state.commit_list_len() == 0 {
            render_empty(f, theme, area, "No commits with bead correlations found");
            return;
        }
    } else if state.histories().is_empty() {
        render_empty(f, theme, area, "No beads with commit correlations found");
        return;
    }

    let rows =
        Layout::vertical([Constraint::Length(HEADER_HEIGHT), Constraint::Fill(1)]).split(area);
    render_header(f, state, theme, rows[0]);
    render_panes(f, state, theme, rows[1]);
}

/// Go `renderHeader` (history.go:2040-2138) — title + mode indicator on the
/// left, the search box or the close hint on the right, then the stats line,
/// the filter line and a full-width rule.
fn render_header(f: &mut Frame, state: &HistoryState, theme: &Theme, area: Rect) {
    let width = area.width as i64;

    // Title + mode indicator (history.go:2043-2081).
    let (mode_icon, mode_label) = if state.is_git_mode() {
        ("◉", "Git")
    } else {
        ("◈", "Beads")
    };
    // The 150ms transition flash (bv-kvlx, history.go:2064-2077).
    let transitioning = state
        .mode_changed_at
        .is_some_and(|t| t.elapsed() <= Duration::from_millis(150));
    let mode_style = if transitioning {
        Style::default()
            .fg(Color::Rgb(26, 26, 46))
            .bg(theme.primary)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(theme.in_progress)
            .add_modifier(Modifier::BOLD)
    };
    let mut title_spans: Vec<Span> = vec![Span::styled(
        " HISTORY ",
        Style::default()
            .fg(theme.primary)
            .add_modifier(Modifier::BOLD),
    )];
    title_spans.push(Span::styled(
        format!(" {mode_icon} {mode_label} "),
        mode_style,
    ));

    // Right-hand content: the search box, or the close/search hint. Go wraps
    // the box in a `lipgloss.RoundedBorder`; a `textinput` is not a ratatui
    // widget, so the mode label, the value and the `[Esc] cancel` hint are
    // inlined on the title line and the box border is dropped. The mode name
    // and the hint text are Go's exact strings.
    let mut right_spans: Vec<Span> = Vec::new();
    if state.is_search_active() {
        right_spans.push(Span::styled(
            format!("[{}] ", state.search_mode_name()),
            Style::default().fg(theme.secondary),
        ));
        right_spans.push(Span::styled(
            state.search_query().to_string(),
            Style::default().fg(theme.fg),
        ));
        right_spans.push(Span::styled("█", Style::default().fg(theme.primary)));
        right_spans.push(Span::styled(
            " [Esc] cancel ",
            Style::default().fg(theme.muted),
        ));
    } else {
        right_spans.push(Span::styled(
            " [/] search  [H] close ",
            Style::default().fg(theme.muted),
        ));
    }
    let left_w: usize = title_spans.iter().map(|s| s.width()).sum();
    let right_w: usize = right_spans.iter().map(|s| s.width()).sum();
    let spacer = ((width - left_w as i64 - right_w as i64).max(1)) as usize;

    let mut lines: Vec<Line> = Vec::new();
    let mut title_line: Vec<Span> = title_spans;
    title_line.push(Span::raw(" ".repeat(spacer)));
    title_line.extend(right_spans);
    lines.push(Line::from(title_line));
    lines.push(render_stats_line(state, theme));
    lines.push(render_filter_line(state, theme));
    lines.push(Line::from(Span::styled(
        rule(width),
        Style::default().fg(theme.muted),
    )));
    f.render_widget(Paragraph::new(lines), area);
}

/// Go `renderStatsLine` (history.go:2141-2190) — `<n> beads • <n> commits •
/// <n> authors • ⌀ <t> cycle • <n.n> commits/bead`, with the value in
/// `theme.primary` and the label in `theme.secondary`.
fn render_stats_line(state: &HistoryState, theme: &Theme) -> Line<'static> {
    let Some(stats) = state.report().map(|r| &r.stats) else {
        return Line::from("");
    };
    let label = Style::default().fg(theme.secondary);
    let value = Style::default()
        .fg(theme.primary)
        .add_modifier(Modifier::BOLD);
    let mut badges: Vec<Vec<Span>> = Vec::new();

    badges.push(vec![
        Span::styled(stats.beads_with_commits.to_string(), value),
        Span::styled(" beads", label),
    ]);
    badges.push(vec![
        Span::styled(stats.total_commits.to_string(), value),
        Span::styled(" commits", label),
    ]);
    badges.push(vec![
        Span::styled(stats.unique_authors.to_string(), value),
        Span::styled(" authors", label),
    ]);
    if let Some(days) = stats.avg_cycle_time_days {
        badges.push(vec![
            Span::styled("⌀ ", label),
            Span::styled(format_cycle_time(days), value),
            Span::styled(" cycle", label),
        ]);
    }
    if stats.avg_commits_per_bead > 0.0 {
        badges.push(vec![
            Span::styled(format!("{:.1}", stats.avg_commits_per_bead), value),
            Span::styled(" commits/bead", label),
        ]);
    }

    let separator = Style::default().fg(theme.muted);
    let mut spans: Vec<Span> = Vec::new();
    for (i, badge) in badges.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" • ", separator));
        }
        spans.extend(badge);
    }
    Line::from(spans)
}

/// Go `renderFilterLine` (history.go:2193-2246) — the active filters joined
/// with `", "`, then a `  │  ` separator, then the bead/commit counts.
fn render_filter_line(state: &HistoryState, theme: &Theme) -> Line<'static> {
    let mut active: Vec<String> = Vec::new();
    if !state.author_filter().is_empty() {
        active.push(format!("@{}", state.author_filter()));
    }
    if state.min_confidence() > 0.0 {
        active.push(format!("≥{:.0}% conf", state.min_confidence() * 100.0));
    }
    let query = state.search_query().trim();
    if !query.is_empty() {
        active.push(format!("\"{query}\""));
    }

    let mut parts: Vec<Vec<Span>> = Vec::new();
    if !active.is_empty() {
        parts.push(vec![Span::styled(
            format!(" Filter: {} ", active.join(", ")),
            Style::default().fg(theme.secondary),
        )]);
    }
    if state.is_git_mode() {
        let shown = state.filtered_commit_list().len();
        let total = state.commit_list_len();
        let text = if shown != total {
            format!(" Showing {shown}/{total} commits ")
        } else {
            format!(" Showing all {total} commits ")
        };
        parts.push(vec![Span::styled(
            text,
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::ITALIC),
        )]);
    } else {
        let total = state.report().map_or(0, |r| r.histories.len());
        let shown = state.histories().len();
        let text = if shown != total {
            format!(" Showing {shown}/{total} beads ")
        } else {
            format!(" Showing all {shown} beads with commits ")
        };
        parts.push(vec![Span::styled(
            text,
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::ITALIC),
        )]);
    }
    let mut spans: Vec<Span> = Vec::new();
    for (i, part) in parts.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  │  "));
        }
        spans.extend(part);
    }
    Line::from(spans)
}

/// Go `renderTwoPaneView` / `renderThreePaneView` (history.go:1526-1610), with
/// the exact fractions and the detail pane taking the remainder so the total
/// width is exact.
fn render_panes(f: &mut Frame, state: &mut HistoryState, theme: &Theme, area: Rect) {
    let w = area.width;
    let frac = |x: f64| ((w as f64) * x) as u16;

    if state.timeline_visible() {
        // Wide bead mode: 20% beads | 22% timeline | 25% commits | remainder.
        let list_w = frac(0.20);
        let timeline_w = frac(0.22);
        let middle_w = frac(0.25);
        // The detail pane takes the remainder, so the four widths sum to `w`.
        let cols = Layout::horizontal([
            Constraint::Length(list_w),
            Constraint::Length(timeline_w),
            Constraint::Length(middle_w),
            Constraint::Fill(1),
        ])
        .split(area);
        render_list_panel(f, state, theme, cols[0]);
        render_timeline_panel(f, state, theme, cols[1]);
        render_commit_middle_panel(f, state, theme, cols[2]);
        render_detail_panel(f, state, theme, cols[3]);
        return;
    }

    match state.determine_layout() {
        HistoryLayout::Narrow => {
            // 45% list / 55% detail.
            let list_w = frac(0.45);
            let cols =
                Layout::horizontal([Constraint::Length(list_w), Constraint::Fill(1)]).split(area);
            if state.is_git_mode() {
                render_git_commit_list_panel(f, state, theme, cols[0]);
                render_git_detail_panel(f, state, theme, cols[1]);
            } else {
                render_list_panel(f, state, theme, cols[0]);
                render_detail_panel(f, state, theme, cols[1]);
            }
        }
        layout => {
            // Standard 30/35/rest, wide 25/30/rest.
            let (lf, mf) = if layout == HistoryLayout::Wide {
                (0.25, 0.30)
            } else {
                (0.30, 0.35)
            };
            let list_w = frac(lf);
            let middle_w = frac(mf);
            let cols = Layout::horizontal([
                Constraint::Length(list_w),
                Constraint::Length(middle_w),
                Constraint::Fill(1),
            ])
            .split(area);
            if state.is_git_mode() {
                render_git_commit_list_panel(f, state, theme, cols[0]);
                render_git_bead_list_panel(f, state, theme, cols[1]);
                render_git_detail_panel(f, state, theme, cols[2]);
            } else {
                render_list_panel(f, state, theme, cols[0]);
                render_commit_middle_panel(f, state, theme, cols[1]);
                render_detail_panel(f, state, theme, cols[2]);
            }
        }
    }
}

/// Visible rows in every list panel: `height - 5` floored to 1
/// (history.go:2296-2299, :2426-2428, :3283-3285, :3510-3512, :3599-3601).
fn visible_items(height: u16) -> usize {
    height.saturating_sub(5).max(1) as usize
}

// ---------------------------------------------------------------------------
// Bead list panel — Go renderListPanel / renderBeadLine
// (history.go:2265-2314 / :2317-2384)
// ---------------------------------------------------------------------------

fn render_list_panel(f: &mut Frame, state: &HistoryState, theme: &Theme, area: Rect) {
    let w = area.width as i64;
    let h = area.height;
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        "BEADS WITH HISTORY",
        Style::default()
            .fg(theme.primary)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(
        rule(w - 4),
        Style::default().fg(theme.primary),
    )));

    let vis = visible_items(h);
    let start = state.scroll_offset;
    let end = (start + vis).min(state.histories.len());
    for i in start..end {
        lines.push(render_bead_line(state, i, w - 4, theme));
    }
    while lines.len() < h.saturating_sub(2) as usize {
        lines.push(Line::from(""));
    }
    let focused = state.focused == HistoryFocus::List;
    f.render_widget(
        Paragraph::new(lines).block(panel_block(theme, focused, "", theme.muted)),
        area,
    );
}

/// Go `renderBeadLine` (history.go:2317-2384):
/// `<indicator><statusIcon> <id padded to 12> <title> <n> commits ⚡<events>`.
fn render_bead_line<'a>(
    state: &'a HistoryState,
    idx: usize,
    width: i64,
    theme: &Theme,
) -> Line<'a> {
    let Some(hist) = state.histories.get(idx) else {
        return Line::from("");
    };
    let selected = idx == state.selected_bead;
    let ind = indicator(selected);
    let status = status_icon(&hist.status);
    let commit_count = format!("{} commits", hist.commits.as_ref().map_or(0, Vec::len));

    // Go's ⚡<n> badge for lifecycle events (history.go:2341-2350).
    let event_badge = render_compact_event_badge(hist.events.len(), theme);
    let mut event_badge_width = str_width(&event_badge);
    if event_badge_width > 0 {
        event_badge_width += 1; // the space Go inserts before the badge
    }

    // `len(...)` in Go is a byte count, so the glyphs are measured in bytes.
    let max_title = (width
        - byte_len(ind)
        - byte_len(status)
        - byte_len(&commit_count)
        - event_badge_width as i64
        - 6)
    .max(10);
    let title = truncate_ellipsis(&hist.title, max_title);

    let highlighted = selected && state.focused == HistoryFocus::List;
    let id_style = if highlighted {
        Style::default()
            .fg(theme.primary)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.secondary)
    };
    let title_style = if highlighted {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };

    let mut spans = vec![
        Span::raw(ind),
        Span::raw(status.to_string()),
        Span::raw(" "),
        Span::styled(pad_right(&hist.bead_id, 12), id_style),
        Span::raw(" "),
        Span::styled(title, title_style),
        Span::raw(" "),
        Span::styled(commit_count, Style::default().fg(theme.muted)),
    ];
    if !event_badge.is_empty() {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            event_badge,
            Style::default().fg(theme.secondary),
        ));
    }
    Line::from(spans)
}

/// Go `renderCompactEventBadge` (history.go:3238-3247) — `⚡<n>`, empty at 0.
fn render_compact_event_badge(event_count: usize, theme: &Theme) -> String {
    if event_count == 0 {
        return String::new();
    }
    let _ = theme;
    format!("⚡{event_count}")
}

// ---------------------------------------------------------------------------
// Commit middle pane — Go renderCommitMiddlePanel (history.go:3481-3567)
// ---------------------------------------------------------------------------

fn render_commit_middle_panel(f: &mut Frame, state: &HistoryState, theme: &Theme, area: Rect) {
    let w = area.width as i64;
    let h = area.height;
    let focused = state.focused == HistoryFocus::Middle;
    let block = panel_block(theme, focused, "", theme.muted);

    let Some(hist) = state.selected_history() else {
        f.render_widget(
            Paragraph::new("Select a bead to view commits").block(block),
            area,
        );
        return;
    };

    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            "COMMITS",
            Style::default()
                .fg(theme.primary)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            rule(w - 4),
            Style::default().fg(theme.primary),
        )),
    ];
    let commits = hist.commits.as_deref().unwrap_or(&[]);
    let vis = visible_items(h);
    let total = commits.len();
    let start = if state.middle_scroll_offset >= total {
        0
    } else {
        state.middle_scroll_offset
    };
    let end = (start + vis).min(total);
    for (i, commit) in commits.iter().enumerate().take(end).skip(start) {
        let is_selected = i == state.selected_commit && focused;
        let max_msg = (w - byte_len(&commit.short_sha) - 8).max(10);
        let msg = truncate_ellipsis(&commit.message, max_msg);
        let sha_style = if is_selected {
            Style::default()
                .fg(theme.primary)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.primary)
        };
        lines.push(Line::from(vec![
            Span::raw(indicator(is_selected)),
            Span::styled(commit.short_sha.clone(), sha_style),
            Span::raw(" "),
            Span::raw(msg),
        ]));
    }
    if total > vis {
        // Go: pct = offset * 100 / (total - visible); zero when there is
        // nothing to scroll (history.go:3552-3558 and :3656-3662).
        let pct = (state.middle_scroll_offset * 100)
            .checked_div(total - vis)
            .unwrap_or(0);
        lines.push(Line::from(Span::styled(
            format!("↕ {end}/{total} ({pct}%)"),
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::ITALIC),
        )));
    }
    while lines.len() < h.saturating_sub(2) as usize {
        lines.push(Line::from(""));
    }
    f.render_widget(Paragraph::new(lines).block(block), area);
}

// ---------------------------------------------------------------------------
// File tree panel — Go renderFileTreePanel / renderFileTreeLine
// (history.go:2387-2452 / :2455-2519)
// ---------------------------------------------------------------------------

/// Go `renderFileTreePanel` (history.go:2387-2452).
///
/// Go's own layout renderers never call this: `f`/`F` sets `showFileTree`,
/// which only changes the `tab` focus cycle (model.go:5595) — the panel is
/// dead in the Go TUI. Ported here with the same reachability, so no invented
/// layout slot was added.
pub fn render_file_tree_panel(f: &mut Frame, state: &HistoryState, theme: &Theme, area: Rect) {
    let w = area.width as i64;
    let h = area.height;
    // Go's header: `FILES`, or `FILES [<filter truncated to 15>]`
    // (history.go:2402-2406).
    let header_text = if state.file_filter.is_empty() {
        "FILES".to_string()
    } else {
        format!("FILES [{}]", truncate_ellipsis(&state.file_filter, 15))
    };
    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            header_text,
            Style::default()
                .fg(theme.primary)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            rule(w - 4),
            Style::default().fg(theme.primary),
        )),
    ];
    let vis = visible_items(h);
    // Go adjusts the scroll during render (history.go:2432-2437); the state is
    // only reachable mutably from the key handler, so clamp on the way out.
    let mut scroll = state.file_tree_scroll;
    if state.selected_file_idx < scroll {
        scroll = state.selected_file_idx;
    }
    if state.selected_file_idx >= scroll + vis {
        scroll = state.selected_file_idx + 1 - vis;
    }
    let start = scroll;
    let end = (start + vis).min(state.flat_file_list.len());
    for i in start..end {
        lines.push(render_file_tree_line(state, i, w - 4, theme));
    }
    while lines.len() < h.saturating_sub(2) as usize {
        lines.push(Line::from(""));
    }
    f.render_widget(
        Paragraph::new(lines).block(panel_block(theme, state.file_tree_focus, "", theme.muted)),
        area,
    );
}

/// Go `renderFileTreeLine` (history.go:2455-2519):
/// `<indent×2><indicator><▼/▶><name> (<count>)`.
fn render_file_tree_line<'a>(
    state: &'a HistoryState,
    idx: usize,
    width: i64,
    theme: &Theme,
) -> Line<'a> {
    let Some(&node_idx) = state.flat_file_list.get(idx) else {
        return Line::from("");
    };
    let node = &state.file_tree[node_idx];
    let selected = idx == state.selected_file_idx;
    let is_filtered = node.path == state.file_filter;
    let indent = "  ".repeat(node.level);
    let ind = indicator(selected && state.file_tree_focus);
    let icon = if node.is_dir {
        if node.expanded {
            "▼ "
        } else {
            "▶ "
        }
    } else {
        "  "
    };
    let count = format!("({})", node.change_count);
    let max_name =
        (width - byte_len(&indent) - byte_len(ind) - byte_len(icon) - byte_len(&count) - 2).max(5);
    let name = truncate_ellipsis(&node.name, max_name);

    let mut name_style = Style::default();
    if node.is_dir {
        name_style = name_style.fg(theme.secondary);
    }
    if is_filtered {
        name_style = name_style.fg(theme.closed).add_modifier(Modifier::BOLD);
    }
    if selected && state.file_tree_focus {
        name_style = name_style.add_modifier(Modifier::BOLD);
        if !is_filtered {
            name_style = name_style.fg(theme.primary);
        }
    }
    Line::from(vec![
        Span::raw(indent),
        Span::raw(ind),
        Span::raw(icon),
        Span::styled(name, name_style),
        Span::raw(" "),
        Span::styled(count, Style::default().fg(theme.muted)),
    ])
}

// ---------------------------------------------------------------------------
// Timeline pane — Go renderTimelinePanel (history.go:1721-1926)
// ---------------------------------------------------------------------------

fn render_timeline_panel(f: &mut Frame, state: &HistoryState, theme: &Theme, area: Rect) {
    let w = area.width as i64;
    let h = area.height;
    let focused = state.focused == HistoryFocus::Timeline;
    // The timeline pane's unfocused border is `t.Border`, not `t.Muted`
    // (history.go:1726-1729).
    let block = panel_block(theme, focused, "", theme.border);

    let bead_id = state.selected_bead_id().to_string();
    if bead_id.is_empty() {
        f.render_widget(
            Paragraph::new("TIMELINE\n\nSelect a bead to view timeline").block(block),
            area,
        );
        return;
    }
    let Some(hist) = state.history_for_bead(&bead_id) else {
        f.render_widget(
            Paragraph::new("TIMELINE\n\nNo history data").block(block),
            area,
        );
        return;
    };

    let now = jiff::Timestamp::now();
    let mut lines: Vec<Line> = vec![Line::from(Span::styled(
        format!("TIMELINE: {bead_id}"),
        Style::default()
            .fg(theme.primary)
            .add_modifier(Modifier::BOLD),
    ))];
    let entries = state.timeline_entries();
    if entries.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "No events recorded",
            Style::default().fg(theme.secondary),
        )));
    } else {
        // Go: `height - 6` floored to 3 (history.go:1770-1773).
        let max_visible = h.saturating_sub(6).max(3) as usize;
        let mut start = state.timeline_scroll_offset;
        if start > entries.len().saturating_sub(max_visible) {
            start = entries.len().saturating_sub(max_visible);
        }
        let end = (start + max_visible).min(entries.len());
        let spine = Style::default().fg(theme.border);
        let ts_style = Style::default().fg(theme.subtext);
        for entry in &entries[start..end] {
            let ts = format_timeline_timestamp(&entry.timestamp, now);
            let mut spans = vec![
                Span::styled(pad_left(&ts, 8), ts_style),
                Span::styled(" ┃ ", spine),
            ];
            match entry.entry_type {
                TimelineEntryType::Event => {
                    let color = match entry.event_type.as_str() {
                        "created" => theme.secondary,
                        "claimed" => theme.in_progress,
                        "closed" => theme.closed,
                        "reopened" => theme.open,
                        _ => theme.secondary,
                    };
                    spans.push(Span::styled(
                        entry.label.clone(),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ));
                    if !entry.detail.is_empty() {
                        spans.push(Span::raw(" "));
                        spans.push(Span::styled(
                            // Note the ASCII "..." here, not "…".
                            truncate_width(&entry.detail, w - 22, "..."),
                            Style::default().fg(theme.subtext),
                        ));
                    }
                }
                TimelineEntryType::Commit => {
                    let color = if entry.confidence >= 0.8 {
                        theme.closed
                    } else if entry.confidence >= 0.5 {
                        theme.in_progress
                    } else {
                        theme.subtext
                    };
                    spans.push(Span::styled("├─ ", Style::default().fg(theme.border)));
                    spans.push(Span::styled(
                        entry.label.clone(),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ));
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(
                        format!("{}%", (entry.confidence * 100.0) as i64),
                        Style::default().fg(color),
                    ));
                    let first_line = entry.detail.split('\n').next().unwrap_or("");
                    let msg = truncate_width(first_line, w - 28, "...");
                    if !msg.is_empty() {
                        lines.push(Line::from(spans));
                        lines.push(Line::from(vec![
                            Span::raw(" ".repeat(8)),
                            Span::styled(" ┃   ", spine),
                            Span::styled(
                                msg,
                                Style::default()
                                    .fg(theme.subtext)
                                    .add_modifier(Modifier::ITALIC),
                            ),
                        ]));
                        continue;
                    }
                }
                TimelineEntryType::Session => {
                    let color = if entry.session_score >= 80.0 {
                        theme.primary
                    } else if entry.session_score >= 50.0 {
                        theme.in_progress
                    } else {
                        theme.subtext
                    };
                    spans.push(Span::styled(
                        entry.label.clone(),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ));
                    if !entry.detail.is_empty() {
                        lines.push(Line::from(spans));
                        lines.push(Line::from(vec![
                            Span::raw(" ".repeat(8)),
                            Span::styled(" ┃   ", spine),
                            Span::styled(
                                truncate_width(&entry.detail, w - 16, "..."),
                                Style::default()
                                    .fg(theme.subtext)
                                    .add_modifier(Modifier::ITALIC),
                            ),
                        ]));
                        continue;
                    }
                }
            }
            lines.push(Line::from(spans));
        }
        if entries.len() > max_visible {
            lines.push(Line::from(vec![
                Span::raw(" ".repeat(8)),
                Span::styled(" ┃ ", spine),
                Span::styled(
                    format!("↕ {}-{} of {}", start + 1, end, entries.len()),
                    Style::default()
                        .fg(theme.subtext)
                        .add_modifier(Modifier::ITALIC),
                ),
            ]));
        }
    }

    // Cycle-time summary, only when the bead has a CycleTime
    // (history.go:1906-1923).
    if let Some(ct) = &hist.cycle_time {
        lines.push(Line::from(Span::styled(
            rule(w - 6),
            Style::default().fg(theme.border),
        )));
        let summary = Style::default().fg(theme.subtext);
        let mut parts: Vec<String> = Vec::new();
        if let Some(create_to_close) = ct.create_to_close {
            parts.push(format!("Cycle: {}", format_duration(create_to_close)));
        }
        let commits = hist.commits.as_deref().unwrap_or(&[]);
        if !commits.is_empty() {
            let avg: f64 = commits.iter().map(|c| c.confidence).sum::<f64>() / commits.len() as f64;
            parts.push(format!(
                " │ {} commits (avg {}%)",
                commits.len(),
                (avg * 100.0) as i64
            ));
        }
        lines.push(Line::from(Span::styled(parts.join(""), summary)));
    }

    f.render_widget(Paragraph::new(lines).block(block), area);
}

// ---------------------------------------------------------------------------
// Detail panel — Go renderDetailPanel / renderCommitDetail / renderEventsSection
// (history.go:2522-2659 / :2662-2848 / :3151-3235)
// ---------------------------------------------------------------------------

fn render_detail_panel(f: &mut Frame, state: &HistoryState, theme: &Theme, area: Rect) {
    let w = area.width as i64;
    let h = area.height;
    let focused = state.focused == HistoryFocus::Detail;
    let block = panel_block(theme, focused, "", theme.muted);

    let Some(hist) = state.selected_history() else {
        f.render_widget(Paragraph::new("No bead selected").block(block), area);
        return;
    };
    let sep = rule(w - 4);

    // Header (history.go:2542-2571).
    let mut lines: Vec<Line> = vec![Line::from(Span::styled(
        "COMMIT DETAILS",
        Style::default()
            .fg(theme.primary)
            .add_modifier(Modifier::BOLD),
    ))];
    let bead_info = format!(
        "{} {}: {}",
        status_icon(&hist.status),
        hist.bead_id,
        hist.title
    );
    let bead_info = if w > 10 {
        truncate_ellipsis(&bead_info, w - 6)
    } else {
        truncate_ellipsis(&bead_info, 5)
    };
    lines.push(Line::from(Span::styled(
        bead_info,
        Style::default().fg(theme.secondary),
    )));
    lines.push(Line::from(Span::styled(
        sep.clone(),
        Style::default().fg(theme.primary),
    )));

    // LIFECYCLE section (history.go:2573-2581).
    if !hist.events.is_empty() {
        lines.extend(render_events_section(&hist.events, w - 4, 5, theme));
        lines.push(Line::from(""));
    }

    // Aggregate stats for the footer (history.go:2583-2599). `totalFiles` is
    // the count of DISTINCT paths, not the sum of per-commit file counts.
    let commits = hist.commits.as_deref().unwrap_or(&[]);
    let mut total_add: i64 = 0;
    let mut total_del: i64 = 0;
    let mut total_conf: f64 = 0.0;
    let mut unique_files: BTreeSet<&str> = BTreeSet::new();
    for commit in commits {
        total_conf += commit.confidence;
        for file in &commit.files {
            unique_files.insert(file.path.as_str());
            total_add += file.insertions;
            total_del += file.deletions;
        }
    }
    let total_files = unique_files.len();
    let avg_conf = if commits.is_empty() {
        0.0
    } else {
        total_conf / commits.len() as f64
    };

    const FOOTER_HEIGHT: usize = 3;
    let content_height = (h as i64 - 2 - FOOTER_HEIGHT as i64 - 3).max(0) as usize;

    // Commit blocks, with a blank spacer between them.
    for (i, commit) in commits.iter().enumerate() {
        let is_selected = i == state.selected_commit && focused;
        lines.extend(render_commit_detail(commit, w - 4, is_selected, theme));
        if i + 1 < commits.len() {
            lines.push(Line::from(""));
        }
    }

    // Pad to push the footer to the bottom, then truncate (history.go:2615-2623).
    let budget = content_height + 3;
    while lines.len() < budget {
        lines.push(Line::from(""));
    }
    lines.truncate(budget);

    // Stats footer (history.go:2625-2651).
    lines.push(Line::from(Span::styled(
        sep,
        Style::default().fg(theme.primary),
    )));
    let conf_color = if avg_conf >= 0.8 {
        theme.open
    } else if avg_conf >= 0.5 {
        theme.secondary
    } else {
        theme.muted
    };
    let mut items: Vec<Span> = vec![
        Span::raw(format!("{} commits", commits.len())),
        Span::raw(format!(" • {total_files} files")),
    ];
    if total_add > 0 || total_del > 0 {
        items.push(Span::raw(" • "));
        items.push(Span::styled(
            format!("+{total_add}"),
            Style::default().fg(theme.open),
        ));
        items.push(Span::raw("/"));
        items.push(Span::styled(
            format!("-{total_del}"),
            Style::default().fg(theme.closed),
        ));
    }
    items.push(Span::raw(" • "));
    items.push(Span::styled(
        format!("{:.0}% avg", avg_conf * 100.0),
        Style::default().fg(conf_color),
    ));
    lines.push(Line::from(items));

    // Navigation hint (history.go:2653-2655) — the exact Go string.
    lines.push(Line::from(Span::styled(
        "J/K:nav  y:copy  o:open  g:graph",
        Style::default()
            .fg(theme.footer_hint)
            .add_modifier(Modifier::ITALIC),
    )));

    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// Go `renderCommitDetail` (history.go:2662-2848) — six parts per commit:
/// header, author, message, confidence+method, file summary, file rows.
fn render_commit_detail<'a>(
    commit: &'a HistoryCommit,
    width: i64,
    selected: bool,
    theme: &Theme,
) -> Vec<Line<'a>> {
    let now = jiff::Timestamp::now();
    let mut lines: Vec<Line> = Vec::new();

    // === COMMIT HEADER ===
    let mut type_icon = commit_type_indicator(&commit.message).to_string();
    if !type_icon.is_empty() {
        type_icon.push(' ');
    }
    let sha_style = if selected {
        Style::default()
            .fg(theme.primary)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.primary)
    };
    lines.push(Line::from(vec![
        Span::raw(indicator(selected)),
        Span::raw(type_icon),
        Span::styled(commit.short_sha.clone(), sha_style),
        Span::raw(" "),
        Span::styled(
            format!("({})", relative_time(&commit.timestamp, now)),
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::ITALIC),
        ),
    ]));

    // === AUTHOR LINE ===
    // The initials badge: theme.fg on theme.muted, bold, one space of padding.
    let initials = author_initials(&commit.author);
    let badge = format!(" {} ", initials);
    let badge_style = Style::default()
        .fg(theme.fg)
        .bg(theme.muted)
        .add_modifier(Modifier::BOLD);
    let author_style = Style::default().fg(theme.secondary);
    let date_str = format_absolute_datetime(&commit.timestamp);
    let author_line = |name: &str| -> Vec<Span> {
        vec![
            Span::raw("    "),
            Span::styled(badge.clone(), badge_style),
            Span::raw(" "),
            Span::styled(name.to_string(), author_style),
            Span::raw(" • "),
            Span::raw(date_str.clone()),
        ]
    };
    // Only truncate when the styled line actually exceeds width-2
    // (history.go:2713-2725).
    let full: Vec<Span> = author_line(&commit.author);
    let full_width: usize = full.iter().map(|s| s.width()).sum();
    if width > 10 && full_width as i64 > width - 2 {
        let max_author = (width - 30).max(10);
        lines.push(Line::from(author_line(&truncate_ellipsis(
            &commit.author,
            max_author,
        ))));
    } else {
        lines.push(Line::from(full));
    }

    // === MESSAGE ===
    let cc = parse_conventional_commit(&commit.message);
    if cc.is_conventional {
        let scope_str = if cc.scope.is_empty() {
            String::new()
        } else {
            format!("({})", cc.scope)
        };
        let max_subject = width - byte_len(&cc.commit_type) - byte_len(&scope_str) - 10;
        let mut spans = vec![
            Span::raw("    "),
            Span::styled(
                cc.commit_type.clone(),
                Style::default()
                    .fg(theme.primary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(scope_str),
        ];
        if cc.breaking {
            spans.push(Span::styled(
                "!",
                Style::default()
                    .fg(theme.closed)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        spans.push(Span::raw(format!(
            ": {}",
            truncate_ellipsis(&cc.subject, max_subject)
        )));
        lines.push(Line::from(spans));
    } else {
        lines.push(Line::from(format!(
            "    {}",
            truncate_ellipsis(&cc.subject, width - 6)
        )));
    }

    // === CONFIDENCE & METHOD ===
    let conf_color = if commit.confidence >= 0.8 {
        theme.open
    } else if commit.confidence >= 0.5 {
        theme.secondary
    } else {
        theme.muted
    };
    lines.push(Line::from(vec![
        Span::raw("    "),
        Span::styled(
            format!("{:.0}% confidence", commit.confidence * 100.0),
            Style::default().fg(conf_color),
        ),
        Span::raw(" "),
        Span::styled(
            method_label(commit.method).to_string(),
            Style::default().fg(theme.muted),
        ),
    ]));

    // === FILE CHANGES ===
    if !commit.files.is_empty() {
        let total_add: i64 = commit.files.iter().map(|f| f.insertions).sum();
        let total_del: i64 = commit.files.iter().map(|f| f.deletions).sum();
        let mut spans = vec![Span::raw(format!("    {} file(s)", commit.files.len()))];
        if total_add > 0 || total_del > 0 {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                format!("+{total_add}"),
                Style::default().fg(theme.open),
            ));
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                format!("-{total_del}"),
                Style::default().fg(theme.closed),
            ));
        }
        lines.push(Line::from(spans));

        // At most 5 rows, grouped by parent directory. The overflow line uses
        // three ASCII dots, unlike every other truncation here.
        const MAX_FILES: usize = 5;
        let groups = group_files_by_directory(&commit.files);
        let mut file_count = 0usize;
        for (_, group) in groups {
            if file_count >= MAX_FILES {
                let more = commit.files.len() - file_count;
                lines.push(Line::from(Span::styled(
                    format!("      +{more} more files..."),
                    Style::default()
                        .fg(theme.muted)
                        .add_modifier(Modifier::ITALIC),
                )));
                break;
            }
            for file in group {
                if file_count >= MAX_FILES {
                    break;
                }
                let filename = match file.path.rfind('/') {
                    Some(i) if i < file.path.len() - 1 => &file.path[i + 1..],
                    _ => file.path.as_str(),
                };
                let mut spans = vec![
                    Span::raw("      "),
                    Span::styled(
                        file_action_icon(&file.action).to_string(),
                        Style::default().fg(file_action_color(&file.action, theme)),
                    ),
                    Span::raw(" "),
                    Span::raw(truncate_ellipsis(filename, width - 15)),
                ];
                if file.insertions > 0 || file.deletions > 0 {
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(
                        format!("+{}", file.insertions),
                        Style::default().fg(theme.open),
                    ));
                    spans.push(Span::raw("/"));
                    spans.push(Span::styled(
                        format!("-{}", file.deletions),
                        Style::default().fg(theme.closed),
                    ));
                }
                lines.push(Line::from(spans));
                file_count += 1;
            }
        }
    }
    lines
}

/// Go `renderEventsSection` (history.go:3151-3235) — `LIFECYCLE (<n>)` then the
/// most recent events in reverse chronological order, newest first, each
/// `<connector> <icon> <time in Width(8)> <initials>`, with `  +<n> more` when
/// events were dropped. The time string is hard-cut to 7 characters.
fn render_events_section<'a>(
    events: &'a [BeadEvent],
    width: i64,
    max_lines: usize,
    theme: &Theme,
) -> Vec<Line<'a>> {
    if events.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![Line::from(Span::styled(
        format!("LIFECYCLE ({})", events.len()),
        Style::default()
            .fg(theme.secondary)
            .add_modifier(Modifier::BOLD),
    ))];

    let mut available_for_events = max_lines.saturating_sub(1);
    let needs_more_line = events.len() > available_for_events;
    if needs_more_line {
        available_for_events = available_for_events.saturating_sub(1);
    }

    let now = jiff::Timestamp::now();
    let time_style = Style::default().fg(theme.muted);
    let author_style = Style::default().fg(theme.secondary);
    let mut displayed = 0usize;
    for i in (0..events.len()).rev() {
        if displayed >= available_for_events {
            break;
        }
        let event = &events[i];
        let et = event.event_type.as_str();
        let icon_span = || {
            Span::styled(
                event_type_icon(et).to_string(),
                Style::default().fg(event_type_color(et, theme)),
            )
        };
        let mut time_str = relative_time(&event.timestamp, now);
        if time_str.chars().count() > 7 {
            time_str = time_str.chars().take(7).collect();
        }
        let initials = author_initials(&event.author);
        // The chronologically-first event closes the spine.
        let connector = if i == 0 { "└" } else { "│" };
        let full: Vec<Span> = vec![
            Span::raw(format!("{connector} ")),
            icon_span(),
            Span::raw(" "),
            Span::styled(pad_right(&time_str, 8), time_style),
            Span::raw(" "),
            Span::styled(initials, author_style),
        ];
        let full_width: usize = full.iter().map(|s| s.width()).sum();
        // Drop the author column rather than overflow.
        if full_width as i64 > width - 2 {
            lines.push(Line::from(vec![
                Span::raw(format!("{connector} ")),
                icon_span(),
                Span::raw(" "),
                Span::styled(pad_right(&time_str, 8), time_style),
            ]));
        } else {
            lines.push(Line::from(full));
        }
        displayed += 1;
    }
    if needs_more_line {
        lines.push(Line::from(Span::styled(
            format!("  +{} more", events.len() - displayed),
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::ITALIC),
        )));
    }
    lines
}

/// Go `renderCompactTimeline` (history.go:1945-2025) — the one-line
/// `○──●──├──├──├──✓  5d cycle, 3 commits` summary, with at most 5 commit
/// markers (4 `├` plus `…`) and an optional second line carrying the date
/// range when both endpoints exist and there is room.
///
/// Like Go's `renderFileTreePanel`, this has no call site in the Go TUI
/// either; it is ported so the behaviour exists and is testable, and no
/// invented caller was added.
pub fn render_compact_timeline(hist: &BeadHistory, max_width: i64) -> String {
    let mut markers: Vec<&str> = Vec::new();
    let m = &hist.milestones;
    if m.created.is_some() {
        markers.push("○");
    }
    if m.claimed.is_some() {
        markers.push("●");
    }
    let commit_count = hist.commits.as_ref().map_or(0, Vec::len);
    const MAX_COMMIT_MARKERS: usize = 5;
    let shown = if commit_count > MAX_COMMIT_MARKERS {
        MAX_COMMIT_MARKERS - 1
    } else {
        commit_count
    };
    markers.extend(std::iter::repeat_n("├", shown));
    if commit_count > MAX_COMMIT_MARKERS {
        markers.push("…");
    }
    if m.closed.is_some() {
        markers.push("✓");
    }
    if markers.is_empty() {
        return "(no timeline data)".to_string();
    }

    let mut summary: Vec<String> = Vec::new();
    if let Some(d) = hist.cycle_time.as_ref().and_then(|ct| ct.create_to_close) {
        summary.push(format!("{} cycle", format_duration(d)));
    }
    if commit_count > 0 {
        summary.push(if commit_count == 1 {
            "1 commit".to_string()
        } else {
            format!("{commit_count} commits")
        });
    }

    let mut result = markers.join("──");
    if !summary.is_empty() {
        result.push_str("  ");
        result.push_str(&summary.join(", "));
    }
    // Go: startTime is the created milestone, or the claimed one when there
    // is no created event; endTime is the closed milestone
    // (history.go:1952-1962, :1980-1982).
    let start = m.created.as_ref().or(m.claimed.as_ref());
    if let (Some(start), Some(closed)) = (start, &m.closed) {
        let date_range = format!(
            "{} ─ {}",
            format_month_day(&start.timestamp),
            format_month_day(&closed.timestamp)
        );
        if byte_len(&result) + byte_len(&date_range) + 4 < max_width {
            result.push('\n');
            result.push_str(&date_range);
        }
    }
    truncate_width(&result, max_width, "...")
}

/// `t.Format("Jan 2")` for the compact timeline's date range.
fn format_month_day(timestamp: &str) -> String {
    match parse_wall_clock(timestamp) {
        Some(w) => format!(
            "{} {}",
            MONTH_ABBREV[(w.month as usize).clamp(1, 12) - 1],
            w.day
        ),
        None => timestamp.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Git-mode panels — Go renderGitCommitListPanel / renderGitCommitLine
// (history.go:3252-3345), renderGitBeadListPanel (:3570-3671) and
// renderGitDetailPanel (:3348-3478)
// ---------------------------------------------------------------------------

fn render_git_commit_list_panel(f: &mut Frame, state: &HistoryState, theme: &Theme, area: Rect) {
    let w = area.width as i64;
    let h = area.height;
    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            "COMMITS",
            Style::default()
                .fg(theme.primary)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            rule(w - 4),
            Style::default().fg(theme.primary),
        )),
    ];
    let commits = state.filtered_commit_list();
    let vis = visible_items(h);
    let start = state.git_scroll_offset;
    let end = (start + vis).min(commits.len());
    for (i, commit) in commits.iter().enumerate().take(end).skip(start) {
        lines.push(render_git_commit_line(state, i, commit, w - 4, theme));
    }
    while lines.len() < h.saturating_sub(2) as usize {
        lines.push(Line::from(""));
    }
    let focused = state.focused == HistoryFocus::List;
    f.render_widget(
        Paragraph::new(lines).block(panel_block(theme, focused, "", theme.muted)),
        area,
    );
}

/// Go `renderGitCommitLine` (history.go:3306-3345) —
/// `<indicator><shortSHA> <message> [<n beads>]`.
fn render_git_commit_line<'a>(
    state: &'a HistoryState,
    idx: usize,
    commit: &'a CommitListEntry,
    width: i64,
    theme: &Theme,
) -> Line<'a> {
    let selected = idx == state.selected_git_commit;
    let ind = indicator(selected);
    let bead_count = format!("[{}]", commit.bead_ids.len());
    let max_msg =
        (width - byte_len(ind) - byte_len(&commit.short_sha) - byte_len(&bead_count) - 6).max(10);
    let msg = truncate_ellipsis(&commit.message, max_msg);
    let highlighted = selected && state.focused == HistoryFocus::List;
    let sha_style = if highlighted {
        Style::default()
            .fg(theme.primary)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.primary)
    };
    let msg_style = if highlighted {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    Line::from(vec![
        Span::raw(ind),
        Span::styled(commit.short_sha.clone(), sha_style),
        Span::raw(" "),
        Span::styled(msg, msg_style),
        Span::raw(" "),
        Span::styled(bead_count, Style::default().fg(theme.secondary)),
    ])
}

/// Go `renderGitBeadListPanel` (history.go:3570-3671) — the middle pane in
/// git mode: the selected commit's related beads.
fn render_git_bead_list_panel(f: &mut Frame, state: &HistoryState, theme: &Theme, area: Rect) {
    let w = area.width as i64;
    let h = area.height;
    let focused = state.focused == HistoryFocus::Middle;
    let block = panel_block(theme, focused, "", theme.muted);

    let Some(commit) = state.selected_git_commit() else {
        f.render_widget(
            Paragraph::new("Select a commit to view beads").block(block),
            area,
        );
        return;
    };
    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            "RELATED BEADS",
            Style::default()
                .fg(theme.primary)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            rule(w - 4),
            Style::default().fg(theme.primary),
        )),
    ];
    let total = commit.bead_ids.len();
    let vis = visible_items(h);
    let start = if state.middle_scroll_offset >= total {
        0
    } else {
        state.middle_scroll_offset
    };
    let end = (start + vis).min(total);
    for (i, bead_id) in commit.bead_ids.iter().enumerate().take(end).skip(start) {
        let (status, title) = match state.history_for_bead(bead_id) {
            Some(hist) => (status_icon(&hist.status), hist.title.clone()),
            None => ("○", bead_id.clone()),
        };
        let is_selected = i == state.selected_related_bead && focused;
        let title = truncate_ellipsis(&title, (w - 12).max(10));
        lines.push(Line::from(vec![
            Span::raw(indicator(is_selected)),
            Span::raw(status),
            Span::raw(" "),
            Span::styled(
                title,
                if is_selected {
                    Style::default()
                        .fg(theme.primary)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                },
            ),
        ]));
    }
    if total > vis {
        // Go: pct = offset * 100 / (total - visible); zero when there is
        // nothing to scroll (history.go:3552-3558 and :3656-3662).
        let pct = (state.middle_scroll_offset * 100)
            .checked_div(total - vis)
            .unwrap_or(0);
        lines.push(Line::from(Span::styled(
            format!("↕ {end}/{total} ({pct}%)"),
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::ITALIC),
        )));
    }
    while lines.len() < h.saturating_sub(2) as usize {
        lines.push(Line::from(""));
    }
    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// Go `renderGitDetailPanel` (history.go:3348-3478) — the commit's related
/// beads, then a `COMMIT DETAILS` block, then the git-mode footer hint.
fn render_git_detail_panel(f: &mut Frame, state: &HistoryState, theme: &Theme, area: Rect) {
    let w = area.width as i64;
    let h = area.height;
    let focused = state.focused == HistoryFocus::Detail;
    let block = panel_block(theme, focused, "", theme.muted);

    let Some(commit) = state.selected_git_commit() else {
        f.render_widget(Paragraph::new("No commit selected").block(block), area);
        return;
    };
    let sep = rule(w - 4);
    let header = Style::default()
        .fg(theme.primary)
        .add_modifier(Modifier::BOLD);
    let mut lines: Vec<Line> = vec![Line::from(Span::styled("RELATED BEADS", header))];
    lines.push(Line::from(Span::styled(sep.clone(), header)));

    for (i, bead_id) in commit.bead_ids.iter().enumerate() {
        let is_selected = i == state.selected_related_bead && focused;
        let (status, title) = match state.history_for_bead(bead_id) {
            Some(hist) => (status_icon(&hist.status), hist.title.clone()),
            None => ("○", bead_id.clone()),
        };
        let title = truncate_ellipsis(&title, (w - 8).max(10));
        lines.push(Line::from(vec![
            Span::raw(indicator(is_selected)),
            Span::raw(status),
            Span::raw(" "),
            Span::raw(bead_id.clone()),
            Span::raw(" "),
            Span::styled(
                title,
                if is_selected {
                    Style::default()
                        .fg(theme.primary)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                },
            ),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(sep.clone(), header)));
    lines.push(Line::from(Span::styled("COMMIT DETAILS", header)));
    lines.push(Line::from(Span::styled(sep.clone(), header)));

    // Go truncates the SHA line by raw bytes; do the same on chars so the
    // slice cannot split a code point.
    let sha_line = format!("SHA: {}", commit.sha);
    let sha_line = if w > 10 && byte_len(&sha_line) > w - 6 {
        let keep = (w - 7).max(0) as usize;
        format!("{}…", sha_line.chars().take(keep).collect::<String>())
    } else {
        sha_line
    };
    lines.push(Line::from(Span::styled(
        sha_line,
        Style::default().fg(theme.primary),
    )));
    lines.push(Line::from(Span::styled(
        truncate_ellipsis(&format!("Author: {}", commit.author), w - 6),
        Style::default().fg(theme.secondary),
    )));
    lines.push(Line::from(Span::styled(
        format!("Date: {}", commit.timestamp),
        Style::default().fg(theme.muted),
    )));
    lines.push(Line::from(Span::styled(
        format!("Files: {} changed", commit.file_count),
        Style::default().fg(theme.muted),
    )));

    lines.push(Line::from(""));
    for ml in commit.message.split('\n') {
        lines.push(Line::from(truncate_ellipsis(ml, w - 6)));
    }

    // Footer budget (history.go:3454-3474).
    const FOOTER_HEIGHT: i64 = 2;
    let content_height = (h as i64 - 2 - FOOTER_HEIGHT).max(1) as usize;
    while lines.len() < content_height {
        lines.push(Line::from(""));
    }
    lines.truncate(content_height);
    lines.push(Line::from(Span::styled(sep, header)));
    lines.push(Line::from(Span::styled(
        "J/K:bead  y:copy  o:open  g:graph",
        Style::default()
            .fg(theme.footer_hint)
            .add_modifier(Modifier::ITALIC),
    )));

    f.render_widget(Paragraph::new(lines).block(block), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(bead: &str, ty: &str, ts: &str, author: &str) -> BeadEvent {
        BeadEvent {
            bead_id: bead.into(),
            event_type: match ty {
                "created" => EventType::Created,
                "claimed" => EventType::Claimed,
                "closed" => EventType::Closed,
                "reopened" => EventType::Reopened,
                _ => EventType::Modified,
            },
            timestamp: ts.into(),
            commit_sha: format!("sha-{bead}-{ty}"),
            commit_msg: "msg".into(),
            author: author.into(),
            author_email: format!("{author}@example.com"),
            before: None,
            after: None,
            transition_observed: true,
        }
    }

    fn file(path: &str, action: &str, ins: i64, del: i64) -> FileChange {
        FileChange {
            path: path.into(),
            action: action.into(),
            insertions: ins,
            deletions: del,
        }
    }

    fn commit(bead: &str, sha: &str, ts: &str, conf: f64) -> HistoryCommit {
        HistoryCommit {
            bead_id: bead.into(),
            sha: sha.into(),
            short_sha: sha.chars().take(7).collect(),
            message: "feat(api): add endpoint".into(),
            author: "Ada Lovelace".into(),
            author_email: "ada@example.com".into(),
            timestamp: ts.into(),
            files: vec![file("crates/api/src/main.rs", "M", 12, 3)],
            method: "explicit_id",
            methods: vec!["explicit_id".into()],
            confidence: conf,
            reason: "because".into(),
            confirmed: false,
        }
    }

    fn report(histories: Vec<(&str, Vec<HistoryCommit>, Vec<BeadEvent>)>) -> HistoryReport {
        let mut map: BTreeMap<String, BeadHistory> = BTreeMap::new();
        for (id, commits, events) in histories {
            let milestones = bv_correlation::history::get_bead_milestones(&events);
            let cycle = bv_correlation::history::calculate_cycle_time(&milestones);
            map.insert(
                id.to_string(),
                BeadHistory {
                    bead_id: id.to_string(),
                    title: format!("title of {id}"),
                    status: "open".into(),
                    events,
                    milestones,
                    commits: if commits.is_empty() {
                        None
                    } else {
                        Some(commits)
                    },
                    cycle_time: cycle,
                    last_author: String::new(),
                },
            );
        }
        HistoryReport {
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
            stats: HistoryStats {
                // Two beads, three distinct commits, one author.
                total_beads: map.len() as i64,
                beads_with_commits: 2,
                total_commits: 3,
                unique_authors: 1,
                avg_commits_per_bead: 1.5,
                avg_cycle_time_days: None,
                method_distribution: BTreeMap::new(),
                strategies: None,
                feedback_applied: None,
            },
            commit_index: BTreeMap::new(),
            histories: map,
            causal_history: None,
        }
    }

    fn sample() -> HistoryState {
        HistoryState::from_report(report(vec![
            (
                "a-1",
                vec![
                    commit("a-1", "aaa1", "2026-01-03T00:00:00Z", 0.95),
                    commit("a-1", "aaa2", "2026-01-02T00:00:00Z", 0.55),
                ],
                vec![event(
                    "a-1",
                    "created",
                    "2026-01-01T00:00:00Z",
                    "Ada Lovelace",
                )],
            ),
            (
                "b-2",
                vec![commit("b-2", "bbb1", "2026-01-02T12:00:00Z", 0.60)],
                vec![],
            ),
        ]))
    }

    // --- G2: confidence ladder -------------------------------------------

    #[test]
    fn confidence_cycles_through_the_go_ladder() {
        let mut s = sample();
        assert_eq!(s.min_confidence(), 0.0);
        assert_eq!(s.cycle_confidence(), 0.5);
        assert_eq!(s.cycle_confidence(), 0.75);
        assert_eq!(s.cycle_confidence(), 0.9);
        assert_eq!(s.cycle_confidence(), 0.0, "wraps around");
    }

    #[test]
    fn confidence_filter_shrinks_the_list_and_drops_emptied_beads() {
        let mut s = sample();
        s.cycle_confidence(); // 0.5 — everything survives
        assert_eq!(s.histories().len(), 2);
        s.cycle_confidence(); // 0.75 — a-1 keeps 0.95, b-2 (0.60) drops out
        assert_eq!(s.histories().len(), 1);
        assert_eq!(s.selected_bead_id(), "a-1");
        s.cycle_confidence(); // 0.9
        assert_eq!(s.histories().len(), 1);
        s.cycle_confidence(); // back to 0
        assert_eq!(s.histories().len(), 2);
    }

    // --- G6: responsive layout -------------------------------------------

    #[test]
    fn layout_breaks_at_100_and_150_columns() {
        let mut s = sample();
        s.set_size(80, 40);
        assert_eq!(s.determine_layout(), HistoryLayout::Narrow);
        assert_eq!(s.pane_count(), 2);
        s.set_size(120, 40);
        assert_eq!(s.determine_layout(), HistoryLayout::Standard);
        assert_eq!(s.pane_count(), 3);
        s.set_size(160, 40);
        assert_eq!(s.determine_layout(), HistoryLayout::Wide);
        assert_eq!(
            s.pane_count(),
            4,
            "the timeline pane is on by default at 150+"
        );
    }

    #[test]
    fn pane_count_follows_the_timeline_toggle() {
        let mut s = sample();
        s.set_size(160, 40);
        assert!(s.timeline_visible());
        // Go's ToggleTimeline returns the new visibility (history.go:196-206).
        assert!(!s.toggle_timeline(), "the first press hides it");
        assert!(!s.timeline_visible());
        assert_eq!(s.pane_count(), 3);
        assert!(s.toggle_timeline(), "the second press shows it");
        assert!(s.timeline_visible());
        assert_eq!(s.pane_count(), 4);
    }

    #[test]
    fn resize_demotes_focus_to_a_pane_that_still_exists() {
        let mut s = sample();
        s.set_size(160, 40);
        s.set_focused(HistoryFocus::Timeline);
        s.set_size(80, 40);
        assert_eq!(s.focused(), HistoryFocus::List, "narrow has no timeline");
    }

    // --- G7: focus model --------------------------------------------------

    #[test]
    fn focus_cycles_follow_the_pane_count() {
        let mut s = sample();
        s.set_size(80, 40); // 2 panes
        assert_eq!(s.focused(), HistoryFocus::List);
        s.toggle_focus();
        assert_eq!(s.focused(), HistoryFocus::Detail);
        s.toggle_focus();
        assert_eq!(s.focused(), HistoryFocus::List);

        s.set_size(120, 40); // 3 panes
        s.toggle_focus();
        assert_eq!(s.focused(), HistoryFocus::Middle);
        s.toggle_focus();
        assert_eq!(s.focused(), HistoryFocus::Detail);
        s.toggle_focus();
        assert_eq!(s.focused(), HistoryFocus::List);

        s.set_size(160, 40); // 4 panes
        s.toggle_focus();
        assert_eq!(s.focused(), HistoryFocus::Timeline);
        s.toggle_focus();
        assert_eq!(s.focused(), HistoryFocus::Middle);
        s.toggle_focus();
        assert_eq!(s.focused(), HistoryFocus::Detail);
        s.toggle_focus();
        assert_eq!(s.focused(), HistoryFocus::List);
    }

    #[test]
    fn j_k_moves_the_cursor_of_the_focused_pane() {
        let mut s = sample();
        s.set_size(120, 40);
        s.set_focused(HistoryFocus::List);
        s.move_down();
        assert_eq!(s.selected_bead_id(), "b-2");
        s.set_focused(HistoryFocus::Middle);
        s.move_down();
        assert_eq!(s.selected_commit().unwrap().short_sha, "bbb1");
    }

    // --- G5: the timeline toggle -----------------------------------------

    #[test]
    fn timeline_is_unavailable_in_git_mode_and_below_100_columns() {
        let mut s = sample();
        s.set_size(80, 40);
        assert!(!s.timeline_available());
        assert!(!s.toggle_timeline(), "the toggle refuses and stays hidden");
        s.set_size(160, 40);
        assert!(s.timeline_available());
        s.toggle_mode();
        assert!(!s.timeline_available(), "git mode has no per-bead timeline");
    }

    // --- G14: git-centric mode -------------------------------------------

    #[test]
    fn git_mode_builds_a_deduped_commit_list_sorted_by_timestamp_desc() {
        let mut s = sample();
        s.set_size(160, 40);
        s.toggle_mode();
        assert!(s.is_git_mode());
        assert_eq!(s.commit_list_len(), 3, "three distinct SHAs");
        // Newest first: 2026-01-03, 2026-01-02 12:00, 2026-01-02 00:00.
        assert_eq!(s.selected_git_commit().unwrap().short_sha, "aaa1");
        assert_eq!(
            s.filtered_commit_list()[0].timestamp,
            "2026-01-03 00:00",
            "the sort key is the formatted string"
        );
    }

    #[test]
    fn git_mode_related_bead_navigation_moves_with_j_k() {
        let mut s = sample();
        s.set_size(120, 40);
        s.toggle_mode();
        s.set_focused(HistoryFocus::List);
        s.move_down();
        assert_eq!(s.selected_git_commit().unwrap().short_sha, "bbb1");
        s.next_commit();
        assert_eq!(s.selected_related_bead_id(), "b-2");
    }

    // --- G15: search -----------------------------------------------------

    #[test]
    fn search_filters_beads_and_restores_the_full_list_when_cleared() {
        let mut s = sample();
        s.set_size(120, 40);
        s.start_search();
        for c in "title of b-2".chars() {
            s.push_search_char(c);
        }
        assert_eq!(s.histories().len(), 1);
        assert_eq!(s.selected_bead_id(), "b-2");
        // "title of b-2" is 13 characters; five backspaces leaves "title o",
        // which both beads' titles still contain.
        for _ in 0..5 {
            s.backspace_search();
        }
        assert_eq!(s.search_query(), "title o");
        assert_eq!(s.histories().len(), 2, "both titles contain 'title o'");
        s.cancel_search();
        assert_eq!(s.histories().len(), 2);
        assert!(s.search_query().is_empty());
    }

    #[test]
    fn search_that_matches_nothing_empties_the_bead_list() {
        let mut s = sample();
        s.set_size(120, 40);
        s.start_search();
        for c in "zzzz".chars() {
            s.push_search_char(c);
        }
        assert!(s.histories().is_empty());
        assert_eq!(s.selected_bead_id(), "");
    }

    // --- G16: report refresh ---------------------------------------------

    #[test]
    fn set_report_keeps_the_selected_bead_and_commit() {
        let mut s = sample();
        s.set_size(120, 40);
        s.move_down();
        assert_eq!(s.selected_bead_id(), "b-2");
        s.set_report(sample().report().cloned().unwrap());
        assert_eq!(s.selected_bead_id(), "b-2", "selection survives a refresh");
    }

    // --- G10: file tree --------------------------------------------------

    #[test]
    fn file_tree_counts_each_directory_once_per_commit() {
        // A single bead with a single commit touching two files in the same
        // directory. Go history.go:530-541 dedupes the prefix set *per commit*,
        // so that directory must count 1, not 2.
        let mut s = HistoryState::from_report(report(vec![(
            "a-1",
            vec![HistoryCommit {
                files: vec![
                    file("crates/api/a.rs", "M", 1, 0),
                    file("crates/api/b.rs", "M", 1, 0),
                ],
                ..commit("a-1", "aaa1", "2026-01-03T00:00:00Z", 0.9)
            }],
            vec![],
        )]));
        s.build_file_tree();
        let paths: Vec<&str> = s.file_tree.iter().map(|n| n.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["crates", "crates/api", "crates/api/a.rs", "crates/api/b.rs"]
        );
        for path in &paths {
            let node = s.file_tree.iter().find(|n| n.path == *path).unwrap();
            assert_eq!(
                node.change_count, 1,
                "{path}: one commit, one increment per distinct ancestor path"
            );
        }
        // Directories sort before files, then by name.
        let roots: Vec<&str> = s
            .file_tree_roots
            .iter()
            .map(|&i| s.file_tree[i].name.as_str())
            .collect();
        assert_eq!(roots, vec!["crates"]);
    }

    #[test]
    fn file_tree_directory_count_tracks_commits_not_files() {
        // `sample()` gives a-1 two commits and b-2 one, and all three touch
        // `crates/api/src/main.rs`. The directory must count 3 — one per
        // commit — even though each commit lists the same single path.
        let mut s = sample();
        s.build_file_tree();
        let dir = s.file_tree.iter().find(|n| n.path == "crates/api").unwrap();
        assert_eq!(
            dir.change_count, 3,
            "one increment per commit that touches the directory"
        );
        let leaf = s
            .file_tree
            .iter()
            .find(|n| n.path == "crates/api/src/main.rs")
            .unwrap();
        assert_eq!(leaf.change_count, 3, "and the leaf agrees");
    }

    #[test]
    fn file_filter_keeps_only_commits_touching_the_path_or_a_subpath() {
        let mut s = sample();
        s.set_size(120, 40);
        s.file_filter = "crates/api".into();
        s.rebuild_filtered_list();
        assert_eq!(s.histories().len(), 2, "both beads touch that path");
        s.file_filter = "does/not/exist".into();
        s.rebuild_filtered_list();
        assert!(s.histories().is_empty(), "no commit touches it");
    }

    // --- helpers ---------------------------------------------------------

    #[test]
    fn author_initials_match_the_go_rules() {
        assert_eq!(author_initials(""), "??");
        assert_eq!(author_initials("   "), "??");
        assert_eq!(author_initials("ada"), "AD");
        assert_eq!(author_initials("Ada Lovelace"), "AL");
        assert_eq!(author_initials("Ada King Lovelace"), "AL");
    }

    #[test]
    fn method_labels_match_the_go_strings() {
        assert_eq!(method_label("co_committed"), "(co-committed)");
        assert_eq!(method_label("explicit_id"), "(explicit ID)");
        assert_eq!(method_label("temporal_author"), "(temporal)");
        assert_eq!(method_label("something_else"), "");
    }

    #[test]
    fn conventional_commits_parse_type_scope_and_breaking_marker() {
        let cc = parse_conventional_commit("feat(api)!: add the thing");
        assert!(cc.is_conventional);
        assert_eq!(cc.commit_type, "feat");
        assert_eq!(cc.scope, "api");
        assert!(cc.breaking);
        assert_eq!(cc.subject, "add the thing");

        let plain = parse_conventional_commit("just a message");
        assert!(!plain.is_conventional);
        assert_eq!(plain.subject, "just a message");

        // Go requires a colon, so a bare prefix is not conventional.
        let nocolon = parse_conventional_commit("feat no colon");
        assert!(!nocolon.is_conventional);
    }

    #[test]
    fn commit_type_icons_cover_merge_revert_and_the_conventional_table() {
        assert_eq!(commit_type_indicator("Merge branch main"), "⊕");
        assert_eq!(commit_type_indicator("Revert \"x\""), "↩");
        assert_eq!(commit_type_indicator("feat: x"), "✨");
        assert_eq!(commit_type_indicator("fix: x"), "🐛");
        assert_eq!(commit_type_indicator("wip: x"), "", "wip has no icon");
        assert_eq!(commit_type_indicator("plain"), "");
    }

    #[test]
    fn file_actions_map_to_go_icons() {
        assert_eq!(file_action_icon("A"), "+");
        assert_eq!(file_action_icon("D"), "-");
        assert_eq!(file_action_icon("M"), "~");
        assert_eq!(file_action_icon("R"), "→");
        assert_eq!(file_action_icon("?"), "?");
    }

    #[test]
    fn event_icons_labels_and_status_icons_match_go() {
        assert_eq!(event_type_icon("created"), "🆕");
        assert_eq!(event_type_icon("claimed"), "👤");
        assert_eq!(event_type_icon("closed"), "✓");
        assert_eq!(event_type_icon("reopened"), "↺");
        assert_eq!(event_type_icon("modified"), "✎");
        assert_eq!(event_type_icon("nope"), "•");
        assert_eq!(event_type_label("created"), "Created");
        assert_eq!(event_type_label("nope"), "nope");
        assert_eq!(status_icon("open"), "○");
        assert_eq!(status_icon("closed"), "✓");
        assert_eq!(status_icon("in_progress"), "●");
    }

    #[test]
    fn truncation_uses_cell_width_and_the_ellipsis_suffix() {
        assert_eq!(truncate_ellipsis("hello", 10), "hello");
        assert_eq!(truncate_ellipsis("hello world", 5), "hell…");
        assert_eq!(truncate_ellipsis("hi", 1), "…");
        assert_eq!(truncate_ellipsis("anything", 0), "");
        // ASCII dots, not an ellipsis, for the timeline-style suffixes.
        assert_eq!(truncate_width("hello world", 8, "..."), "hello...");
    }

    #[test]
    fn duration_and_cycle_time_match_the_go_ladders() {
        assert_eq!(format_duration(30 * 60 * 1_000_000_000), "30m");
        assert_eq!(format_duration(3 * 3600 * 1_000_000_000), "3h");
        assert_eq!(format_duration(24 * 3600 * 1_000_000_000), "1d");
        assert_eq!(format_duration(50 * 3600 * 1_000_000_000), "2d");

        assert_eq!(format_cycle_time(0.01), "14m");
        assert_eq!(format_cycle_time(0.5), "12.0h");
        assert_eq!(format_cycle_time(3.0), "3.0d");
        assert_eq!(format_cycle_time(14.0), "2.0w");
    }

    #[test]
    fn absolute_datetime_uses_the_go_layout() {
        assert_eq!(
            format_absolute_datetime("2026-08-23T01:18:39+07:00"),
            "2026-08-23 01:18"
        );
    }

    #[test]
    fn timeline_timestamp_picks_one_of_the_four_go_layouts() {
        let now = "2026-08-25T12:00:00Z".parse::<jiff::Timestamp>().unwrap();
        assert_eq!(
            format_timeline_timestamp("2026-08-25T09:04:00Z", now),
            "9:04 AM"
        );
        assert_eq!(
            format_timeline_timestamp("2026-08-23T15:04:00Z", now),
            "Sun 3PM"
        );
        assert_eq!(
            format_timeline_timestamp("2026-03-04T15:04:00Z", now),
            "Mar 4"
        );
        assert_eq!(
            format_timeline_timestamp("2024-03-04T15:04:00Z", now),
            "Mar '24"
        );
    }

    #[test]
    fn relative_time_walks_the_go_ladder() {
        let now = "2026-08-25T12:00:00Z".parse::<jiff::Timestamp>().unwrap();
        let at = |s: &str| relative_time(s, now);
        assert_eq!(at("2026-08-25T11:59:30Z"), "just now");
        assert_eq!(at("2026-08-25T11:30:00Z"), "30m ago");
        assert_eq!(at("2026-08-25T09:00:00Z"), "3h ago");
        assert_eq!(at("2026-08-23T12:00:00Z"), "2d ago");
        assert_eq!(at("2026-08-10T12:00:00Z"), "2w ago");
        assert_eq!(at("2026-04-10T12:00:00Z"), "4mo ago");
        assert_eq!(at("2024-04-10T12:00:00Z"), "2y ago");
        assert_eq!(at("2027-01-01T00:00:00Z"), "in future");
        assert_eq!(at("not a timestamp"), "unknown");
    }

    // --- G13/G17: timeline -----------------------------------------------

    #[test]
    fn timeline_prefers_milestones_and_sorts_chronologically() {
        let mut s = sample();
        s.set_size(160, 40);
        let entries = s.timeline_entries();
        // a-1 first by commit count, so it is the selection.
        assert_eq!(entries.len(), 3, "one milestone plus two commits");
        assert_eq!(entries[0].entry_type, TimelineEntryType::Event);
        assert_eq!(entries[0].label, "○ Created");
        assert_eq!(entries[1].entry_type, TimelineEntryType::Commit);
        assert_eq!(entries[1].label, "aaa2", "2026-01-02 before 2026-01-03");
        assert_eq!(entries[2].label, "aaa1");
    }

    #[test]
    fn compact_timeline_caps_commit_markers_and_reports_the_cycle() {
        let mut s = sample();
        s.set_size(160, 40);
        let hist = s.selected_history().unwrap().clone();
        let out = render_compact_timeline(&hist, 80);
        // Markers joined with "──" (Go `strings.Join`): one ○ Created
        // milestone plus two commits, no close.
        assert_eq!(out, "○──├──├  2 commits");
    }

    #[test]
    fn compact_timeline_caps_at_five_commit_markers() {
        let mut commits = Vec::new();
        for i in 0..9 {
            commits.push(commit("a-1", &format!("s{i}"), "2026-01-02T00:00:00Z", 0.9));
        }
        let hist = BeadHistory {
            bead_id: "a-1".into(),
            title: "t".into(),
            status: "open".into(),
            events: vec![],
            milestones: Default::default(),
            commits: Some(commits),
            cycle_time: None,
            last_author: String::new(),
        };
        let out = render_compact_timeline(&hist, 120);
        assert_eq!(
            out, "├──├──├──├──…  9 commits",
            "4 markers joined with ──, then … when more than 5 commits exist"
        );
    }

    #[test]
    fn compact_timeline_without_markers_says_so() {
        let hist = BeadHistory {
            bead_id: "a-1".into(),
            title: "t".into(),
            status: "open".into(),
            events: vec![],
            milestones: Default::default(),
            commits: None,
            cycle_time: None,
            last_author: String::new(),
        };
        assert_eq!(render_compact_timeline(&hist, 80), "(no timeline data)");
    }

    // --- rendering -------------------------------------------------------

    fn draw(state: &mut HistoryState, theme: &Theme, w: u16, h: u16) -> String {
        let backend = ratatui::backend::TestBackend::new(w, h);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render_history(f, state, theme, f.area()))
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn render_draws_the_four_header_lines_and_the_bead_panel() {
        let theme = Theme::default();
        let mut s = sample();
        let out = draw(&mut s, &theme, 120, 30);
        assert!(out.contains("HISTORY"), "{out}");
        assert!(out.contains("◈ Beads"), "{out}");
        assert!(out.contains("[/] search  [H] close"), "{out}");
        assert!(out.contains("2 beads"), "{out}");
        assert!(out.contains("BEADS WITH HISTORY"), "{out}");
        assert!(out.contains("a-1"), "{out}");
        assert!(out.contains("b-2"), "{out}");
        assert!(out.contains("title of a-1"), "{out}");
        assert!(out.contains("2 beads"), "{out}");
        assert!(out.contains("3 commits"), "{out}");
        assert!(out.contains("1 authors"), "{out}");
        assert!(out.contains("1.5 commits/bead"), "{out}");
        assert!(out.contains("Showing all 2 beads with commits"), "{out}");
    }

    #[test]
    fn render_shows_the_confidence_filter_and_the_middle_pane_at_100_plus() {
        let theme = Theme::default();
        let mut s = sample();
        s.cycle_confidence();
        let out = draw(&mut s, &theme, 120, 30);
        assert!(out.contains("≥50% conf"), "{out}");
        assert!(
            out.contains("COMMITS"),
            "middle pane appears at 100+ columns"
        );
        assert!(out.contains("COMMIT DETAILS"), "{out}");
        assert!(
            out.contains("J/K:nav  y:copy  o:open  g:graph"),
            "the Go footer hint string"
        );
    }

    #[test]
    fn render_drops_the_middle_pane_below_100_columns() {
        let theme = Theme::default();
        let mut s = sample();
        let out = draw(&mut s, &theme, 90, 30);
        // The list panel survives; the separate COMMITS middle pane does not.
        assert!(out.contains("BEADS WITH HISTORY"), "{out}");
        assert!(
            !out.contains("COMMITS"),
            "no middle pane below 100 columns:\n{out}"
        );
        assert!(out.contains("COMMIT DETAILS"), "{out}");
    }

    #[test]
    fn render_shows_the_timeline_pane_at_150_plus() {
        let theme = Theme::default();
        let mut s = sample();
        let out = draw(&mut s, &theme, 180, 30);
        assert!(out.contains("TIMELINE: a-1"), "{out}");
    }

    #[test]
    fn render_git_mode_swaps_the_three_panels() {
        let theme = Theme::default();
        let mut s = sample();
        s.set_size(120, 30);
        s.toggle_mode();
        let out = draw(&mut s, &theme, 120, 30);
        assert!(out.contains("◉ Git"), "{out}");
        assert!(out.contains("COMMITS"), "{out}");
        assert!(out.contains("RELATED BEADS"), "{out}");
        assert!(
            out.contains("J/K:bead  y:copy  o:open  g:graph"),
            "git mode uses its own footer hint"
        );
    }

    #[test]
    fn render_shows_the_lifecycle_section_when_events_exist() {
        let theme = Theme::default();
        let mut s = sample();
        s.report = {
            let mut r = s.report().cloned().unwrap();
            r.histories.get_mut("a-1").unwrap().events = vec![
                event("a-1", "created", "2026-01-01T00:00:00Z", "Ada Lovelace"),
                event("a-1", "claimed", "2026-01-02T00:00:00Z", "Ada Lovelace"),
                event("a-1", "closed", "2026-01-03T00:00:00Z", "Ada Lovelace"),
            ];
            Some(r)
        };
        s.set_report(s.report().cloned().unwrap());
        let out = draw(&mut s, &theme, 120, 34);
        assert!(out.contains("LIFECYCLE (3)"), "{out}");
        // The reverse-chronological events and their icons.
        assert!(out.contains("👤"), "{out}");
        assert!(out.contains("🆕"), "{out}");
        // The compact list-row badge is a separate renderer (Go's
        // renderCompactEventBadge); it is clipped out of a 120-column row, so
        // assert it directly rather than through the panel.
        assert_eq!(render_compact_event_badge(0, &theme), "");
        assert_eq!(render_compact_event_badge(3, &theme), "⚡3");
    }

    #[test]
    fn render_uses_go_empty_state_copy() {
        let theme = Theme::default();
        let mut s = HistoryState::new();
        let out = draw(&mut s, &theme, 60, 10);
        assert!(out.contains("No history data loaded"), "{out}");
        assert!(out.contains("Press H to close"), "{out}");

        let mut s2 = HistoryState::from_report(report(vec![]));
        let out2 = draw(&mut s2, &theme, 60, 10);
        assert!(
            out2.contains("No beads with commit correlations found"),
            "{out2}"
        );
    }

    #[test]
    fn render_search_box_replaces_the_close_hint() {
        let theme = Theme::default();
        let mut s = sample();
        s.start_search();
        s.push_search_char('b');
        let out = draw(&mut s, &theme, 120, 20);
        assert!(out.contains("[all] b"), "{out}");
        assert!(out.contains("[Esc] cancel"), "{out}");
    }

    #[test]
    fn render_file_tree_panel_draws_the_files_header_and_rows() {
        let theme = Theme::default();
        let mut s = sample();
        s.build_file_tree();
        let backend = ratatui::backend::TestBackend::new(40, 12);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render_file_tree_panel(f, &s, &theme, f.area()))
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        let text: String = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("FILES"), "{text}");
        assert!(text.contains("crates"), "{text}");
    }
}
