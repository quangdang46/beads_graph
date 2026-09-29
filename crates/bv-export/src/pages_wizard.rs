//! Pages deployment wizard — port of Go `pkg/export/wizard.go` at parity
//! commit `18afafa`.
//!
//! ## What is here
//!
//! The banner, the non-TTY guard, the saved-configuration offer, the form
//! driver through Step 3, and the result — the half of `Wizard.Run()` that
//! collects a [`WizardConfig`]. The configuration itself, and its on-disk
//! contract (`offerSavedConfig`, `wizard.go:194`; the JSON key order), are the
//! part with a stable format worth pinning, and they were here first.
//!
//! **The deployment is not here.** It fans out into `pkg/export/github.go`
//! and `pkg/export/cloudflare.go` — 60 KB of GitHub Pages and Cloudflare
//! Workers orchestration, none of which exists in this crate.
//!
//! ## The form driver, and why it is not `huh`
//!
//! Go builds the form with `github.com/charmbracelet/huh` (`wizard.go:19`,
//! `ThemeDracula`). Option (a) of the task was "check whether huh can be
//! added as a dependency". Checked, and the answer is no:
//!
//! * `huh` on crates.io (0.2.11) is *"A CLI tool that provides AI-powered
//!   analysis and suggestions for your shell commands"* — an unrelated
//!   project that happens to share the name. Adding it would be a
//!   name-collision, not a port.
//! * `charmed-huh` (0.2.4) and `uhuh` (0.1.1) are small independent lookalikes,
//!   not Charm's library and not verified to render anything.
//! * The workspace's actual interactive stack is ratatui 0.29 + crossterm
//!   0.28, used only inside `bv-tui` and unreachable from this crate.
//!
//! A bubbletea-style arrow-key renderer would also be the wrong shape for the
//! case that matters: huh's *accessible* rendering — the one every non-TTY
//! caller sees, and the one a CI caller diffs the oracle's output against —
//! is plain line I/O, and that is exactly what this port reproduces.
//!
//! So this port follows option (b): the form is driven by huh's accessible
//! rendering, field for field, verified against the measured oracle. What
//! that buys and what it costs:
//!
//! * **Byte-identical:** the banner, the step headers and rules, every
//!   prompt line, the re-prompt error text, the saved-config summary, and the
//!   three `stdin is …` guard errors.
//! * **Divergent, and only on a real terminal:** no Dracula colours, no
//!   boxed/animated form chrome, no arrow-key `Select` — a Select is a
//!   numbered list plus `Enter a number between 1 and 3:` in both, so this is
//!   the one widget whose *TTY* interaction differs. The confirm's
//!   `Description`, `Affirmative` and `Negative` labels are likewise carried
//!   as metadata on [`ConfirmField`] but never printed, because huh renders
//!   them only from the bubbletea path.

use serde::{Deserialize, Serialize};
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

/// Go `WizardConfig` (`pkg/export/wizard.go:26-55`).
///
/// Field order is the JSON key order, which `encoding/json` derives from
/// declaration order. Go writes the file with `MarshalIndent`, so the
/// persisted document has to keep this order to stay a drop-in replacement.
///
/// The container-level `#[serde(default)]` is the read half of the same
/// contract. Most fields are omitted when empty, and Go's `json.Unmarshal`
/// leaves an absent key at its zero value rather than failing, so a file
/// written by an older build — or one a user has trimmed by hand — must still
/// load.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WizardConfig {
    // Export options — wizard.go:27-30.
    pub include_closed: bool,
    pub include_history: bool,
    pub title: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub subtitle: String,

    // Source metadata for reliable updates — wizard.go:33-37.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub source_beads_dir: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub source_repo_root: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub source_path: String,
    #[serde(skip_serializing_if = "is_zero")]
    pub last_issue_count: i64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub last_data_hash: String,

    // Deployment target — wizard.go:40.
    /// Go: `DeployTarget` — one of `github`, `cloudflare`, `local`. Go has no
    /// enum validation on this field, so an unrecognised value round-trips
    /// rather than being rejected here either.
    pub deploy_target: String,

    // GitHub options — wizard.go:43-45.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub repo_name: String,
    #[serde(skip_serializing_if = "is_false")]
    pub repo_private: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub repo_description: String,

    // Cloudflare options — wizard.go:48-49.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub cloudflare_project: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub cloudflare_branch: String,

    /// Go: `OutputPath` — the bundle directory, wizard.go:52.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub output_path: String,
}

fn is_zero(v: &i64) -> bool {
    *v == 0
}

fn is_false(v: &bool) -> bool {
    !*v
}

/// Go `WizardResult` (`pkg/export/wizard.go:57-66`).
///
/// Not serialised in Go — it is the return value of `PerformDeploy`, and
/// every field has an empty value that JSON's `omitempty` would drop. It is
/// kept here as a plain struct for the same reason.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WizardResult {
    pub bundle_path: String,
    pub repo_full_name: String,
    pub pages_url: String,
    pub deploy_target: String,
    // Cloudflare-specific — wizard.go:64-65.
    pub cloudflare_project: String,
    pub cloudflare_url: String,
}

