//! Keybinding registry — documents all key bindings per focus context.
//! Feeds shortcuts sidebar, help overlay, and --robot-help.
//! Runtime dispatch is in App::handle_key; this registry is for documentation only.
//! Port of Go `pkg/ui/keybindings.go`.

use std::collections::BTreeMap;

/// Focus context for key bindings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Focus {
    List,
    Detail,
    Board,
    Tree,
    Graph,
    Insights,
    Alerts,
    FlowMatrix,
    Attention,
    Sprint,
    History,
    /// Revision-diff mode (Go `focusTimeTravelInput` → `SnapshotDiff`);
    /// see TUI_UX_PARITY_PLAN.md Phase B.
    TimeTravel,
    /// Per-label health dashboard (Go `focusLabelDashboard`);
    /// see TUI_UX_PARITY_PLAN.md Phase C.
    LabelDashboard,
    Tutorial,
    Actionable,
    Search,
}

/// A single key binding document.
#[derive(Debug, Clone)]
pub struct KeyBinding {
    pub focus: Focus,
    pub key: String,
    pub desc: String,
    pub category: String,
}

/// Registry of all key bindings, indexed by focus.
pub struct KeyRegistry {
    bindings: BTreeMap<Focus, Vec<KeyBinding>>,
}

impl Default for KeyRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyRegistry {
    pub fn new() -> Self {
        KeyRegistry {
            bindings: BTreeMap::new(),
        }
    }

    pub fn register(&mut self, binding: KeyBinding) {
        self.bindings
            .entry(binding.focus)
            .or_default()
            .push(binding);
    }

    pub fn bindings_for(&self, focus: Focus) -> &[KeyBinding] {
        self.bindings.get(&focus).map_or(&[], |v| v.as_slice())
    }

    pub fn all_bindings(&self) -> &BTreeMap<Focus, Vec<KeyBinding>> {
        &self.bindings
    }
}

