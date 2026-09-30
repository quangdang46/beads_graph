//! RobotEnvelope + output encoding — port of Go `cmd/bv/main.go`
//! envelope (lines 8177-8260) per api-freeze-v1.

use serde::{Deserialize, Serialize};

pub const ROBOT_CONTRACT_VERSION: &str = "1.0.0";

/// Per-line parse accounting surfaced ONLY when the loader dropped records
/// (#190). Mirrors Go `RobotLoadStats`.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct RobotLoadStats {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source_path: String,
    pub valid: usize,
    pub errors: usize,
    pub skipped: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Go `RobotSourceReport` (cmd/bv/main.go:7235) — one configured source before
/// view projection. Field order is part of the serialized contract.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct RobotSourceReport {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub repo_path: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source_path: String,
    pub source_kind: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub data_hash: String,
    pub valid: usize,
    pub errors: usize,
    pub skipped: usize,
    pub read_errors: usize,
    pub visible: usize,
    pub tombstones: usize,
    pub stale: bool,
    pub warning_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
}

/// Go `RobotSourceAuthority` (cmd/bv/main.go:7250) — distinguishes proven
/// readiness from calculations over incomplete or stale source data.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct RobotSourceAuthority {
    pub state: String,
    pub claim_safe: bool,
    pub readiness: String,
    pub loaded: usize,
    pub failed: usize,
    pub disabled: usize,
    pub valid: usize,
    pub errors: usize,
    pub skipped: usize,
    pub read_errors: usize,
    pub visible: usize,
    pub tombstones: usize,
    pub warning_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<RobotSourceReport>,
}

/// Go `RobotScope` (cmd/bv/main.go:7418) — scoping flags in effect, plus the
/// flags the command could not honour.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct RobotScope {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub recipe: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub repo: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unsupported: Vec<String>,
}

/// Standard envelope for all robot outputs. Field order = serialization
/// order (serde preserves declaration order) — part of the contract.
/// Mirrors Go `RobotEnvelope` (cmd/bv/main.go:7215).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct RobotEnvelope {
    /// RFC3339 UTC timestamp.
    pub generated_at: String,
    /// Fingerprint of source data (bv_core::data_hash).
    pub data_hash: String,
    /// "json" | "toon" — present when a format override is active.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub output_format: String,
    /// bv version string.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub version: String,
    /// File (or "<file>@<rev>") the issue set was loaded from.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source_path: String,
    /// jsonl | sqlite | git | workspace | bd
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source_kind: String,
    /// --as-of ref, when time-travelling.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub as_of: String,
    /// Resolved SHA for --as-of.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub as_of_commit: String,
    /// Active --label/--recipe/--repo scoping and what this command could not honour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<RobotScope>,
    /// Present only when loader dropped records (#190).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_stats: Option<RobotLoadStats>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_authority: Option<RobotSourceAuthority>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub authority_hash: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub scope_hash: String,
}

impl RobotEnvelope {
    /// Go: `NewRobotEnvelope`. `load_stats` emitted only when errors > 0.
    pub fn new(
        data_hash: impl Into<String>,
        version: impl Into<String>,
        load_report: Option<&bv_core::loader::LoadReport>,
        output_format: OutputFormat,
    ) -> Self {
        let load_stats = load_report.and_then(|rep| {
            if rep.errors > 0 {
                Some(RobotLoadStats {
                    source_path: rep.path.clone(),
                    valid: rep.valid,
                    errors: rep.errors,
                    skipped: rep.skipped,
                    warnings: rep.warnings.clone(),
                })
            } else {
                None
            }
        });
        // Go parity: truncate to second precision (no microseconds).
        let ts = jiff::Timestamp::now().to_string();
        let generated_at = if let Some(pos) = ts.find('.') {
            format!("{}Z", &ts[..pos])
        } else {
            ts
        };
        RobotEnvelope {
            generated_at,
            data_hash: data_hash.into(),
            output_format: output_format.as_str().to_string(),
            version: version.into(),
            source_path: String::new(),
            source_kind: String::new(),
            as_of: String::new(),
            as_of_commit: String::new(),
            scope: None,
            load_stats,
            source_authority: None,
            authority_hash: String::new(),
            scope_hash: String::new(),
        }
    }
}