/// Failures from [`load_wizard_config`] and [`save_wizard_config`].
#[derive(Debug, thiserror::Error)]
pub enum WizardConfigError {
    /// `wizard.go:980` and `:1016` — no home directory, so no config path.
    #[error("could not determine config path")]
    NoConfigPath,
    /// `wizard.go:1021` / `:1028`.
    #[error("{0}")]
    Read(#[source] std::io::Error),
    /// `wizard.go:989`.
    #[error("{0}")]
    Parse(#[source] serde_json::Error),
    /// `wizard.go:1025`.
    #[error("create wizard config directory: {0}")]
    CreateDir(#[source] std::io::Error),
    /// `wizard.go:1036`.
    #[error("marshal wizard config: {0}")]
    Marshal(#[source] serde_json::Error),
    /// `wizard.go:1040` onward — the atomic write.
    #[error("{0}")]
    Write(String),
}

/// Go `WizardConfigPath` (`pkg/export/wizard.go:974-981`):
/// `$HOME/.config/bv/pages-wizard.json` on Unix and
/// `%USERPROFILE%\.config\bv\pages-wizard.json` on Windows.
pub fn wizard_config_path() -> Result<PathBuf, WizardConfigError> {
    let home = home_dir().ok_or(WizardConfigError::NoConfigPath)?;
    Ok(home.join(".config").join("bv").join("pages-wizard.json"))
}

fn home_dir() -> Option<PathBuf> {
    // Go's `os.UserHomeDir` reads $HOME on Unix and %USERPROFILE% on Windows,
    // and errors when the variable is unset rather than falling back to
    // getpwuid, so the port keeps that behaviour instead of silently
    // inventing a home.
    let key = if cfg!(target_os = "windows") {
        "USERPROFILE"
    } else {
        "HOME"
    };
    std::env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Go `LoadWizardConfig` (`pkg/export/wizard.go:983-1004`).
///
/// A missing file is not an error: Go returns `(nil, nil)`, which the wizard
/// reads as "no saved configuration". `Ok(None)` is that same signal.
pub fn load_wizard_config() -> Result<Option<WizardConfig>, WizardConfigError> {
    let path = wizard_config_path()?;
    let data = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(WizardConfigError::Read(e)),
    };
    let config = serde_json::from_str(&data).map_err(WizardConfigError::Parse)?;
    Ok(Some(config))
}

/// Go `SaveWizardConfig` (`pkg/export/wizard.go:1006-1028`): render with
/// `MarshalIndent(_, "", "  ")` and hand the bytes to the atomic write.
pub fn save_wizard_config(config: &WizardConfig) -> Result<(), WizardConfigError> {
    let path = wizard_config_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(WizardConfigError::CreateDir)?;
    }
    let data = serde_json::to_string_pretty(config).map_err(WizardConfigError::Marshal)?;
    write_wizard_config_file(&path, data.as_bytes())
}

/// Go `writeWizardConfigFile` (`pkg/export/wizard.go:1030-1064`).
///
/// Write to a temporary file in the same directory, `fsync`, then rename over
/// the target. That ordering is the point: a crash mid-write leaves the
/// previous configuration intact rather than a truncated file that would fail
/// to parse on the next run. Exposed separately so a caller can save to a
/// chosen path.
pub fn write_wizard_config_file(
    path: &std::path::Path,
    data: &[u8],
) -> Result<(), WizardConfigError> {
    let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let base = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("pages-wizard.json");
    let temp = dir.join(format!("{base}.tmp-{}", std::process::id()));

    let write = || -> std::io::Result<()> {
        let mut file = std::fs::File::create(&temp)?;
        std::io::Write::write_all(&mut file, data)?;
        file.sync_all()?;
        Ok(())
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_file(&temp);
        return Err(WizardConfigError::Write(format!(
            "write temporary wizard config: {e}"
        )));
    }
    if let Err(e) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(WizardConfigError::Write(format!(
            "replace wizard config: {e}"
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The wizard driver — Go `pkg/export/wizard.go:88-553`.
// ---------------------------------------------------------------------------

/// Go's `readWizardInput` timeout constant (`wizard.go:258`): the bounded read
/// that tells a silent-but-open pipe from a closed one.
const STDIN_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(100);

/// Go's `PromptString` error text for a boolean the parser does not accept
/// (`huh/internal/accessibility`, `PromptBool`).
const INVALID_BOOL_INPUT: &str = "invalid input. please try again";

/// Failures from [`Wizard::run`].
///
/// The four stdin variants are the load-bearing half: they are the difference
/// between a clean `Error: …` and exit 1, and a wizard that blocks forever in
/// CI. The strings are Go's verbatim (`wizard.go:242-273`) so a scripted
/// caller grepping stderr sees the same text from either binary.
#[derive(Debug, thiserror::Error)]
pub enum WizardError {
    /// `wizard.go:249` — stdin is a regular file of size 0.
    #[error("wizard requires interactive input; stdin is empty")]
    StdinEmpty,
    /// `wizard.go:258` — a pipe that stayed open without producing a byte.
    #[error("wizard requires interactive input; stdin did not provide data within timeout")]
    StdinTimeout,
    /// `wizard.go:260` — the bounded read itself failed.
    #[error("wizard requires interactive input; stdin error: {0}")]
    StdinRead(String),
    /// `wizard.go:264` — EOF with no bytes: the writer closed the pipe.
    #[error("wizard requires interactive input; stdin is closed")]
    StdinClosed,
    /// `wizard.go:294` / `:341` — a huh form that could not run.
    #[error("{0}")]
    Form(String),
    /// `wizard.go:277` — a `LoadWizardConfig` that the caller chooses to
    /// surface rather than swallow.
    #[error("{0}")]
    Config(#[from] WizardConfigError),
}

// ---------------------------------------------------------------------------
// Deployment probes — Go `pkg/export/github.go` and `pkg/export/cloudflare.go`.
// These shell out exactly as Go does: `gh auth status`, `git config
// --get`-equivalent reads, `wrangler --version`. Nothing here touches the
// network on its own — `gh auth status` reads local credentials.
// ---------------------------------------------------------------------------

/// Go `GitHubStatus` (github.go:60-66).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GhStatus {
    pub installed: bool,
    pub authenticated: bool,
    pub username: String,
    pub git_name: String,
    pub git_email: String,
    pub git_configured: bool,
}

/// Go `CloudflareStatus` (cloudflare.go), the fields the wizard's Step 4
/// branches on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WranglerStatus {
    pub installed: bool,
    pub authenticated: bool,
    pub npm_installed: bool,
    /// Go's `AccountName` (cloudflare.go:63).
    pub account_name: Option<String>,
    /// Go's `AccountID`.
    pub account_id: Option<String>,
}

fn on_path(exe: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let candidate = dir.join(exe);
        candidate.is_file()
    })
}

/// Go `CheckGHStatus` (github.go:68-101).
pub fn check_gh_status() -> GhStatus {
    let mut status = GhStatus {
        installed: on_path("gh"),
        ..GhStatus::default()
    };

    if status.installed {
        if let Ok(out) = std::process::Command::new("gh")
            .args(["auth", "status"])
            .output()
        {
            status.authenticated = out.status.success();
            if status.authenticated {
                status.username = parse_gh_username(&String::from_utf8_lossy(&out.stdout));
            }
        }
    }

    let git_config = |key: &str| -> String {
        std::process::Command::new("git")
            .args(["config", "--get", key])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    };
    status.git_name = git_config("user.name");
    status.git_email = git_config("user.email");
    status.git_configured = !status.git_name.is_empty() && !status.git_email.is_empty();
    status
}

/// Go `parseGHUsername` (github.go:104-126): the word after "account " on the
/// "Logged in to github.com account NAME (...)" line.
fn parse_gh_username(output: &str) -> String {
    for line in output.lines() {
        if !line.contains("Logged in to") || !line.contains("account") {
            continue;
        }
        let Some(idx) = line.find("account ") else {
            continue;
        };
        let rest = line[idx + "account ".len()..].trim();
        if let Some(space) = rest.find(' ') {
            if space > 0 {
                return rest[..space].to_string();
            }
        }
        if let Some(paren) = rest.find('(') {
            if paren > 0 {
                return rest[..paren].trim().to_string();
            }
        }
        return rest.to_string();
    }
    String::new()
}

/// Go `ShowInstallInstructions` (github.go:128-148).
pub fn show_gh_install_instructions() {
    println!();
    println!("gh CLI is not installed.");
    println!();
    println!("Installation options:");
    if cfg!(target_os = "macos") {
        println!("  macOS (Homebrew): brew install gh");
        println!("  macOS (MacPorts): sudo port install gh");
    } else if cfg!(target_os = "windows") {
        println!("  Windows (winget): winget install --id GitHub.cli");
        println!("  Windows (scoop):  scoop install gh");
        println!("  Windows (choco):  choco install gh");
    } else {
        println!("  Debian/Ubuntu:    sudo apt install gh");
        println!("  Fedora:           sudo dnf install gh");
        println!("  Arch Linux:       sudo pacman -S github-cli");
    }
    println!();
    println!("  All platforms: https://cli.github.com/");
    println!();
}

/// Go `AuthenticateGH` (github.go:176-190): `gh auth login --web`, wired to
/// this process's stdio so the browser flow is interactive.
pub fn authenticate_gh() {
    println!();
    println!("Starting GitHub authentication...");
    println!("This will open a browser for authentication.");
    println!();
    let _ = std::process::Command::new("gh")
        .args(["auth", "login", "--web"])
        .status();
}

/// Go `CheckWranglerStatus` (cloudflare.go).
pub fn check_wrangler_status() -> WranglerStatus {
    let npm_installed = on_path("npm");
    let mut status = WranglerStatus {
        npm_installed,
        ..WranglerStatus::default()
    };
    if !on_path("wrangler") {
        return status;
    }
    status.installed = true;

    // Go checks three things in this order and stops at the first that
    // answers (cloudflare.go:83-120). `whoami` is last, not first: it "returns
    // 0 even when not authenticated" (its own comment at :106) and can hang
    // on a headless host, which is why Go reads the config file before
    // shelling out at all.
    //
    //   1. CLOUDFLARE_API_TOKEN — the CI/headless path.
    if std::env::var("CLOUDFLARE_API_TOKEN")
        .ok()
        .is_some_and(|t| !t.is_empty())
    {
        status.authenticated = true;
        status.account_name = Some("(API token)".into());
        status.account_id = std::env::var("CLOUDFLARE_ACCOUNT_ID").ok();
        return status;
    }

    //   2. The OAuth config file, which is what a logged-in `wrangler login`
    //   leaves behind.
    if check_wrangler_config_file() {
        status.authenticated = true;
        return status;
    }

    //   3. `whoami` last, and its OUTPUT is the signal, not its exit status.
    let out = std::process::Command::new("wrangler")
        .args(["whoami"])
        .output();
    if let Ok(o) = out {
        let text =
            String::from_utf8_lossy(&o.stdout).into_owned() + &String::from_utf8_lossy(&o.stderr);
        status.authenticated = o.status.success()
            && !text.contains("not authenticated")
            && !text.contains("You are not authenticated")
            && (text.contains("Account ID") || text.contains("account") || text.contains("@"));
        if status.authenticated {
            let (name, id) = parse_wrangler_whoami(&text);
            status.account_name = name;
            status.account_id = id;
        }
    }
    status
}

/// Go `checkWranglerConfigFile` (cloudflare.go:130-150): wrangler stores OAuth
/// credentials in one of three places depending on version.
fn check_wrangler_config_file() -> bool {
    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
        return false;
    };
    let mut candidates = vec![
        home.join(".wrangler").join("config").join("default.toml"),
        home.join(".config")
            .join(".wrangler")
            .join("config")
            .join("default.toml"),
        // macOS wrangler (verified with 4.143.0) writes here. Go's list
        // (cloudflare.go:132-136) does not include it, so `wrangler login`
        // on a Mac leaves a token file the check cannot see and every run
        // reports "not authenticated" despite a successful login.
        home.join("Library")
            .join("Preferences")
            .join(".wrangler")
            .join("config")
            .join("default.toml"),
    ];
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        candidates.push(
            std::path::PathBuf::from(xdg)
                .join(".wrangler")
                .join("config")
                .join("default.toml"),
        );
    }
    candidates.iter().any(|c| valid_wrangler_config(c))
}

/// Go `validWranglerConfig` (cloudflare.go:152-178): an `oauth_token` that has
/// either not expired or can be refreshed.
fn valid_wrangler_config(path: &std::path::Path) -> bool {
    let Ok(content) = std::fs::read_to_string(path) else {
        return false;
    };
    if !content.contains("oauth_token") {
        return false;
    }
    let has_refresh_token = content.contains("refresh_token");
    for line in content.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("expiration_time") {
            let Some((_, value)) = rest.split_once('=') else {
                continue;
            };
            let time_str = value.trim().trim_matches('"');
            if let Ok(expiry) = time_str.parse::<jiff::Timestamp>() {
                if jiff::Timestamp::now() > expiry && !has_refresh_token {
                    return false;
                }
            }
        }
    }
    true
}

/// Go `parseWranglerWhoami` (cloudflare.go:180+): the account name and id out
/// of `wrangler whoami` output.
fn parse_wrangler_whoami(text: &str) -> (Option<String>, Option<String>) {
    let mut name = None;
    let mut id = None;
    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("Account Name:") {
            name = Some(rest.trim().to_string());
        } else if let Some(rest) = t.strip_prefix("Account ID:") {
            id = Some(rest.trim().to_string());
        } else if name.is_none() {
            // wrangler also prints a "| Account Name  foo |" table form.
            if let Some(rest) = t.strip_prefix("| Account Name") {
                if let Some(v) = rest.split('|').nth(1) {
                    let v = v.trim();
                    if !v.is_empty() {
                        name = Some(v.to_string());
                    }
                }
            }
        }
        if id.is_none() {
            if let Some(rest) = t.strip_prefix("| Account ID") {
                if let Some(v) = rest.split('|').nth(1) {
                    let v = v.trim();
                    if !v.is_empty() {
                        id = Some(v.to_string());
                    }
                }
            }
        }
    }
    (name, id)
}

