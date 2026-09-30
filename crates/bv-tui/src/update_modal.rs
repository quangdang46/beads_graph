//! Update modal — the self-update confirmation + progress dialog.
//! Port of Go `pkg/ui/update_modal.go`.
//!
//! Go drives this with a Bubble Tea `tea.Cmd` for the background install
//! (`PerformUpdateCmd`, `update_modal.go:101-142`) plus a 100 ms `tea.Tick`
//! for the spinner (`:54-58`). bv-tui's event loop in `lib.rs` is a plain
//! synchronous key dispatch with no command bus, so the install runs on a
//! `std::thread` (AGENTS.md forbids tokio) that reports through a
//! `crossbeam_channel`, and [`UpdateModal::poll`] drains that channel and
//! advances the elapsed clock in place of the tick.
//!
//! Wired into `App` as `App::update_modal` (Go's `m.updateModal`,
//! `model.go:957`). The parent drives it from three places, mirroring Go:
//! [`UpdateModal::handle_key`] + [`UpdateModal::close_policy`] from the key
//! dispatch (`model.go:3788-3821`), [`UpdateModal::render`] from the overlay
//! pass (`model.go:6303`), and [`UpdateModal::poll`] from the event-loop tick
//! in place of Go's `UpdateProgressMsg` / `UpdateCompleteMsg` arms
//! (`model.go:2304-2333`).

use std::time::{Duration, Instant};

use bv_update::github::Release;
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Padding, Paragraph},
    Frame,
};

/// The current state of the update process.
/// Port of Go `UpdateState` (`update_modal.go:16-27`) — the discriminants
/// are in the same order so the `as i32` values line up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateState {
    Idle,
    Confirm,
    Downloading,
    Verifying,
    Installing,
    Success,
    Error,
}

/// Progress during download. Port of Go `UpdateProgress`
/// (`update_modal.go:30-35`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdateProgress {
    pub bytes_downloaded: i64,
    pub total_bytes: i64,
    /// One of `"downloading"`, `"verifying"`, `"installing"`.
    pub stage: String,
}

/// Terminal outcome of the background install. Port of Go
/// `UpdateCompleteMsg` (`update_modal.go:37-48`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdateComplete {
    pub success: bool,
    pub message: String,
    pub new_version: String,
    pub backup_path: String,
    /// Go sets this when the binary is not writable so the modal can suggest
    /// `sudo`. Rust reports the same condition through
    /// [`bv_update::update::UpdateError::NoPermission`].
    pub require_root: bool,
}

/// What the background worker reports back. Port of Go's `UpdateProgressMsg`
/// / `UpdateCompleteMsg` union (`update_modal.go:37-48`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateEvent {
    Progress(UpdateProgress),
    Complete(UpdateComplete),
}

/// Reject a release whose tag no longer matches what the user confirmed.
/// Port of Go `validateConfirmedRelease` (`update_modal.go:91-99`).
pub fn validate_confirmed_release(
    release: Option<&Release>,
    expected_version: &str,
) -> Result<(), String> {
    let Some(release) = release else {
        return Err("release metadata is nil".to_string());
    };
    if release.tag_name.trim() != expected_version.trim() {
        return Err(format!(
            "latest release changed from {} to {}; reopen the update dialog to review it",
            expected_version, release.tag_name
        ));
    }
    Ok(())
}

/// The background worker. Port of Go `PerformUpdateCmd`
/// (`update_modal.go:101-142`): fetch the latest release, re-validate the tag
/// against the version the user confirmed, then install.
pub struct UpdateWorker {
    rx: crossbeam_channel::Receiver<UpdateEvent>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for UpdateWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpdateWorker").finish_non_exhaustive()
    }
}

impl Drop for UpdateWorker {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            // The install writes the running binary; never cancel it
            // mid-flight just because the modal closed.
            let _ = h.join();
        }
    }
}

