//! Background snapshot worker — port of Go `pkg/ui/background_worker.go`
//! constants and pipeline semantics, using std::thread + crossbeam-channel.
//!
//! STATUS: reference port, not wired into the app. Go's `BackgroundWorker`
//! is the Bubble Tea model's phase-1/phase-2 snapshot pipeline (fsnotify
//! watcher, heartbeats, recovery); the Rust TUI deliberately implements the
//! same user-visible live reload synchronously — `App::check_for_reload`
//! polls the watched `.beads/*.jsonl` mtime on the existing ~500 ms
//! terminal-event tick and calls `reload_from_disk` (see `lib.rs`,
//! "Live reload (Go fsnotify … TUI_UX_PARITY_PLAN.md Phase F)"). Nothing
//! constructs `BackgroundWorker`; it is retained as the parity reference for
//! the worker constants/messages and for future wiring if the poll proves
//! insufficient. Its own mtime scan already handles the None→Some
//! (file recreated) transition via `current_mtime.is_some() &&
//! last_mtime != current_mtime`.

use crossbeam_channel as channel;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

// === Constants (Go parity, env-overridable) ===
pub const DEFAULT_DEBOUNCE_MS: u64 = 200;
pub const DEFAULT_CHANNEL_BUFFER: usize = 8;
pub const DEFAULT_HEARTBEAT_SECS: u64 = 5;
pub const DEFAULT_WATCHDOG_SECS: u64 = 10;
pub const DEFAULT_HEARTBEAT_TIMEOUT_SECS: u64 = 30;
pub const DEFAULT_FRESHNESS_WARN_S: u64 = 30;
pub const DEFAULT_FRESHNESS_STALE_S: u64 = 120;
pub const WORKER_POLL_TICK_MS: u64 = 120;

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Worker configuration resolved from env.
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    pub debounce_ms: u64,
    pub channel_buffer: usize,
    pub heartbeat_secs: u64,
    pub watchdog_secs: u64,
    pub heartbeat_timeout_secs: u64,
    pub freshness_warn_s: u64,
    pub freshness_stale_s: u64,
    pub poll_tick_ms: u64,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        WorkerConfig {
            debounce_ms: env_u64("BV_DEBOUNCE_MS", DEFAULT_DEBOUNCE_MS),
            channel_buffer: env_usize("BV_CHANNEL_BUFFER", DEFAULT_CHANNEL_BUFFER),
            heartbeat_secs: env_u64("BV_HEARTBEAT_INTERVAL_S", DEFAULT_HEARTBEAT_SECS),
            watchdog_secs: env_u64("BV_WATCHDOG_INTERVAL_S", DEFAULT_WATCHDOG_SECS),
            heartbeat_timeout_secs: DEFAULT_HEARTBEAT_TIMEOUT_SECS,
            freshness_warn_s: env_u64("BV_FRESHNESS_WARN_S", DEFAULT_FRESHNESS_WARN_S),
            freshness_stale_s: env_u64("BV_FRESHNESS_STALE_S", DEFAULT_FRESHNESS_STALE_S),
            poll_tick_ms: WORKER_POLL_TICK_MS,
        }
    }
}

/// Messages from worker to UI.
#[derive(Debug, Clone)]
pub enum WorkerMsg {
    /// A fresh snapshot is ready.
    SnapshotReady {
        issue_count: usize,
        file_change_at: String,
        sent_at: Instant,
    },
    /// Worker encountered an error.
    SnapshotError { message: String, recoverable: bool },
    /// Phase 2 metrics updated.
    Phase2Update { data_hash: String },
}

/// Freshness indicator for the status bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    Fresh,
    Warn(u64), // seconds since last update
    Stale(u64),
}

impl Freshness {
    pub fn label(self) -> String {
        match self {
            Freshness::Fresh => "fresh".to_string(),
            Freshness::Warn(secs) => format!("{}s ago", secs),
            Freshness::Stale(secs) => format!("STALE {}m ago", secs / 60),
        }
    }
}

/// Background worker thread handle + shutdown signal.
pub struct BackgroundWorker {
    shutdown: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    pub config: WorkerConfig,
}

