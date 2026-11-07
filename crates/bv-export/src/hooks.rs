//! Export hooks — port of Go `pkg/hooks` at the frozen parity commit `18afafa`.
//!
//! Go splits the package in two: `config.go` owns *loading* `.bv/hooks.yaml`
//! (including defaulting, validation and the accumulated warnings) and
//! `executor.go` owns *running* the hooks. Both halves are reproduced here;
//! the previous revision of this file only carried the data types, so nothing
//! ever read a hooks file or spawned a hook — meaning even the default
//! (no `--no-hooks`) export path diverged from Go, which always runs the
//! pre/post-export phases.
//!
//! Two behaviours are easy to miss and are reproduced deliberately:
//!
//! * `on_error` defaults **by phase** — `fail` for `pre-export`, `continue`
//!   for `post-export` (`config.go:154-159`). A single unconditional default is
//!   wrong: it turns a logging-only post-export hook into a hard failure.
//! * The subprocess environment is **scrubbed** of credential-bearing
//!   variables (`executor.go:186-219`) before the context and hook variables
//!   are added, because `.bv/hooks.yaml` can come from an untrusted checkout.
//!   A hook re-grants a credential explicitly through
//!   `env: { GITHUB_TOKEN: "${GITHUB_TOKEN}" }`.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Go `DefaultTimeout` (`config.go:85`).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Go `HookPhase` (`config.go:18-24`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HookPhase {
    /// Runs before export generation; failure cancels the export.
    PreExport,
    /// Runs after the export is written; failure is reported but does not
    /// break the export.
    PostExport,
}

impl HookPhase {
    /// The YAML key, and the prefix Go gives a generated hook name.
    pub fn as_str(self) -> &'static str {
        match self {
            HookPhase::PreExport => "pre-export",
            HookPhase::PostExport => "post-export",
        }
    }
}

impl std::fmt::Display for HookPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Go `Hook.OnError` after normalisation (`config.go:31`, values set at
/// `config.go:139-148`).
///
/// Deliberately not `Deserialize`: on the wire this is a free-form string
/// whose default depends on the phase and whose invalid values produce a
/// warning rather than a parse error, none of which a derived impl can
/// express. [`normalize_hooks`] does it instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnError {
    Fail,
    Continue,
}

impl OnError {
    pub fn as_str(self) -> &'static str {
        match self {
            OnError::Fail => "fail",
            OnError::Continue => "continue",
        }
    }
}

/// Go's `defaultOnError` (`config.go:154-159`): the default is a function of
/// the phase, not a constant.
fn default_on_error(phase: HookPhase) -> OnError {
    match phase {
        HookPhase::PreExport => OnError::Fail,
        HookPhase::PostExport => OnError::Continue,
    }
}

/// Go `Hook` (`config.go:26-33`) after [`Loader::load`] has defaulted and
/// validated it. `timeout` is always non-zero once loaded: Go applies
/// `DefaultTimeout` in `normalizeHooks` (`config.go:135-137`).
#[derive(Debug, Clone)]
pub struct Hook {
    pub name: String,
    pub command: String,
    pub timeout: Duration,
    pub env: BTreeMap<String, String>,
    pub on_error: OnError,
}

/// Go `HooksByPhase` (`config.go:40-43`).
#[derive(Debug, Clone, Default)]
pub struct HooksByPhase {
    pub pre_export: Vec<Hook>,
    pub post_export: Vec<Hook>,
}

/// Go `Config` (`config.go:36-38`).
#[derive(Debug, Clone, Default)]
pub struct HooksConfig {
    pub hooks: HooksByPhase,
}

/// Errors surfaced by loading or running hooks. The `Display` text reproduces
/// Go's error strings so a caller can print them unchanged.
#[derive(Debug, thiserror::Error)]
pub enum HooksError {
    /// `config.go:118` — `reading hooks config: %w`.
    #[error("reading hooks config: {0}")]
    Read(#[source] std::io::Error),
    /// `config.go:123` — `parsing %s: %w`.
    #[error("parsing {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: serde_yaml_ng::Error,
    },
    /// `config.go:213` / `:222` — `invalid timeout %q: ...`.
    #[error("invalid timeout {raw:?}: {reason}")]
    Timeout { raw: String, reason: String },
    /// `executor.go:298` — `loading hooks: %w`, raised by [`run_hooks`].
    #[error("loading hooks: {0}")]
    LoadFailed(#[source] Box<HooksError>),
    /// `executor.go:66` — `pre-export hook %q failed: %w`.
    #[error("pre-export hook {name:?} failed: {source}")]
    PreExportFailed {
        name: String,
        #[source]
        source: HookRunError,
    },
    /// `executor.go:81` — `post-export hook %q failed: %w`.
    #[error("post-export hook {name:?} failed: {source}")]
    PostExportFailed {
        name: String,
        #[source]
        source: HookRunError,
    },
}

/// Why a single hook invocation failed. Go stores this on
/// `HookResult.Error` and wraps it in the phase error.
#[derive(Debug, thiserror::Error)]
pub enum HookRunError {
    /// `executor.go:150` — `timeout after %v`.
    #[error("timeout after {}", go_duration_string(*.0))]
    Timeout(Duration),
    /// The process could not be started, or `cmd.Run()` returned a non-nil
    /// error that was not a deadline.
    #[error("{0}")]
    Spawn(#[source] std::io::Error),
    /// A non-zero exit status. Go surfaces this as `exit status N` from
    /// `os/exec`; `Exit` keeps the code so a caller can match on it.
    #[error("exit status {0}")]
    Exit(i32),
}

impl Clone for HookRunError {
    /// `std::io::Error` is not `Clone`, so a re-created error keeps the kind
    /// and the rendered message — everything a caller can act on.
    fn clone(&self) -> Self {
        match self {
            HookRunError::Timeout(d) => HookRunError::Timeout(*d),
            HookRunError::Spawn(e) => {
                HookRunError::Spawn(std::io::Error::new(e.kind(), e.to_string()))
            }
            HookRunError::Exit(code) => HookRunError::Exit(*code),
        }
    }
}

/// Go `Loader` (`config.go:88-90`) plus its `Load`/`Config`/`HasHooks`/
/// `GetHooks`/`Warnings` methods (`config.go:110-172`).
#[derive(Debug)]
pub struct Loader {
    project_dir: PathBuf,
    config: Option<HooksConfig>,
    warnings: Vec<String>,
}

impl Loader {
    /// Go `NewLoader(WithProjectDir(dir))` (`config.go:96-108`). An empty
    /// `dir` falls back to the current directory, which is what Go's
    /// `os.Getwd()` default does.
    pub fn new(dir: impl AsRef<Path>) -> Self {
        let mut project_dir = dir.as_ref().to_path_buf();
        if project_dir.as_os_str().is_empty() {
            project_dir = std::env::current_dir().unwrap_or_default();
        }
        Self {
            project_dir,
            config: None,
            warnings: Vec::new(),
        }
    }