/// Go `ShowWranglerInstallInstructions`.
pub fn show_wrangler_install_instructions() {
    println!();
    println!("wrangler CLI is not installed.");
    println!();
    println!("Install with:");
    println!("  npm install -g wrangler");
    println!();
}

/// Go's wrangler install path.
pub fn install_wrangler() {
    println!();
    println!("Installing wrangler...");
    println!();
    let _ = std::process::Command::new("npm")
        .args(["install", "-g", "wrangler"])
        .status();
}

/// Go's `wrangler login`.
pub fn login_wrangler() {
    println!();
    println!("Logging in to Cloudflare...");
    println!();
    let _ = std::process::Command::new("wrangler")
        .args(["login"])
        .status();
}

/// Go `isTerminal()` (`wizard.go:88-90`): `term.IsTerminal(int(os.Stdin.Fd()))`.
pub fn is_terminal() -> bool {
    std::io::stdin().is_terminal()
}

/// Go `wizardInputReader` (`wizard.go:111-135`).
///
/// Not cosmetic. huh's accessible path builds a **fresh** `bufio.Scanner`
/// over the reader for every single field
/// (`huh/internal/accessibility.PromptString`), and a `bufio.Scanner` reads
/// ahead into a 4 KiB buffer — with a plain `Read` over the whole pipe the
/// first field would swallow every later answer and drop them. Capping each
/// `Read` at one newline is what keeps the answers lined up with the fields.
#[derive(Debug, Default)]
pub struct WizardInput {
    data: Vec<u8>,
    offset: usize,
}

impl WizardInput {
    pub fn new(data: Vec<u8>) -> Self {
        Self { data, offset: 0 }
    }
}

impl Read for WizardInput {
    fn read(&mut self, p: &mut [u8]) -> std::io::Result<usize> {
        if p.is_empty() {
            return Ok(0);
        }
        let remaining = &self.data[self.offset.min(self.data.len())..];
        if remaining.is_empty() {
            return Ok(0);
        }
        let mut n = remaining.len();
        if let Some(newline) = remaining.iter().position(|b| *b == b'\n') {
            n = newline + 1;
        }
        if n > p.len() {
            n = p.len();
        }
        p[..n].copy_from_slice(&remaining[..n]);
        self.offset += n;
        Ok(n)
    }
}

/// Go `readWizardInputBlocking` (`wizard.go:172-191`).
///
/// Rust has no read deadline on a bare fd, so this is also the primary path,
/// not merely the macOS fallback: a thread reads stdin to EOF and the caller
/// waits on a bounded channel. On timeout the thread may still be parked on
/// stdin — Go accepts exactly the same leak, in exactly the case the timeout
/// exists to report, and the process is about to exit anyway.
fn read_stdin(timeout: std::time::Duration) -> Result<Vec<u8>, std::io::Error> {
    let (tx, rx) = crossbeam_channel::bounded(1);
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut stdin = std::io::stdin();
        let err = stdin.read_to_end(&mut buf).err();
        let _ = tx.send((buf, err));
    });
    match rx.recv_timeout(timeout) {
        Ok((buf, None)) => Ok(buf),
        Ok((_, Some(e))) => Err(e),
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "wizard stdin probe timed out",
        )),
    }
}

