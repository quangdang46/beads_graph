//! Download → verify → extract → install — port of Go `PerformUpdate`,
//! `Rollback`, `downloadFile`, `verifyChecksum`, `extractBinary`.

use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::github::{FetchError, Release};
use crate::{BINARY_NAME, REPO_NAME, REPO_OWNER};

/// Max download / extracted-binary size (Go: 512 MiB).
pub const MAX_DOWNLOAD_BYTES: u64 = 512 << 20;
/// Max release-metadata body (Go parity, also in github.rs).
pub const MAX_RELEASE_METADATA_BYTES: usize = 1 << 20;
/// Go `maxChecksumManifestBytes` (updater.go:33), enforced when parsing.
pub const MAX_CHECKSUM_MANIFEST_BYTES: usize = 1 << 20;
/// Go `maxBinaryVersionOutputBytes` (updater.go:1464).
const MAX_BINARY_VERSION_OUTPUT_BYTES: usize = 4 << 10;
/// Go's `verifyBinaryVersion` context timeout (updater.go:1487).
const BINARY_VERSION_TIMEOUT: Duration = Duration::from_secs(10);
/// Go's User-Agent for `downloadFile` (updater.go:987), renamed for bvr.
const USER_AGENT_UPDATER: &str = "bvr-updater";

/// Versions a stock bvr release binary is allowed to self-report from
/// `--version` *in addition to* the tag being installed.
///
/// This is a fork-specific accommodation, not a weakening of Go's check.
/// `bvr --version` prints `bv <GO_APP_VERSION>` where `GO_APP_VERSION` is the
/// hardcoded Go-compatibility constant `"v0.25.0"`
/// (`crates/bv/src/main.rs:2492`, printed at `main.rs:229-231`) — it is
/// deliberately frozen so the binary's own `--version` matches the Go oracle
/// byte for byte, and it therefore does **not** track the release tag.
/// Demanding `fields[1] == tag` would make every `bvr --update` fail with
/// `downloaded binary reports v0.25.0, expected v0.2.1`.
///
/// The tag↔archive binding is not weakened by this: the archive was verified
/// against the SHA-256 digest GitHub published for that exact release asset
/// (`checked_platform_asset` requires the downloaded manifest and the API
/// digest to agree, then `verify_checksum` re-hashes the bytes). The
/// `--version` run is the *last* check and catches the failure modes that
/// survive a byte-exact download: a wrong-architecture binary that cannot
/// execute, a truncated file that still runs, or a file that is not a bvr at
/// all. Anything outside this list is still a hard error, so if the constant
/// is ever bumped the updater fails loudly rather than installing blindly —
/// add the new value here at the same time.
const ACCEPTED_SELF_REPORTED_VERSIONS: [&str; 1] = ["v0.25.0"];

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("fetch failed: {0}")]
    Fetch(#[from] FetchError),
    #[error("no binary available for this platform")]
    NoAsset,
    #[error("unsafe release asset name {0:?}")]
    UnsafeAssetName(String),
    #[error("download failed: {0}")]
    Download(String),
    #[error("size mismatch: expected {expected}, got {got}")]
    SizeMismatch { expected: i64, got: i64 },
    #[error("download size {0} exceeds maximum {MAX_DOWNLOAD_BYTES}")]
    TooLarge(i64),
    #[error("checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },
    #[error("no checksum found for {0}")]
    NoChecksum(String),
    #[error("invalid checksum {0:?}: {1}")]
    BadChecksum(String, String),
    #[error("duplicate checksum entry for {0}")]
    DuplicateChecksum(String),
    #[error("extraction failed: {0}")]
    Extract(String),
    #[error("new binary verification failed: {0}")]
    Verify(String),
    #[error("cannot determine binary path: {0}")]
    BinaryPath(String),
    /// The install directory is not writable. The payload carries Go's
    /// message tail — `"<dir> (try running with sudo): <cause>"`, matching
    /// `updater.go:1314` in *shape*. The tail is not byte-identical: Go
    /// formats a wrapped `*os.PathError` (`open <probe>: permission denied`)
    /// and Rust an `io::Error` (`Permission denied (os error 13)`), and the
    /// random probe filename Go embeds has no Rust counterpart here.
    #[error("no write permission to {0}")]
    NoPermission(String),
    /// The directory could not be prepared for a reason that is *not* a
    /// permission problem (ENOSPC, EROFS, EIO, …). Go refuses to suggest
    /// `sudo` for these (updater.go:1316).
    #[error("cannot prepare update in {dir}: {cause}")]
    CannotPrepare { dir: String, cause: String },
    #[error("release {tag} is not installable: {reason}")]
    NotInstallable { tag: String, reason: String },
    #[error("checksum manifest verification failed: {0}")]
    ChecksumManifest(String),
    #[error("failed to parse checksums: {0}")]
    ParseChecksums(String),
    /// A failure Go returns unwrapped (`return nil, err`, updater.go:1332 and
    /// 1344). Re-wrapping these would add a "failed to …" prefix Go never
    /// prints — the manifest-digest failure and the "manifest disagrees with
    /// GitHub" / "no entry for this platform" failures are terminal in Go and
    /// should read identically here.
    #[error("{0}")]
    Manifest(String),
    #[error("invalid version: {0}")]
    Version(String),
    #[error("backup failed: {reason}")]
    Backup {
        reason: String,
        backup_path: Option<String>,
    },
    #[error("installation failed: {reason}")]
    Install {
        reason: String,
        backup_path: Option<String>,
    },
    #[error("no backup found at {0}")]
    NoBackup(String),
    #[error("rollback failed: {0}")]
    Rollback(String),
    #[error("io error: {0}")]
    Io(String),
}