/// Build the default key registry with all documented bindings.
pub fn build_default_registry() -> KeyRegistry {
    let mut reg = KeyRegistry::new();
    let nav = "Navigation".to_string();
    let act = "Actions".to_string();
    let view = "Views".to_string();

    // List view bindings
    for (key, desc) in [
        ("j/↓", "Move down"),
        ("k/↑", "Move up"),
        ("ctrl+d", "Page down (half-screen)"),
        ("ctrl+u", "Page up (half-screen)"),
        ("enter", "Toggle detail pane"),
        ("tab", "Focus detail pane"),
        (
            "/",
            "Fuzzy search (nucleo-ranked, Enter accepts, Esc clears)",
        ),
        (
            "ctrl+s",
            "Semantic search (hybrid: text×preset×graph, Tab cycles preset)",
        ),
        ("a", "Show all issues"),
        ("o", "Show open issues"),
        ("c", "Show closed issues"),
        ("r", "Show ready issues"),
        ("s", "Cycle sort mode"),
        ("S", "Triage sort (priority)"),
        ("L", "Cycle label filter (one-key)"),
        ("l", "Open label picker (filterable popup)"),
        ("'", "Open recipe picker (6 built-in recipes)"),
        ("w", "Open workspace repo picker (workspace mode only)"),
        (
            "U",
            "Show update-available modal (if an update was detected)",
        ),
        (
            "p",
            "Toggle priority hints (\u{2191}/\u{2193} suggested-priority arrows in the list)",
        ),
    ] {
        reg.register(KeyBinding {
            focus: Focus::List,
            key: key.to_string(),
            desc: desc.to_string(),
            category: nav.clone(),
        });
    }
    // Search-input bindings (active while `/` or Ctrl+S mode captures keys).
    for (key, desc) in [
        ("type", "Narrow results (fuzzy / hybrid rank)"),
        ("backspace", "Delete query char and re-rank"),
        ("enter", "Accept (back to standard filter order)"),
        ("esc", "Clear query and exit search"),
        ("tab", "Cycle hybrid preset (semantic mode only)"),
    ] {
        reg.register(KeyBinding {
            focus: Focus::Search,
            key: key.to_string(),
            desc: desc.to_string(),
            category: nav.clone(),
        });
    }

    // View toggle bindings
    for (key, desc) in [
        ("b", "Toggle board view"),
        ("E", "Toggle tree view"),
        (
            "D",
            "Toggle dependency-tree left pane (flat list is the default)",
        ),
        ("G", "Toggle graph view"),
        ("h", "Toggle history view (bead↔commit correlation)"),
        ("i", "Toggle insights view"),
        ("f", "Toggle flow-matrix view"),
        ("A", "Toggle attention view"),
        ("!", "Toggle alerts view"),
        ("P", "Toggle sprint view"),
        ("F", "Toggle actionable view"),
        (
            "t / T",
            "Time-travel: t prompts for a revision, T diffs vs HEAD~5",
        ),
        ("`", "Tutorial (single tap) / context help (double tap)"),
        ("~", "Context help for current view"),
        (";", "Toggle sidebar"),
    ] {
        reg.register(KeyBinding {
            focus: Focus::List,
            key: key.to_string(),
            desc: desc.to_string(),
            category: view.clone(),
        });
    }

    // Action bindings
    for (key, desc) in [
        ("?", "Show help"),
        ("x", "Export markdown report"),
        ("C", "Copy issue to clipboard"),
        ("O", "Open issue in $EDITOR"),
        ("Ctrl+R", "Refresh from disk"),
        ("q", "Quit / close view"),
        (
            "esc",
            "Clear filters, then quit confirm (Esc/Y quits, other key cancels)",
        ),
        (
            "V",
            "Cass sessions for selected issue (j/k nav, y copies, V/Esc closes)",
        ),
    ] {
        reg.register(KeyBinding {
            focus: Focus::List,
            key: key.to_string(),
            desc: desc.to_string(),
            category: act.clone(),
        });
    }

    // Detail view bindings
    for (key, desc) in [
        ("j/↓", "Scroll down"),
        ("k/↑", "Scroll up"),
        ("tab", "Return to list"),
        ("esc", "Return to list"),
        ("C", "Copy full issue"),
        ("O", "Open in $EDITOR"),
    ] {
        reg.register(KeyBinding {
            focus: Focus::Detail,
            key: key.to_string(),
            desc: desc.to_string(),
            category: nav.clone(),
        });
    }

    // Sprint view bindings
    for (key, desc) in [
        ("j/↓", "Switch to next sprint"),
        ("k/↑", "Switch to previous sprint"),
        ("v", "Toggle velocity comparison"),
        ("P/esc", "Close sprint view"),
    ] {
        reg.register(KeyBinding {
            focus: Focus::Sprint,
            key: key.to_string(),
            desc: desc.to_string(),
            category: nav.clone(),
        });
    }

    // Graph view bindings
    for (key, desc) in [
        ("hjkl", "Navigate graph"),
        ("H", "Scroll left"),
        ("L", "Scroll right"),
        ("PgUp", "Scroll up"),
        ("PgDn", "Scroll down"),
    ] {
        reg.register(KeyBinding {
            focus: Focus::Graph,
            key: key.to_string(),
            desc: desc.to_string(),
            category: nav.clone(),
        });
    }

    // History view bindings (lazy-loaded on first `h` press — real git-log
    // correlation data, not a placeholder). The table mirrors Go's
    // `handleHistoryKeys` (pkg/ui/model.go:5483-5753) key for key: the view
    // is entered with `h` and left with `h`/`esc`, and every other binding
    // is scoped to it, so the runtime arms guard on `ViewMode::History`.
    for (key, desc) in [
        (
            "j/↓",
            "Move down in the focused pane (beads on the list, commits in the middle/detail pane)",
        ),
        (
            "k/↑",
            "Move up in the focused pane (timeline scroll on the timeline pane)",
        ),
        ("J", "Next commit (next related bead in git mode)"),
        ("K", "Previous commit (previous related bead in git mode)"),
        ("tab", "Cycle pane focus: list → timeline → middle → detail"),
        (
            "enter",
            "Jump to the selected bead in the main list and close",
        ),
        ("y", "Copy selected commit SHA to clipboard"),
        ("v", "Toggle bead/git mode"),
        ("c", "Cycle confidence threshold (0% / 50% / 75% / 90%)"),
        (
            "f/F",
            "Toggle the file tree (j/k navigate, enter/l expand or filter, h collapse)",
        ),
        ("t", "Toggle the timeline pane (bead mode, ≥100 columns)"),
        (
            "/",
            "Search commits, beads, authors… (enter keeps the filter, esc clears)",
        ),
        ("o", "Open the selected commit in the browser"),
        ("g", "Open the graph view focused on the selected bead"),
        ("h/esc", "Close (back to list)"),
    ] {
        reg.register(KeyBinding {
            focus: Focus::History,
            key: key.to_string(),
            desc: desc.to_string(),
            category: nav.clone(),
        });
    }

    // Time-Travel — revision diff vs a git ref (Go `focusTimeTravelInput`
    // → `SnapshotDiff`), backed by `bv_analysis::diff::diff_issues` over
    // `GitLoader::load_at` (shared with `--robot-diff`; see plan Q4).
    // Rows in the List that changed vs the ref carry a diff badge (Go
    // `DiffStatus.Badge()`: new / closed / ~ modified).
    //
    // NOTE: `t` is only the Time-Travel prompt *outside* the History view —
    // inside History it toggles the timeline pane (Go model.go:5680-5693),
    // and the runtime arms are ordered so that guard wins.
    for (key, desc) in [
        ("t", "Enter revision (prompt) and diff vs ref"),
        ("T", "Instant diff vs HEAD~5"),
        ("enter", "Submit revision (in prompt)"),
        ("esc", "Cancel prompt / back to list"),
        ("j/↓", "Next diff entry"),
        ("k/↑", "Previous diff entry"),
    ] {
        reg.register(KeyBinding {
            focus: Focus::TimeTravel,
            key: key.to_string(),
            desc: desc.to_string(),
            category: nav.clone(),
        });
    }
    // Label Dashboard — per-label health (Go `focusLabelDashboard`),
    // backed by `bv_analysis::label_health` (shared with
    // `--robot-label-health`); see TUI_UX_PARITY_PLAN.md Phase C.
    for (key, desc) in [
        ("[", "Toggle label dashboard"),
        ("j/↓", "Next label (worst health first)"),
        ("k/↑", "Previous label"),
        ("h", "Health-detail modal for selected label"),
        ("d", "Drilldown: issues for selected label"),
        ("enter", "List filtered by label (in drilldown)"),
        ("esc", "Close overlay / back to list"),
    ] {
        reg.register(KeyBinding {
            focus: Focus::LabelDashboard,
            key: key.to_string(),
            desc: desc.to_string(),
            category: nav.clone(),
        });
    }

    // Board — swimlane grouping with column nav (Go `board.go`).
    for (key, desc) in [
        ("j/↓", "Move down (shared list cursor)"),
        ("k/↑", "Move up (shared list cursor)"),
        ("h/l", "Previous / next column"),
        ("1-9", "Jump to column"),
        ("s", "Cycle grouping (Status → Priority → Type)"),
        ("enter", "Toggle detail pane"),
        ("b / esc", "Close (back to list)"),
    ] {
        reg.register(KeyBinding {
            focus: Focus::Board,
            key: key.to_string(),
            desc: desc.to_string(),
            category: nav.clone(),
        });
    }
    // Tree — Go's expand/collapse toggle key is not yet wired to a
    // dedicated Tree keybinding beyond the shared list cursor.
    for (key, desc) in [
        ("j/↓", "Move down (shared list cursor)"),
        ("k/↑", "Move up (shared list cursor)"),
        ("E / esc", "Close (back to list)"),
    ] {
        reg.register(KeyBinding {
            focus: Focus::Tree,
            key: key.to_string(),
            desc: desc.to_string(),
            category: nav.clone(),
        });
    }

    // Insights — 6-panel metric view; navigated read-only for now (Go's
    // panel-switching h/l and explanation/proof toggles are not ported).
    reg.register(KeyBinding {
        focus: Focus::Insights,
        key: "i / esc".to_string(),
        desc: "Close (back to list)".to_string(),
        category: nav.clone(),
    });

    // Alerts
    for (key, desc) in [
        ("j/↓", "Next alert"),
        ("k/↑", "Previous alert"),
        ("! / esc", "Close (back to list)"),
    ] {
        reg.register(KeyBinding {
            focus: Focus::Alerts,
            key: key.to_string(),
            desc: desc.to_string(),
            category: nav.clone(),
        });
    }

    // Flow-Matrix
    for (key, desc) in [
        ("j/↓", "Next label"),
        ("k/↑", "Previous label"),
        ("f / esc", "Close (back to list)"),
    ] {
        reg.register(KeyBinding {
            focus: Focus::FlowMatrix,
            key: key.to_string(),
            desc: desc.to_string(),
            category: nav.clone(),
        });
    }

    // Attention
    for (key, desc) in [
        ("j/↓", "Next label"),
        ("k/↑", "Previous label"),
        ("A / esc", "Close (back to list)"),
    ] {
        reg.register(KeyBinding {
            focus: Focus::Attention,
            key: key.to_string(),
            desc: desc.to_string(),
            category: nav.clone(),
        });
    }
    // Tutorial — real paged content (`tutorial.rs`), turned with j/k.
    for (key, desc) in [
        ("j/↓", "Next page"),
        ("k/↑", "Previous page"),
        ("` / esc", "Close tutorial"),
    ] {
        reg.register(KeyBinding {
            focus: Focus::Tutorial,
            key: key.to_string(),
            desc: desc.to_string(),
            category: nav.clone(),
        });
    }

    // Actionable — cursor over computed items, moved with j/k.
    for (key, desc) in [
        ("j/↓", "Next item"),
        ("k/↑", "Previous item"),
        ("F / esc", "Close (back to list)"),
    ] {
        reg.register(KeyBinding {
            focus: Focus::Actionable,
            key: key.to_string(),
            desc: desc.to_string(),
            category: nav.clone(),
        });
    }

    reg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_list_bindings() {
        let reg = build_default_registry();
        let list = reg.bindings_for(Focus::List);
        assert!(!list.is_empty());
        assert!(list.iter().any(|b| b.key == "j/↓"));
        assert!(list.iter().any(|b| b.key == "q"));
    }

    #[test]
    fn registry_has_all_focuses() {
        let reg = build_default_registry();
        assert!(!reg.bindings_for(Focus::List).is_empty());
        assert!(!reg.bindings_for(Focus::Detail).is_empty());
        assert!(!reg.bindings_for(Focus::Graph).is_empty());
    }

    /// Every Focus variant must have at least one registered binding —
    /// otherwise the dynamic `?` help overlay (lib.rs `render_overlays`)
    /// silently shows an empty popup for that view. Structural regression
    /// guard for TUI_UX_PARITY_PLAN.md G8 (registry/runtime drift): fails
    /// loudly if a new ViewMode/Focus is added without registering its
    /// bindings, instead of leaving `?` broken to be discovered by a user.
    #[test]
    fn every_focus_has_at_least_one_binding() {
        let reg = build_default_registry();
        for focus in [
            Focus::List,
            Focus::Detail,
            Focus::Board,
            Focus::Tree,
            Focus::Graph,
            Focus::Insights,
            Focus::Alerts,
            Focus::FlowMatrix,
            Focus::Attention,
            Focus::Sprint,
            Focus::History,
            Focus::TimeTravel,
            Focus::Tutorial,
            Focus::Actionable,
        ] {
            assert!(
                !reg.bindings_for(focus).is_empty(),
                "{focus:?} has no registered keybindings — the ? help overlay \
                 will render empty for this view"
            );
        }
    }

    /// Regression guard for the specific drift TUI_UX_PARITY_PLAN.md G8
    /// found: the registry said lowercase "g" toggled Graph while runtime
    /// (`lib.rs` `handle_key`) actually binds uppercase "G". Pin the
    /// corrected value so it can't silently drift back. (Lowercase `g` is
    /// unbound — Go has no manual agent-prompt key, which auto-shows once at
    /// startup instead.)
    #[test]
    fn graph_toggle_is_uppercase_g_matching_runtime() {
        let reg = build_default_registry();
        let list = reg.bindings_for(Focus::List);
        assert!(
            list.iter().any(|b| b.key == "G"),
            "registry should document the real runtime binding (uppercase G)"
        );
        assert!(
            !list.iter().any(|b| b.key == "g"),
            "lowercase g must stay unbound — no manual agent-prompt key"
        );
    }

    /// The History confidence ladder is Go's `{0, 0.5, 0.75, 0.9}`
    /// (history.go:894). The registry used to advertise the drifted
    /// `0% / 50% / 80%` that the old inlined arithmetic produced; pin the
    /// corrected text so it cannot drift back with the runtime.
    #[test]
    fn history_confidence_help_matches_the_go_ladder() {
        let reg = build_default_registry();
        let history = reg.bindings_for(Focus::History);
        let c = history
            .iter()
            .find(|b| b.key == "c")
            .expect("`c` must be registered for the History view");
        assert!(
            c.desc.contains("0% / 50% / 75% / 90%"),
            "help text drifted from the Go threshold ladder: {}",
            c.desc
        );
        assert!(
            !c.desc.contains("80%"),
            "the 0.8 step is Go's 0.75: {}",
            c.desc
        );
    }

    /// Go binds `tab`, `enter`, `o`, `g`, `f`/`F`, `t` and `/` inside
    /// `handleHistoryKeys`. Pin them so a future registry edit cannot quietly
    /// drop a key the runtime handles.
    #[test]
    fn history_view_registers_every_go_key() {
        let reg = build_default_registry();
        let history = reg.bindings_for(Focus::History);
        for key in [
            "j/↓", "k/↑", "J", "K", "tab", "enter", "y", "v", "c", "f/F", "t", "/", "o", "g",
            "h/esc",
        ] {
            assert!(
                history.iter().any(|b| b.key == key),
                "History is missing the `{key}` binding"
            );
        }
    }
}