    /// Go `Loader.Load` (`config.go:110-128`).
    ///
    /// A missing `.bv/hooks.yaml` is not an error — it means "no hooks" — and
    /// yields an empty config. Every other read failure is `reading hooks
    /// config: …`.
    pub fn load(&mut self) -> Result<(), HooksError> {
        self.warnings.clear();
        let config_path = self.project_dir.join(".bv").join("hooks.yaml");

        let data = match std::fs::read_to_string(&config_path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // No config file means no hooks — this is OK.
                self.config = Some(HooksConfig::default());
                return Ok(());
            }
            Err(e) => return Err(HooksError::Read(e)),
        };

        let mut root: ConfigDto =
            serde_yaml_ng::from_str(&data).map_err(|source| HooksError::Parse {
                path: config_path.display().to_string(),
                source,
            })?;

        let mut warnings = Vec::new();
        let by_phase = std::mem::take(&mut root.hooks);
        let pre_export = normalize_hooks(by_phase.pre_export, HookPhase::PreExport, &mut warnings)?;
        let post_export =
            normalize_hooks(by_phase.post_export, HookPhase::PostExport, &mut warnings)?;

        self.warnings = warnings;
        self.config = Some(HooksConfig {
            hooks: HooksByPhase {
                pre_export,
                post_export,
            },
        });
        Ok(())
    }

    /// Go `Loader.Config` (`config.go:175-180`).
    pub fn config(&self) -> HooksConfig {
        self.config.clone().unwrap_or_default()
    }

    /// Go `Loader.HasHooks` (`config.go:182-188`).
    pub fn has_hooks(&self) -> bool {
        match &self.config {
            None => false,
            Some(c) => !c.hooks.pre_export.is_empty() || !c.hooks.post_export.is_empty(),
        }
    }

    /// Go `Loader.GetHooks` (`config.go:190-202`).
    pub fn hooks(&self, phase: HookPhase) -> &[Hook] {
        match &self.config {
            None => &[],
            Some(c) => match phase {
                HookPhase::PreExport => &c.hooks.pre_export,
                HookPhase::PostExport => &c.hooks.post_export,
            },
        }
    }

    /// Go `Loader.Warnings` (`config.go:204-206`).
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
}

/// Go `Config` on the wire (`config.go:36-43`).
#[derive(Debug, Default, Deserialize)]
struct ConfigDto {
    #[serde(default)]
    hooks: HooksByPhaseDto,
}

#[derive(Debug, Default, Deserialize)]
struct HooksByPhaseDto {
    #[serde(default, rename = "pre-export")]
    pre_export: Vec<HookDto>,
    #[serde(default, rename = "post-export")]
    post_export: Vec<HookDto>,
}

/// Go's `hookDTO` (`config.go:199-205`) — deliberately mirrors the Go comment
/// there: `Timeout` is a *string* on the wire, because `timeout: 30` is the
/// common spelling and yaml.v3 happily puts a scalar's text into a string
/// field. `serde_yaml_ng` is stricter, so [`de_scalar_text`] reproduces the
/// permissive read.
#[derive(Debug, Default, Deserialize)]
struct HookDto {
    #[serde(default)]
    name: String,
    #[serde(default)]
    command: String,
    #[serde(default, deserialize_with = "de_scalar_text")]
    timeout: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default, rename = "on_error")]
    on_error: String,
}

/// Read any YAML scalar as its textual form, matching yaml.v3's behaviour of
/// assigning the node's raw value to a `string` field. An explicit `null`
/// reads as absent, which Go also treats as "leave the zero value".
fn de_scalar_text<'de, D>(d: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;
    let value = serde_yaml_ng::Value::deserialize(d)?;
    Ok(match value {
        serde_yaml_ng::Value::Null => None,
        serde_yaml_ng::Value::Bool(b) => Some(b.to_string()),
        serde_yaml_ng::Value::Number(n) => Some(n.to_string()),
        serde_yaml_ng::Value::String(s) => Some(s),
        other => {
            return Err(D::Error::custom(format!(
                "cannot read {other:?} as a timeout string"
            )))
        }
    })
}

/// Go `normalizeHooks` (`config.go:132-152`): drop empty commands, apply the
/// timeout and `on_error` defaults, name unnamed hooks, and accumulate
/// warnings.
fn normalize_hooks(
    hooks: Vec<HookDto>,
    phase: HookPhase,
    warnings: &mut Vec<String>,
) -> Result<Vec<Hook>, HooksError> {
    let mut out = Vec::with_capacity(hooks.len());
    for (i, dto) in hooks.into_iter().enumerate() {
        // 1-based position in the *file*, matching Go's `i+1` even though
        // earlier entries may have been skipped.
        let ordinal = i + 1;
        if dto.command.trim().is_empty() {
            warnings.push(format!(
                "{phase} hook {ordinal} has empty command; skipping"
            ));
            continue;
        }
        let timeout = parse_go_timeout(dto.timeout.as_deref().unwrap_or(""))?;
        // config.go:135-137 — a zero timeout means "unset", not "no limit".
        // This has to happen here rather than through a serde default: a
        // config that spells out `timeout: 0` means the same thing in Go, and
        // only a post-parse check sees both spellings.
        let timeout = if timeout.is_zero() {
            DEFAULT_TIMEOUT
        } else {
            timeout
        };

        let raw_on_error = dto.on_error.trim().to_lowercase();
        let on_error = match raw_on_error.as_str() {
            "" => default_on_error(phase),
            "fail" => OnError::Fail,
            "continue" => OnError::Continue,
            other => {
                let fallback = default_on_error(phase);
                warnings.push(format!(
                    "{phase} hook {ordinal} has invalid on_error {other:?}; using {:?}",
                    fallback.as_str()
                ));
                fallback
            }
        };

        let name = if dto.name.is_empty() {
            format!("{phase}-{ordinal}")
        } else {
            dto.name
        };

        out.push(Hook {
            name,
            command: dto.command,
            timeout,
            env: dto.env,
            on_error,
        });
    }
    Ok(out)
}

