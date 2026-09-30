//! Live tracker route + `actions` suggestions — port of Go
//! `pkg/loader/resolveIssueOrigin` + `pkg/model/types.go` (`IssueOrigin`,
//! `IssueCommand`, `Issue.Actions`).
//!
//! Go attaches an `IssueOrigin` to every loaded issue so a robot payload can
//! suggest executable `show`/`claim` commands against the real tracker. The
//! suggestions are advisory: the tracker re-checks claimability when run.

use serde::Serialize;
use std::path::{Path, PathBuf};

/// Go `model.IssueOrigin` (pkg/model/types.go:49).
#[derive(Debug, Clone, Default)]
pub struct IssueOrigin {
    pub local_id: String,
    pub working_directory: String,
    pub tracker_directory: String,
    pub database: String,
    pub tracker: String,
    pub executable: String,
    pub supports_claim: bool,
    pub read_only_reason: String,
}

impl IssueOrigin {
    /// Go `routeAvailable()` (pkg/model/types.go:83).
    pub fn route_available(&self) -> bool {
        !self.executable.is_empty()
            && !self.database.is_empty()
            && !self.working_directory.is_empty()
            && !self.tracker_directory.is_empty()
            && !self.local_id.is_empty()
            && (self.tracker == "br" || self.tracker == "bd")
    }
}

/// Go `model.IssueCommand` (pkg/model/types.go:62).
#[derive(Debug, Clone, Serialize)]
pub struct IssueCommand {
    pub working_directory: String,
    pub argv: Vec<String>,
    pub shell: String,
}

/// Go `model.IssueActions` (pkg/model/types.go:66).
#[derive(Debug, Clone, Default, Serialize)]
pub struct IssueActions {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub working_directory: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub local_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub tracker: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show: Option<IssueCommand>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claim: Option<IssueCommand>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub unavailable_reason: String,
}

/// Go `MutationKind` (pkg/model/types.go:108-116).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationKind {
    AddDependency,
    Relate,
    RemoveDependency,
    AddLabel,
}

/// Go `Issue.MutationAction` (pkg/model/types.go:120-145). Returns the shell
/// command for a tracker mutation, or the reason it cannot be built — which
/// the caller surfaces as the suggestion's `action_unavailable_reason`
/// metadata. The absent-route wording is Go's own, deliberately not reusing
/// `origin.read_only_reason`.
pub fn mutation_action(
    origin: &IssueOrigin,
    kind: MutationKind,
    peer: Option<&IssueOrigin>,
    value: &str,
) -> Result<IssueCommand, String> {
    // Go (types.go:120-145) returns the command, or a reason string. Its
    // wording for an absent route is its own, not whatever the origin
    // recorded, so return it explicitly rather than reusing
    // read_only_reason.
    if !origin.route_available() {
        return Err("source has no verified live tracker route".to_string());
    }
    if !origin.read_only_reason.is_empty() {
        return Err(origin.read_only_reason.clone());
    }
    match kind {
        MutationKind::AddLabel => {
            if value.trim().is_empty() || value.contains('\0') {
                return Err("suggested label is empty or contains a NUL byte".to_string());
            }
            Ok(build_command(
                origin,
                true,
                &[
                    "update",
                    "--json",
                    &format!("--add-label={value}"),
                    "--",
                    &origin.local_id,
                ],
            ))
        }
        MutationKind::AddDependency => {
            let peer =
                peer.ok_or_else(|| "related issue has no verified live tracker route".to_string())?;
            if !peer.route_available() {
                return Err("related issue has no verified live tracker route".to_string());
            }
            if origin.tracker != peer.tracker
                || origin.database != peer.database
                || origin.working_directory != peer.working_directory
                || origin.tracker_directory != peer.tracker_directory
                || origin.executable != peer.executable
            {
                return Err("related issues belong to different trackers".to_string());
            }
            if origin.local_id == peer.local_id {
                return Err("dependency action refers to the same local issue".to_string());
            }
            Ok(build_command(
                origin,
                true,
                &[
                    "dep",
                    "add",
                    "--json",
                    "--",
                    &origin.local_id,
                    &peer.local_id,
                ],
            ))
        }
        MutationKind::Relate => {
            let peer =
                peer.ok_or_else(|| "related issue has no verified live tracker route".to_string())?;
            if !peer.route_available() {
                return Err("related issue has no verified live tracker route".to_string());
            }
            if origin.tracker != peer.tracker
                || origin.database != peer.database
                || origin.working_directory != peer.working_directory
                || origin.tracker_directory != peer.tracker_directory
                || origin.executable != peer.executable
            {
                return Err("related issues belong to different trackers".to_string());
            }
            if origin.local_id == peer.local_id {
                return Err("dependency action refers to the same local issue".to_string());
            }
            Ok(build_command(
                origin,
                true,
                &[
                    "dep",
                    "add",
                    "--json",
                    "--type",
                    "related",
                    "--",
                    &origin.local_id,
                    &peer.local_id,
                ],
            ))
        }
        MutationKind::RemoveDependency => {
            let peer =
                peer.ok_or_else(|| "related issue has no verified live tracker route".to_string())?;
            if !peer.route_available() {
                return Err("related issue has no verified live tracker route".to_string());
            }
            if origin.tracker != peer.tracker
                || origin.database != peer.database
                || origin.working_directory != peer.working_directory
                || origin.tracker_directory != peer.tracker_directory
                || origin.executable != peer.executable
            {
                return Err("related issues belong to different trackers".to_string());
            }
            if origin.local_id == peer.local_id {
                return Err("dependency action refers to the same local issue".to_string());
            }
            Ok(build_command(
                origin,
                true,
                &[
                    "dep",
                    "remove",
                    "--json",
                    "--",
                    &origin.local_id,
                    &peer.local_id,
                ],
            ))
        }
    }
}

