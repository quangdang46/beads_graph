//! Update-policy preferences — port of Go `pkg/updater` lines 40-239:
//! `envTruthy`, `savedConfigDisabled`, `UserConfigDir`, `loadUserUpdateConfig`,
//! `LoadPreferences`, `ambientGitHubToken`, `githubToken`, `isGitHubHost`
//! (updater.go:64-239).
//!
//! Public release metadata needs no credential, so the ambient
//! `GITHUB_TOKEN` / `GH_TOKEN` is never sent unless the user opts in with
//! `BV_UPDATE_USE_TOKEN=1` or `updates: {use_token: true}` in
//! `config.yaml` (Go updater.go:203-220).

use std::path::PathBuf;

/// Disables the TUI's automatic release check when set to a truthy value
/// (Go `EnvNoUpdateCheck`, updater.go:46).
pub const ENV_NO_UPDATE_CHECK: &str = "BV_NO_UPDATE_CHECK";
/// Opts in to sending the ambient `GITHUB_TOKEN` / `GH_TOKEN` with GitHub
/// requests (Go `EnvUseToken`, updater.go:49).
pub const ENV_USE_TOKEN: &str = "BV_UPDATE_USE_TOKEN";
/// Project-wide switch that makes bv ignore every file under the user config
/// directory, read and write (Go `envNoSavedConfig`, updater.go:52).
pub const ENV_NO_SAVED_CONFIG: &str = "BV_NO_SAVED_CONFIG";

/// Name of the user config file inside the config dir (Go `userConfigFileName`).
const USER_CONFIG_FILE_NAME: &str = "config.yaml";

/// The resolved update policy: defaults, then `config.yaml` `updates:` keys,
/// then environment overrides (Go `Preferences`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preferences {
    /// Controls the TUI's background release check.
    pub check_on_startup: bool,
    /// Allows `GITHUB_TOKEN` / `GH_TOKEN` to be attached to requests that
    /// target GitHub hosts.
    pub use_ambient_token: bool,
}

impl Default for Preferences {
    /// Go updater.go:141 — check on startup, never send the ambient token.
    fn default() -> Self {
        Self {
            check_on_startup: true,
            use_ambient_token: false,
        }
    }
}

/// The subset of `config.yaml` the updater understands:
///
/// ```yaml
/// updates:
///   check: false      # skip the startup release check
///   use_token: true   # send the ambient GITHUB_TOKEN / GH_TOKEN
/// ```
#[derive(Debug, Default, serde::Deserialize)]
struct UserUpdateConfig {
    #[serde(default)]
    updates: UpdatesSection,
}

#[derive(Debug, Default, serde::Deserialize)]
struct UpdatesSection {
    #[serde(default)]
    check: Option<bool>,
    #[serde(default)]
    use_token: Option<bool>,
}

/// Go `envTruthy` (updater.go:87-93): any non-empty value other than an
/// explicit `0` / `false` / `no` / `off` is enabled, so `=1` and `=true` both
/// work while `=0` re-enables the default.
pub fn env_truthy(value: &str) -> bool {
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "" | "0" | "false" | "no" | "off"
    )
}

/// Go `savedConfigDisabled` (updater.go:95-97) — presence, not truthiness.
fn saved_config_disabled() -> bool {
    std::env::var(ENV_NO_SAVED_CONFIG).is_ok_and(|v| !v.is_empty())
}

/// Go `UserConfigDir` (updater.go:99-113): `$XDG_CONFIG_HOME/bv` when
/// `XDG_CONFIG_HOME` is set, otherwise `~/.config/bv`.
pub fn user_config_dir() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .map(|v| v.trim().to_string())
    {
        if !xdg.is_empty() {
            return Some(PathBuf::from(xdg).join("bv"));
        }
    }
    let home = match std::env::var("HOME") {
        Ok(h) if !h.trim().is_empty() => h,
        _ => std::env::var("USERPROFILE")
            .ok()
            .filter(|h| !h.is_empty())?,
    };
    Some(PathBuf::from(home).join(".config").join("bv"))
}