/// Spawn the install thread. `expected_version` is the tag the user
/// confirmed, so a release published between confirm and execute is refused.
pub fn spawn_perform_update(expected_version: &str) -> UpdateWorker {
    let expected = expected_version.to_string();
    let (tx, rx) = crossbeam_channel::unbounded();
    let handle = std::thread::Builder::new()
        .name("bvr-update".to_string())
        .spawn(move || {
            let release = match bv_update::github::get_latest_release() {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send(UpdateEvent::Complete(UpdateComplete {
                        success: false,
                        message: format!("Failed to fetch release info: {e}"),
                        ..Default::default()
                    }));
                    return;
                }
            };
            if let Err(msg) = validate_confirmed_release(Some(&release), &expected) {
                let _ = tx.send(UpdateEvent::Complete(UpdateComplete {
                    success: false,
                    message: msg,
                    ..Default::default()
                }));
                return;
            }

            // The updater reports human-readable steps
            // ("Downloading v…", "Verifying checksum…", "Installing new
            // version…"). Map them onto Go's stage names so the modal can show
            // the downloading/verifying/installing states it defines.
            let progress_tx = tx.clone();
            let on_progress = move |line: &str| {
                let stage = if line.starts_with("Verifying") {
                    "verifying"
                } else if line.starts_with("Installing") {
                    "installing"
                } else {
                    "downloading"
                };
                let _ = progress_tx.send(UpdateEvent::Progress(UpdateProgress {
                    bytes_downloaded: 0,
                    total_bytes: 0,
                    stage: stage.to_string(),
                }));
            };

            match bv_update::update::perform_update(&release, &on_progress) {
                Ok(result) => {
                    let _ = tx.send(UpdateEvent::Complete(UpdateComplete {
                        success: true,
                        message: result.message,
                        new_version: result.new_version,
                        backup_path: result.backup_path.unwrap_or_default(),
                        require_root: false,
                    }));
                }
                Err(err) => {
                    // Go: `result.RequireRoot` swaps in a sudo hint
                    // (`updater.go:1312-1314`).
                    let require_root =
                        matches!(err, bv_update::update::UpdateError::NoPermission(_));
                    let message = if require_root {
                        "Update requires elevated permissions. Run: sudo bv --update".to_string()
                    } else {
                        format!("Update failed: {err}")
                    };
                    let _ = tx.send(UpdateEvent::Complete(UpdateComplete {
                        success: false,
                        message,
                        require_root,
                        ..Default::default()
                    }));
                }
            }
        })
        .ok();
    UpdateWorker { rx, handle }
}

/// What the caller should do after [`UpdateModal::handle_key`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateModalAction {
    /// Nothing happened (an unhandled key, or a key that does not close).
    None,
    /// The install thread was started.
    Started,
    /// The parent should set `showUpdateModal = false` and refocus the list.
    Dismiss,
}

/// The update modal. Port of Go `UpdateModal` (`update_modal.go:61-75`).
pub struct UpdateModal {
    pub current_version: String,
    pub new_version: String,
    pub release_url: String,
    pub state: UpdateState,
    pub progress: UpdateProgress,
    pub error_message: String,
    pub success_message: String,
    pub backup_path: String,
    /// `UpdateCompleteMsg.NewVersion` (`update_modal.go:37-48`) — the tag the
    /// install actually wrote. Go compares it against `Model.updateTag` to
    /// decide whether a successful update retires the "update available"
    /// notice (`model.go:2309-2314`); an empty string until the install
    /// completes.
    pub completed_version: String,
    pub width: u16,
    pub height: u16,
    /// Spinner/elapsed clock origin, set when the install starts.
    pub start_time: Instant,
    /// 0 = Update, 1 = Cancel. Go's `confirmFocus`.
    pub confirm_focus: u8,
    worker: Option<UpdateWorker>,
}

impl std::fmt::Debug for UpdateModal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpdateModal")
            .field("state", &self.state)
            .field("new_version", &self.new_version)
            .field("confirm_focus", &self.confirm_focus)
            .finish_non_exhaustive()
    }
}