/// Go `ShellQuote` (pkg/model/types.go:76) — wrap one literal argument.
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

/// Go `IssueOrigin.command` (pkg/model/types.go:89). Read-only inspection omits
/// `--no-auto-flush`; mutating operations keep normal export behavior.
pub fn build_command(origin: &IssueOrigin, mutating: bool, operation: &[&str]) -> IssueCommand {
    let mut args: Vec<String> = vec![
        "env".to_string(),
        format!("BEADS_DIR={}", origin.tracker_directory),
        format!("BEADS_DB={}", origin.database),
        format!("BD_DB={}", origin.database),
        origin.executable.clone(),
        "--db".to_string(),
        origin.database.clone(),
    ];
    if origin.tracker == "br" {
        args.push("--no-auto-import".to_string());
        if !mutating {
            args.push("--no-auto-flush".to_string());
        }
    }
    for op in operation {
        args.push((*op).to_string());
    }
    let quoted: Vec<String> = args.iter().map(|a| shell_quote(a)).collect();
    IssueCommand {
        working_directory: origin.working_directory.clone(),
        argv: args,
        shell: format!(
            "cd -- {} && {}",
            shell_quote(&origin.working_directory),
            quoted.join(" ")
        ),
    }
}

/// Go `Issue.Actions(claimable)` (pkg/model/types.go:163).
/// Go `Issue.Actions` (types.go:163-167) for an issue with NO tracker origin:
/// it returns immediately with just that reason, before looking at the origin at
/// all.
///
/// The time-travel loader is the case that reaches it — a `--as-of` issue
/// carries no Origin, so Go never derives a local id or reads
/// `.beads/metadata.json`. Rust synthesised an origin anyway and then reported
/// whatever its resolution failure was ("source has no readable tracker
/// metadata"), which named a file problem that does not exist here.
pub fn actions_without_origin() -> IssueActions {
    IssueActions {
        unavailable_reason: "source has no verified live tracker".to_string(),
        ..Default::default()
    }
}