impl UpdateError {
    /// Path of the preserved backup when the failure happened *after* the
    /// backup was written. Go returns a partially filled `UpdateResult`
    /// alongside its error for exactly this case (`updater.go:1394` sets
    /// `BackupPath` before the risky rename) and `cmd/bv` prints
    /// `Backup preserved at: %s` before exiting 1 (`main.go:2145-2151`).
    /// Without it the backup exists on disk but the user is never told.
    pub fn backup_path(&self) -> Option<&str> {
        match self {
            UpdateError::Backup { backup_path, .. } | UpdateError::Install { backup_path, .. } => {
                backup_path.as_deref()
            }
            _ => None,
        }
    }
}

/// Outcome of a successful update.
#[derive(Debug, Clone)]
pub struct UpdateResult {
    pub old_version: String,
    pub new_version: String,
    pub backup_path: Option<String>,
    pub message: String,
}

/// Path of the currently running binary (resolves symlinks like Go).
pub fn current_binary_path() -> Result<PathBuf, UpdateError> {
    let exe = std::env::current_exe().map_err(|e| UpdateError::BinaryPath(e.to_string()))?;
    exe.canonicalize()
        .map_err(|e| UpdateError::BinaryPath(e.to_string()))
}

/// Render a path for a message, the way Go's `filepath` would print it.
///
/// `Path::canonicalize` returns a *verbatim* path on Windows — `\\?\C:\...` —
/// which is a correct way to hand a path back to the OS but not what a user
/// expects to read, and not what Go produces. Go resolves symlinks through
/// `filepath.EvalSymlinks` and prints an ordinary `C:\...`, so a rollback error
/// that named `\\?\C:\...\bvr-under-test.backup` would differ from Go's text on
/// every Windows install of the same file.
///
/// The prefix is stripped for display only; nothing here changes the path that
/// is opened, renamed or written.
fn display_path(path: &Path) -> String {
    let s = path.display().to_string();
    // `\\?\UNC\server\share\...` is the verbatim spelling of a UNC path.
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    match s.strip_prefix(r"\\?\") {
        Some(rest) => rest.to_string(),
        None => s,
    }
}

/// Backup path for a binary (`<bin>.backup`, Go parity).
pub fn backup_path(binary: &Path) -> PathBuf {
    let mut s = binary.as_os_str().to_owned();
    s.push(".backup");
    PathBuf::from(s)
}

/// Reject empty / `.` / `..` / path-traversal asset names (Go `safeAssetName`).
pub fn safe_asset_name(name: &str) -> Result<&str, UpdateError> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || Path::new(name)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            != Some(name.to_string())
    {
        return Err(UpdateError::UnsafeAssetName(name.to_string()));
    }
    Ok(name)
}

/// Parse a `checksums.txt`-style manifest into filename → sha256.
///
/// Line-for-line port of Go `parseChecksumData` (updater.go:1056-1097), whose
/// two subtleties the previous Rust version got wrong:
///  * the filename key is `TrimSpace(line[len(rawHash):])` — so the GNU
///    binary-mode line `<hash> *file` keys as `*file`, and a later platform
///    lookup for `file` correctly misses. Stripping the `*` here would let Rust
///    install an archive Go refuses.
///  * `strings.TrimSpace` trims both ends of the line, and `strings.Fields`
///    splits on any Unicode whitespace, so a manifest padded with spaces or
///    tabs parses identically to one that is not.
pub fn parse_checksums(
    data: &str,
) -> Result<std::collections::HashMap<String, String>, UpdateError> {
    use std::collections::HashMap;
    if data.len() > MAX_CHECKSUM_MANIFEST_BYTES {
        return Err(UpdateError::BadChecksum(
            format!("{} bytes", data.len()),
            format!("checksum manifest exceeds {MAX_CHECKSUM_MANIFEST_BYTES} bytes"),
        ));
    }
    let mut out = HashMap::new();
    // Go splits on "\n" only; a "\r" therefore stays part of the filename,
    // exactly as it does there.
    for raw_line in data.split('\n') {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        // Format: "<sha256> <whitespace> <filename> (may itself contain spaces)>"
        let mut fields = line.split_whitespace();
        let Some(raw_hash) = fields.next() else {
            continue;
        };
        if fields.next().is_none() {
            // len(parts) < 2 — no filename to key on.
            continue;
        }
        let hash = raw_hash.to_lowercase();
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        if line.len() < raw_hash.len() || !line.starts_with(raw_hash) {
            continue;
        }
        let filename = line[raw_hash.len()..].trim();
        if filename.is_empty() {
            continue;
        }
        if out.contains_key(filename) {
            return Err(UpdateError::DuplicateChecksum(filename.to_string()));
        }
        out.insert(filename.to_string(), hash);
    }
    Ok(out)
}