/// Go `encoding/json` HTML-escapes `<`, `>` and `&` by default;
/// `serde_json::to_vec` escapes none of them. Any path or label containing
/// one of those bytes therefore hashes to a different digest than Go's, even
/// though the emitted envelope body is escaped correctly elsewhere.
///
/// `<`, `>` and `&` can only occur *inside* JSON string literals — never as
/// JSON syntax — so rewriting the serialized bytes wholesale is exactly
/// equivalent to escaping during serialization, and it cannot corrupt
/// structure the way a naive pre-serialization replace of the input could.
fn go_html_escape_json(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    for &b in raw {
        match b {
            b'<' => out.extend_from_slice(b"\\u003c"),
            b'>' => out.extend_from_slice(b"\\u003e"),
            b'&' => out.extend_from_slice(b"\\u0026"),
            b => out.push(b),
        }
    }
    out
}

/// Go `robotAuthorityHash` (cmd/bv/main.go:7393) — SHA-256 over the JSON
/// encoding of the authority struct, lowercase hex. Field order must match the
/// struct declaration for the digest to match.
pub fn authority_hash(authority: &RobotSourceAuthority) -> String {
    match serde_json::to_vec(authority) {
        Ok(raw) => sha256_hex(&go_html_escape_json(&raw)),
        Err(_) => String::new(),
    }
}

/// Go `robotScopeHash` (cmd/bv/main.go:7404) — SHA-256 over a JSON object with
/// the field order Label, Recipe, Repo, DataHash, IDs.
pub fn scope_hash(
    label: &str,
    recipe: &str,
    repo: &str,
    data_hash: &str,
    ids: &[String],
) -> String {
    // Build the exact anonymous-struct JSON Go marshals.
    let value = serde_json::json!({
        "Label": label,
        "Recipe": recipe,
        "Repo": repo,
        "DataHash": data_hash,
        "IDs": ids,
    });
    // serde_json::json! uses a Map; with preserve_order it keeps insertion order.
    match serde_json::to_vec(&value) {
        Ok(raw) => sha256_hex(&go_html_escape_json(&raw)),
        Err(_) => String::new(),
    }
}

fn sha256_hex(raw: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(raw);
    let mut out = String::with_capacity(64);
    for b in digest.iter() {
        out.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        out.push(char::from_digit((b & 0x0f) as u32, 16).unwrap());
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Json,
    Toon,
}

impl OutputFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            OutputFormat::Json => "json",
            OutputFormat::Toon => "toon",
        }
    }
}

/// Whether the external `tru` encoder is usable.
///
/// Whether a TOON encoder is installed.
///
/// Go resolves the same binary and degrades with the same warning when it is
/// absent (main.go:2038-2041), so `--format toon` on a machine without it
/// reports `output_format: "json"` rather than claiming TOON over JSON bytes.
/// The gate is what makes that downgrade honest, and the encoder is what makes
/// the claim true when it does not fire.
pub fn tru_available() -> bool {
    tru_path().is_some()
}

/// Resolve the `tru` binary using Go's lookup order: `TOON_TRU_BIN` then
/// `TOON_BIN` (either a path or a command name), then `tru`/`toon` on PATH,
/// then a short list of common install locations.
fn tru_path() -> Option<std::path::PathBuf> {
    for env in ["TOON_TRU_BIN", "TOON_BIN"] {
        if let Ok(raw) = std::env::var(env) {
            let raw = raw.trim();
            if raw.is_empty() {
                continue;
            }
            // Go treats an explicitly configured but unusable value as a hard
            // error rather than silently falling through to PATH.
            return is_toon_rust_binary(&resolve_candidate(raw)).then(|| raw.into());
        }
    }
    for name in ["tru", "toon"] {
        if let Some(path) = look_path(name) {
            if is_toon_rust_binary(&path) {
                return Some(path);
            }
        }
    }
    for p in [
        "/usr/local/bin/tru",
        "/usr/bin/tru",
        "/data/tmp/cargo-target/release/tru",
        "/data/tmp/cargo-target/debug/tru",
    ] {
        let path = std::path::PathBuf::from(p);
        if is_toon_rust_binary(&path) {
            return Some(path);
        }
    }
    None
}

fn resolve_candidate(raw: &str) -> std::path::PathBuf {
    let direct = std::path::PathBuf::from(raw);
    if direct.is_file() {
        return direct;
    }
    look_path(raw).unwrap_or(direct)
}