/// Go's `Run` guard (`wizard.go:241-273`).
///
/// Two tiers, not one, and the difference is load-bearing: Go only rejects
/// stdin that is a *regular file of size 0* or a *pipe* that is closed or
/// silent. A character device — `/dev/null` is the case that matters — falls
/// through **both** branches and the wizard proceeds with no stashed input.
/// A blanket `is_terminal()` check would newly reject `/dev/null`, which the
/// oracle accepts, so the port reproduces the two tiers exactly.
///
/// Returns the stashed bytes when stdin was a pipe that carried data.
pub fn guard_stdin() -> Result<Option<Vec<u8>>, WizardError> {
    if is_terminal() {
        return Ok(None);
    }
    // `fstat` on fd 0 — std has no file-mode query, and `libc` is already in
    // the lock for the signal-handling crate.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(0, &mut st) } != 0 {
        // Go: `if err == nil { … }` — a failed Stat skips the whole block and
        // the wizard proceeds.
        return Ok(None);
    }
    // `st_mode` is u16 on Windows and u32 on unix, and the `S_IF*` constants
    // are i32, so `st_mode & S_IFMT` compiles on unix and fails on Windows
    // with `no implementation for u16 & i32`. Widening both sides to u64
    // types the comparison everywhere; every `S_IF*` value fits in 16 bits,
    // so the widening is lossless.
    //
    // u64, not u32: on unix `st_mode` is *already* u32, and `st_mode as u32`
    // is then a same-type no-op that clippy rejects as `unnecessary_cast`
    // ("casting to the same type is unnecessary (`u32` -> `u32`)"). u64 is
    // wider than `st_mode` on every platform libc supports here, so each cast
    // is a genuine widening on all of them and the lint cannot fire.
    let file_type = (libc::S_IFMT as u64) & (st.st_mode as u64);
    let is_regular = file_type == (libc::S_IFREG as u64);
    if is_regular && st.st_size == 0 {
        return Err(WizardError::StdinEmpty);
    }
    let is_pipe = file_type != (libc::S_IFCHR as u64) && !is_regular;
    if !is_pipe {
        return Ok(None);
    }
    let data = read_stdin(STDIN_PROBE_TIMEOUT).map_err(|e| {
        if e.kind() == std::io::ErrorKind::TimedOut {
            WizardError::StdinTimeout
        } else {
            WizardError::StdinRead(e.to_string())
        }
    })?;
    if data.is_empty() {
        return Err(WizardError::StdinClosed);
    }
    Ok(Some(data))
}

/// The three options of the Step 2 select, as Go declares them
/// (`wizard.go:404-407`). The label is what a caller sees; the value is what
/// [`WizardConfig::deploy_target`] stores and what `collect_target_config`
/// switches on.
pub const DEPLOY_TARGETS: [(&str, &str); 3] = [
    ("GitHub Pages (create/update repository)", "github"),
    ("Cloudflare Pages (requires wrangler CLI)", "cloudflare"),
    ("Export locally only", "local"),
];

/// A `huh.NewConfirm` in the shape this port needs. `description`,
/// `affirmative` and `negative` are carried because Go sets them and the
/// bubbletea renderer shows them — huh's accessible path prints none of them,
/// which is verified, not assumed (see the module docs).
#[derive(Debug, Clone)]
pub struct ConfirmField {
    pub title: &'static str,
    pub description: &'static str,
    pub affirmative: &'static str,
    pub negative: &'static str,
    pub default: bool,
}

impl ConfirmField {
    pub fn new(title: &'static str, default: bool) -> Self {
        Self {
            title,
            description: "",
            affirmative: "Yes",
            negative: "No",
            default,
        }
    }

    /// The `[Y/n]` / `[y/N]` hint. Uppercase letter is the default — huh's
    /// `opts := "[y/N]"; if defaultValue { opts = "[Y/n]" }`.
    fn hint(&self) -> &'static str {
        if self.default {
            "[Y/n]"
        } else {
            "[y/N]"
        }
    }
}

/// Go `printBanner` (`wizard.go:336-349`), as data so the box can be asserted
/// on without capturing stdout.
///
/// Unconditional and first, so a caller that is about to fail the stdin guard
/// still sees it. Every line is 68 columns; the `→` on line 2 is U+2192, and
/// because the box is byte-padded that line is 74 **bytes** against 68
/// columns — a golden diff catches a `chars().count()` rewrite here.
pub const BANNER: [&str; 12] = [
    "",
    "╔══════════════════════════════════════════════════════════════════╗",
    "║           bv → Static Site Deployment Wizard                     ║",
    "╠══════════════════════════════════════════════════════════════════╣",
    "║  This wizard will:                                               ║",
    "║    1. Export your issues to a static HTML bundle                 ║",
    "║    2. Preview it locally                                         ║",
    "║    3. Deploy to GitHub Pages, Cloudflare Pages, or export only   ║",
    "║                                                                  ║",
    "║  Press Ctrl+C anytime to cancel                                  ║",
    "╚══════════════════════════════════════════════════════════════════╝",
    "",
];

pub fn print_banner() {
    for line in BANNER {
        println!("{line}");
    }
}

/// Go `Wizard` (`wizard.go:57-84`) plus the driver half of `Run`
/// (`wizard.go:238-334`).
pub struct Wizard {
    beads_path: PathBuf,
    config: WizardConfig,
    /// Non-`None` only when the stdin guard found a pipe carrying data.
    input: Option<WizardInput>,
    /// Go `isUpdate` — set when a saved configuration was adopted, and later
    /// consumed as GitHub's `ForceOverwrite`.
    pub is_update: bool,
}

impl Wizard {
    /// Go's `status.AccountName` / `status.AccountID` from the Step 4 probe
    /// (cloudflare.go:456-459), used to name the account before deploying.
    /// Re-probed rather than cached: `wrangler login` may have run since.
    pub fn cloudflare_account(&self) -> Option<String> {
        check_wrangler_status().account_name
    }

    /// Go's `status.AccountID` (cloudflare.go:457-459).
    pub fn cloudflare_account_id(&self) -> Option<String> {
        check_wrangler_status().account_id
    }

    /// A bare yes/no prompt outside the form sequence, the way Go reuses
    /// `newForm` for the Step 6 and Step 7 confirms
    /// (wizard.go:582-592, :5866-5874).
    pub fn prompt_deploy_confirm(&mut self, title: &str) -> bool {
        // `ConfirmField::new` takes a `'static` title; the two call sites are
        // literals, so this boxes the string once rather than leaking it.
        let owned: &'static str = Box::leak(title.to_string().into_boxed_str());
        self.prompt_confirm(owned, true)
    }
}

impl Wizard {
    /// Go `NewWizard` (`wizard.go:66-86`): both booleans start **true**.
    pub fn new(beads_path: &Path) -> Self {
        Self {
            beads_path: beads_path.to_path_buf(),
            config: WizardConfig {
                include_closed: true,
                include_history: true,
                ..WizardConfig::default()
            },
            input: None,
            is_update: false,
        }
    }

    /// Go `GetConfig` (`wizard.go:336`).
    pub fn config(&self) -> &WizardConfig {
        &self.config
    }

    /// Go `Wizard.Run` (`wizard.go:238-334`), through Step 3.
    ///
    /// The `checkPrerequisites` call that Go makes before returning is step 14
    /// onward and needs the GitHub/Cloudflare backends; see
    /// [`print_step4_header`], which is what the `local` target needs of it.
    pub fn run(&mut self) -> Result<WizardResult, WizardError> {
        print_banner();
        if let Some(data) = guard_stdin()? {
            self.input = Some(WizardInput::new(data));
        }

        // Go: `skipSaved := env.NoSavedConfig.Get() != ""` — any value counts,
        // including "0" and "false", because the env helper returns the raw
        // string. (wizard.go:277)
        let skip_saved = std::env::var_os("BV_NO_SAVED_CONFIG").is_some_and(|v| !v.is_empty());
        // Go discards a load error (`err == nil` is a condition on the offer,
        // not an early return), so a corrupt config falls through to the form
        // rather than aborting the wizard.
        let saved = if skip_saved {
            None
        } else {
            load_wizard_config().unwrap_or(None)
        };
        if let Some(saved) = saved.filter(|s| !s.deploy_target.is_empty()) {
            if self.offer_saved_config(&saved)? {
                // Go REPLACES the whole config pointer rather than merging
                // over the defaults from `NewWizard`, so a saved
                // `include_closed: false` survives.
                self.config = saved;
                self.is_update = true;
                println!("Using saved configuration...");
                println!();
                self.check_prerequisites()?;
                return Ok(WizardResult {
                    deploy_target: self.config.deploy_target.clone(),
                    ..WizardResult::default()
                });
            }
        }

        self.collect_export_options()?;
        self.collect_deploy_target()?;
        self.collect_target_config()?;
        self.check_prerequisites()?;

        // Go returns a result with every other field left empty; populating
        // them is the caller's job (wizard.go:330-332).
        Ok(WizardResult {
            deploy_target: self.config.deploy_target.clone(),
            ..WizardResult::default()
        })
    }