/// Verify the SHA-256 of a file against `expected` hex.
pub fn verify_checksum(path: &Path, expected: &str) -> Result<(), UpdateError> {
    let expected = expected.trim().to_lowercase();
    if expected.len() != 64 || !expected.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(UpdateError::BadChecksum(
            expected,
            "invalid sha256 length/charset".into(),
        ));
    }
    let mut f = std::fs::File::open(path).map_err(|e| UpdateError::Io(e.to_string()))?;
    let mut h = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| UpdateError::Io(e.to_string()))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    let actual = hex_encode(h.finalize());
    if actual != expected {
        return Err(UpdateError::ChecksumMismatch { expected, actual });
    }
    Ok(())
}

fn hex_encode(bytes: impl AsRef<[u8]>) -> String {
    bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// Download `url` to `dest`, enforcing `expected_size` when > 0.
///
/// Port of Go `downloadFile` (updater.go:961-1036). The `Authorization` header
/// is attached only when the user opted in *and* the URL is a GitHub host
/// (`prefs::github_auth_header`, Go `setGitHubAuth` at updater.go:988).
/// Redirects drop the header unconditionally, which is stricter than Go's
/// `CheckRedirect` — Go deletes it only when a redirect leaves the GitHub
/// hosts (updater.go:977-979) — so a credential can never ride a redirect to
/// a third-party CDN.
pub fn download_file(url: &str, dest: &Path, expected_size: i64) -> Result<(), UpdateError> {
    if expected_size < 0 {
        return Err(UpdateError::Download(format!(
            "download size cannot be negative: {expected_size}"
        )));
    }
    if expected_size as u64 > MAX_DOWNLOAD_BYTES {
        return Err(UpdateError::TooLarge(expected_size));
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(300)))
        .redirect_auth_headers(ureq::config::RedirectAuthHeaders::Never)
        .http_status_as_error(false)
        .build()
        .into();
    let mut req = agent.get(url).header("User-Agent", USER_AGENT_UPDATER);
    if let Some(auth) = crate::prefs::github_auth_header(url) {
        req = req.header("Authorization", &auth);
    }
    let resp = req
        .call()
        .map_err(|e| UpdateError::Download(e.to_string()))?;
    if resp.status().as_u16() != 200 {
        return Err(UpdateError::Download(format!(
            "download returned status: {}",
            resp.status()
        )));
    }
    // Go parity: reject early when the Content-Length header disagrees.
    if expected_size > 0 {
        if let Some(len) = resp.headers().get("content-length") {
            if let Ok(s) = len.to_str() {
                if let Ok(header_len) = s.trim().parse::<i64>() {
                    if header_len != expected_size {
                        return Err(UpdateError::SizeMismatch {
                            expected: expected_size,
                            got: header_len,
                        });
                    }
                }
            }
        }
    }
    let out = std::fs::File::create(dest).map_err(|e| UpdateError::Io(e.to_string()))?;
    let mut out = std::io::BufWriter::new(out);
    let limit: u64 = if expected_size > 0 {
        expected_size as u64 + 1
    } else {
        MAX_DOWNLOAD_BYTES + 1
    };
    let n = std::io::copy(&mut resp.into_body().into_reader().take(limit), &mut out)
        .map_err(|e| UpdateError::Download(format!("failed to write file: {e}")))?;
    out.flush().map_err(|e| UpdateError::Io(e.to_string()))?;
    if expected_size > 0 && n != expected_size as u64 {
        return Err(UpdateError::SizeMismatch {
            expected: expected_size,
            got: n as i64,
        });
    }
    if expected_size == 0 && n > MAX_DOWNLOAD_BYTES {
        return Err(UpdateError::TooLarge(n as i64));
    }
    Ok(())
}

/// Extract the `bvr` binary from a `.tar.gz` or `.zip` archive.
pub fn extract_binary(archive: &Path, dest: &Path) -> Result<(), UpdateError> {
    let is_zip = archive
        .extension()
        .map(|e| e.eq_ignore_ascii_case("zip"))
        .unwrap_or(false);
    if is_zip {
        extract_from_zip(archive, dest)
    } else {
        extract_from_tar_gz(archive, dest)
    }
}

fn binary_name_in_archive(name: &str) -> bool {
    let base = Path::new(name)
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    base == BINARY_NAME || base == format!("{BINARY_NAME}.exe")
}