pub fn build_actions(origin: &IssueOrigin, claimable: bool) -> IssueActions {
    let mut actions = IssueActions {
        working_directory: origin.working_directory.clone(),
        local_id: origin.local_id.clone(),
        tracker: origin.tracker.clone(),
        ..Default::default()
    };
    if !origin.route_available() {
        actions.unavailable_reason = if origin.read_only_reason.is_empty() {
            "live tracker route is incomplete".to_string()
        } else {
            origin.read_only_reason.clone()
        };
        return actions;
    }
    actions.show = Some(build_command(
        origin,
        false,
        &["show", "--json", "--", &origin.local_id],
    ));
    if !origin.read_only_reason.is_empty() {
        actions.unavailable_reason = origin.read_only_reason.clone();
    } else if !claimable {
        actions.unavailable_reason = "snapshot does not establish claim readiness".to_string();
    } else if !origin.supports_claim {
        actions.unavailable_reason =
            "installed tracker does not advertise atomic --claim".to_string();
    } else {
        actions.claim = Some(build_command(
            origin,
            true,
            &["update", "--json", "--claim", "--", &origin.local_id],
        ));
    }
    actions
}

/// Go `installedTrackerCapabilities` (pkg/loader/loader.go:47) — probe the
/// tracker for the flags the explicit-database route depends on.
#[derive(Debug, Clone)]
pub struct TrackerCapabilities {
    pub executable: String,
    pub claim: bool,
    pub error: String,
}

/// Resolve the `br` executable and detect atomic-claim support. Runs only
/// `update --help`, never a command that opens or mutates a tracker.
///
/// Go bounds the probe with a 2s context and caches on the executable's
/// identity (`path:size:mtime`) so a long-running TUI neither re-spawns the
/// tracker on every payload nor keeps a stale answer after it is replaced.
pub fn installed_tracker_capabilities(tracker: &str) -> TrackerCapabilities {
    let path = match lookup_path(tracker) {
        Some(p) => p,
        None => {
            return TrackerCapabilities {
                executable: String::new(),
                claim: false,
                error: format!("tracker executable is unavailable: {tracker}"),
            }
        }
    };
    let executable = resolve_executable(&path);
    // Go `os.Stat` is part of capability identity: replacing the installed
    // binary must invalidate the cached answer.
    let meta = match std::fs::metadata(&executable) {
        Ok(m) => m,
        Err(_) => {
            return TrackerCapabilities {
                executable: String::new(),
                claim: false,
                error: "cannot inspect tracker executable".to_string(),
            }
        }
    };
    let key = format!(
        "{}:{}:{}",
        executable.to_string_lossy(),
        meta.len(),
        meta_modtime_nanos(&meta)
    );
    if let Some(hit) = capability_cache_get(&key) {
        return hit;
    }
    let caps = probe_tracker(&executable, tracker);
    capability_cache_put(key, &caps);
    caps
}

/// Go's 2s bound on the help probe. Without it a wedged tracker hangs the
/// whole `bv` invocation, which is the failure this gate is meant to survive.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