impl UpdateModal {
    /// Go `NewUpdateModal` (`update_modal.go:78-89`): opens in the confirm
    /// state with the Update button focused.
    pub fn new(current_version: &str, new_version: &str, release_url: &str) -> Self {
        UpdateModal {
            current_version: current_version.to_string(),
            new_version: new_version.to_string(),
            release_url: release_url.to_string(),
            state: UpdateState::Confirm,
            progress: UpdateProgress::default(),
            error_message: String::new(),
            success_message: String::new(),
            backup_path: String::new(),
            completed_version: String::new(),
            width: 60,
            height: 20,
            start_time: Instant::now(),
            confirm_focus: 0,
            worker: None,
        }
    }

    /// Go `SetSize` (`update_modal.go:382-392`): clamp to [50, 70].
    pub fn set_size(&mut self, width: u16, height: u16) {
        self.width = (width as i32 - 10).clamp(50, 70) as u16;
        self.height = height;
    }

    /// Go `IsConfirming` (`update_modal.go:395-397`).
    pub fn is_confirming(&self) -> bool {
        self.state == UpdateState::Confirm
    }

    /// Go `IsCancelled` (`update_modal.go:400-402`).
    pub fn is_cancelled(&self) -> bool {
        self.state == UpdateState::Confirm && self.confirm_focus == 1
    }

    /// Go `IsComplete` (`update_modal.go:405-407`).
    pub fn is_complete(&self) -> bool {
        self.state == UpdateState::Success || self.state == UpdateState::Error
    }

    /// Go `IsInProgress` (`update_modal.go:410-414`).
    pub fn is_in_progress(&self) -> bool {
        matches!(
            self.state,
            UpdateState::Downloading | UpdateState::Verifying | UpdateState::Installing
        )
    }

    /// Port of Go's parent-side close policy (`model.go:3796-3819`): the
    /// parent, not the modal, decides when a key closes the overlay.
    ///
    /// * `esc` / `q` — always close, unless an install is in flight.
    /// * `enter` — close when complete, or when confirming on Cancel.
    /// * `n` / `N` — close while confirming.
    pub fn close_policy(&self, key: &ratatui::crossterm::event::KeyCode) -> bool {
        use ratatui::crossterm::event::KeyCode;
        match key {
            KeyCode::Esc | KeyCode::Char('q') => !self.is_in_progress(),
            KeyCode::Enter => self.is_complete() || (self.is_confirming() && self.is_cancelled()),
            KeyCode::Char('n') | KeyCode::Char('N') => self.is_confirming(),
            _ => false,
        }
    }