fn write_bounded(src: impl Read, dest: &Path) -> Result<(), UpdateError> {
    let mut src = src.take(MAX_DOWNLOAD_BYTES + 1);
    let out = std::fs::File::create(dest).map_err(|e| UpdateError::Extract(e.to_string()))?;
    let mut out = std::io::BufWriter::new(out);
    let n = std::io::copy(&mut src, &mut out).map_err(|e| UpdateError::Extract(e.to_string()))?;
    out.flush()
        .map_err(|e| UpdateError::Extract(e.to_string()))?;
    if n > MAX_DOWNLOAD_BYTES {
        return Err(UpdateError::Extract(format!(
            "extracted binary exceeds maximum size {MAX_DOWNLOAD_BYTES}"
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755));
    }
    Ok(())
}

fn extract_from_tar_gz(archive: &Path, dest: &Path) -> Result<(), UpdateError> {
    let f = std::fs::File::open(archive).map_err(|e| UpdateError::Extract(e.to_string()))?;
    let gz = flate2::read::GzDecoder::new(f);
    let mut tar = tar::Archive::new(gz);
    let entries = tar
        .entries()
        .map_err(|e| UpdateError::Extract(e.to_string()))?;
    for entry in entries {
        let entry = entry.map_err(|e| UpdateError::Extract(e.to_string()))?;
        let path = entry
            .path()
            .map_err(|e| UpdateError::Extract(e.to_string()))?;
        let name = path.to_string_lossy().into_owned();
        if !binary_name_in_archive(&name) {
            continue;
        }
        if entry.header().entry_type().is_dir() {
            continue;
        }
        let size = entry
            .header()
            .size()
            .map_err(|e| UpdateError::Extract(e.to_string()))?;
        if size > MAX_DOWNLOAD_BYTES {
            return Err(UpdateError::Extract(format!(
                "extracted binary size {size} exceeds maximum {MAX_DOWNLOAD_BYTES}"
            )));
        }
        return write_bounded(entry, dest);
    }
    Err(UpdateError::Extract("binary not found in archive".into()))
}

fn extract_from_zip(archive: &Path, dest: &Path) -> Result<(), UpdateError> {
    let f = std::fs::File::open(archive).map_err(|e| UpdateError::Extract(e.to_string()))?;
    let mut zip = zip::ZipArchive::new(f).map_err(|e| UpdateError::Extract(e.to_string()))?;
    for i in 0..zip.len() {
        let entry = zip
            .by_index(i)
            .map_err(|e| UpdateError::Extract(e.to_string()))?;
        let name = entry.name().to_string();
        if !binary_name_in_archive(&name) || entry.is_dir() {
            continue;
        }
        if entry.size() > MAX_DOWNLOAD_BYTES {
            return Err(UpdateError::Extract(format!(
                "extracted binary size {} exceeds maximum {MAX_DOWNLOAD_BYTES}",
                entry.size()
            )));
        }
        return write_bounded(entry, dest);
    }
    Err(UpdateError::Extract("binary not found in archive".into()))
}

/// Monotonic counter folded into the probe filename so two updates in one
/// process can never pick the same name.
static PROBE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A path under `dir` that cannot collide with anything.
///
/// Stands in for Go's `os.CreateTemp(dir, ".bv-update-test-*")`
/// (updater.go:1256) and `os.MkdirTemp("", "bv-update-*")` (updater.go:1320).
///
/// The previous Rust probe used the fixed name `<dir>/.bvr-update-test`, which
/// a pre-existing symlink would make `File::create` follow — clobbering the
/// symlink's target — and which two concurrent updates would fight over.
fn unique_path(dir: &Path, prefix: &str) -> PathBuf {
    let n = PROBE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    dir.join(format!("{prefix}-{}-{n}-{nanos:x}", std::process::id()))
}

/// Create a fresh staging directory, standing in for Go's
/// `os.MkdirTemp("", "bv-update-*")` (updater.go:1320).
///
/// `create_dir` is the atomic `mkdir(2)`: it fails if the name is taken, so
/// two concurrent updates can never share a staging directory the way
/// `create_dir_all` would (which silently reuses an existing one). Retries
/// exist only to survive the vanishingly unlikely name collision.
fn create_unique_dir() -> Result<PathBuf, UpdateError> {
    let parent = std::env::temp_dir();
    let mut last = std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not create a unique staging directory",
    );
    for _ in 0..16 {
        let candidate = unique_path(&parent, "bvr-update");
        match std::fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = e,
            Err(e) => {
                return Err(UpdateError::Io(format!(
                    "failed to create temp directory: {e}"
                )))
            }
        }
    }
    Err(UpdateError::Io(format!(
        "failed to create temp directory: {last}"
    )))
}

/// Map a probe failure onto Go's two distinct errors (updater.go:1311-1317):
/// only a genuine permission failure earns the `sudo` hint; a full disk, a
/// read-only mount, or an I/O error is "cannot prepare update".
fn probe_error(dir: &Path, cause: &std::io::Error) -> UpdateError {
    if cause.kind() == std::io::ErrorKind::PermissionDenied {
        UpdateError::NoPermission(format!(
            "{} (try running with sudo): {cause}",
            display_path(dir)
        ))
    } else {
        UpdateError::CannotPrepare {
            dir: display_path(dir),
            cause: cause.to_string(),
        }
    }
}

/// Check the install directory is writable (Go `checkBinaryDirectoryWritable`).
fn check_writable(binary_path: &Path) -> Result<(), UpdateError> {
    use std::fs::OpenOptions;
    let dir = binary_path.parent().unwrap_or(Path::new("."));
    let probe = unique_path(dir, ".bvr-update-test");
    // `create_new` is O_EXCL: the call fails if anything already occupies the
    // name, so the probe can never truncate or follow a planted symlink.
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .map_err(|e| probe_error(dir, &e))?;
    // Go closes the probe and reports a close failure (updater.go:1261-1265).
    // The file is empty, so there is nothing to flush and `drop` cannot fail;
    // Rust's `File` has no fallible close.
    drop(file);
    std::fs::remove_file(&probe).map_err(|e| UpdateError::CannotPrepare {
        dir: display_path(dir),
        cause: format!("remove update permission probe: {e}"),
    })
}