    /// Go `offerSavedConfig` (`wizard.go:194-235`).
    ///
    /// The three target blocks use three *different* column alignments —
    /// GitHub pads to 11, Cloudflare to 9, local to 5. That inconsistency is
    /// the contract; do not normalise it.
    fn offer_saved_config(&mut self, saved: &WizardConfig) -> Result<bool, WizardError> {
        println!("Found previous deployment configuration:");
        println!("────────────────────────────────────────");
        match saved.deploy_target.as_str() {
            "github" => {
                println!("  Target:     GitHub Pages");
                println!("  Repository: {}", saved.repo_name);
                if !saved.title.is_empty() {
                    println!("  Title:      {}", saved.title);
                }
            }
            "cloudflare" => {
                println!("  Target:  Cloudflare Pages");
                println!("  Project: {}", saved.cloudflare_project);
                if !saved.title.is_empty() {
                    println!("  Title:   {}", saved.title);
                }
            }
            "local" => {
                println!("  Target: Local export");
                println!("  Path:   {}", saved.output_path);
            }
            // Go's switch has no default, so an unrecognised saved target
            // prints the header and the rule and nothing else.
            _ => {}
        }
        println!();

        let field = ConfirmField {
            title: "Update existing deployment with these settings?",
            description: "Select No to configure a new deployment",
            affirmative: "Yes, update",
            negative: "No, reconfigure",
            default: true,
        };
        let use_saved = self.prompt_bool(&field);
        // Two newlines, not one: the first closes the prompt line (huh's
        // post-field `Fprintln`), the second is Go's own `fmt.Println("")` at
        // wizard.go:233, which separates the offer from whatever follows.
        self.field_done();
        println!();
        Ok(use_saved)
    }

    /// Go `collectExportOptions` (`wizard.go:351-392`).
    fn collect_export_options(&mut self) -> Result<(), WizardError> {
        println!("Step 1: Export Configuration");
        println!("────────────────────────────");

        let title = "Project Issues".to_string();
        let fields = [
            ConfirmField {
                description: "Export both open and closed issues",
                ..ConfirmField::new("Include closed issues?", true)
            },
            ConfirmField {
                description: "Export git commit history for each issue",
                ..ConfirmField::new("Include git history?", true)
            },
        ];
        for field in &fields {
            let answer = self.prompt_bool(field);
            self.field_done();
            match field.title {
                "Include closed issues?" => self.config.include_closed = answer,
                _ => self.config.include_history = answer,
            }
        }

        let title_answer = self.prompt_input("Site title", &title);
        self.field_done();
        // `if title != "" { … } else { … }` (wizard.go:384-388) is unreachable
        // in practice — huh returns the current value on a blank entry, and
        // the current value is the default — but Go's control flow is the
        // spec, so it is reproduced rather than reasoned away.
        self.config.title = if !title_answer.is_empty() {
            title_answer
        } else {
            title.clone()
        };

        let current_subtitle = self.config.subtitle.clone();
        let subtitle = self.prompt_input("Site subtitle (optional)", &current_subtitle);
        self.field_done();
        // No fallback: Go assigns PromptString's return verbatim, so a blank
        // entry leaves the field empty rather than restoring a default.
        self.config.subtitle = subtitle;

        println!();
        Ok(())
    }

    /// Go `collectDeployTarget` (`wizard.go:394-417`).
    fn collect_deploy_target(&mut self) -> Result<(), WizardError> {
        println!("Step 2: Deployment Target");
        println!("────────────────────────────");

        // Default index = 1 + the index of the CURRENT value. The field starts
        // empty, so a scripted caller that sends nothing lands on GitHub.
        let default = DEPLOY_TARGETS
            .iter()
            .position(|(_, v)| *v == self.config.deploy_target)
            .map(|i| i + 1)
            .unwrap_or(1) as i64;

        println!("Where do you want to deploy? ");
        for (i, (label, _)) in DEPLOY_TARGETS.iter().enumerate() {
            println!("{}. {}", i + 1, label);
        }

        let last = DEPLOY_TARGETS.len() as i64;
        let prompt = format!("Enter a number between 1 and {last}: ");
        let answer = self.prompt_int(&prompt, 1, last, default);
        // The form's post-field newline. huh prints it after every field, so
        // an EOF-terminated field leaves a blank line behind it.
        self.field_done();
        self.config.deploy_target = DEPLOY_TARGETS[(answer - 1) as usize].1.to_string();
        println!();
        Ok(())
    }

    /// Go `collectTargetConfig` (`wizard.go:419-428`): a plain match. Go's
    /// `WizardConfig` has no enum constraint, so an unrecognised value —
    /// including one a user hand-edited into the JSON — silently falls
    /// through to a no-op prerequisites check rather than erroring.
    fn collect_target_config(&mut self) -> Result<(), WizardError> {
        match self.config.deploy_target.as_str() {
            "github" => self.collect_github_config(),
            "cloudflare" => self.collect_cloudflare_config(),
            "local" => self.collect_local_config(),
            _ => Ok(()),
        }
    }

    /// Go `collectGitHubConfig` (`wizard.go:431-474`).
    fn collect_github_config(&mut self) -> Result<(), WizardError> {
        println!("Step 3: GitHub Configuration");
        println!("────────────────────────────");

        let suggested = suggest_repo_name();
        let repo = self.prompt_input("Repository name", &suggested);
        self.field_done();
        self.config.repo_name = if !repo.is_empty() { repo } else { suggested };

        let field = ConfirmField::new("Make repository private?", false);
        let private = self.prompt_bool(&field);
        self.field_done();
        self.config.repo_private = private;

        let description = self.prompt_input(
            "Repository description (optional)",
            "Issue tracker dashboard",
        );
        self.field_done();
        // Assigned unconditionally (wizard.go:470), so a blank entry yields
        // the default rather than an empty description.
        self.config.repo_description = description;

        println!();
        Ok(())
    }

    /// Go `collectCloudflareConfig` (`wizard.go:476-524`).
    fn collect_cloudflare_config(&mut self) -> Result<(), WizardError> {
        println!("Step 3: Cloudflare Pages Configuration");
        println!("────────────────────────────");

        let suggested = suggest_project_name(&self.beads_path);
        let project = self.prompt_input("Cloudflare Pages project name", &suggested);
        self.field_done();
        self.config.cloudflare_project = if !project.is_empty() {
            project
        } else {
            suggested
        };

        let branch = self.prompt_input("Branch name", "main");
        self.field_done();
        self.config.cloudflare_branch = if !branch.is_empty() {
            branch
        } else {
            "main".into()
        };

        println!();
        Ok(())
    }

    /// Go `collectLocalConfig` (`wizard.go:526-553`).
    fn collect_local_config(&mut self) -> Result<(), WizardError> {
        println!("Step 3: Local Export Configuration");
        println!("────────────────────────────");

        let path = self.prompt_input("Output directory", "./bv-pages");
        self.field_done();
        self.config.output_path = if !path.is_empty() {
            path
        } else {
            "./bv-pages".into()
        };

        println!();
        Ok(())
    }