fn probe_tracker(executable: &Path, tracker: &str) -> TrackerCapabilities {
    let child = std::process::Command::new(executable)
        .args(["update", "--help"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn();
    let mut out: Option<(std::process::ExitStatus, Vec<u8>)> = None;
    if let Ok(mut c) = child {
        // Poll for the deadline rather than blocking on `wait`, so a tracker
        // that never exits is killed instead of hanging bv.
        let deadline = std::time::Instant::now() + PROBE_TIMEOUT;
        loop {
            match c.try_wait() {
                Ok(Some(status)) => {
                    let mut buf = Vec::new();
                    use std::io::Read;
                    if let Some(mut o) = c.stdout.take() {
                        let _ = o.read_to_end(&mut buf);
                    }
                    out = Some((status, buf));
                    break;
                }
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Ok(None) => {
                    let _ = c.kill();
                    let _ = c.wait();
                    break;
                }
                Err(_) => break,
            }
        }
    }
    let mut caps = TrackerCapabilities {
        executable: executable.to_string_lossy().to_string(),
        claim: false,
        error: String::new(),
    };
    match out {
        Some((status, buf)) if status.success() => {
            // Trackers may honor inherited forced-color settings even when
            // help is piped, so styling must not change token recognition
            // (Go applies ansi.Strip for the same reason).
            let help = String::from_utf8_lossy(&buf);
            let stripped = strip_ansi(&help);
            let has = |flag: &str| stripped.split_whitespace().any(|f| f == flag);
            if !has("--db")
                || !has("--json")
                || (tracker == "br" && (!has("--no-auto-import") || !has("--no-auto-flush")))
            {
                caps.error = "installed tracker cannot bind the required explicit database route"
                    .to_string();
            }
            caps.claim = has("--claim");
        }
        Some(_) => caps.error = "cannot establish installed tracker capabilities".to_string(),
        None => caps.error = "cannot establish installed tracker capabilities".to_string(),
    }
    caps
}

/// Modification time as Unix nanoseconds, for the capability cache key.
/// Go uses `info.ModTime().UnixNano()`.
fn meta_modtime_nanos(meta: &std::fs::Metadata) -> u128 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Remove ANSI SGR/CSI sequences so forced-color output does not hide a
/// capability token. Mirrors what Go's `ansi.Strip` does to the help text.
fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            // CSI sequence: ESC '[' params(0x30-0x3f) intermediates(0x20-0x2f) final
            Some('[') => {
                chars.next();
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            // OSC: ESC ']' ... BEL or ESC '\'
            Some(']') => {
                chars.next();
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {
                chars.next();
            }
        }
    }
    out
}

/// Go's `trackerCapabilityCache` (a `sync.Map` keyed by executable identity).
static CAPABILITY_CACHE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, TrackerCapabilities>>,
> = std::sync::OnceLock::new();

fn capability_cache(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, TrackerCapabilities>> {
    CAPABILITY_CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn capability_cache_get(key: &str) -> Option<TrackerCapabilities> {
    let map = capability_cache().lock().ok()?;
    map.get(key).cloned()
}

fn capability_cache_put(key: String, caps: &TrackerCapabilities) {
    if let Ok(mut map) = capability_cache().lock() {
        map.insert(key, caps.clone());
    }
}

/// Go `exec.LookPath` — resolve a bare command name against `PATH`.
///
/// On Windows this must honour `PATHEXT`. Go's `LookPath` tries each directory
/// with each extension the environment declares (`.COM`, `.EXE`, …), so
/// `br` resolves to `br.exe`; joining the bare name instead finds nothing, and
/// the tracker then reports as unavailable even though it is installed. That
/// silently blanks the `claim`/`show` argv on every recommendation.
fn lookup_path(name: &str) -> Option<PathBuf> {
    if name.contains('/') || name.contains('\\') {
        let p = Path::new(name);
        return if p.is_file() {
            Some(p.to_path_buf())
        } else {
            None
        };
    }
    let path_var = std::env::var_os("PATH")?;
    let extensions = executable_extensions(name);
    for dir in std::env::split_paths(&path_var) {
        for ext in &extensions {
            let candidate = dir.join(format!("{name}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// The suffixes to try for a bare command name, most specific first.
///
/// A name that already carries an extension is used verbatim. Otherwise the
/// bare name is tried first (correct on Unix, and on Windows for a
/// extensionless binary), followed by each `PATHEXT` entry.
fn executable_extensions(name: &str) -> Vec<String> {
    if Path::new(name).extension().is_some() {
        return vec![String::new()];
    }
    let mut out = vec![String::new()];
    if cfg!(windows) {
        let pathext =
            std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
        for ext in pathext.split(';').map(str::trim).filter(|e| !e.is_empty()) {
            out.push(ext.to_string());
        }
    }
    out
}

/// Canonicalize a path for embedding in emitted argv, stripping the Windows
/// verbatim prefix.
///
/// `std::fs::canonicalize` returns the extended-length form
/// (`\\?\C:\Users\...`) on Windows. Go's `filepath.Abs` never produces that
/// prefix, so leaving it in makes every `working_directory`, `BEADS_DIR` and
/// tracker `argv` entry differ from Go's by three characters — and a shell
/// command that works from Go's form can be handed a form no shell parses the
/// same way. The prefix is only a Win32 API detail, so it is dropped here
/// rather than at every call site.
fn resolve_executable(path: &Path) -> PathBuf {
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    #[cfg(windows)]
    {
        let text = resolved.to_string_lossy();
        if let Some(rest) = text.strip_prefix(r"\\?\") {
            // Strip the verbatim marker. For a UNC target the remainder is
            // `\server\share\...`, which is the form Go prints.
            if !rest.starts_with("UNC\\") {
                return PathBuf::from(rest);
            }
            return PathBuf::from(format!(r"\\{}", &rest[4..]));
        }
    }
    resolved
}

/// Go `resolveIssueOrigin(sourcePath)` (pkg/loader/loader.go:157) — bind the
/// loaded JSONL to a real tracker route, or explain why it cannot.
/// Go `IsBDWorkspace` (pkg/loader/loader.go:106) — a bd (Dolt) workspace, not
/// a br one. bd keeps its data under `.beads/dolt/` (server mode) or
/// `.beads/embeddeddolt/` (embedded, the bd 1.1+ default), and may also say
/// so in `metadata.json`.
pub fn is_bd_workspace(beads_dir: &Path) -> bool {
    if beads_dir.as_os_str().is_empty() {
        return false;
    }
    for dir in ["dolt", "embeddeddolt"] {
        if let Ok(info) = std::fs::metadata(beads_dir.join(dir)) {
            if info.is_dir() {
                return true;
            }
        }
    }
    let Ok(data) = std::fs::read_to_string(beads_dir.join("metadata.json")) else {
        return false;
    };
    let Ok(meta) = serde_json::from_str::<serde_json::Value>(&data) else {
        return false;
    };
    meta.get("backend")
        .and_then(|b| b.as_str())
        .unwrap_or("")
        .trim()
        .eq_ignore_ascii_case("dolt")
}

/// Go `resolveIssueOrigin(sourcePath)` (pkg/loader/loader.go:157) — bind the
/// loaded JSONL to a real tracker route, or explain why it cannot.
///
/// The binding is deliberately strict: the source path must be the database or
/// the export **the tracker metadata itself declares**. An arbitrary `--db`
/// JSONL/SQLite input stays readable but may not borrow a nearby tracker just
/// by containing matching IDs (Go `AttachIssueOrigins`, loader.go:89).
pub fn resolve_issue_origin(source_path: &str, local_id: &str) -> IssueOrigin {
    let mut origin = IssueOrigin {
        local_id: local_id.to_string(),
        ..Default::default()
    };
    let refuse = |origin: &mut IssueOrigin, reason: &str| -> IssueOrigin {
        origin.read_only_reason = reason.to_string();
        origin.clone()
    };

    let path = resolve_executable(&PathBuf::from(source_path));
    let Some(beads_dir) = path.parent() else {
        return refuse(
            &mut origin,
            "source path cannot be resolved to a live tracker",
        );
    };
    let meta_raw = match std::fs::read_to_string(beads_dir.join("metadata.json")) {
        Ok(m) => m,
        Err(_) => return refuse(&mut origin, "source has no readable tracker metadata"),
    };
    let meta: serde_json::Value = match serde_json::from_str(&meta_raw) {
        Ok(m) => m,
        Err(_) => return refuse(&mut origin, "source tracker metadata is invalid"),
    };
    let backend = meta
        .get("backend")
        .and_then(|b| b.as_str())
        .unwrap_or("")
        .trim()
        .to_lowercase();
    if !matches!(backend.as_str(), "" | "sqlite" | "dolt") {
        return refuse(
            &mut origin,
            &format!("unsupported tracker backend: {backend}"),
        );
    }

    // Go `filepath.EvalSymlinks` on a metadata-declared name, resolved against
    // the beads directory.
    let resolve_name = |name: &str| -> Option<PathBuf> {
        if name.is_empty() {
            return None;
        }
        let p = Path::new(name);
        let joined = if p.is_absolute() {
            p.to_path_buf()
        } else {
            beads_dir.join(p)
        };
        // Normalized exactly as `path` is, and that is the whole point: the
        // anti-hijack comparison below is `path != database && Some(&path) !=
        // export`, a byte comparison of two PathBufs. A bare `canonicalize`
        // here returns a `\\?\` verbatim path on Windows while
        // `resolve_executable` strips that prefix from `path`, so on Windows
        // the two sides were never equal and the tracker refused its own
        // metadata-declared live export with "source is not the
        // metadata-declared live database or export".
        std::fs::canonicalize(&joined)
            .ok()
            .map(|resolved| resolve_executable(&resolved))
    };

    origin.tracker_directory = beads_dir.to_string_lossy().to_string();
    origin.working_directory = beads_dir
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    origin.tracker = "br".to_string();

    if is_bd_workspace(beads_dir) {
        origin.tracker = "bd".to_string();
        // The bd bridge specifically exports issues.jsonl from its Dolt
        // directory; an unrelated sidecar file is not that live source.
        if path.file_name().and_then(|n| n.to_str()) != Some("issues.jsonl") {
            return refuse(
                &mut origin,
                "source is not the live bd compatibility export",
            );
        }
        origin.database = beads_dir.to_string_lossy().to_string();
    } else {
        let Some(database) =
            resolve_name(meta.get("database").and_then(|d| d.as_str()).unwrap_or(""))
        else {
            return refuse(
                &mut origin,
                "metadata does not resolve to an existing tracker database",
            );
        };
        let is_regular = std::fs::metadata(&database)
            .map(|m| m.is_file())
            .unwrap_or(false);
        if !is_regular {
            return refuse(&mut origin, "metadata database is not a regular file");
        }
        // Anti-hijack: the loaded file must be the declared database or its
        // declared JSONL export. Otherwise any JSONL next to a live tracker
        // could be presented as that tracker's data.
        let export = resolve_name(
            meta.get("jsonl_export")
                .and_then(|d| d.as_str())
                .unwrap_or(""),
        );
        if path != database && Some(&path) != export.as_ref() {
            return refuse(
                &mut origin,
                "source is not the metadata-declared live database or export",
            );
        }
        origin.database = database.to_string_lossy().to_string();
    }

    let caps = installed_tracker_capabilities(&origin.tracker);
    if !caps.error.is_empty() {
        // Go returns `refuse(caps.Error)` immediately (pkg/loader/loader.go:168-170),
        // leaving `Executable` empty so `routeAvailable()` is false and the
        // payload carries no show/claim command. Setting only the reason and
        // still assigning the executable would emit live commands against a
        // tracker that just failed its capability probe.
        return refuse(&mut origin, &caps.error);
    }
    origin.executable = caps.executable;
    origin.supports_claim = caps.claim;
    origin
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin() -> IssueOrigin {
        IssueOrigin {
            local_id: "abc-1".into(),
            working_directory: "/repo".into(),
            tracker_directory: "/repo/.beads".into(),
            database: "/repo/.beads/beads.db".into(),
            tracker: "br".into(),
            executable: "/usr/local/bin/br".into(),
            supports_claim: true,
            read_only_reason: String::new(),
        }
    }

    #[test]
    fn show_command_argv_matches_go() {
        let cmd = build_command(&origin(), false, &["show", "--json", "--", "abc-1"]);
        assert_eq!(
            cmd.argv,
            vec![
                "env",
                "BEADS_DIR=/repo/.beads",
                "BEADS_DB=/repo/.beads/beads.db",
                "BD_DB=/repo/.beads/beads.db",
                "/usr/local/bin/br",
                "--db",
                "/repo/.beads/beads.db",
                "--no-auto-import",
                "--no-auto-flush",
                "show",
                "--json",
                "--",
                "abc-1",
            ]
        );
    }

    #[test]
    fn claim_command_omits_no_auto_flush() {
        let cmd = build_command(
            &origin(),
            true,
            &["update", "--json", "--claim", "--", "abc-1"],
        );
        assert!(!cmd.argv.contains(&"--no-auto-flush".to_string()));
        assert!(cmd.argv.contains(&"--claim".to_string()));
    }

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("a'b"), "'a'\"'\"'b'");
        assert_eq!(shell_quote("plain"), "'plain'");
    }

    #[test]
    fn not_claimable_yields_reason_and_no_claim() {
        let a = build_actions(&origin(), false);
        assert!(a.claim.is_none());
        assert!(a.show.is_some());
        assert_eq!(
            a.unavailable_reason,
            "snapshot does not establish claim readiness"
        );
    }

    #[test]
    fn claimable_emits_both_commands() {
        let a = build_actions(&origin(), true);
        assert!(a.claim.is_some());
        assert!(a.show.is_some());
        assert!(a.unavailable_reason.is_empty());
    }

    #[test]
    fn incomplete_route_reports_reason() {
        let mut o = origin();
        o.executable = String::new();
        let a = build_actions(&o, true);
        assert!(a.show.is_none());
        assert_eq!(a.unavailable_reason, "live tracker route is incomplete");
    }

    /// A tracker that fails its capability probe must not yield executable
    /// show/claim commands. Go returns early on `caps.Error`
    /// (pkg/loader/loader.go:168-170), leaving `Executable` empty.
    #[test]
    fn failed_capability_probe_blocks_commands() {
        let mut o = origin();
        o.executable = String::new();
        o.read_only_reason =
            "installed tracker cannot bind the required explicit database route".to_string();
        assert!(!o.route_available(), "failed probe must not be routable");
        let a = build_actions(&o, true);
        assert!(a.show.is_none(), "no show command for an unroutable origin");
        assert!(
            a.claim.is_none(),
            "no claim command for an unroutable origin"
        );
        assert!(!a.unavailable_reason.is_empty());
    }
}

#[cfg(test)]
mod probe_tests {
    use super::*;

    /// A tracker that colorizes its help must still be recognized: Go strips
    /// ANSI before matching tokens, and we must too or `--claim` is missed
    /// whenever the environment forces color.
    #[test]
    fn ansi_strip_removes_sgr_and_csi() {
        let colored = "\u{1b}[1m--db\u{1b}[0m \u{1b}[32m--json\u{1b}[0m";
        assert_eq!(strip_ansi(colored), "--db --json");
    }

    #[test]
    fn ansi_strip_handles_osc_and_keeps_plain_text() {
        assert_eq!(strip_ansi("\u{1b}]0;title\u{7}--claim"), "--claim");
        assert_eq!(strip_ansi("plain --db text"), "plain --db text");
    }

    #[test]
    fn colorized_help_still_matches_capability_tokens() {
        // The exact shape a forced-color tracker emits around a flag.
        let help = "\u{1b}[36m  --db\u{1b}[0m <path>\n  --json\n  --claim\n";
        let stripped = strip_ansi(help);
        let has = |f: &str| stripped.split_whitespace().any(|x| x == f);
        assert!(has("--db") && has("--json") && has("--claim"));
    }

    #[test]
    fn capability_cache_roundtrips() {
        let caps = TrackerCapabilities {
            executable: "/usr/local/bin/br".into(),
            claim: true,
            error: String::new(),
        };
        let key = "test-key".to_string();
        capability_cache_put(key.clone(), &caps);
        let got = capability_cache_get(&key).expect("cached");
        assert!(got.claim);
        assert_eq!(got.executable, "/usr/local/bin/br");
    }

    #[test]
    fn missing_executable_reports_unavailable() {
        let caps = installed_tracker_capabilities("definitely-not-a-real-tracker-xyz");
        assert!(!caps.executable.is_empty() || !caps.error.is_empty());
        assert!(caps.error.contains("unavailable"), "{}", caps.error);
    }
}

#[cfg(test)]
mod resolution_tests {
    use super::*;

    fn write_workspace(dir: &Path, meta: &str, files: &[&str]) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("metadata.json"), meta).unwrap();
        for f in files {
            std::fs::write(dir.join(f), "x").unwrap();
        }
    }

    /// A JSONL that is neither the declared database nor the declared export
    /// must not borrow the nearby tracker — Go refuses it
    /// (pkg/loader/loader.go:161) so an arbitrary `--db` input cannot be
    /// presented as that tracker's live data.
    #[test]
    fn foreign_source_is_refused() {
        let root = std::env::temp_dir().join("bv-tracker-foreign");
        let beads = root.join(".beads");
        let _ = std::fs::remove_dir_all(&root);
        write_workspace(
            &beads,
            r#"{"database":"beads.db","jsonl_export":"issues.jsonl"}"#,
            &["beads.db", "issues.jsonl", "somebody-elses.jsonl"],
        );
        let o = resolve_issue_origin(beads.join("somebody-elses.jsonl").to_str().unwrap(), "X-1");
        assert!(
            o.read_only_reason.contains("not the metadata-declared"),
            "{}",
            o.read_only_reason
        );
        assert!(!o.route_available());
    }

    /// The declared export itself is the live source and must resolve.
    #[test]
    fn declared_export_is_accepted() {
        let root = std::env::temp_dir().join("bv-tracker-export");
        let _ = std::fs::remove_dir_all(&root);
        let beads = root.join(".beads");
        write_workspace(
            &beads,
            r#"{"database":"beads.db","jsonl_export":"issues.jsonl"}"#,
            &["beads.db", "issues.jsonl"],
        );
        let o = resolve_issue_origin(beads.join("issues.jsonl").to_str().unwrap(), "X-1");
        // No refusal about the source binding; only the tracker probe may fail
        // here, because the test machine has no `br` on PATH.
        assert!(
            !o.read_only_reason.contains("metadata-declared"),
            "{}",
            o.read_only_reason
        );
        assert_eq!(o.tracker, "br");
    }

    /// A missing database is refused rather than silently binding the dir.
    #[test]
    fn missing_database_is_refused() {
        let root = std::env::temp_dir().join("bv-tracker-nodb");
        let _ = std::fs::remove_dir_all(&root);
        let beads = root.join(".beads");
        write_workspace(
            &beads,
            r#"{"database":"absent.db","jsonl_export":"issues.jsonl"}"#,
            &["issues.jsonl"],
        );
        let o = resolve_issue_origin(beads.join("issues.jsonl").to_str().unwrap(), "X-1");
        assert!(
            o.read_only_reason.contains("does not resolve")
                || o.read_only_reason.contains("regular file"),
            "{}",
            o.read_only_reason
        );
    }

    #[test]
    fn bd_workspace_detected_from_dolt_dir() {
        let root = std::env::temp_dir().join("bv-tracker-bd");
        let _ = std::fs::remove_dir_all(&root);
        let beads = root.join(".beads");
        std::fs::create_dir_all(beads.join("dolt")).unwrap();
        std::fs::write(beads.join("metadata.json"), r#"{"backend":"dolt"}"#).unwrap();
        std::fs::write(beads.join("issues.jsonl"), "x").unwrap();
        assert!(is_bd_workspace(&beads));
        let o = resolve_issue_origin(beads.join("issues.jsonl").to_str().unwrap(), "X-1");
        assert_eq!(o.tracker, "bd");
        // `beads` comes from a temp path that may be a symlink (macOS
        // /var -> /private/var); the origin records the resolved directory.
        // `resolve_executable` rather than a bare `canonicalize`, because the
        // latter yields a `\\?\` verbatim path on Windows while the origin
        // stores the stripped form.
        let expected = resolve_executable(&beads);
        assert_eq!(o.database, expected.to_string_lossy());
    }

    /// bd exports issues.jsonl specifically; any other file is not its live
    /// source even inside a bd workspace.
    #[test]
    fn bd_rejects_non_export_file() {
        let root = std::env::temp_dir().join("bv-tracker-bd2");
        let _ = std::fs::remove_dir_all(&root);
        let beads = root.join(".beads");
        std::fs::create_dir_all(beads.join("dolt")).unwrap();
        std::fs::write(beads.join("metadata.json"), r#"{"backend":"dolt"}"#).unwrap();
        std::fs::write(beads.join("other.jsonl"), "x").unwrap();
        let o = resolve_issue_origin(beads.join("other.jsonl").to_str().unwrap(), "X-1");
        assert!(
            o.read_only_reason.contains("bd compatibility export"),
            "{}",
            o.read_only_reason
        );
    }

    #[test]
    fn unsupported_backend_refused() {
        let root = std::env::temp_dir().join("bv-tracker-bad");
        let _ = std::fs::remove_dir_all(&root);
        let beads = root.join(".beads");
        write_workspace(&beads, r#"{"backend":"postgres"}"#, &["issues.jsonl"]);
        let o = resolve_issue_origin(beads.join("issues.jsonl").to_str().unwrap(), "X-1");
        assert!(
            o.read_only_reason.contains("unsupported tracker backend"),
            "{}",
            o.read_only_reason
        );
    }
}