/// Output of a child process, truncated to `MAX_BINARY_VERSION_OUTPUT_BYTES`
/// (Go `limitedOutputBuffer`, updater.go:1466-1484). Bytes past the cap are
/// drained and discarded so a chatty child can never block on a full pipe.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct CappedOutput {
    text: String,
    truncated: bool,
}

fn cap_output(mut reader: impl Read, cap: usize) -> CappedOutput {
    let mut kept: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut truncated = false;
    loop {
        match reader.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                // Go marks `truncated` when a write would exceed the buffer
                // (updater.go:1475-1481), and keeps accepting the remainder so
                // the pipe never fills.
                let room = cap.saturating_sub(kept.len());
                if n > room {
                    truncated = true;
                }
                if room > 0 {
                    kept.extend_from_slice(&chunk[..room.min(n)]);
                }
            }
        }
    }
    CappedOutput {
        text: String::from_utf8_lossy(&kept).into_owned(),
        truncated,
    }
}

/// Go `parseBinaryVersionOutput` (updater.go:1453-1462): exactly two
/// whitespace-separated fields, the first being the program name.
///
/// `bvr --version` prints Go's exact shape — `bv <version>` (see the
/// `--version` short-circuit in `crates/bv/src/main.rs:229-231`) — so this
/// check is both Go parity and true of the Rust binary.
pub fn parse_binary_version_output(output: &str) -> Result<String, String> {
    let fields: Vec<&str> = output.split_whitespace().collect();
    if fields.len() != 2 || fields[0] != "bv" {
        return Err(format!("unexpected --version output {:?}", output.trim()));
    }
    crate::version::parse_version(fields[1])
        .map_err(|e| format!("invalid version in --version output: {e}"))?;
    Ok(fields[1].to_string())
}

/// Go `verifyBinaryVersion` (updater.go:1486-1519): run `<binary> --version`
/// under a 10s timeout and require it to report the tag being installed (or a
/// version in `ACCEPTED_SELF_REPORTED_VERSIONS` — see there for why).
///
/// Before this, Rust only checked that the child exited zero, so a truncated,
/// stale, or wrong-architecture binary that happened to run was installed
/// silently. Exposed so the behaviour can be tested against synthetic
/// `--version` scripts without touching the network or the real binary.
pub fn verify_binary_version(binary: &Path, expected_version: &str) -> Result<(), String> {
    use std::process::{Command, Stdio};
    let mut child = Command::new(binary)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("run --version: {e}"))?;
    let out_pipe = child.stdout.take();
    let err_pipe = child.stderr.take();
    let out_thread = std::thread::spawn(move || {
        cap_output(
            out_pipe.expect("stdout was piped"),
            MAX_BINARY_VERSION_OUTPUT_BYTES,
        )
    });
    let err_thread = std::thread::spawn(move || {
        cap_output(
            err_pipe.expect("stderr was piped"),
            MAX_BINARY_VERSION_OUTPUT_BYTES,
        )
    });

    let deadline = Instant::now() + BINARY_VERSION_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    // Go's `cmd.WaitDelay` analogue: kill, then reap.
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                let _ = child.wait();
                return Err(format!("run --version: {e}"));
            }
        }
    };
    let stdout = out_thread.join().unwrap_or_default();
    let stderr = err_thread.join().unwrap_or_default();

    let Some(status) = status else {
        return Err("--version timed out: context deadline exceeded".to_string());
    };
    if !status.success() {
        return Err(format!(
            "run --version: {status} (stderr: {:?})",
            stderr.text.trim()
        ));
    }
    if stdout.truncated {
        return Err(format!(
            "--version output exceeds {MAX_BINARY_VERSION_OUTPUT_BYTES} bytes"
        ));
    }
    let reported = parse_binary_version_output(&stdout.text)?;
    crate::version::parse_version(expected_version)?;
    // Go's `normalize` (updater.go:1512-1514): prepend "v" after trimming and
    // stripping one leading "v".
    let normalize = |value: &str| {
        let value = value.trim();
        format!("v{}", value.strip_prefix('v').unwrap_or(value))
    };
    if normalize(&reported) == normalize(expected_version) {
        return Ok(());
    }
    if ACCEPTED_SELF_REPORTED_VERSIONS
        .iter()
        .any(|allowed| normalize(allowed) == normalize(&reported))
    {
        return Ok(());
    }
    Err(format!(
        "downloaded binary reports {reported}, expected {expected_version}"
    ))
}