/// Go `Hook.UnmarshalYAML`'s timeout handling (`config.go:208-237`).
///
/// Go tries `time.ParseDuration` first, which needs a unit (`"30"` alone is
/// rejected) and rejects negatives. It then falls back to reading the value as
/// a bare number of seconds, which is what makes `timeout: 30` work. Only an
/// empty value leaves the timeout at zero for `normalizeHooks` to default.
fn parse_go_timeout(raw: &str) -> Result<Duration, HooksError> {
    let timeout = raw.trim();
    if timeout.is_empty() {
        return Ok(Duration::ZERO);
    }
    match parse_go_duration(timeout) {
        // config.go:213 — ParseDuration accepted it, so the complaint is
        // about the sign rather than the syntax.
        Ok(d) if d.nanos < 0 => Err(HooksError::Timeout {
            raw: timeout.to_string(),
            reason: "must be non-negative".to_string(),
        }),
        Ok(d) => Ok(d.as_std().expect("non-negative")),
        // config.go:218 — the bare-numeric fallback, in seconds. Go wraps the
        // *ParseDuration* error here, not the ParseFloat one, so the parser's
        // own message ("missing unit", "unknown unit …") is what surfaces.
        Err(duration_error) => {
            let seconds = timeout.parse::<f64>().ok();
            let max_seconds = i64::MAX as f64 / 1e9;
            match seconds {
                Some(s) if s >= 0.0 && s <= max_seconds && !s.is_nan() && !s.is_infinite() => {
                    Ok(Duration::from_secs_f64(s))
                }
                _ => Err(HooksError::Timeout {
                    raw: timeout.to_string(),
                    reason: duration_error,
                }),
            }
        }
    }
}

/// Go's signed `time.Duration`, in nanoseconds.
///
/// Rust's [`Duration`] is unsigned, but Go's is not, and `parse_go_timeout`
/// has to be able to tell "negative and syntactically fine" (`-5s`, which
/// produces its own error message) from "unparseable" (`-5`, which falls
/// through to the numeric path). Keeping the parsed value signed is what makes
/// that distinction expressible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GoDuration {
    nanos: i64,
}

impl GoDuration {
    fn as_std(self) -> Option<Duration> {
        u64::try_from(self.nanos).ok().map(Duration::from_nanos)
    }
}

/// Go `time.ParseDuration` (`time/format.go`), returning the parse error text
/// Go would print so the wrapped `invalid timeout %q: %w` matches.
///
/// Go distinguishes three failure shapes and the difference is visible in the
/// error a user sees: `time: invalid duration "soon"` (no leading digit),
/// `time: missing unit in duration "-5"` (digits but no unit), and
/// `time: unknown unit "x" in duration "5x"` (digits plus a bad unit).
const MAX_GO_DURATION: i64 = i64::MAX;

fn parse_go_duration(orig: &str) -> Result<GoDuration, String> {
    let invalid = || format!("time: invalid duration {}", go_quote(orig));

    let mut s = orig;
    let mut negative = false;
    if let Some(c) = s.chars().next() {
        if c == '-' || c == '+' {
            negative = c == '-';
            s = &s[1..];
        }
    }
    // Go's special case: a bare zero needs no unit.
    if s == "0" {
        return Ok(GoDuration { nanos: 0 });
    }
    if s.is_empty() {
        return Err(invalid());
    }

    // Go accumulates in a `uint64` and range-checks at the end, so this does
    // too — an `i64` accumulator would make Go's own bound check vacuous.
    let mut total: u64 = 0;
    while !s.is_empty() {
        // The next character must be a digit or a period.
        let first = s.as_bytes()[0];
        if first != b'.' && !first.is_ascii_digit() {
            return Err(invalid());
        }

        // Consume `[0-9]*`.
        let before_int = s.len();
        let (whole, after_int) = leading_int(s);
        let whole = whole.ok_or_else(invalid)?;
        s = after_int;
        let had_whole = before_int != s.len();

        // Consume `(\.[0-9]*)?`.
        let mut fraction = 0f64;
        let mut scale = 1f64;
        let mut had_fraction = false;
        if s.starts_with('.') {
            s = &s[1..];
            let before_frac = s.len();
            let (f, sc, after_frac) = leading_fraction(s);
            fraction = f;
            scale = sc;
            s = after_frac;
            had_fraction = before_frac != s.len();
        }
        if !had_whole && !had_fraction {
            return Err(invalid());
        }

        // Consume the unit: everything up to the next digit or period.
        let unit_len = s
            .find(|c: char| c == '.' || c.is_ascii_digit())
            .unwrap_or(s.len());
        if unit_len == 0 {
            return Err(format!("time: missing unit in duration {}", go_quote(orig)));
        }
        let (unit, after_unit) = s.split_at(unit_len);
        s = after_unit;
        let unit_nanos: i64 = match unit {
            "ns" => 1,
            "us" | "µs" | "μs" => 1_000,
            "ms" => 1_000_000,
            "s" => 1_000_000_000,
            "m" => 60_000_000_000,
            "h" => 3_600_000_000_000,
            _ => {
                return Err(format!(
                    "time: unknown unit {} in duration {}",
                    go_quote(unit),
                    go_quote(orig)
                ))
            }
        };

        if whole > (MAX_GO_DURATION as u64) / (unit_nanos as u64) {
            return Err(invalid());
        }
        let mut value = whole * (unit_nanos as u64);
        if fraction > 0.0 {
            // float64 is what Go uses here too: a fraction of an hour needs
            // nanosecond accuracy.
            value += (fraction * ((unit_nanos as f64) / scale)) as u64;
            if value > MAX_GO_DURATION as u64 {
                return Err(invalid());
            }
        }
        total = total.checked_add(value).ok_or_else(invalid)?;
        if total > MAX_GO_DURATION as u64 {
            return Err(invalid());
        }
    }

    Ok(GoDuration {
        nanos: if negative {
            -(total as i64)
        } else {
            total as i64
        },
    })
}

/// Go `leadingInt`: consume `[0-9]*`, failing only on overflow.
fn leading_int(s: &str) -> (Option<u64>, &str) {
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    if end == 0 {
        return (Some(0), s);
    }
    match s[..end].parse::<u64>() {
        Ok(v) => (Some(v), &s[end..]),
        // Go's error propagates to the caller's "invalid duration".
        Err(_) => (None, s),
    }
}

/// Go `leadingFraction`: consume `[0-9]*` and return the value and the
/// `10^n` scale the caller divides the unit by.
fn leading_fraction(s: &str) -> (f64, f64, &str) {
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let value: f64 = s[..end].parse().unwrap_or(0.0);
    (value, 10f64.powi(end as i32), &s[end..])
}

/// Go's `strconv.Quote` for a plain ASCII string, used only to build the
/// `time: invalid duration "x"` text.
fn go_quote(s: &str) -> String {
    format!("{s:?}")
}