    /// Port of Go `UpdateModal.Update`'s `tea.KeyMsg` arm
    /// (`update_modal.go:147-183`).
    pub fn handle_key(&mut self, key: ratatui::crossterm::event::KeyCode) -> UpdateModalAction {
        use ratatui::crossterm::event::KeyCode;
        match self.state {
            UpdateState::Confirm => match key {
                KeyCode::Left | KeyCode::Char('h') => {
                    self.confirm_focus = 0;
                    UpdateModalAction::None
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    self.confirm_focus = 1;
                    UpdateModalAction::None
                }
                KeyCode::Tab => {
                    self.confirm_focus = (self.confirm_focus + 1) % 2;
                    UpdateModalAction::None
                }
                KeyCode::Enter => {
                    if self.confirm_focus == 0 {
                        self.start_update();
                        UpdateModalAction::Started
                    } else {
                        // Cancel — handled by the parent.
                        UpdateModalAction::Dismiss
                    }
                }
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.start_update();
                    UpdateModalAction::Started
                }
                KeyCode::Char('n') | KeyCode::Char('N') => UpdateModalAction::Dismiss,
                _ => UpdateModalAction::None,
            },
            // Success/Error: the parent dismisses on enter/esc/q
            // (`model.go:3796-3817`), which close_policy covers.
            _ => UpdateModalAction::None,
        }
    }

    /// Go's `m.state = UpdateStateDownloading; m.startTime = time.Now()` plus
    /// `tea.Batch(PerformUpdateCmd(...), updateTickCmd(...))`
    /// (`update_modal.go:159-170`).
    fn start_update(&mut self) {
        self.state = UpdateState::Downloading;
        self.start_time = Instant::now();
        self.worker = Some(spawn_perform_update(&self.new_version));
    }

    /// Drain worker messages and advance the state. This is the stand-in for
    /// Go's `updateTickCmd` (`update_modal.go:185-188`) and the
    /// `UpdateProgressMsg` / `UpdateCompleteMsg` arms (`:190-210`).
    /// Returns `true` when something changed and the modal needs a redraw.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        let Some(worker) = &self.worker else {
            return changed;
        };
        while let Ok(event) = worker.rx.try_recv() {
            changed = true;
            match event {
                UpdateEvent::Progress(p) => {
                    self.progress = p;
                    match self.progress.stage.as_str() {
                        "downloading" => self.state = UpdateState::Downloading,
                        "verifying" => self.state = UpdateState::Verifying,
                        "installing" => self.state = UpdateState::Installing,
                        _ => {}
                    }
                }
                UpdateEvent::Complete(c) => {
                    if c.success {
                        self.state = UpdateState::Success;
                        self.success_message = c.message;
                        self.backup_path = c.backup_path;
                        self.completed_version = c.new_version;
                    } else {
                        self.state = UpdateState::Error;
                        self.error_message = c.message;
                    }
                }
            }
        }
        changed
    }

    /// Elapsed time since the install started, as Go's
    /// `time.Since(m.startTime).Round(time.Second)`.
    fn elapsed(&self) -> Duration {
        self.start_time.elapsed()
    }

    /// Go `renderSpinner` (`update_modal.go:355-359`).
    pub fn render_spinner(&self) -> &'static str {
        const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let idx = (self.elapsed().as_millis() as usize / 100) % FRAMES.len();
        FRAMES[idx]
    }

    /// Go `renderProgressBar` (`update_modal.go:362-379`).
    pub fn render_progress_bar(&self) -> String {
        if self.progress.total_bytes <= 0 {
            // Indeterminate progress.
            return "[                    ]".to_string();
        }
        // Go: `if percent < 0 { 0 } else if percent > 1 { 1 }` (update_modal.go:
        // 369-373) — clamp is equivalent for every reachable input, since
        // total_bytes > 0 is the only way past the guard above.
        let percent = (self.progress.bytes_downloaded as f64 / self.progress.total_bytes as f64)
            .clamp(0.0, 1.0);
        const WIDTH: usize = 20;
        let filled = (percent * WIDTH as f64) as usize;
        let bar = format!("{}{}", "█".repeat(filled), "░".repeat(WIDTH - filled));
        format!("[{bar}] {:.0}%", percent * 100.0)
    }

    /// The modal body, mirroring Go's `View` switch
    /// (`update_modal.go:267-349`) minus the lipgloss styling.
    fn body_lines(&self) -> Vec<Line<'static>> {
        let header = Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD);
        let new_version_style = Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD);
        let subtext = Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC);
        let success = Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD);
        let error = Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);

        let mut b: Vec<Line<'static>> = Vec::new();
        match self.state {
            UpdateState::Confirm => {
                b.push(Line::from(Span::styled("Update Available", header)));
                b.push(Line::from(""));
                b.push(Line::from(vec![
                    Span::raw("Current version: "),
                    Span::styled(
                        self.current_version.clone(),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
                b.push(Line::from(vec![
                    Span::raw("New version:     "),
                    Span::styled(self.new_version.clone(), new_version_style),
                ]));
                b.push(Line::from(""));
                b.push(Line::from("Would you like to update now?"));
                b.push(Line::from(""));
                b.push(self.button_line());
                b.push(Line::from(""));
                b.push(Line::from(Span::styled(
                    "[Y] Update   [N] Cancel   [Enter] Select",
                    subtext,
                )));
            }
            UpdateState::Downloading => {
                b.push(Line::from(Span::styled("Updating...", header)));
                b.push(Line::from(""));
                b.push(Line::from(vec![
                    Span::raw(self.render_spinner().to_string()),
                    Span::raw(" Applying "),
                    Span::styled(self.new_version.clone(), new_version_style),
                    Span::raw("..."),
                ]));
                b.push(Line::from(""));
                if self.progress.total_bytes > 0 {
                    b.push(Line::from(self.render_progress_bar()));
                    b.push(Line::from(""));
                }
                b.push(Line::from(Span::styled(
                    format!("Elapsed: {}", format_elapsed(self.elapsed())),
                    subtext,
                )));
            }
            UpdateState::Verifying => {
                b.push(Line::from(Span::styled("Updating...", header)));
                b.push(Line::from(""));
                b.push(Line::from(vec![
                    Span::raw(self.render_spinner().to_string()),
                    Span::raw(" Verifying checksum..."),
                ]));
            }
            UpdateState::Installing => {
                b.push(Line::from(Span::styled("Updating...", header)));
                b.push(Line::from(""));
                b.push(Line::from(vec![
                    Span::raw(self.render_spinner().to_string()),
                    Span::raw(" Installing new version..."),
                ]));
            }
            UpdateState::Success => {
                b.push(Line::from(Span::styled("Update Complete!", success)));
                b.push(Line::from(""));
                b.push(Line::from(self.success_message.clone()));
                b.push(Line::from(""));
                if !self.backup_path.is_empty() {
                    b.push(Line::from(Span::styled(
                        format!("Backup: {}", self.backup_path),
                        subtext,
                    )));
                    b.push(Line::from(Span::styled(
                        "Run 'bv --rollback' to restore if needed",
                        subtext,
                    )));
                    b.push(Line::from(""));
                }
                b.push(Line::from(Span::styled(
                    "Restart bv to use the new version.",
                    success,
                )));
                b.push(Line::from(""));
                b.push(Line::from(Span::styled("[Enter] Close", subtext)));
            }
            UpdateState::Error => {
                b.push(Line::from(Span::styled("Update Failed", error)));
                b.push(Line::from(""));
                b.push(Line::from(self.error_message.clone()));
                b.push(Line::from(""));
                b.push(Line::from(Span::styled("[Enter] Close", subtext)));
            }
            UpdateState::Idle => {
                b.push(Line::from(""));
            }
        }
        b
    }

    /// Go's two bordered buttons (`update_modal.go:283-294`). Ratatui draws
    /// one line at a time, so the unselected button keeps its `NormalBorder`
    /// edges as `(` / `)` rather than the three-line lipgloss box; the
    /// selected one is filled with `Primary` exactly as Go's
    /// `selectedButtonStyle` does.
    fn button_line(&self) -> Line<'static> {
        let selected = Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD);
        let unselected = Style::default().fg(Color::White);
        let render = |label: &'static str, focused: bool| -> Span<'static> {
            if focused {
                Span::styled(format!(" {label} "), selected)
            } else {
                Span::styled(format!("( {label} )"), unselected)
            }
        };
        Line::from(vec![
            Span::raw("    "),
            render("Update", self.confirm_focus == 0),
            Span::raw("  "),
            render("Cancel", self.confirm_focus == 1),
        ])
    }

    /// Go `CenterModal` (`update_modal.go:417-444`), expressed as a rect.
    /// Go pads top/left by `(term - modal)/2` clamped at 0.
    pub fn modal_area(&self, term_width: u16, term_height: u16) -> Rect {
        // `Ord::clamp` asserts `min <= max`, so `clamp(1, 0)` panics — and it
        // panics in release too, on its own account. The `.max(1)` keeps the
        // bound valid when the terminal reports a zero dimension (an 80x1
        // window, or a SIGWINCH landing between `handle_resize` and the next
        // draw). The rest of the file uses `saturating_sub` throughout, which
        // is what made these two stand out.
        let w = self.width.clamp(1, term_width.max(1));
        let lines = self.body_lines().len() as u32 + 4; // padding(1,2) + borders
        let h = (lines as u16).clamp(1, term_height.max(1));
        Rect {
            x: (term_width.saturating_sub(w)) / 2,
            y: (term_height.saturating_sub(h)) / 2,
            width: w,
            height: h,
        }
    }

    /// Go `View` (`update_modal.go:216-352`).
    pub fn render(&self, f: &mut Frame, area: Rect) {
        let popup = self.modal_area(area.width, area.height);
        f.render_widget(Clear, popup);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .padding(Padding::new(2, 2, 1, 1));
        f.render_widget(Paragraph::new(self.body_lines()).block(block), popup);
    }
}