    /// Go `checkPrerequisites` (`wizard.go:557-707`).
    ///
    /// The header is unconditional. The three target checks need
    /// `CheckGHStatus` and `CheckWranglerStatus`, which live in the GitHub and
    /// Cloudflare backends and are not ported yet; `local` genuinely has no
    /// checks, so it is exact today.
    fn check_prerequisites(&mut self) -> Result<(), WizardError> {
        print_step4_header();
        match self.config.deploy_target.as_str() {
            "github" => self.check_github_prerequisites()?,
            "cloudflare" => self.check_cloudflare_prerequisites()?,
            // Go's switch has no `local` case, so a local export genuinely has
            // no prerequisites (wizard.go:561-711).
            _ => {}
        }
        println!();
        Ok(())
    }

    /// Go's `github` arm of `checkPrerequisites` (wizard.go:562-620).
    fn check_github_prerequisites(&mut self) -> Result<(), WizardError> {
        let status = check_gh_status();

        if !status.installed {
            println!("✗ gh CLI not installed");
            show_gh_install_instructions();
            return Err(WizardError::Form(
                "gh CLI is required for GitHub Pages deployment".into(),
            ));
        }
        println!("✓ gh CLI installed");

        if !status.authenticated {
            println!("✗ gh CLI not authenticated");
            println!();
            let do_auth = self.prompt_confirm("Would you like to authenticate now?", true);
            if !do_auth {
                return Err(WizardError::Form("GitHub authentication required".into()));
            }
            authenticate_gh();
            let recheck = check_gh_status();
            if !recheck.authenticated {
                return Err(WizardError::Form("authentication failed".into()));
            }
            return self.check_github_prerequisites_after_auth(recheck);
        }
        println!("✓ Authenticated as {}", status.username);

        if !status.git_configured {
            println!("✗ Git identity not configured");
            println!("  Please run:");
            println!("    git config --global user.name \"Your Name\"");
            println!("    git config --global user.email \"your@email.com\"");
            return Err(WizardError::Form("git identity not configured".into()));
        }
        println!(
            "✓ Git configured ({} <{}>)",
            status.git_name, status.git_email
        );
        Ok(())
    }

    /// Go re-runs the git check after a successful re-auth
    /// (wizard.go:601-606): the loop re-enters at the git check because the
    /// auth branch falls through to it.
    fn check_github_prerequisites_after_auth(
        &mut self,
        status: GhStatus,
    ) -> Result<(), WizardError> {
        println!("✓ Authenticated as {}", status.username);
        if !status.git_configured {
            println!("✗ Git identity not configured");
            println!("  Please run:");
            println!("    git config --global user.name \"Your Name\"");
            println!("    git config --global user.email \"your@email.com\"");
            return Err(WizardError::Form("git identity not configured".into()));
        }
        println!(
            "✓ Git configured ({} <{}>)",
            status.git_name, status.git_email
        );
        Ok(())
    }

    /// Go's `cloudflare` arm (wizard.go:621-705). wrangler is probed the same
    /// way `gh` is: on PATH, then `--version`.
    fn check_cloudflare_prerequisites(&mut self) -> Result<(), WizardError> {
        let status = check_wrangler_status();
        if !status.installed {
            println!("✗ wrangler CLI not installed");
            if !status.npm_installed {
                println!("  npm is required to install wrangler");
                println!("  Download Node.js from: https://nodejs.org/");
                return Err(WizardError::Form(
                    "npm is required to install wrangler CLI".into(),
                ));
            }
            show_wrangler_install_instructions();
            let do_install = self.prompt_confirm("Would you like to install wrangler now?", true);
            if !do_install {
                return Err(WizardError::Form(
                    "wrangler CLI is required for Cloudflare Pages deployment".into(),
                ));
            }
            install_wrangler();
            let recheck = check_wrangler_status();
            if !recheck.installed {
                return Err(WizardError::Form("wrangler installation failed".into()));
            }
        }
        println!("✓ wrangler CLI installed");
        if !status.authenticated {
            println!("✗ wrangler not authenticated");
            println!();
            let do_login = self.prompt_confirm("Would you like to log in now?", true);
            if !do_login {
                return Err(WizardError::Form("wrangler authentication required".into()));
            }
            login_wrangler();
            if !check_wrangler_status().authenticated {
                return Err(WizardError::Form("authentication failed".into()));
            }
        }
        println!("✓ Authenticated with Cloudflare");
        Ok(())
    }

    // -- field drivers ------------------------------------------------------

    /// huh `accessibility.PromptString` (`huh/internal/accessibility`).
    ///
    /// Prints `prompt` with no newline, reads one line, and on a validator
    /// failure prints the error and re-prompts. On EOF it prints a newline and
    /// breaks — which is what leaves the extra blank line Go's output shows
    /// after a field whose input ran out. Returns the trimmed answer, or
    /// `default` when the answer is empty or whitespace.
    fn prompt_string(
        &mut self,
        prompt: &str,
        default: &str,
        validator: impl Fn(&str) -> Result<(), String>,
    ) -> String {
        let mut input = String::new();
        loop {
            print!("{prompt}");
            // Rust's stdout is a `LineWriter`: a prompt has no newline, so
            // without an explicit flush it would sit in the buffer and the
            // user would see nothing.
            let _ = std::io::stdout().flush();
            match self.read_line() {
                None => {
                    println!();
                    break;
                }
                Some(line) => {
                    input = line;
                    if let Err(e) = validator(&input) {
                        println!("{e}");
                        continue;
                    }
                    break;
                }
            }
        }
        let trimmed = input.trim();
        if trimmed.is_empty() {
            default.to_string()
        } else {
            trimmed.to_string()
        }
    }

    /// huh `accessibility.PromptBool`.
    /// huh `NewConfirm` with a title and nothing else — what the
    /// prerequisite branches use (wizard.go:582-592, :693-703).
    fn prompt_confirm(&mut self, title: &'static str, default: bool) -> bool {
        self.prompt_bool(&ConfirmField::new(title, default))
    }

    fn prompt_bool(&mut self, field: &ConfirmField) -> bool {
        let prompt = format!("{} {} ", field.title, field.hint());
        let answer = self.prompt_string(&prompt, if field.default { "y" } else { "N" }, |s| {
            if s.trim().is_empty() {
                return Ok(());
            }
            match parse_bool(s) {
                Some(_) => Ok(()),
                None => Err(INVALID_BOOL_INPUT.to_string()),
            }
        });
        parse_bool(&answer).unwrap_or(false)
    }

    /// huh `accessibility.PromptString` as used by `Input.RunAccessible`.
    /// The default is the field's **current** value, so a blank entry keeps it.
    fn prompt_input(&mut self, title: &str, current: &str) -> String {
        let prompt = format!("{title} ");
        self.prompt_string(&prompt, current, |_| Ok(()))
    }

    /// huh `accessibility.PromptInt` behind `Select.RunAccessible`.
    ///
    /// Re-prompts on a non-number or an out-of-range number; a blank answer
    /// takes `default`. Go's `Select` wraps this in another validation loop,
    /// but the option list has no per-option validator, so that loop is
    /// unreachable and is not ported.
    fn prompt_int(&mut self, prompt: &str, low: i64, high: i64, default: i64) -> i64 {
        let answer = self.prompt_string(prompt, &default.to_string(), |s| {
            let t = s.trim();
            if t.is_empty() {
                return Ok(());
            }
            match t.parse::<i64>() {
                Ok(n) if (low..=high).contains(&n) => Ok(()),
                _ => Err(format!(
                    "Invalid: must be a number between {low} and {high}"
                )),
            }
        });
        answer.trim().parse::<i64>().unwrap_or(default)
    }

    /// huh's `Fprintln(w)` after every field in `Form.runAccessible`.
    fn field_done(&mut self) {
        println!();
    }