/// Go `ExportContext` (`config.go:46-52`) and `ToEnv` (`config.go:55-62`).
#[derive(Debug, Clone)]
pub struct ExportContext {
    pub export_path: String,
    pub export_format: String,
    pub issue_count: usize,
    /// Go's `Timestamp`, rendered with `time.RFC3339`. jiff's `Timestamp` is
    /// always an instant, so the rendering is UTC with a `Z`, which is what Go
    /// prints for the `time.Now()`/`robotNow()` values the CLI supplies.
    pub timestamp: jiff::Timestamp,
}

impl ExportContext {
    /// The four `BV_*` variables, as `KEY=VALUE` strings in Go's order.
    pub fn to_env(&self) -> Vec<String> {
        vec![
            format!("BV_EXPORT_PATH={}", self.export_path),
            format!("BV_EXPORT_FORMAT={}", self.export_format),
            format!("BV_ISSUE_COUNT={}", self.issue_count),
            // Go: time.RFC3339 = "2006-01-02T15:04:05Z07:00" — no fractional
            // part, so the seconds field is truncated rather than rounded.
            format!(
                "BV_TIMESTAMP={}",
                self.timestamp.strftime("%Y-%m-%dT%H:%M:%SZ")
            ),
        ]
    }
}

/// Go `HookResult` (`executor.go:15-23`).
#[derive(Debug, Clone)]
pub struct HookResult {
    pub hook: Hook,
    pub phase: HookPhase,
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub duration: Duration,
    pub error: Option<HookRunError>,
}

/// Go `Executor.logger` (`executor.go:31`) — a `func(string)`, no-op by default.
type Logger = Box<dyn Fn(&str)>;

/// Go `Executor` (`executor.go:26-33`).
pub struct Executor {
    config: HooksConfig,
    context: ExportContext,
    results: Vec<HookResult>,
    logger: Option<Logger>,
}

impl Executor {
    /// Go `NewExecutor` (`executor.go:35-43`).
    pub fn new(config: HooksConfig, context: ExportContext) -> Self {
        Self {
            config,
            context,
            results: Vec::new(),
            logger: None,
        }
    }

    /// Go `Executor.SetLogger` (`executor.go:45-51`).
    pub fn set_logger(&mut self, logger: impl Fn(&str) + 'static) {
        self.logger = Some(Box::new(logger));
    }

    fn log(&self, message: &str) {
        if let Some(logger) = &self.logger {
            logger(message);
        }
    }

    /// Go `Executor.RunPreExport` (`executor.go:53-73`): stops at the first
    /// hook that both failed and declared `on_error: fail`.
    pub fn run_pre_export(&mut self) -> Result<(), HooksError> {
        for hook in std::mem::take(&mut self.config.hooks.pre_export) {
            let name = hook.name.clone();
            let command = hook.command.clone();
            self.log(&format!("Running pre-export hook {name:?}: {command}"));
            let result = self.run_hook(&hook, HookPhase::PreExport);
            let failed = !result.success;
            let on_error = result.hook.on_error;
            self.results.push(result);
            if failed && on_error == OnError::Fail {
                // Go stops the loop, so the remaining hooks stay unrun.
                self.config.hooks.pre_export.clear();
                let error = self.results.last().and_then(|r| r.error.clone());
                return Err(HooksError::PreExportFailed {
                    name,
                    source: error.unwrap_or(HookRunError::Exit(-1)),
                });
            }
        }
        Ok(())
    }

    /// Go `Executor.RunPostExport` (`executor.go:75-90`): runs every hook and
    /// reports only the first `on_error: fail` failure.
    pub fn run_post_export(&mut self) -> Result<(), HooksError> {
        let mut first_error = None;
        for hook in std::mem::take(&mut self.config.hooks.post_export) {
            let name = hook.name.clone();
            let command = hook.command.clone();
            self.log(&format!("Running post-export hook {name:?}: {command}"));
            let result = self.run_hook(&hook, HookPhase::PostExport);
            let failed = !result.success;
            let on_error = result.hook.on_error;
            self.results.push(result);
            if failed && on_error == OnError::Fail && first_error.is_none() {
                let error = self.results.last().and_then(|r| r.error.clone());
                first_error = Some(HooksError::PostExportFailed {
                    name,
                    source: error.unwrap_or(HookRunError::Exit(-1)),
                });
            }
        }
        match first_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Go `Executor.runHook` (`executor.go:95-171`).
    fn run_hook(&mut self, hook: &Hook, phase: HookPhase) -> HookResult {
        let start = Instant::now();
        let timeout = if hook.timeout.is_zero() {
            DEFAULT_TIMEOUT
        } else {
            hook.timeout
        };

        let (program, flag) = shell_command();
        let mut cmd = Command::new(program);
        cmd.arg(flag).arg(&hook.command);

        // Go builds the child environment from a scrubbed copy of the parent
        // (`executor.go:138`), then layers on the export context and the
        // hook's own variables. `Command` inherits by default, so scrubbing
        // is expressed as an explicit removal of each matching key — removing
        // the *actual* key spelling keeps Windows' case-insensitive
        // environment working — and then everything the hook should see is
        // set explicitly on top.
        // `env::vars()` panics on a non-UTF-8 value; `vars_os` plus a lossy
        // conversion keeps the hook runnable, matching Go where `os.Environ`
        // strings are arbitrary bytes.
        let parent: Vec<(String, String)> = std::env::vars_os()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.to_string_lossy().into_owned(),
                )
            })
            .collect();
        let child = child_environment(&parent, &self.context, &hook.env);
        for (key, _) in &parent {
            if is_sensitive_env_key(key) {
                cmd.env_remove(key);
            }
        }
        for (key, value) in &child {
            // `Command::env` panics on an empty key, a key containing `=` or
            // NUL, or a NUL in the value. Hook `env:` keys come from YAML and
            // can be any of those; skip them rather than crash the export.
            if valid_env_key(key) && !value.contains('\0') {
                cmd.env(key, value);
            }
        }

        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let mut result = HookResult {
            hook: hook.clone(),
            phase,
            success: false,
            stdout: String::new(),
            stderr: String::new(),
            duration: Duration::ZERO,
            error: None,
        };

        let outcome = run_with_timeout(cmd, timeout);
        result.duration = go_round_to_millis(start.elapsed());
        match outcome {
            Ok(captured) => {
                result.stdout = captured.stdout.trim().to_string();
                result.stderr = captured.stderr.trim().to_string();
                result.success = true;
            }
            Err(failure) => {
                result.stdout = failure.stdout.trim().to_string();
                result.stderr = failure.stderr.trim().to_string();
                result.success = false;
                result.error = Some(failure.error);
            }
        }
        result
    }

    /// Go `Executor.Results` (`executor.go:245-247`).
    pub fn results(&self) -> &[HookResult] {
        &self.results
    }

    /// Go `Executor.Summary` (`executor.go:249-271`).
    pub fn summary(&self) -> String {
        if self.results.is_empty() {
            return "No hooks executed".to_string();
        }
        let mut sb = String::new();
        let (mut succeeded, mut failed) = (0, 0);
        for r in &self.results {
            if r.success {
                succeeded += 1;
                let _ = writeln!(
                    sb,
                    "  [OK] {} ({})",
                    r.hook.name,
                    go_duration_string(r.duration)
                );
            } else {
                failed += 1;
                let _ = writeln!(
                    sb,
                    "  [FAIL] {}: {}",
                    r.hook.name,
                    r.error
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_default()
                );
                if !r.stderr.is_empty() {
                    let _ = writeln!(sb, "         stderr: {}", truncate(&r.stderr, 200));
                }
            }
        }
        let header = format!("Hook execution: {succeeded} succeeded, {failed} failed\n");
        header + &sb
    }
}