/// Go's `time.Duration.String()` for whole seconds, e.g. `1.5s` -> `2s`.
/// `Round(time.Second)` then `String()` drops the fractional part and the
/// `m` suffix for values under a minute.
fn format_elapsed(d: Duration) -> String {
    let secs = d.as_secs_f64().round() as u64;
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else {
        format!("{}h{}m{}s", secs / 3600, (secs % 3600) / 60, secs % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::KeyCode;

    fn modal() -> UpdateModal {
        UpdateModal::new("v1.0.0", "v1.2.3", "")
    }

    // -- Go pkg/ui/update_modal_test.go ------------------------------------

    #[test]
    fn test_validate_confirmed_release() {
        let ok = Release {
            tag_name: "v1.2.3".into(),
            html_url: String::new(),
            draft: false,
            prerelease: false,
            assets: vec![],
        };
        assert!(validate_confirmed_release(Some(&ok), "v1.2.3").is_ok());
        // Changed release must be refused without renewed confirmation.
        let moved = Release {
            tag_name: "v1.2.4".into(),
            html_url: String::new(),
            draft: false,
            prerelease: false,
            assets: vec![],
        };
        let err = validate_confirmed_release(Some(&moved), "v1.2.3").unwrap_err();
        assert_eq!(
            err,
            "latest release changed from v1.2.3 to v1.2.4; reopen the update dialog to review it"
        );
        assert_eq!(
            validate_confirmed_release(None, "v1.2.3").unwrap_err(),
            "release metadata is nil"
        );
    }

    #[test]
    fn test_is_confirming() {
        let m = modal();
        assert!(m.is_confirming());
        assert!(!m.is_cancelled());
        let mut m = m;
        m.state = UpdateState::Downloading;
        assert!(!m.is_confirming());
    }

    #[test]
    fn test_is_cancelled() {
        let mut m = modal();
        assert!(!m.is_cancelled());
        m.confirm_focus = 1;
        assert!(m.is_cancelled());
    }

    #[test]
    fn test_is_in_progress_and_complete() {
        let mut m = modal();
        assert!(!m.is_in_progress() && !m.is_complete());
        m.state = UpdateState::Verifying;
        assert!(m.is_in_progress());
        m.state = UpdateState::Installing;
        assert!(m.is_in_progress());
        m.state = UpdateState::Success;
        assert!(m.is_complete() && !m.is_in_progress());
        m.state = UpdateState::Error;
        assert!(m.is_complete());
    }

    #[test]
    fn left_and_right_move_confirm_focus() {
        let mut m = modal();
        assert_eq!(m.handle_key(KeyCode::Char('l')), UpdateModalAction::None);
        assert_eq!(m.confirm_focus, 1);
        assert_eq!(m.handle_key(KeyCode::Char('h')), UpdateModalAction::None);
        assert_eq!(m.confirm_focus, 0);
        m.handle_key(KeyCode::Tab);
        assert_eq!(m.confirm_focus, 1);
        m.handle_key(KeyCode::Tab);
        assert_eq!(m.confirm_focus, 0);
    }

    #[test]
    fn enter_on_cancel_dismisses_without_starting() {
        let mut m = modal();
        m.confirm_focus = 1;
        assert_eq!(m.handle_key(KeyCode::Enter), UpdateModalAction::Dismiss);
        assert_eq!(m.state, UpdateState::Confirm);
        assert!(m.worker.is_none());
    }

    #[test]
    fn n_and_n_dismiss_while_confirming() {
        let mut m = modal();
        assert_eq!(m.handle_key(KeyCode::Char('n')), UpdateModalAction::Dismiss);
        let mut m = modal();
        assert_eq!(m.handle_key(KeyCode::Char('N')), UpdateModalAction::Dismiss);
    }

    #[test]
    fn close_policy_matches_go() {
        let mut m = modal();
        // esc/q always close while confirming.
        assert!(m.close_policy(&KeyCode::Esc));
        assert!(m.close_policy(&KeyCode::Char('q')));
        // Enter does NOT close while confirming on Update.
        assert!(!m.close_policy(&KeyCode::Enter));
        // Enter closes while confirming on Cancel.
        m.confirm_focus = 1;
        assert!(m.close_policy(&KeyCode::Enter));
        // n/N close while confirming.
        assert!(m.close_policy(&KeyCode::Char('n')));
        // An install in flight blocks the escape hatch.
        let mut m = modal();
        m.state = UpdateState::Downloading;
        assert!(!m.close_policy(&KeyCode::Esc));
        // Enter closes once complete.
        m.state = UpdateState::Success;
        assert!(m.close_policy(&KeyCode::Enter));
    }

    #[test]
    fn test_set_size_clamps_width_to_50_70() {
        let mut m = modal();
        m.set_size(30, 24);
        assert_eq!(m.width, 50);
        m.set_size(200, 24);
        assert_eq!(m.width, 70);
        m.set_size(60, 24);
        assert_eq!(m.width, 50);
    }

    #[test]
    fn test_render_progress_bar() {
        let mut m = modal();
        // Indeterminate when TotalBytes <= 0.
        assert_eq!(m.render_progress_bar(), "[                    ]");
        m.progress = UpdateProgress {
            bytes_downloaded: 50,
            total_bytes: 100,
            stage: "downloading".into(),
        };
        let bar = format!("{}{}", "█".repeat(10), "░".repeat(10));
        assert_eq!(m.render_progress_bar(), format!("[{bar}] 50%"));
    }

    #[test]
    fn test_spinner_cycles_frames() {
        let m = modal();
        // Any frame is one of the ten Go uses.
        const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        assert!(FRAMES.contains(&m.render_spinner()));
    }

    #[test]
    fn test_elapsed_rounds_to_seconds() {
        assert_eq!(format_elapsed(Duration::from_millis(1_400)), "1s");
        assert_eq!(format_elapsed(Duration::from_millis(1_600)), "2s");
        assert_eq!(format_elapsed(Duration::from_secs(75)), "1m15s");
    }

    #[test]
    fn test_modal_area_is_centered() {
        let m = modal();
        let area = m.modal_area(120, 40);
        assert!(area.x > 0 && area.y > 0);
        assert!(area.x + area.width <= 120);
        assert!(area.y + area.height <= 40);
        // Never larger than the terminal.
        let tiny = m.modal_area(10, 5);
        assert!(tiny.width <= 10 && tiny.height <= 5);
    }

    /// A terminal can report a zero dimension — an 80x1 window, or a SIGWINCH
    /// landing between `handle_resize` and the next draw — and `render`
    /// forwards `area.width`/`area.height` here unguarded. `Ord::clamp`
    /// asserts `min <= max`, so the old `clamp(1, term_width)` took the
    /// process down on `clamp(1, 0)`, in release as well as debug.
    #[test]
    fn modal_area_survives_a_zero_dimension() {
        let m = modal();
        for (w, h) in [(0u16, 40u16), (120, 0), (0, 0), (1, 1), (1, 0), (0, 1)] {
            let area = m.modal_area(w, h);
            assert!(
                area.width >= 1 && area.height >= 1,
                "{w}x{h} produced a degenerate modal {area:?}"
            );
            assert!(
                u32::from(area.x) + u32::from(area.width) <= u32::from(w.max(1)),
                "{w}x{h} overflowed horizontally: {area:?}"
            );
        }
    }
}