fn look_path(name: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Go `isToonRustBinary`: a binary qualifies if `--help` mentions the Rust
/// reference implementation, or `--version` starts with `tru ` / `toon_rust `.
/// Probing on every call would fork twice per invocation, so the answer is
/// cached after the first lookup.
fn is_toon_rust_binary(path: &std::path::Path) -> bool {
    use std::sync::OnceLock;
    static CACHE: OnceLock<std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, bool>>> =
        OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    if let Ok(map) = cache.lock() {
        if let Some(hit) = map.get(path) {
            return *hit;
        }
    }
    let verdict = probe_toon_binary(path);
    if let Ok(mut map) = cache.lock() {
        map.insert(path.to_path_buf(), verdict);
    }
    verdict
}

fn probe_toon_binary(path: &std::path::Path) -> bool {
    use std::process::Command;
    if let Ok(out) = Command::new(path).arg("--help").output() {
        let text = String::from_utf8_lossy(&out.stdout).to_lowercase();
        let text = format!(
            "{text}{}",
            String::from_utf8_lossy(&out.stderr).to_lowercase()
        );
        if text.contains("reference implementation in rust") {
            return true;
        }
    }
    if let Ok(out) = Command::new(path).arg("--version").output() {
        let text = String::from_utf8_lossy(&out.stdout).trim().to_lowercase();
        if text.starts_with("tru ") || text.starts_with("toon_rust ") {
            return true;
        }
    }
    false
}

/// Encode a robot payload.
///
/// The JSON path is plain `serde_json`: field order follows declaration order
/// (payloads are structs, and serde_json keeps map order off), and the compact
/// form is what the goldens hold.
///
/// The TOON path hands the compact JSON to the `tru`/`toon` binary on stdin and
/// returns its stdout. Go reaches the same bytes through
/// `toon.EncodeWithOptions` (`cmd/bv/main.go:7510`), which the `toon-go`
/// library computes in-process; piping the identical document through the
/// reference implementation reproduces it, verified field-for-field against
/// `golden/toon/`.
pub fn encode_payload<T: Serialize>(
    payload: &T,
    format: OutputFormat,
) -> Result<Vec<u8>, serde_json::Error> {
    // serde_json serializes struct fields in declaration order and maps in
    // insertion order only with preserve_order feature; payloads use structs,
    // so field order is stable. Compact form (no spaces) matches goldens.
    let mut v = serde_json::to_vec(payload)?;
    if format == OutputFormat::Toon {
        // Fall back to the JSON bytes rather than failing the command: Go
        // degrades to JSON when `tru` is missing too (main.go:2040), and the
        // envelope's `output_format` has already been downgraded to "json" on
        // that path, so the marker still tells the truth.
        if let Ok(toon) = encode_toon(&v) {
            return Ok(toon);
        }
    }
    v.push(b'\n');
    Ok(v)
}

/// Run the TOON encoder over a compact JSON document.
///
/// Go trims the encoder's own trailing newline and writes the result with a
/// single one (`strings.TrimRight(out, "\n")`, main.go:7516); the reference
/// binary already terminates with exactly one, but the trim is kept so a
/// different build cannot introduce a second one.
pub fn encode_toon(json: &[u8]) -> Result<Vec<u8>, String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let path = tru_path().ok_or_else(|| "no toon encoder on PATH".to_string())?;
    let mut child = Command::new(&path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawning {}: {e}", path.display()))?;

    child
        .stdin
        .take()
        .ok_or_else(|| "encoder stdin unavailable".to_string())?
        .write_all(json)
        .map_err(|e| format!("writing to {}: {e}", path.display()))?;

    let out = child
        .wait_with_output()
        .map_err(|e| format!("running {}: {e}", path.display()))?;
    if !out.status.success() {
        return Err(format!("{} exited with {}", path.display(), out.status));
    }

    let mut bytes = out.stdout;
    while bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    bytes.push(b'\n');
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RobotLoadStats;

    #[derive(Serialize)]
    struct Sample {
        generated_at: String,
        data_hash: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        load_stats: Option<RobotLoadStats>,
    }

    #[test]
    fn load_stats_emitted_only_when_errors() {
        let clean = bv_core::loader::LoadReport::default();
        assert!(
            RobotEnvelope::new("h", "v", Some(&clean), OutputFormat::Json)
                .load_stats
                .is_none()
        );
        let dirty = bv_core::loader::LoadReport {
            errors: 2,
            ..bv_core::loader::LoadReport::default()
        };
        let env = RobotEnvelope::new("h", "v", Some(&dirty), OutputFormat::Json);
        let stats = env.load_stats.expect("errors>0 must emit load_stats");
        assert_eq!(stats.errors, 2);
    }

    #[test]
    fn field_order_matches_golden() {
        let env = Sample {
            generated_at: "T".into(),
            data_hash: "H".into(),
            load_stats: None,
        };
        let bytes = encode_payload(&env, OutputFormat::Json).unwrap();
        let s = String::from_utf8(bytes).unwrap();
        // generated_at before data_hash, matching golden key order
        let g = s.find("generated_at").unwrap();
        let d = s.find("data_hash").unwrap();
        assert!(g < d);
        assert!(s.ends_with("}\n"));
    }

    #[test]
    fn warnings_capped_shape_roundtrip() {
        let stats = RobotLoadStats {
            source_path: "/x/issues.jsonl".into(),
            valid: 10,
            errors: 1,
            skipped: 3,
            warnings: vec!["skipping malformed JSON on line 4: eof".into()],
        };
        let v = serde_json::to_value(&stats).unwrap();
        assert_eq!(v["valid"], 10);
        assert_eq!(v["warnings"].as_array().unwrap().len(), 1);
    }
}

#[cfg(test)]
mod hash_escaping_tests {
    use super::*;

    /// The concrete divergence: any path or label containing `&` (or `<`/`>`)
    /// hashed differently from Go before this fix, because Go's
    /// `encoding/json` HTML-escapes those bytes and serde_json does not.
    #[test]
    fn scope_hash_escapes_ampersand_and_angle_brackets() {
        let got = scope_hash("a&b<c>d", "", "", "h", &[]);
        let want = sha256_hex(
            br#"{"Label":"a\u0026b\u003cc\u003ed","Recipe":"","Repo":"","DataHash":"h","IDs":[]}"#,
        );
        assert_eq!(got, want, "scope_hash must hash Go-escaped bytes");
    }

    /// Same bytes, escaped vs not, must produce different digests — otherwise
    /// the test above would pass even if escaping were a no-op.
    #[test]
    fn unescaped_and_escaped_differ() {
        let escaped = scope_hash("a&b", "", "", "h", &[]);
        let unescaped =
            sha256_hex(br#"{"Label":"a&b","Recipe":"","Repo":"","DataHash":"h","IDs":[]}"#);
        assert_ne!(escaped, unescaped);
    }

    /// A `&` inside an authority field must reach the digest escaped too.
    #[test]
    fn authority_hash_escapes_html_bytes() {
        let a = RobotSourceAuthority {
            state: "a&b".into(),
            ..RobotSourceAuthority::default()
        };
        let raw = serde_json::to_vec(&a).expect("serializes");
        assert!(
            raw.windows(1).any(|w| w == b"&"),
            "precondition: serde_json left the ampersand unescaped"
        );
        assert_eq!(authority_hash(&a), sha256_hex(&go_html_escape_json(&raw)));
    }

    /// Escaping must not alter anything when no HTML-escapable byte is present,
    /// so ordinary digests are untouched by the fix.
    #[test]
    fn plain_values_are_unchanged_by_escaping() {
        let raw = br#"{"Label":"tui","Recipe":"","Repo":"","DataHash":"abc","IDs":["A-1"]}"#;
        assert_eq!(go_html_escape_json(raw), raw.to_vec());
    }

    /// The rewrite is only sound because these bytes cannot appear as JSON
    /// syntax. Assert structure survives untouched.
    #[test]
    fn escaping_does_not_corrupt_json_structure() {
        let raw = br#"{"a":[1,2],"b":"x"}"#;
        assert_eq!(go_html_escape_json(raw), raw.to_vec());
    }
}

/// Go `boundedSourceMessage` (cmd/bv/main.go:7273) — cap at 1024 *runes*
/// (not bytes) and append an ellipsis. The rune count matters: a multi-byte
/// path or warning truncates at the same character index Go would pick.
pub fn bounded_source_message(message: &str) -> String {
    const MAX_RUNES: usize = 1024;
    if message.chars().count() <= MAX_RUNES {
        return message.to_string();
    }
    let cut: String = message.chars().take(MAX_RUNES).collect();
    format!("{cut}…")
}

/// Go `newRobotSourceAuthority` (cmd/bv/main.go:7282) — the authority reducer.
///
/// This is behaviour, not formatting: `claim_safe` is the field an agent reads
/// before claiming work, and it is false in more cases than "there were
/// errors". In particular a source whose `status` is neither `loaded` nor
/// `disabled` marks the whole authority unsafe on its own, and a *disabled*
/// source contributes nothing to any counter. Both are easy to drop when
/// porting by hand, so the branches are spelled out here.
///
/// Mirrors, in order:
/// - `source_kind == ""` becomes `"unknown"`
/// - warnings truncate to 10 entries, then each entry is rune-bounded
/// - `error` is rune-bounded
/// - accumulate counters, skipping `disabled` entirely
/// - `Loaded == 0` forces `state: "unknown"` and clears `claim_safe`
pub fn new_source_authority(mut sources: Vec<RobotSourceReport>) -> RobotSourceAuthority {
    let mut authority = RobotSourceAuthority {
        state: "complete".to_string(),
        claim_safe: true,
        readiness: "proven".to_string(),
        sources: Vec::new(),
        ..Default::default()
    };

    for mut source in sources.drain(..) {
        if source.source_kind.is_empty() {
            source.source_kind = "unknown".to_string();
        }
        if source.warnings.len() > MAX_SOURCE_WARNINGS {
            source.warnings.truncate(MAX_SOURCE_WARNINGS);
        }
        for warning in &mut source.warnings {
            *warning = bounded_source_message(warning);
        }
        source.error = bounded_source_message(&source.error);

        match source.status.as_str() {
            // A disabled source is not evidence about the workspace: it adds
            // nothing to any counter and cannot make the authority unsafe.
            "disabled" => {
                authority.disabled += 1;
                continue;
            }
            "loaded" => authority.loaded += 1,
            // Anything else is a failed source, and one is enough to make the
            // whole authority unsafe regardless of its error count.
            _ => {
                authority.failed += 1;
                authority.claim_safe = false;
            }
        }

        authority.valid += source.valid;
        authority.errors += source.errors;
        authority.skipped += source.skipped;
        authority.read_errors += source.read_errors;
        authority.visible += source.visible;
        authority.tombstones += source.tombstones;
        authority.warning_count += source.warning_count;
        if source.errors > 0 || source.read_errors > 0 || source.stale {
            authority.claim_safe = false;
        }
        authority.sources.push(source);
    }

    // Go sorts before hashing, so the digest does not depend on the order the
    // loader happened to discover sources in. `sort.SliceStable` keeps
    // discovery order among equal keys.
    authority.sources.sort_by(|a, b| {
        a.repo_path
            .cmp(&b.repo_path)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.source_path.cmp(&b.source_path))
    });

    if authority.loaded == 0 {
        authority.state = "unknown".to_string();
        authority.claim_safe = false;
    } else if !authority.claim_safe {
        authority.state = "partial".to_string();
    }
    if !authority.claim_safe {
        authority.readiness = "provisional".to_string();
    }
    authority
}

/// Go truncates a source's warning list to 10 before the authority hashes it
/// (cmd/bv/main.go:7291), so the cap is part of the digest, not just display.
const MAX_SOURCE_WARNINGS: usize = 10;

#[cfg(test)]
mod source_authority_tests {
    use super::*;

    fn report(status: &str) -> RobotSourceReport {
        RobotSourceReport {
            source_kind: "jsonl".into(),
            status: status.into(),
            valid: 3,
            visible: 3,
            ..Default::default()
        }
    }

    /// The branch that is easiest to miss: a source that is neither `loaded`
    /// nor `disabled` makes the authority unsafe even with zero errors.
    #[test]
    fn failed_status_makes_authority_unsafe_without_any_errors() {
        // A lone failed source leaves Loaded == 0, so Go's first branch wins
        // and the state is "unknown" — "partial" requires a loaded source too.
        let only_failed = new_source_authority(vec![report("failed")]);
        assert!(!only_failed.claim_safe);
        assert_eq!(only_failed.failed, 1);
        assert_eq!(only_failed.state, "unknown");
        assert_eq!(only_failed.readiness, "provisional");

        let mixed = new_source_authority(vec![report("loaded"), report("failed")]);
        assert!(!mixed.claim_safe);
        assert_eq!(mixed.loaded, 1);
        assert_eq!(mixed.failed, 1);
        assert_eq!(mixed.state, "partial");
        assert_eq!(mixed.readiness, "provisional");
    }

    /// A disabled source contributes to `Disabled` and to nothing else, and
    /// cannot make the authority unsafe.
    #[test]
    fn disabled_source_contributes_no_counters() {
        let mut s = report("disabled");
        s.valid = 99;
        s.errors = 0;
        s.read_errors = 0;
        s.stale = true;
        let a = new_source_authority(vec![s]);
        assert_eq!(a.disabled, 1);
        assert_eq!(a.valid, 0, "disabled must not accumulate valid");
        assert_eq!(a.tombstones, 0);
        // Loaded == 0 forces the override, so claim_safe is false regardless.
        assert_eq!(a.state, "unknown");
        assert!(!a.claim_safe);
    }

    #[test]
    fn stale_read_errors_and_errors_each_clear_claim_safe() {
        for mutate in [
            (|s: &mut RobotSourceReport| s.errors = 1) as fn(&mut RobotSourceReport),
            |s: &mut RobotSourceReport| s.read_errors = 1,
            |s: &mut RobotSourceReport| s.stale = true,
        ] {
            let mut s = report("loaded");
            mutate(&mut s);
            let a = new_source_authority(vec![s]);
            assert!(!a.claim_safe, "expected unsafe for {mutate:?}");
            assert_eq!(a.state, "partial");
        }
    }

    #[test]
    fn clean_loaded_source_is_proven() {
        let a = new_source_authority(vec![report("loaded")]);
        assert!(a.claim_safe);
        assert_eq!(a.state, "complete");
        assert_eq!(a.readiness, "proven");
        assert_eq!(a.loaded, 1);
    }

    /// `Loaded == 0` overwrites claim_safe even when every source is disabled.
    #[test]
    fn zero_loaded_forces_unknown_even_with_all_disabled() {
        let a = new_source_authority(vec![report("disabled"), report("disabled")]);
        assert_eq!(a.loaded, 0);
        assert_eq!(a.disabled, 2);
        assert_eq!(a.state, "unknown");
        assert!(!a.claim_safe);
        assert_eq!(a.readiness, "provisional");
    }

    #[test]
    fn empty_source_kind_becomes_unknown() {
        let mut s = report("loaded");
        s.source_kind = String::new();
        let a = new_source_authority(vec![s]);
        assert_eq!(a.sources[0].source_kind, "unknown");
    }

    #[test]
    fn warnings_truncate_to_ten_and_are_rune_bounded() {
        let mut s = report("loaded");
        s.warnings = (0..25).map(|i| format!("w{i}")).collect();
        let a = new_source_authority(vec![s]);
        assert_eq!(a.sources[0].warnings.len(), MAX_SOURCE_WARNINGS);
        assert_eq!(a.sources[0].warnings[0], "w0");
        assert_eq!(a.sources[0].warnings[9], "w9");
    }

    #[test]
    fn bounded_source_message_counts_runes_not_bytes() {
        assert_eq!(bounded_source_message("short"), "short");
        let long = "é".repeat(2000);
        let out = bounded_source_message(&long);
        assert_eq!(out.chars().count(), 1025, "1024 runes plus the ellipsis");
        assert!(out.ends_with('…'));
    }

    /// The sort is part of the digest contract: discovery order must not leak
    /// into `authority_hash`.
    #[test]
    fn sources_sort_by_repo_path_name_source_path() {
        let mk = |repo: &str, name: &str, path: &str| RobotSourceReport {
            repo_path: repo.into(),
            name: name.into(),
            source_path: path.into(),
            status: "loaded".into(),
            ..Default::default()
        };
        let a = new_source_authority(vec![
            mk("b", "x", "2"),
            mk("a", "z", "1"),
            mk("a", "a", "3"),
        ]);
        let keys: Vec<_> = a
            .sources
            .iter()
            .map(|s| {
                (
                    s.repo_path.as_str(),
                    s.name.as_str(),
                    s.source_path.as_str(),
                )
            })
            .collect();
        assert_eq!(
            keys,
            vec![("a", "a", "3"), ("a", "z", "1"), ("b", "x", "2")]
        );
    }
}