/// Go `getShellCommand` (`executor.go:92-97`).
fn shell_command() -> (&'static str, &'static str) {
    if cfg!(target_os = "windows") {
        ("cmd", "/C")
    } else {
        ("sh", "-c")
    }
}

struct Captured {
    stdout: String,
    stderr: String,
}

struct CaptureFailure {
    stdout: String,
    stderr: String,
    error: HookRunError,
}

/// Run `cmd` with a deadline, capturing both pipes.
///
/// `std::process` has no timeout, so this polls `try_wait` and kills the child
/// once the deadline passes. Both pipes are drained on their own threads:
/// waiting for the child before reading would deadlock as soon as a hook
/// writes more than one pipe buffer of output.
fn run_with_timeout(mut cmd: Command, timeout: Duration) -> Result<Captured, CaptureFailure> {
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return Err(CaptureFailure {
                stdout: String::new(),
                stderr: String::new(),
                error: HookRunError::Spawn(e),
            })
        }
    };

    let out_reader = child.stdout.take().map(spawn_reader);
    let err_reader = child.stderr.take().map(spawn_reader);

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(e) => {
                return Err(CaptureFailure {
                    stdout: join(out_reader),
                    stderr: join(err_reader),
                    error: HookRunError::Spawn(e),
                })
            }
        }
        if Instant::now() >= deadline {
            timed_out = true;
            // Go's `exec.CommandContext` kills the process when the context
            // expires; the tree is not signalled, so neither is this.
            let _ = child.kill();
            break child.wait().ok();
        }
        std::thread::sleep(Duration::from_millis(5));
    };

    let stdout = join(out_reader);
    let stderr = join(err_reader);

    if timed_out {
        return Err(CaptureFailure {
            stdout,
            stderr,
            error: HookRunError::Timeout(timeout),
        });
    }
    match status {
        Some(s) if s.success() => Ok(Captured { stdout, stderr }),
        Some(s) => Err(CaptureFailure {
            stdout,
            stderr,
            // `ExitCode::None` means the child was killed by a signal, which
            // on this platform has no portable code to report.
            error: HookRunError::Exit(s.code().unwrap_or(-1)),
        }),
        None => Err(CaptureFailure {
            stdout,
            stderr,
            error: HookRunError::Spawn(std::io::Error::other("child wait failed")),
        }),
    }
}

fn spawn_reader<R: Read + Send + 'static>(mut pipe: R) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        buf
    })
}

fn join(handle: Option<std::thread::JoinHandle<Vec<u8>>>) -> String {
    handle
        .and_then(|h| h.join().ok())
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default()
}

/// Substrings that mark a variable name as credential-bearing when present in
/// its upper-cased form — Go `sensitiveEnvMarkers` (`executor.go:173-183`).
const SENSITIVE_ENV_MARKERS: [&str; 9] = [
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "CREDENTIAL",
    "API_KEY",
    "APIKEY",
    "ACCESS_KEY",
    "PRIVATE_KEY",
];

/// Go `sensitiveEnvExact` (`executor.go:186-189`).
const SENSITIVE_ENV_EXACT: [&str; 1] = ["SSH_AUTH_SOCK"];

/// Go `isSensitiveEnvKey` (`executor.go:191-204`).
pub fn is_sensitive_env_key(key: &str) -> bool {
    let upper = key.to_uppercase();
    if SENSITIVE_ENV_EXACT.contains(&upper.as_str()) {
        return true;
    }
    SENSITIVE_ENV_MARKERS
        .iter()
        .any(|marker| upper.contains(marker))
}

/// The environment a hook subprocess receives — Go `executor.go:130-161`,
/// extracted as pure data so the layering can be asserted without spawning.
///
/// Three rules, in order:
///
/// 1. The parent environment minus every credential-bearing variable
///    (`executor.go:138`). Hooks live in `.bv/hooks.yaml`, which can come from
///    an untrusted checkout, so an ambient token must not leak in by default.
/// 2. The four export-context variables, appended unconditionally
///    (`executor.go:140`).
/// 3. The hook's own `env:` block, in sorted key order, each value expanded
///    against the **unscrubbed** parent plus the context plus the hook pairs
///    declared before it (`executor.go:143-161`). Expanding against the
///    unscrubbed parent is what makes the documented re-grant —
///    `GITHUB_TOKEN: "${GITHUB_TOKEN}"` — work.
pub fn child_environment(
    parent: &[(String, String)],
    context: &ExportContext,
    hook_env: &BTreeMap<String, String>,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = parent
        .iter()
        .filter(|(key, _)| !is_sensitive_env_key(key))
        .cloned()
        .collect();

    out.extend(context_env_pairs(context));

    let mut expansion: Vec<OsString> = parent
        .iter()
        .map(|(k, v)| OsString::from(format!("{k}={v}")))
        .collect();
    expansion.extend(context.to_env().into_iter().map(OsString::from));
    // `BTreeMap` iterates in sorted key order, which is the order Go
    // imposes with its explicit `sort.Strings` at `executor.go:151`.
    for (key, value) in hook_env {
        let expanded = expand_env(value, &expansion);
        out.push((key.clone(), expanded.clone()));
        expansion.push(OsString::from(format!("{key}={expanded}")));
    }
    out
}

/// [`ExportContext::to_env`] as pairs, preserving Go's order.
fn context_env_pairs(context: &ExportContext) -> Vec<(String, String)> {
    context
        .to_env()
        .into_iter()
        .filter_map(|pair| {
            pair.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
        })
        .collect()
}