/// Go `loadUserUpdateConfig` (updater.go:115-135): a missing or unreadable
/// file, a YAML parse failure, or `BV_NO_SAVED_CONFIG` yields the zero value
/// so callers fall back to defaults.
fn load_user_update_config() -> UserUpdateConfig {
    if saved_config_disabled() {
        return UserUpdateConfig::default();
    }
    let Some(dir) = user_config_dir() else {
        return UserUpdateConfig::default();
    };
    let Ok(data) = std::fs::read_to_string(dir.join(USER_CONFIG_FILE_NAME)) else {
        return UserUpdateConfig::default();
    };
    serde_yaml_ng::from_str(&data).unwrap_or_default()
}

/// Go `LoadPreferences` (updater.go:137-156): defaults, then `config.yaml`,
/// then the environment — which wins over both.
pub fn load_preferences() -> Preferences {
    let mut prefs = Preferences::default();
    let cfg = load_user_update_config();
    if let Some(check) = cfg.updates.check {
        prefs.check_on_startup = check;
    }
    if let Some(use_token) = cfg.updates.use_token {
        prefs.use_ambient_token = use_token;
    }
    if env_truthy(&std::env::var(ENV_NO_UPDATE_CHECK).unwrap_or_default()) {
        prefs.check_on_startup = false;
    }
    if env_truthy(&std::env::var(ENV_USE_TOKEN).unwrap_or_default()) {
        prefs.use_ambient_token = true;
    }
    prefs
}

/// Go `StartupCheckEnabled` (updater.go:160-162).
pub fn startup_check_enabled() -> bool {
    load_preferences().check_on_startup
}

/// Go `ambientGitHubToken` (updater.go:203-208): `GITHUB_TOKEN` first, then
/// `GH_TOKEN`, each trimmed; empty when neither is set.
pub fn ambient_github_token() -> Option<String> {
    for key in ["GITHUB_TOKEN", "GH_TOKEN"] {
        let value = std::env::var(key).unwrap_or_default();
        let value = value.trim().to_string();
        if !value.is_empty() {
            return Some(value);
        }
    }
    None
}

/// Go `githubToken` (updater.go:215-220): the token to attach to GitHub
/// requests, or `None` when the user has not opted in. Using a token only
/// raises the rate limit from 60 to 5,000 requests/hour, so a credential that
/// happens to be in the environment is never sent by default (#117).
pub fn github_token() -> Option<String> {
    if !load_preferences().use_ambient_token {
        return None;
    }
    ambient_github_token()
}

/// Extract the host from a URL the way Go's `url.URL.Hostname()` does:
/// drop scheme, userinfo, port, and any query/fragment; strip IPv6 brackets.
/// The `url` crate is not a workspace dependency, so this reproduces the two
/// behaviours the updater actually relies on. Returns `None` for a URL with
/// no authority component.
pub fn url_hostname(raw: &str) -> Option<String> {
    let after_scheme = match raw.find("://") {
        Some(i) => &raw[i + 3..],
        None => raw,
    };
    let authority_end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    let authority = &after_scheme[..authority_end];
    if authority.is_empty() {
        return None;
    }
    // Strip userinfo: everything up to and including the last '@'.
    let host_port = match authority.rfind('@') {
        Some(i) => &authority[i + 1..],
        None => authority,
    };
    let host = if let Some(rest) = host_port.strip_prefix('[') {
        // IPv6 literal: host ends at the closing bracket.
        &rest[..rest.find(']')?]
    } else {
        match host_port.rfind(':') {
            Some(i) if host_port[i + 1..].bytes().all(|b| b.is_ascii_digit()) => &host_port[..i],
            _ => host_port,
        }
    };
    if host.is_empty() {
        return None;
    }
    Some(host.to_ascii_lowercase())
}

/// Go `isGitHubHost` (updater.go:225-229): `github.com` /
/// `githubusercontent.com` including subdomains such as `api.github.com`
/// and `objects.githubusercontent.com`.
pub fn is_github_host(url: &str) -> bool {
    let Some(host) = url_hostname(url) else {
        return false;
    };
    host == "github.com"
        || host.ends_with(".github.com")
        || host == "githubusercontent.com"
        || host.ends_with(".githubusercontent.com")
}