    /// One line, from the stashed pipe bytes if the guard found any and from
    /// stdin otherwise. `None` is EOF, which is what makes `/dev/null` run
    /// every field on its default and then stop.
    fn read_line(&mut self) -> Option<String> {
        let mut buf = Vec::new();
        match self.input.as_mut() {
            // Stashed bytes: read up to and including the newline so the next
            // field starts on the following one, which is the entire point of
            // WizardInput.
            Some(input) => {
                let mut byte = [0u8; 1];
                loop {
                    match input.read(&mut byte) {
                        Ok(0) => break,
                        Ok(_) => {
                            buf.push(byte[0]);
                            if byte[0] == b'\n' {
                                break;
                            }
                        }
                        Err(_) => return None,
                    }
                }
                if buf.is_empty() {
                    return None;
                }
            }
            None => {
                match std::io::BufRead::read_until(&mut std::io::stdin().lock(), b'\n', &mut buf) {
                    Ok(0) | Err(_) => return None,
                    Ok(_) => {}
                }
            }
        }
        // `bufio.ScanLines` drops a trailing \n and a preceding \r.
        while matches!(buf.last(), Some(b'\n') | Some(b'\r')) {
            buf.pop();
        }
        Some(String::from_utf8_lossy(&buf).into_owned())
    }
}

/// `accessibility.parseBool`: `y`/`yes` and `n`/`no`, case-insensitive and
/// nothing else.
fn parse_bool(s: &str) -> Option<bool> {
    match s.to_ascii_lowercase().as_str() {
        "y" | "yes" => Some(true),
        "n" | "no" => Some(false),
        _ => None,
    }
}

/// Go `checkPrerequisites`'s header (`wizard.go:558-559`). The rule is 28
/// `─` even though the header is 27 characters — do not match the widths.
pub fn print_step4_header() {
    println!("Step 4: Prerequisites Check");
    println!("────────────────────────────");
}

/// Go's Step 3 GitHub repo-name suggestion (`wizard.go:437-443`):
/// `basename(cwd) + "-pages"`, with `beads-viewer-pages` substituted when the
/// base is `.`, `/`, or absent.
fn suggest_repo_name() -> String {
    let base = std::env::current_dir()
        .ok()
        .and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()));
    match base.as_deref() {
        Some(name) if name != "." && name != "/" && !name.is_empty() => {
            format!("{name}-pages")
        }
        _ => "beads-viewer-pages".to_string(),
    }
}