/// Go `expandEnv` (`executor.go:227-240`), built on `os.Expand` semantics:
/// `${NAME}` and `$NAME`, an unset name expanding to the empty string, and an
/// unrecognised `$` left in place rather than swallowed.
pub fn expand_env(value: &str, env: &[OsString]) -> String {
    let mut out = String::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'$' || i + 1 >= bytes.len() {
            // Push the whole UTF-8 character, not `bytes[i] as char` — the
            // latter turns every byte of a multibyte sequence into a
            // separate mojibake character.
            let ch = value[i..].chars().next().expect("i is in bounds");
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        let (name, consumed) = shell_name(&value[i + 1..]);
        match name {
            Some(name) => out.push_str(&lookup_env(name, env)),
            // Malformed syntax (`${` with no closing brace, or `${}`): Go eats
            // the characters without expanding anything.
            None if consumed > 0 => {}
            // A `$` with no name after it stays literal.
            None => out.push('$'),
        }
        i += 1 + consumed;
    }
    out
}

/// Go `os.getShellName`: the variable name at the head of `s`, and how many
/// bytes of `s` it spans. A name of `None` with a non-zero width means
/// "malformed, skip this many"; `None` with width 0 means "no name here".
fn shell_name(s: &str) -> (Option<&str>, usize) {
    let bytes = s.as_bytes();
    if bytes.first() == Some(&b'{') {
        return match s.find('}') {
            // `"${}"` is bad syntax; Go eats three bytes in total.
            Some(1) => (None, 2),
            Some(end) => (Some(&s[1..end]), end + 1),
            // No closing brace: eat just the `{`.
            None => (None, 1),
        };
    }
    let len = bytes
        .iter()
        .position(|c| !(c.is_ascii_alphanumeric() || *c == b'_'))
        .unwrap_or(bytes.len());
    if len == 0 {
        (None, 0)
    } else {
        (Some(&s[..len]), len)
    }
}

/// Go looks the name up by scanning the environment slice *backwards*, so a
/// later entry wins when a key appears twice.
fn lookup_env(name: &str, env: &[OsString]) -> String {
    let prefix = format!("{name}=");
    env.iter()
        .rev()
        .find_map(|kv| {
            let text = kv.to_string_lossy();
            text.starts_with(&prefix)
                .then(|| text[prefix.len()..].to_string())
        })
        .unwrap_or_default()
}

/// Go `truncate` (`executor.go:273-284`) — three dots, unlike the `…` used by
/// `pkg/export/markdown.go:955`.
///
/// Go measures the limit in **bytes** (`len(s)`), not runes, so a multibyte
/// string is cut earlier than a char count would suggest. The cut is still
/// snapped back to a UTF-8 boundary: Go can emit invalid UTF-8 here, Rust's
/// `String` cannot, and a boundary snap is the closest faithful rendering.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    if max <= 3 {
        return floor_char_boundary(s, max).to_string();
    }
    let mut out = floor_char_boundary(s, max - 3).to_string();
    out.push_str("...");
    out
}