fn copy_file(src: &Path, dst: &Path) -> Result<(), UpdateError> {
    std::fs::copy(src, dst).map_err(|e| UpdateError::Io(e.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(dst, std::fs::Permissions::from_mode(0o755));
    }
    Ok(())
}

/// Download + verify + install `release` over the running binary.
/// `progress` receives human-readable lines (Go's `fmt.Println` steps).
///
/// Step-for-step port of Go `PerformUpdate` (updater.go:1275-1426). Two
/// behaviours the previous Rust version did not have:
///
///  * the checksum manifest is **mandatory** (Go `releaseAssetsForUpdate`
///    errors with `checksums.txt asset is missing`, updater.go:849-851) and the
///    manifest is itself verified against the `sha256:` digest GitHub reports
///    for that asset (updater.go:1330-1336) before it is parsed, and
///  * the extracted binary's `--version` is compared against the tag
///    (`verifyBinaryVersion`, updater.go:1375).
pub fn perform_update(
    release: &Release,
    progress: &dyn Fn(&str),
) -> Result<UpdateResult, UpdateError> {
    let old = crate::version::current_version();
    // Go `isNewerVersion` (updater.go:1288-1296): a malformed tag is an error,
    // not a silent "already up to date".
    let newer =
        crate::version::is_newer_version(&release.tag_name, &old).map_err(UpdateError::Version)?;
    if !newer {
        return Ok(UpdateResult {
            message: format!("Already at version {old} (latest: {})", release.tag_name),
            old_version: old,
            new_version: release.tag_name.clone(),
            backup_path: None,
        });
    }

    // Go `releaseAssetsForUpdate` (updater.go:1298-1301): identity, platform
    // asset, and the mandatory checksum manifest are all validated up front.
    let (_, checksum_asset) =
        crate::github::release_assets_for_update(release).map_err(|reason| {
            UpdateError::NotInstallable {
                tag: release.tag_name.clone(),
                reason,
            }
        })?;

    let binary_path = current_binary_path()?;
    check_writable(&binary_path)?;

    // Go `os.MkdirTemp("", "bv-update-*")` — unique per invocation, so two
    // concurrent updates cannot share a staging directory.
    let tmp = create_unique_dir()?;
    let cleanup_tmp = || {
        let _ = std::fs::remove_dir_all(&tmp);
    };

    // 1. Download the manifest, then verify the manifest itself against the
    //    digest GitHub published for that asset (updater.go:1326-1336).
    let sum_path = tmp.join("checksums.txt");
    if let Err(e) = download_file(
        &checksum_asset.browser_download_url,
        &sum_path,
        checksum_asset.size,
    ) {
        cleanup_tmp();
        return Err(UpdateError::Download(format!(
            "checksum download failed: {e}"
        )));
    }
    let checksum_digest =
        crate::github::asset_sha256_digest(checksum_asset).map_err(UpdateError::Manifest)?;
    if let Err(e) = verify_checksum(&sum_path, &checksum_digest) {
        cleanup_tmp();
        return Err(UpdateError::ChecksumManifest(e.to_string()));
    }
    let data = match std::fs::read_to_string(&sum_path) {
        Ok(data) => data,
        Err(e) => {
            cleanup_tmp();
            return Err(UpdateError::Io(e.to_string()));
        }
    };
    // `.sha256` sidecar is "<hash>  <archive>"; `checksums.txt` is multi-line.
    // Both parse through the same parser; single-line works fine.
    let checksums = match parse_checksums(&data) {
        Ok(m) => m,
        Err(e) => {
            cleanup_tmp();
            return Err(UpdateError::ParseChecksums(e.to_string()));
        }
    };

    // 2. The manifest must cover the platform archive *and* agree with the
    //    API digest for it (Go `checkedPlatformAsset`, updater.go:1342).
    //    Go returns this error unwrapped, so it renders without a prefix.
    let (asset, asset_digest) = match crate::github::checked_platform_asset(release, &checksums) {
        Ok(pair) => pair,
        Err(e) => {
            cleanup_tmp();
            return Err(UpdateError::Manifest(e));
        }
    };

    // 3. Download and verify the archive itself.
    let asset_name = match safe_asset_name(&asset.name) {
        Ok(name) => name.to_string(),
        Err(e) => {
            cleanup_tmp();
            return Err(e);
        }
    };
    let archive_path = tmp.join(&asset_name);
    progress(&format!("Downloading {}...", release.tag_name));
    if let Err(e) = download_file(&asset.browser_download_url, &archive_path, asset.size) {
        cleanup_tmp();
        return Err(UpdateError::Download(format!("download failed: {e}")));
    }
    progress("Verifying checksum...");
    if let Err(e) = verify_checksum(&archive_path, &asset_digest) {
        cleanup_tmp();
        return Err(UpdateError::Download(format!(
            "checksum verification failed: {e}"
        )));
    }

    let mut new_bin = tmp.join(format!("{BINARY_NAME}-new"));
    if cfg!(target_os = "windows") {
        new_bin.set_extension("exe");
    }
    progress("Extracting...");
    if let Err(e) = extract_binary(&archive_path, &new_bin) {
        cleanup_tmp();
        return Err(UpdateError::Extract(format!("extraction failed: {e}")));
    }

    // Verify the new binary runs AND reports the tag we asked for
    // (Go `verifyBinaryVersion`, updater.go:1375).
    progress("Verifying new binary...");
    if let Err(e) = verify_binary_version(&new_bin, &release.tag_name) {
        cleanup_tmp();
        return Err(UpdateError::Verify(format!(
            "new binary verification failed: {e}"
        )));
    }

    // Backup: rename current out of the way (avoids ETXTBSY / file-in-use),
    // fall back to copy so an existing backup is never destroyed first.
    let backup = backup_path(&binary_path);
    progress(&format!(
        "Backing up current binary to {}...",
        backup.display()
    ));
    let mut moved_for_backup = false;
    if std::fs::rename(&binary_path, &backup).is_ok() {
        moved_for_backup = true;
    } else if copy_file(&binary_path, &backup).is_err() {
        cleanup_tmp();
        return Err(UpdateError::Backup {
            reason: "backup failed".into(),
            // The rename may have half-succeeded; Go reports the path either way.
            backup_path: Some(display_path(&backup)),
        });
    }

    // Install.
    progress("Installing new version...");
    if std::fs::rename(&new_bin, &binary_path).is_err() {
        if !moved_for_backup {
            let _ = std::fs::remove_file(&binary_path);
        }
        if copy_file(&new_bin, &binary_path).is_err() {
            // Restore from backup.
            let restored = std::fs::rename(&backup, &binary_path).is_ok()
                || copy_file(&backup, &binary_path).is_ok();
            cleanup_tmp();
            // Go sets `result.BackupPath` before the risky rename
            // (updater.go:1394) and `cmd/bv` prints it so the user can still
            // roll back by hand when the restore itself failed.
            let backup_path = Some(display_path(&backup));
            if !restored {
                return Err(UpdateError::Install {
                    reason: format!(
                        "(restore also failed; manual recovery: mv {} {})",
                        display_path(&backup),
                        binary_path.display()
                    ),
                    backup_path,
                });
            }
            return Err(UpdateError::Install {
                reason: "(restored from backup)".into(),
                backup_path,
            });
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if let Err(e) =
            std::fs::set_permissions(&binary_path, std::fs::Permissions::from_mode(0o755))
        {
            // Go writes this through the progress writer (updater.go:1420), not
            // straight to stderr, so the TUI can render it.
            progress(&format!("Warning: could not set permissions: {e}"));
        }
    }
    cleanup_tmp();
    let message = format!("Successfully updated from {old} to {}", release.tag_name);
    Ok(UpdateResult {
        old_version: old,
        new_version: release.tag_name.clone(),
        backup_path: Some(display_path(&backup)),
        message,
    })
}

/// Restore the previous binary from `<bin>.backup` (Go `Rollback`).
pub fn perform_rollback() -> Result<(), UpdateError> {
    let binary_path = current_binary_path()?;
    let backup = backup_path(&binary_path);
    if !backup.exists() {
        return Err(UpdateError::NoBackup(display_path(&backup)));
    }
    println!("Rolling back from backup at {}...", display_path(&backup));
    let bad = {
        let mut s = binary_path.as_os_str().to_owned();
        s.push(".bad");
        PathBuf::from(s)
    };
    let _ = std::fs::remove_file(&bad);
    let moved_to_bad = std::fs::rename(&binary_path, &bad).is_ok();
    if std::fs::rename(&backup, &binary_path).is_err() {
        if !moved_to_bad {
            let _ = std::fs::remove_file(&binary_path);
        }
        if copy_file(&backup, &binary_path).is_err() {
            if moved_to_bad {
                let _ = std::fs::rename(&bad, &binary_path);
            }
            return Err(UpdateError::Rollback("rollback failed".into()));
        }
    }
    if moved_to_bad {
        let _ = std::fs::remove_file(&bad);
    }
    println!("Rollback complete");
    Ok(())
}

/// Download URL helpers (exported for `--update-dry-run` output).
pub fn releases_url() -> String {
    format!("https://github.com/{REPO_OWNER}/{REPO_NAME}/releases")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `\\?\` prefix is Windows-only, so the test builds the string rather
    /// than a `PathBuf` — on a unix host there is no way to make
    /// `canonicalize` produce one, and on Windows `Path::new` would keep the
    /// prefix verbatim and stop testing the stripping.
    #[test]
    fn display_path_strips_the_windows_verbatim_prefix() {
        assert_eq!(
            display_path(Path::new(r"C:\Users\me\bvr.exe")),
            r"C:\Users\me\bvr.exe",
            "a plain path is untouched"
        );
        // What `Path::canonicalize` actually returns on Windows.
        assert_eq!(
            display_path(Path::new(r"\\?\C:\Users\me\bvr.exe")),
            r"C:\Users\me\bvr.exe",
            "the verbatim prefix is stripped"
        );
        assert_eq!(
            display_path(Path::new(r"\\?\UNC\server\share\bvr.exe")),
            r"\\server\share\bvr.exe",
            "a verbatim UNC path becomes a normal UNC path"
        );
    }

    #[test]
    fn safe_names() {
        assert!(safe_asset_name("bvr-v0.1.8-linux-x86_64.tar.gz").is_ok());
        assert!(safe_asset_name("").is_err());
        assert!(safe_asset_name("..").is_err());
        assert!(safe_asset_name("a/b").is_err());
    }

    #[test]
    fn checksums_parse() {
        let data = "abc123  file one.tar.gz\n\nDEF456 *file-two.zip\nnot-a-hash file\n";
        // abc123/DEF456 are not 64-hex so they are skipped
        let m = parse_checksums(data).unwrap();
        assert!(m.is_empty());
        let h = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let data = format!("{h}  bvr-v0.1.8-linux-x86_64.tar.gz\n");
        let m = parse_checksums(&data).unwrap();
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn extract_tar_gz_roundtrip() {
        let dir = std::env::temp_dir();
        let arch = dir.join(format!("bvr-arch-test-{}.tar.gz", std::process::id()));
        let dest = dir.join(format!("bvr-bin-test-{}", std::process::id()));
        {
            let f = std::fs::File::create(&arch).unwrap();
            let gz = flate2::write::GzEncoder::new(f, flate2::Compression::fast());
            let mut tar = tar::Builder::new(gz);
            let payload = b"fake-bvr-binary";
            let mut hdr = tar::Header::new_gnu();
            hdr.set_size(payload.len() as u64);
            hdr.set_mode(0o755);
            hdr.set_cksum();
            tar.append_data(&mut hdr, "bvr", &payload[..]).unwrap();
            tar.into_inner().unwrap().finish().unwrap();
        }
        extract_binary(&arch, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"fake-bvr-binary");
        let _ = std::fs::remove_file(&arch);
        let _ = std::fs::remove_file(&dest);
    }

    #[test]
    fn extract_zip_roundtrip() {
        let dir = std::env::temp_dir();
        let arch = dir.join(format!("bvr-arch-test-{}.zip", std::process::id()));
        let dest = dir.join(format!("bvr-bin-zip-test-{}", std::process::id()));
        {
            let f = std::fs::File::create(&arch).unwrap();
            let mut zip = zip::ZipWriter::new(f);
            zip.start_file("bvr", zip::write::SimpleFileOptions::default())
                .unwrap();
            use std::io::Write as _;
            zip.write_all(b"fake-bvr-binary").unwrap();
            zip.finish().unwrap();
        }
        extract_binary(&arch, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"fake-bvr-binary");
        let _ = std::fs::remove_file(&arch);
        let _ = std::fs::remove_file(&dest);
    }

    #[test]
    fn checksum_roundtrip() {
        let dir = std::env::temp_dir();
        let p = dir.join(format!("bvr-sum-test-{}", std::process::id()));
        std::fs::write(&p, b"hello").unwrap();
        let h = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        verify_checksum(&p, h).unwrap();
        verify_checksum(&p, &h.to_uppercase()).unwrap();
        assert!(verify_checksum(&p, "00").is_err());
        let _ = std::fs::remove_file(&p);
    }

    // --- gap 4: the permission probe must not use a fixed, clobberable name --

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bvr-probe-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_successful_probe_leaves_nothing_behind() {
        let dir = scratch_dir("clean");
        check_writable(&dir.join("bvr")).unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            leftovers.is_empty(),
            "the probe file must be removed; found {leftovers:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_probe_never_follows_or_clobbers_a_planted_name() {
        // The pre-fix probe wrote the fixed path `<dir>/.bvr-update-test`, so a
        // pre-existing entry there was silently truncated — or followed, if it
        // was a symlink. The probe now uses O_EXCL on a unique name.
        let dir = scratch_dir("planted");
        let planted = dir.join(".bvr-update-test");
        let victim = dir.join("victim");
        std::fs::write(&victim, b"precious").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&victim, &planted).unwrap();
        #[cfg(not(unix))]
        std::fs::write(&planted, b"precious").unwrap();

        check_writable(&dir.join("bvr")).unwrap();
        assert_eq!(
            std::fs::read(&victim).unwrap(),
            b"precious",
            "the probe must not touch a pre-existing entry"
        );
        assert!(std::fs::read_link(&planted).is_ok() || planted.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_concurrent_probes_do_not_collide() {
        let dir = scratch_dir("concurrent");
        let a = unique_path(&dir, ".bvr-update-test");
        let b = unique_path(&dir, ".bvr-update-test");
        assert_ne!(a, b, "unique_path must not repeat within a process");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- gap 5: only a real permission failure earns the `sudo` hint --------

    #[test]
    fn a_permission_failure_is_the_only_one_that_suggests_sudo() {
        let dir = Path::new("/usr/local/bin");
        let perm = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let err = probe_error(dir, &perm);
        assert!(matches!(err, UpdateError::NoPermission(_)));
        assert_eq!(
            err.to_string(),
            format!(
                "no write permission to {} (try running with sudo): denied",
                dir.display()
            ),
            "Go's shape (updater.go:1314)"
        );

        // ENOSPC / EROFS / EIO are *not* permission problems. Telling the user
        // to run sudo there sends them down the wrong path entirely.
        for kind in [
            std::io::ErrorKind::StorageFull,
            std::io::ErrorKind::ReadOnlyFilesystem,
            std::io::ErrorKind::Other,
        ] {
            let err = probe_error(dir, &std::io::Error::new(kind, "io"));
            assert!(
                matches!(err, UpdateError::CannotPrepare { .. }),
                "{kind:?} must not be reported as a permission problem"
            );
            assert!(!err.to_string().contains("sudo"), "{err}");
        }
    }

    #[test]
    fn a_missing_directory_is_a_prepare_failure_not_a_permission_problem() {
        // Observable through the real probe: probing a path whose parent does
        // not exist yields NotFound, which must not surface as "run sudo".
        let missing = std::env::temp_dir().join(format!("bvr-absent-{}", std::process::id()));
        let err = check_writable(&missing.join("bvr")).unwrap_err();
        assert!(
            matches!(err, UpdateError::CannotPrepare { .. }),
            "got {err}"
        );
    }

    #[test]
    fn a_staging_directory_is_never_shared() {
        let a = create_unique_dir().unwrap();
        let b = create_unique_dir().unwrap();
        assert_ne!(a, b, "concurrent updates must not share a staging dir");
        // `create_dir` already made it; nothing else should have.
        assert!(a.is_dir() && b.is_dir());
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }
}