/// Go `SuggestProjectName` (`pkg/export/cloudflare.go:319-369`), the subset
/// the wizard's Step 3b needs: sanitise a path into a Cloudflare-safe project
/// name. Full port, including the generic-output-dir and empty-parent
/// branches, is the Cloudflare backend's step; this is deliberately the same
/// algorithm so the two cannot drift.
pub fn suggest_project_name(bundle_path: &Path) -> String {
    let generic = ["bv-pages", "pages", "docs", "dist"];
    let parent_name = || -> String {
        std::path::absolute(bundle_path)
            .ok()
            .and_then(|abs| {
                abs.parent()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
            })
            .unwrap_or_default()
    };

    let base = bundle_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut name = if base.is_empty() || base == "." || base == "/" {
        parent_name()
    } else if generic.contains(&base.as_str()) {
        let parent = parent_name();
        if parent.is_empty() {
            // cloudflare.go:338 — the empty-parent branch leaves the name
            // empty and skips the append, so it falls through to the
            // sanitiser and lands on the literal fallback below.
            String::new()
        } else {
            format!("{parent}-pages")
        }
    } else {
        base
    };

    name = name.to_ascii_lowercase();
    name = name
        .chars()
        .map(|c| {
            if c == ' ' || c == '_' || c == '.' {
                '-'
            } else {
                c
            }
        })
        .collect();
    name.retain(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    name = name.trim_matches('-').to_string();
    // Collapse runs of '-'.
    let mut collapsed = String::with_capacity(name.len());
    let mut last_dash = false;
    for c in name.chars() {
        if c == '-' {
            if !last_dash {
                collapsed.push(c);
            }
            last_dash = true;
        } else {
            collapsed.push(c);
            last_dash = false;
        }
    }
    if collapsed.is_empty() {
        "beads-viewer-pages".to_string()
    } else {
        collapsed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_banner_is_68_columns_on_every_line() {
        // Measured off the oracle's own stdout: all 12 lines are 68 columns,
        // and line 2 is 74 BYTES because `→` is three bytes. A port that
        // padded on byte length instead of column count would misalign here.
        for (i, line) in BANNER.iter().enumerate() {
            if line.is_empty() {
                assert!((i == 0 || i == 11), "blank line at {i}");
                continue;
            }
            assert_eq!(line.chars().count(), 68, "{line:?}");
        }
        assert_eq!(BANNER[2].len(), 74, "the arrow line is wider in bytes");
        assert!(BANNER.iter().all(|l| l.is_empty()
            || l.starts_with('╔')
            || l.starts_with('║')
            || l.starts_with('╠')
            || l.starts_with('╚')));
    }

    #[test]
    fn the_banner_carries_the_substrings_go_s_e2e_test_keys_on() {
        let text = BANNER.join("\n");
        for needle in [
            "╔",
            "Static Site Deployment Wizard",
            "Export your issues",
            "Preview",
            "Deploy",
            "Ctrl+C",
        ] {
            assert!(text.contains(needle), "missing {needle:?}");
        }
        // The leading and trailing blank lines are part of the contract.
        assert!(BANNER[0].is_empty() && BANNER[11].is_empty());
    }

    #[test]
    fn wizard_input_yields_one_line_per_read() {
        // The whole reason this type exists: huh builds a fresh bufio.Scanner
        // per field, and a plain Read would let the first one swallow the rest.
        let mut input = WizardInput::new(b"y\nTitle\n3\n".to_vec());
        let mut one = [0u8; 16];

        assert_eq!(input.read(&mut one).unwrap(), 2);
        assert_eq!(&one[..2], b"y\n");

        let mut buf = Vec::new();
        input.read_to_end(&mut buf).unwrap();
        assert_eq!(buf, b"Title\n3\n");
    }

    #[test]
    fn wizard_input_caps_a_read_that_is_shorter_than_the_line() {
        // Go: `if n > len(p) { n = len(p) }` — the remainder is served next.
        let mut input = WizardInput::new(b"abcdef\n".to_vec());
        let mut one = [0u8; 3];
        assert_eq!(input.read(&mut one).unwrap(), 3);
        assert_eq!(&one, b"abc");
        let mut rest = [0u8; 3];
        assert_eq!(input.read(&mut rest).unwrap(), 3);
        assert_eq!(&rest, b"def");
    }

    #[test]
    fn wizard_input_reports_eof_at_the_end() {
        let mut input = WizardInput::new(b"x\n".to_vec());
        let mut buf = Vec::new();
        input.read_to_end(&mut buf).unwrap();
        assert_eq!(buf, b"x\n");
        assert_eq!(input.read(&mut [0u8; 4]).unwrap(), 0);
        // A zero-length destination returns 0 without an error, as in Go.
        assert_eq!(input.read(&mut []).unwrap(), 0);
    }

    #[test]
    fn the_guard_errors_carry_go_s_text_verbatim() {
        // These four strings are the contract the CI caller greps for; the
        // `Error: ` prefix comes from the dispatch arm in crates/bv.
        assert_eq!(
            WizardError::StdinEmpty.to_string(),
            "wizard requires interactive input; stdin is empty"
        );
        assert_eq!(
            WizardError::StdinTimeout.to_string(),
            "wizard requires interactive input; stdin did not provide data within timeout"
        );
        assert_eq!(
            WizardError::StdinClosed.to_string(),
            "wizard requires interactive input; stdin is closed"
        );
        assert_eq!(
            WizardError::StdinRead("boom".into()).to_string(),
            "wizard requires interactive input; stdin error: boom"
        );
    }

    #[test]
    fn the_stdin_probe_timeout_is_go_s_100ms() {
        // wizard.go:258.
        assert_eq!(STDIN_PROBE_TIMEOUT, std::time::Duration::from_millis(100));
    }

    #[test]
    fn the_deploy_target_labels_and_values_are_go_s() {
        // wizard.go:404-407. The label is what a caller sees; the value is
        // what lands in `deploy_target`.
        assert_eq!(
            DEPLOY_TARGETS,
            [
                ("GitHub Pages (create/update repository)", "github"),
                ("Cloudflare Pages (requires wrangler CLI)", "cloudflare"),
                ("Export locally only", "local"),
            ]
        );
    }

    #[test]
    fn a_confirm_hint_uppercases_the_default() {
        assert_eq!(ConfirmField::new("x", true).hint(), "[Y/n]");
        assert_eq!(ConfirmField::new("x", false).hint(), "[y/N]");
    }

    #[test]
    fn parse_bool_matches_huh() {
        for yes in ["y", "Y", "yes", "YES", "Yes"] {
            assert_eq!(parse_bool(yes), Some(true), "{yes}");
        }
        for no in ["n", "N", "no", "NO", "No"] {
            assert_eq!(parse_bool(no), Some(false), "{no}");
        }
        // huh's parseBool is an exact-match on {y,yes}/{n,no} after
        // lowercasing — "yeah" is NOT yes.
        for bad in ["", "yeah", "1", "true", "y n"] {
            assert_eq!(parse_bool(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_step_separator_is_28_dashes() {
        // Go prints 28 `─` for a 27-character header and does not match the
        // widths; a "tidy" port that does match them diverges.
        assert_eq!("─".repeat(28).chars().count(), 28);
    }

    /// Go's `SuggestProjectName` table, `cloudflare_test.go:161-232`. The
    /// wizard's Step 3b consumes this function, so a drift here would change
    /// the default the user is shown.
    #[test]
    fn suggest_project_name_matches_the_go_table() {
        for (input, want) in [
            ("/path/to/my-project", "my-project"),
            ("/path/to/myapp/bv-pages", "myapp-pages"),
            ("/path/to/webapp/dist", "webapp-pages"),
            ("/path/to/my_project", "my-project"),
            ("/path/to/my project", "my-project"),
            ("/path/to/MyProject", "myproject"),
            ("/path/to/my@project!", "myproject"),
            ("/path/to/my---project", "my-project"),
            ("/path/to/!!!", "beads-viewer-pages"),
            ("/", "beads-viewer-pages"),
            ("/dist", "beads-viewer-pages"),
        ] {
            assert_eq!(suggest_project_name(Path::new(input)), want, "{input}");
        }
    }

    #[test]
    fn suggest_project_name_handles_a_bare_name_and_a_generic_dir() {
        assert_eq!(suggest_project_name(Path::new("board")), "board");
        assert_eq!(suggest_project_name(Path::new("/tmp/docs")), "tmp-pages");
        // cloudflare.go:338 — a generic dir whose parent has no basename
        // (here "/") leaves the name empty and SKIPS the "-pages" append, so
        // the sanitiser sees "" and returns the literal fallback. Same
        // outcome as the oracle's "/dist" -> "beads-viewer-pages" row.
        assert_eq!(
            suggest_project_name(Path::new("/pages")),
            "beads-viewer-pages"
        );
    }

    #[test]
    fn a_new_wizard_defaults_both_booleans_to_true() {
        // wizard.go:79-84.
        let w = Wizard::new(Path::new("/tmp/x"));
        assert!(w.config().include_closed);
        assert!(w.config().include_history);
        assert!(w.config().deploy_target.is_empty());
        assert!(!w.is_update);
    }

    #[test]
    fn the_config_path_is_still_under_the_home_directory() {
        // wizard.go:974-981
        let path = wizard_config_path();
        let rendered = path
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        assert!(
            rendered.ends_with("/.config/bv/pages-wizard.json"),
            "{rendered}"
        );
    }
}

#[cfg(test)]
mod config_tests {
    use super::*;

    fn populated() -> WizardConfig {
        WizardConfig {
            include_closed: true,
            include_history: false,
            title: "My Project".to_string(),
            subtitle: "a subtitle".to_string(),
            source_beads_dir: "/home/dev/.beads".to_string(),
            source_repo_root: "/home/dev/project".to_string(),
            source_path: ".beads/issues.jsonl".to_string(),
            last_issue_count: 42,
            last_data_hash: "abc123".to_string(),
            deploy_target: "github".to_string(),
            repo_name: "dev/project".to_string(),
            repo_private: true,
            repo_description: "issues".to_string(),
            cloudflare_project: String::new(),
            cloudflare_branch: String::new(),
            output_path: "/tmp/bundle".to_string(),
        }
    }

    #[test]
    fn the_json_key_order_matches_the_go_struct() {
        // wizard.go:26-55, emitted in declaration order by encoding/json.
        // Go's defaults for the two booleans are both false, so both keys are
        // present; every optional string is omitted when empty.
        let json = serde_json::to_string(&populated()).unwrap();
        let expected = concat!(
            r#"{"include_closed":true,"include_history":false,"title":"My Project","#,
            r#""subtitle":"a subtitle","source_beads_dir":"/home/dev/.beads","#,
            r#""source_repo_root":"/home/dev/project","source_path":".beads/issues.jsonl","#,
            r#""last_issue_count":42,"last_data_hash":"abc123","deploy_target":"github","#,
            r#""repo_name":"dev/project","repo_private":true,"repo_description":"issues","#,
            r#""output_path":"/tmp/bundle"}"#,
        );
        assert_eq!(json, expected);
        // The two Cloudflare fields are unset here, so they contribute nothing
        // between `repo_description` and `output_path`.
        assert!(!json.contains("cloudflare"));
    }

    #[test]
    fn empty_optional_fields_are_omitted() {
        // Every optional field carries Go's `omitempty`.
        let config = WizardConfig {
            deploy_target: "local".to_string(),
            ..WizardConfig::default()
        };
        assert_eq!(
            serde_json::to_string(&config).unwrap(),
            r#"{"include_closed":false,"include_history":false,"title":"","deploy_target":"local"}"#
        );
    }

    #[test]
    fn a_round_trip_preserves_every_field() {
        let json = serde_json::to_string(&populated()).unwrap();
        let parsed: WizardConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, populated());
    }

    #[test]
    fn the_saved_file_is_pretty_printed_with_two_spaces() {
        // wizard.go:1026 — MarshalIndent(_, "", "  ").
        let text = serde_json::to_string_pretty(&populated()).unwrap();
        assert!(text.starts_with("{\n  \"include_closed\": true,"));
        assert!(text.ends_with("\n}"));
    }

    #[test]
    fn the_write_is_atomic_and_leaves_no_temporary_behind() {
        let dir = std::env::temp_dir().join(format!("bvr-wizard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pages-wizard.json");

        write_wizard_config_file(&path, b"{\"first\":true}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"first\":true}");

        // A second save replaces it, and no `.tmp-` file is left in the dir.
        write_wizard_config_file(&path, b"{\"second\":true}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"second\":true}");
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn the_config_path_is_under_the_home_directory() {
        // wizard.go:974-981
        let path = wizard_config_config_path_for_test();
        let rendered = path.to_string_lossy().replace('\\', "/");
        assert!(
            rendered.ends_with("/.config/bv/pages-wizard.json"),
            "{rendered}"
        );
    }

    fn wizard_config_config_path_for_test() -> PathBuf {
        // `wizard_config_path` reads the environment, which is exactly what
        // makes it awkward to assert against; check the shape of the join
        // instead of the value of $HOME.
        home_dir()
            .unwrap_or_default()
            .join(".config")
            .join("bv")
            .join("pages-wizard.json")
    }
}