/// The longest prefix of `s` of at most `end` bytes that ends on a char
/// boundary.
fn floor_char_boundary(s: &str, mut end: usize) -> &str {
    end = end.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// A key `Command::env`/`env_remove` accepts: non-empty, no `=`, no NUL.
fn valid_env_key(key: &str) -> bool {
    !key.is_empty() && !key.contains('=') && !key.contains('\0')
}

/// Go `time.Duration.Round(time.Millisecond)` (`executor.go:259`). Go rounds
/// half away from zero, so `Duration::round`'s half-to-even differs at exact
/// half-milliseconds; that case is corrected here rather than left to the
/// standard library.
fn go_round_to_millis(d: Duration) -> Duration {
    let nanos = d.as_nanos();
    let unit = 1_000_000u128;
    let remainder = nanos % unit;
    if remainder == 0 {
        return d;
    }
    let rounded = if remainder * 2 >= unit {
        ((nanos / unit) + 1) * unit
    } else {
        (nanos / unit) * unit
    };
    Duration::from_nanos(rounded.min(u64::MAX as u128) as u64)
}

/// Go `time.Duration.String()` (`time/time.go`), needed because
/// `Executor.Summary` prints durations with `%v`.
///
/// Go's `Duration` is signed; Rust's is not. Every duration this crate formats
/// is one it measured or one it read from a validated (hence non-negative)
/// `timeout`, so the negative branch Go can print is unreachable here and is
/// omitted rather than faked.
pub fn go_duration_string(d: Duration) -> String {
    if d.is_zero() {
        return "0s".to_string();
    }
    go_positive_duration_string(d)
}

fn go_positive_duration_string(d: Duration) -> String {
    let u = d.as_nanos();
    if u < 1_000_000_000 {
        // Below one second Go picks ns / µs / ms and trims trailing zeros.
        let (prec, unit) = if u < 1_000 {
            (0usize, "ns")
        } else if u < 1_000_000 {
            (3, "µs")
        } else {
            (6, "ms")
        };
        let (frac, whole) = fmt_frac(u, prec);
        format!("{whole}{frac}{unit}")
    } else {
        let (frac, secs) = fmt_frac(u, 9);
        let minutes_total = secs / 60;
        let seconds = secs % 60;
        if minutes_total == 0 {
            return format!("{seconds}{frac}s");
        }
        let hours_total = minutes_total / 60;
        let minutes = minutes_total % 60;
        if hours_total == 0 {
            return format!("{minutes}m{seconds}{frac}s");
        }
        format!("{hours_total}h{minutes}m{seconds}{frac}s")
    }
}

/// Go's `time.fmtFrac`: render `v / 10**prec` as a fraction with trailing
/// zeros (and the point itself) omitted, returning the remaining whole part.
fn fmt_frac(v: u128, prec: usize) -> (String, u128) {
    let mut digits = Vec::with_capacity(prec);
    let mut v = v;
    let mut printing = false;
    for _ in 0..prec {
        let digit = (v % 10) as u8;
        printing = printing || digit != 0;
        if printing {
            digits.push(char::from(b'0' + digit));
        }
        v /= 10;
    }
    if digits.is_empty() {
        (String::new(), v)
    } else {
        let mut frac = String::from(".");
        frac.extend(digits.iter().rev());
        (frac, v)
    }
}

/// Go `RunHooks` (`executor.go:286-301`): a ready-to-run executor, or `None`
/// when hooks are disabled or none are configured.
pub fn run_hooks(
    project_dir: impl AsRef<Path>,
    context: ExportContext,
    no_hooks: bool,
) -> Result<Option<Executor>, HooksError> {
    if no_hooks {
        return Ok(None);
    }
    let mut loader = Loader::new(project_dir);
    // Go wraps the loader failure as `loading hooks: %w` (executor.go:298).
    loader
        .load()
        .map_err(|e| HooksError::LoadFailed(Box::new(e)))?;
    if !loader.has_hooks() {
        return Ok(None);
    }
    Ok(Some(Executor::new(loader.config(), context)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ExportContext {
        ExportContext {
            export_path: "/tmp/out.md".into(),
            export_format: "markdown".into(),
            issue_count: 42,
            timestamp: "2024-01-15T10:00:00Z".parse().unwrap(),
        }
    }

    #[test]
    fn context_env_matches_go_order_and_format() {
        // config.go:55-62
        assert_eq!(
            ctx().to_env(),
            vec![
                "BV_EXPORT_PATH=/tmp/out.md",
                "BV_EXPORT_FORMAT=markdown",
                "BV_ISSUE_COUNT=42",
                "BV_TIMESTAMP=2024-01-15T10:00:00Z",
            ]
        );
    }

    #[test]
    fn missing_config_file_is_not_an_error() {
        // config.go:114-119
        let mut loader = Loader::new("this-directory-does-not-exist");
        loader.load().expect("missing hooks.yaml is fine");
        assert!(!loader.has_hooks());
        assert!(loader.warnings().is_empty());
        assert!(loader.hooks(HookPhase::PreExport).is_empty());
    }

    #[test]
    fn on_error_default_depends_on_phase() {
        // config.go:139-141 via defaultOnError at config.go:154-159.
        let dir = tempdir("phase-defaults");
        write_hooks(
            &dir,
            r#"
hooks:
  pre-export:
    - command: "echo pre"
  post-export:
    - command: "echo post"
"#,
        );
        let mut loader = Loader::new(&dir);
        loader.load().unwrap();
        assert_eq!(
            loader.hooks(HookPhase::PreExport)[0].on_error,
            OnError::Fail
        );
        assert_eq!(
            loader.hooks(HookPhase::PostExport)[0].on_error,
            OnError::Continue
        );
        assert_eq!(loader.hooks(HookPhase::PreExport)[0].name, "pre-export-1");
        assert_eq!(loader.hooks(HookPhase::PostExport)[0].name, "post-export-1");
        assert_eq!(
            loader.hooks(HookPhase::PreExport)[0].timeout,
            DEFAULT_TIMEOUT
        );
    }

    #[test]
    fn empty_command_is_dropped_with_a_warning() {
        // config.go:133-136
        let dir = tempdir("empty-command");
        write_hooks(
            &dir,
            r#"
hooks:
  pre-export:
    - name: "blank"
      command: "   "
    - name: "real"
      command: "echo hi"
"#,
        );
        let mut loader = Loader::new(&dir);
        loader.load().unwrap();
        assert_eq!(loader.hooks(HookPhase::PreExport).len(), 1);
        assert_eq!(loader.hooks(HookPhase::PreExport)[0].name, "real");
        assert_eq!(
            loader.warnings(),
            ["pre-export hook 1 has empty command; skipping"]
        );
    }

    #[test]
    fn invalid_on_error_warns_and_falls_back() {
        // config.go:145-148
        let dir = tempdir("bad-on-error");
        write_hooks(
            &dir,
            r#"
hooks:
  post-export:
    - command: "echo hi"
      on_error: "RETRY"
"#,
        );
        let mut loader = Loader::new(&dir);
        loader.load().unwrap();
        assert_eq!(
            loader.hooks(HookPhase::PostExport)[0].on_error,
            OnError::Continue
        );
        assert_eq!(
            loader.warnings(),
            ["post-export hook 1 has invalid on_error \"retry\"; using \"continue\""]
        );
    }

    #[test]
    fn on_error_is_trimmed_and_lowercased() {
        // config.go:142 `strings.ToLower(strings.TrimSpace(hook.OnError))`
        let dir = tempdir("on-error-case");
        write_hooks(
            &dir,
            r#"
hooks:
  pre-export:
    - command: "echo hi"
      on_error: "  FAIL  "
"#,
        );
        let mut loader = Loader::new(&dir);
        loader.load().unwrap();
        assert_eq!(
            loader.hooks(HookPhase::PreExport)[0].on_error,
            OnError::Fail
        );
        assert!(loader.warnings().is_empty());
    }

    #[test]
    fn timeout_accepts_both_spellings() {
        // config.go:208-237
        let ok = |s: &str| parse_go_timeout(s).unwrap_or_else(|e| panic!("{s:?}: {e}"));
        assert_eq!(ok(""), Duration::ZERO);
        assert_eq!(ok("   "), Duration::ZERO);
        assert_eq!(ok("30s"), Duration::from_secs(30));
        assert_eq!(ok("1m30s"), Duration::from_secs(90));
        assert_eq!(ok("500ms"), Duration::from_millis(500));
        assert_eq!(ok("2h"), Duration::from_secs(7200));
        assert_eq!(
            ok("1500ns"),
            Duration::from_micros(1) + Duration::from_nanos(500)
        );
        // Bare numbers are seconds via the ParseFloat fallback.
        assert_eq!(ok("30"), Duration::from_secs(30));
        assert_eq!(ok("0"), Duration::ZERO);
        assert_eq!(ok("0.5"), Duration::from_millis(500));
    }

    #[test]
    fn negative_and_garbage_timeouts_are_rejected() {
        // config.go:213 and :222 — the messages differ, and so do the three
        // shapes of Go's own `time.ParseDuration` error.
        let neg = parse_go_timeout("-5s").unwrap_err().to_string();
        assert_eq!(neg, "invalid timeout \"-5s\": must be non-negative");

        let bare_neg = parse_go_timeout("-5").unwrap_err().to_string();
        assert_eq!(
            bare_neg,
            "invalid timeout \"-5\": time: missing unit in duration \"-5\""
        );

        let garbage = parse_go_timeout("soon").unwrap_err().to_string();
        assert_eq!(
            garbage,
            "invalid timeout \"soon\": time: invalid duration \"soon\""
        );

        // Digits plus a unit Go does not know.
        let bad_unit = parse_go_timeout("5x").unwrap_err().to_string();
        assert_eq!(
            bad_unit,
            "invalid timeout \"5x\": time: unknown unit \"x\" in duration \"5x\""
        );

        // A YAML boolean decodes to its text, which is not a duration.
        let boolean = parse_go_timeout("true").unwrap_err().to_string();
        assert_eq!(
            boolean,
            "invalid timeout \"true\": time: invalid duration \"true\""
        );

        // Digits with no unit are not an error at all: the bare-number
        // fallback reads them as seconds, which is what makes `timeout: 30`
        // work. Only a *negative* bare number falls through to an error.
        assert_eq!(parse_go_timeout("5").unwrap(), Duration::from_secs(5));
    }

    #[test]
    fn sensitive_env_keys_match_go() {
        // executor.go:173-204
        for key in [
            "GITHUB_TOKEN",
            "MY_SECRET",
            "DB_PASSWORD",
            "PASSWORD",
            "AWS_ACCESS_KEY_ID",
            "SOMEAPIKEY",
            "my_private_key",
            "ssh_auth_sock",
        ] {
            assert!(is_sensitive_env_key(key), "{key} should be scrubbed");
        }
        for key in ["PATH", "HOME", "BV_EXPORT_PATH", "CARGO_PKG_NAME"] {
            assert!(!is_sensitive_env_key(key), "{key} should survive");
        }
    }

    #[test]
    fn expand_env_handles_both_forms() {
        // executor.go:227-240 via os.Expand
        let env: Vec<OsString> = ["A=1", "B=two words", "EMPTY="]
            .iter()
            .map(OsString::from)
            .collect();
        assert_eq!(expand_env("$A", &env), "1");
        assert_eq!(expand_env("${A}", &env), "1");
        assert_eq!(expand_env("x${A}y", &env), "x1y");
        assert_eq!(expand_env("${B}", &env), "two words");
        assert_eq!(expand_env("$EMPTY!", &env), "!");
        assert_eq!(expand_env("$MISSING", &env), "");
        // No name after the dollar sign stays literal.
        assert_eq!(expand_env("100$", &env), "100$");
        assert_eq!(expand_env("$ A", &env), "$ A");
    }

    #[test]
    fn expand_env_prefers_later_entries() {
        // executor.go:235-239 — the scan runs from the end.
        let env: Vec<OsString> = vec![OsString::from("A=first"), OsString::from("A=second")];
        assert_eq!(expand_env("$A", &env), "second");
    }

    #[test]
    fn truncate_uses_three_dots() {
        // executor.go:273-284
        assert_eq!(truncate("short", 200), "short");
        assert_eq!(truncate("abcdefghij", 6), "abc...");
        assert_eq!(truncate("abcdef", 2), "ab");
    }

    #[test]
    fn go_duration_string_matches_go() {
        // Shapes produced by Go's time.Duration.String().
        assert_eq!(go_duration_string(Duration::ZERO), "0s");
        assert_eq!(go_duration_string(Duration::from_millis(1)), "1ms");
        assert_eq!(go_duration_string(Duration::from_millis(12)), "12ms");
        assert_eq!(go_duration_string(Duration::from_millis(1500)), "1.5s");
        assert_eq!(go_duration_string(Duration::from_secs(2)), "2s");
        assert_eq!(go_duration_string(Duration::from_secs(90)), "1m30s");
        assert_eq!(go_duration_string(Duration::from_secs(3600)), "1h0m0s");
        assert_eq!(
            go_duration_string(Duration::from_millis(3661500)),
            "1h1m1.5s"
        );
    }

    #[test]
    fn go_round_to_millis_rounds_half_away_from_zero() {
        // Go's Duration.Round is half-away-from-zero, unlike Duration::round.
        assert_eq!(
            go_round_to_millis(Duration::from_nanos(1_500_000)),
            Duration::from_millis(2)
        );
        assert_eq!(
            go_round_to_millis(Duration::from_nanos(1_400_000)),
            Duration::from_millis(1)
        );
        assert_eq!(
            go_round_to_millis(Duration::from_millis(7)),
            Duration::from_millis(7)
        );
    }

    #[test]
    fn child_env_scrubs_credentials_and_layers_the_rest() {
        // executor.go:130-161
        let parent = vec![
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("HOME".to_string(), "/home/dev".to_string()),
            ("GITHUB_TOKEN".to_string(), "ghp_secret".to_string()),
            ("MY_API_KEY".to_string(), "sk_secret".to_string()),
            ("SSH_AUTH_SOCK".to_string(), "/tmp/agent.sock".to_string()),
        ];
        let child = child_environment(&parent, &ctx(), &BTreeMap::new());
        let lookup = |name: &str| {
            child
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(lookup("PATH").as_deref(), Some("/usr/bin"));
        assert_eq!(lookup("HOME").as_deref(), Some("/home/dev"));
        assert_eq!(lookup("GITHUB_TOKEN"), None);
        assert_eq!(lookup("MY_API_KEY"), None);
        assert_eq!(lookup("SSH_AUTH_SOCK"), None);
        // The context is appended after the scrubbed parent.
        assert_eq!(lookup("BV_EXPORT_FORMAT").as_deref(), Some("markdown"));
        assert_eq!(lookup("BV_ISSUE_COUNT").as_deref(), Some("42"));
    }

    #[test]
    fn child_env_lets_a_hook_regrant_a_credential() {
        // executor.go:132-137 documents the explicit re-grant. Expansion
        // sees the *unscrubbed* parent, so the hook can name the token and
        // get it back.
        let parent = vec![
            ("GITHUB_TOKEN".to_string(), "ghp_secret".to_string()),
            ("KEEP".to_string(), "yes".to_string()),
        ];
        let hook_env =
            BTreeMap::from([("GITHUB_TOKEN".to_string(), "${GITHUB_TOKEN}".to_string())]);
        let child = child_environment(&parent, &ctx(), &hook_env);
        let lookup = |name: &str| {
            child
                .iter()
                .rev()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        };
        // The scrubbed parent entry is replaced by the explicit one.
        assert_eq!(child.iter().filter(|(k, _)| k == "GITHUB_TOKEN").count(), 1);
        assert_eq!(lookup("GITHUB_TOKEN").as_deref(), Some("ghp_secret"));
        assert_eq!(lookup("KEEP").as_deref(), Some("yes"));
    }

    #[test]
    fn child_env_expands_in_sorted_key_order() {
        // executor.go:147-161 — a later (sorted) key can read an earlier one.
        let hook_env = BTreeMap::from([
            ("A_FIRST".to_string(), "${BV_ISSUE_COUNT}".to_string()),
            ("Z_LAST".to_string(), "${A_FIRST}!".to_string()),
        ]);
        let child = child_environment(&[], &ctx(), &hook_env);
        let lookup = |name: &str| {
            child
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
                .unwrap()
        };
        assert_eq!(lookup("A_FIRST"), "42");
        assert_eq!(lookup("Z_LAST"), "42!");
        // Sorted order, so the hook pairs come after the context pairs.
        let keys: Vec<&str> = child.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys[keys.len() - 2..], ["A_FIRST", "Z_LAST"]);
    }

    // ---- helpers ----------------------------------------------------------

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bvr-hooks-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".bv")).expect("create temp dir");
        dir
    }

    fn write_hooks(dir: &Path, body: &str) {
        std::fs::write(dir.join(".bv").join("hooks.yaml"), body).expect("write hooks.yaml");
    }
}
