//! Self-update for bvr — port of Go `pkg/updater` + `pkg/version`.
//!
//! Asset scheme follows `install.sh` / `install.ps1`:
//! `bvr-{VERSION}-{platform}.tar.gz` (.zip on Windows) where platform is
//! `linux-x86_64`, `linux-aarch64`, `macos-x86_64`, `macos-aarch64`,
//! `windows-x86_64`.

pub mod github;
pub mod prefs;
pub mod update;
pub mod version;

pub use github::{Asset, Release, UpdateInfo};
pub use prefs::{load_preferences, Preferences};
pub use update::{perform_rollback, perform_update, UpdateResult};
pub use version::{
    compare_versions, current_version, is_dev_version, is_newer_than_current, APP_VERSION,
    PORT_VERSION,
};

/// GitHub repo hosting bvr releases.
pub const REPO_OWNER: &str = "quangdang46";
/// GitHub repo hosting bvr releases.
///
/// Renamed from `beads_viewer_rust` at the request of @Dicklesworthstone, who
/// started their own Rust port of `beads_viewer` six months earlier. Left
/// stale here, the release-identity check rejected every release with
/// `release page URL path ... does not match`, so `--check-update` and
/// `--update` failed outright.
pub const REPO_NAME: &str = "beads_graph";
/// Binary name inside release archives and on disk.
pub const BINARY_NAME: &str = "bvr";

/// Platform suffix used in release asset names, e.g. `linux-x86_64`.
/// Returns `None` on unsupported platforms so callers can report a clean error.
pub fn platform_suffix() -> Option<&'static str> {
    let os = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        return None;
    };
    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        return None;
    };
    Some(match (os, arch) {
        ("linux", "x86_64") => "linux-x86_64",
        ("linux", "aarch64") => "linux-aarch64",
        ("macos", "x86_64") => "macos-x86_64",
        ("macos", "aarch64") => "macos-aarch64",
        ("windows", "x86_64") => "windows-x86_64",
        _ => return None,
    })
}

/// Archive extension for the current platform (`.zip` on Windows).
pub fn archive_extension() -> &'static str {
    if cfg!(target_os = "windows") {
        ".zip"
    } else {
        ".tar.gz"
    }
}

/// Expected asset name for `tag`, e.g. `bvr-v0.1.8-linux-x86_64.tar.gz`.
pub fn asset_name_for(tag: &str) -> Option<String> {
    let platform = platform_suffix()?;
    Some(format!(
        "{BINARY_NAME}-{tag}-{platform}{}",
        archive_extension()
    ))
}