/// The full Go `setGitHubAuth` condition (updater.go:235-239): both the
/// opt-in and the GitHub-host guard must hold. Returns the header value to
/// set, or `None` when the request must stay anonymous.
pub fn github_auth_header(url: &str) -> Option<String> {
    let token = github_token()?;
    if !is_github_host(url) {
        return None;
    }
    Some(format!("Bearer {token}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    /// The environment is process-global, so every test that mutates it takes
    /// this lock and restores the previous values.
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    struct EnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        saved: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn set(pairs: &[(&'static str, &str)]) -> Self {
            // Recover from poisoning: one failing assertion must not cascade
            // into every other test in this module.
            let lock = ENV_LOCK.get_or_init(|| Mutex::new(()));
            let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
            let saved = pairs
                .iter()
                .map(|(k, _)| (*k, std::env::var(k).ok()))
                .collect();
            for (k, v) in pairs {
                // SAFETY: ENV_LOCK is held for the guard's whole lifetime, and
                // every test that mutates the environment takes it.
                unsafe { std::env::set_var(k, v) };
            }
            Self {
                _lock: guard,
                saved,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in &self.saved {
                // SAFETY: still holding ENV_LOCK via self._lock.
                unsafe {
                    match v {
                        Some(val) => std::env::set_var(k, val),
                        None => std::env::remove_var(k),
                    }
                }
            }
        }
    }

    /// Every variable the resolver consults, blanked so a developer machine
    /// with `GITHUB_TOKEN` exported cannot influence the result.
    const RESOLVER_VARS: [&str; 6] = [
        ENV_NO_UPDATE_CHECK,
        ENV_USE_TOKEN,
        ENV_NO_SAVED_CONFIG,
        "GITHUB_TOKEN",
        "GH_TOKEN",
        "XDG_CONFIG_HOME",
    ];

    /// Blank every resolver variable, then apply `pairs`. Restores the
    /// previous process environment on drop.
    fn clean_env(pairs: &[(&'static str, &str)]) -> EnvGuard {
        let mut all: Vec<(&'static str, &str)> = RESOLVER_VARS.iter().map(|k| (*k, "")).collect();
        all.extend_from_slice(pairs);
        EnvGuard::set(&all)
    }

    #[test]
    fn env_truthy_matches_go() {
        for truthy in ["1", "true", "TRUE", "yes", "on", "anything", " true "] {
            assert!(env_truthy(truthy), "{truthy} should be truthy");
        }
        for falsy in ["", "0", "false", "FALSE", "no", "off", " 0 "] {
            assert!(!env_truthy(falsy), "{falsy:?} should be falsy");
        }
    }

    #[test]
    fn token_is_never_sent_without_opt_in() {
        let _env = clean_env(&[("GITHUB_TOKEN", "ghp_secret")]);
        // No opt-in: the ambient credential stays put.
        assert_eq!(github_token(), None);
        assert_eq!(
            github_auth_header("https://api.github.com/repos/a/b/releases/latest"),
            None
        );
        // The ambient token itself is still readable — just not transmitted.
        assert_eq!(ambient_github_token().as_deref(), Some("ghp_secret"));
    }

    #[test]
    fn env_opt_in_releases_the_token_to_github_hosts_only() {
        let _env = clean_env(&[("GITHUB_TOKEN", "ghp_secret"), (ENV_USE_TOKEN, "1")]);
        assert_eq!(github_token().as_deref(), Some("ghp_secret"));
        assert_eq!(
            github_auth_header("https://api.github.com/repos/a/b/releases/latest").as_deref(),
            Some("Bearer ghp_secret")
        );
        assert_eq!(
            github_auth_header("https://objects.githubusercontent.com/x").as_deref(),
            Some("Bearer ghp_secret")
        );
        // A non-GitHub host never receives the credential, even when opted in.
        assert_eq!(github_auth_header("https://cdn.example.com/x"), None);
        assert_eq!(
            github_auth_header("https://evil-github.com.attacker.io/x"),
            None
        );
    }

    #[test]
    fn env_opt_in_is_truthiness_based() {
        // BV_UPDATE_USE_TOKEN=0 must NOT opt in.
        let _env = clean_env(&[("GITHUB_TOKEN", "ghp_secret"), (ENV_USE_TOKEN, "0")]);
        assert_eq!(github_token(), None);
    }

    #[test]
    fn no_update_check_zero_does_not_opt_out() {
        // Each phase gets its own scope: the guard holds ENV_LOCK, so two
        // live guards in one scope would deadlock rather than nest.
        {
            let _env = clean_env(&[(ENV_NO_UPDATE_CHECK, "0")]);
            assert!(startup_check_enabled());
        }
        {
            let _env = clean_env(&[(ENV_NO_UPDATE_CHECK, "1")]);
            assert!(!startup_check_enabled());
        }
    }

    /// Write `body` to `<root>/bv/config.yaml` — the path Go reads for
    /// `XDG_CONFIG_HOME=<root>` (updater.go:104-108, 127) — and return `root`.
    fn scratch_xdg(name: &str, body: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("bvr-prefs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let config_dir = root.join("bv");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(config_dir.join("config.yaml"), body).unwrap();
        root
    }

    #[test]
    fn config_file_flips_both_switches() {
        let root = scratch_xdg("both", "updates:\n  check: false\n  use_token: true\n");
        {
            let _env = clean_env(&[("XDG_CONFIG_HOME", root.to_str().unwrap())]);
            let prefs = load_preferences();
            assert!(!prefs.check_on_startup, "config `check: false` must apply");
            assert!(
                prefs.use_ambient_token,
                "config `use_token: true` must apply"
            );
        }
        // The environment still wins over the file.
        {
            let _env = clean_env(&[
                ("XDG_CONFIG_HOME", root.to_str().unwrap()),
                (ENV_NO_UPDATE_CHECK, "1"),
            ]);
            assert!(!load_preferences().check_on_startup);
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn no_saved_config_ignores_the_config_file() {
        let root = scratch_xdg("nsc", "updates:\n  use_token: true\n");
        let _env = clean_env(&[
            ("XDG_CONFIG_HOME", root.to_str().unwrap()),
            (ENV_NO_SAVED_CONFIG, "1"),
        ]);
        assert!(!load_preferences().use_ambient_token);
        drop(_env);
        // Without BV_NO_SAVED_CONFIG the same file does apply — proving the
        // switch is what suppressed it, not a parse failure.
        let _env = clean_env(&[("XDG_CONFIG_HOME", root.to_str().unwrap())]);
        assert!(load_preferences().use_ambient_token);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn malformed_config_falls_back_to_defaults() {
        let root = scratch_xdg("bad", "updates: [this is not a mapping\n");
        let _env = clean_env(&[("XDG_CONFIG_HOME", root.to_str().unwrap())]);
        assert_eq!(load_preferences(), Preferences::default());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn user_config_dir_honours_xdg_then_home() {
        {
            let _env = clean_env(&[("XDG_CONFIG_HOME", "/tmp/xdg-test")]);
            assert_eq!(
                user_config_dir().unwrap(),
                PathBuf::from("/tmp/xdg-test/bv")
            );
        }
        let _env = clean_env(&[]);
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap();
        assert_eq!(
            user_config_dir().unwrap(),
            PathBuf::from(home).join(".config").join("bv")
        );
    }

    #[test]
    fn github_host_matching_covers_subdomains_and_rejects_lookalikes() {
        for url in [
            "https://github.com/",
            "https://api.github.com/repos/a/b",
            "https://objects.githubusercontent.com/x",
            "https://GITHUB.COM/",
            "https://github.com:443/x",
        ] {
            assert!(is_github_host(url), "{url} should be a GitHub host");
        }
        for url in [
            "https://example.com/",
            "https://github.com.attacker.io/",
            "https://notgithub.com/",
            "https://raw.githubusercontent.com.evil.io/x",
            "",
            "not a url at all",
        ] {
            assert!(!is_github_host(url), "{url} should not be a GitHub host");
        }
    }

    #[test]
    fn url_hostname_strips_userinfo_port_and_path() {
        assert_eq!(
            url_hostname("https://api.github.com/a/b").as_deref(),
            Some("api.github.com")
        );
        assert_eq!(
            url_hostname("https://user:pw@api.github.com:443/a?b#c").as_deref(),
            Some("api.github.com")
        );
        assert_eq!(
            url_hostname("https://[2001:db8::1]:443/x").as_deref(),
            Some("2001:db8::1")
        );
        assert_eq!(url_hostname("https://").as_deref(), None);
    }
}