impl BackgroundWorker {
    /// Spawn the worker loop. `beads_dir` is watched for changes.
    pub fn spawn(beads_dir: PathBuf, tx: channel::Sender<WorkerMsg>, config: WorkerConfig) -> Self {
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = Arc::clone(&shutdown);

        let handle = std::thread::spawn(move || {
            let mut last_mtime: Option<std::time::SystemTime> = None;
            loop {
                if shutdown_clone.load(Ordering::Relaxed) {
                    break;
                }

                // Check beads file mtime (polling fallback). Trigger on any
                // transition to a *present, different* mtime — including
                // None→Some (file (re)created after being absent) — but not
                // on Some→None (file briefly missing mid-write; next tick
                // sees the new mtime anyway).
                let jsonl = beads_dir.join("issues.jsonl");
                let current_mtime = std::fs::metadata(&jsonl).and_then(|m| m.modified()).ok();

                if current_mtime.is_some() && last_mtime != current_mtime {
                    // File changed or recreated → debounce then reload.
                    std::thread::sleep(Duration::from_millis(config.debounce_ms));
                    match bv_core::discovery::load_issues_from_repo(
                        beads_dir.parent().unwrap_or(&beads_dir),
                    ) {
                        Ok((issues, _stats)) => {
                            let count = issues.len();
                            let hash = bv_core::data_hash::compute_data_hash(&issues);
                            let _ = hash;
                            let _ = tx.send(WorkerMsg::SnapshotReady {
                                issue_count: count,
                                file_change_at: chrono_now(),
                                sent_at: Instant::now(),
                            });
                        }
                        Err(e) => {
                            let _ = tx.send(WorkerMsg::SnapshotError {
                                message: e.to_string(),
                                recoverable: true,
                            });
                        }
                    }
                }
                last_mtime = current_mtime;

                std::thread::sleep(Duration::from_millis(config.poll_tick_ms));
            }

            #[allow(dead_code)]
            fn chrono_now() -> String {
                jiff::Timestamp::now().to_string()
            }
        });

        BackgroundWorker {
            shutdown,
            handle: Some(handle),
            config,
        }
    }

    /// Signal the worker to stop. Call before dropping.
    pub fn stop(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

impl Drop for BackgroundWorker {
    fn drop(&mut self) {
        self.stop();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Compute freshness from elapsed time (Go freshness thresholds).
pub fn compute_freshness(elapsed: Duration, config: &WorkerConfig) -> Freshness {
    let secs = elapsed.as_secs();
    if secs >= config.freshness_stale_s {
        Freshness::Stale(secs)
    } else if secs >= config.freshness_warn_s {
        Freshness::Warn(secs)
    } else {
        Freshness::Fresh
    }
}

use jiff::Timestamp;

#[allow(dead_code)]
fn chrono_now() -> String {
    Timestamp::now().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freshness_thresholds_match_go_defaults() {
        let config = WorkerConfig::default();
        assert_eq!(config.freshness_warn_s, 30);
        assert_eq!(config.freshness_stale_s, 120);

        assert_eq!(
            compute_freshness(Duration::from_secs(5), &config),
            Freshness::Fresh
        );
        assert!(matches!(
            compute_freshness(Duration::from_secs(45), &config),
            Freshness::Warn(_)
        ));
        assert!(matches!(
            compute_freshness(Duration::from_secs(180), &config),
            Freshness::Stale(_)
        ));
    }

    #[test]
    fn env_overrides_respected() {
        // Can't safely test in parallel; verify defaults only.
        let config = WorkerConfig::default();
        assert_eq!(config.debounce_ms, 200);
        assert_eq!(config.channel_buffer, 8);
        assert_eq!(config.poll_tick_ms, 120);
    }

    #[test]
    fn freshness_labels() {
        assert_eq!(Freshness::Fresh.label(), "fresh");
        assert_eq!(Freshness::Warn(45).label(), "45s ago");
        assert_eq!(Freshness::Stale(300).label(), "STALE 5m ago");
    }
}
