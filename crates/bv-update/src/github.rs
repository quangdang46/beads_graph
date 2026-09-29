//! GitHub release API client — port of Go `checkForUpdates`,
//! `GetLatestRelease`, `FindPlatformAsset`, `FindChecksumAsset`,
//! `validateReleaseIdentity`, `validateReleaseAsset`, `parseGitHubHTTPSURL`
//! and `assetSHA256Digest`.
//!
//! Asset *naming* follows the Rust release scheme (`bvr-{tag}-{platform}`),
//! which is an intentional fork of the Go repo; see `lib.rs`. Everything
//! about release *identity* and *trust* — draft/prerelease rejection, the
//! three-component tag rule, the GitHub URL allowlist, `state`/`size`/
//! `digest` validation — is ported verbatim.

use serde::Deserialize;
use std::time::Duration;

use crate::prefs::github_auth_header;
use crate::version::parse_version;
use crate::{REPO_NAME, REPO_OWNER};

const MAX_RELEASE_METADATA_BYTES: usize = 1 << 20;
/// Go `githubAPIVersion` (updater.go:34).
const GITHUB_API_VERSION: &str = "2022-11-28";
/// Go `setGitHubAPIHeaders` Accept value (updater.go:241-246).
const ACCEPT_HEADER: &str = "application/vnd.github+json";
/// Go's User-Agent for `GetLatestRelease` / `downloadFile`, renamed for the
/// Rust binary (updater.go:622, 987).
const USER_AGENT: &str = "bvr-updater";
/// Go's User-Agent for the check paths (updater.go:333, 904), renamed.
const USER_AGENT_CHECK: &str = "bvr-update-check";
/// Go's release-check client timeout (updater.go:312-315).
const CHECK_TIMEOUT: Duration = Duration::from_secs(10);
/// Go's `GetLatestRelease` client timeout (updater.go:617).
const RELEASE_TIMEOUT: Duration = Duration::from_secs(30);
/// Go `maxDownloadBytes` (updater.go:34).
pub const MAX_DOWNLOAD_BYTES: i64 = 512 << 20;
/// Go `maxChecksumManifestBytes` (updater.go:33).
pub const MAX_CHECKSUM_MANIFEST_BYTES: i64 = 1 << 20;

/// A single asset attached to a GitHub release (Go `Asset`, updater.go:289).
#[derive(Debug, Clone, Deserialize)]
pub struct Asset {
    /// File name, e.g. `bvr-v0.1.8-linux-x86_64.tar.gz`.
    pub name: String,
    /// Direct download URL.
    pub browser_download_url: String,
    /// Size in bytes reported by the API.
    #[serde(default)]
    pub size: i64,
    /// GitHub-computed digest, e.g. `sha256:ab12…`. Empty on API responses
    /// predating the field; the installer then refuses the release.
    #[serde(default)]
    pub digest: String,
    /// Upload state; only `uploaded` is installable (updater.go:793-795).
    #[serde(default)]
    pub state: String,
}

/// A GitHub release (Go `Release`, updater.go:280).
#[derive(Debug, Clone, Deserialize)]
pub struct Release {
    /// Tag name, e.g. `v0.1.8`.
    pub tag_name: String,
    /// Web URL of the release page.
    #[serde(default)]
    pub html_url: String,
    /// Draft releases are never installable.
    #[serde(default)]
    pub draft: bool,
    /// Prerelease builds are never auto-installed.
    #[serde(default)]
    pub prerelease: bool,
    /// Attached files.
    #[serde(default)]
    pub assets: Vec<Asset>,
}

/// Update info for `--check-update` output.
#[derive(Debug, Clone)]
pub struct UpdateInfo {
    /// New tag, e.g. `v0.1.8`.
    pub new_version: String,
    /// Release page URL.
    pub release_url: String,
}

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("network error: {0}")]
    Network(String),
    /// A non-200 from the GitHub API. Carries the full status line so the
    /// message is Go's `github api returned status: 403 Forbidden`
    /// (`updater.go:344, 634`) rather than a bare number.
    #[error("github api returned status: {0}")]
    Status(String),
    #[error("release metadata exceeds size limit")]
    TooLarge,
    #[error("failed to parse release info: {0}")]
    Parse(String),
    /// A release that is real but must not be auto-installed
    /// (Go `ValidateReleaseForUpdate`).
    #[error("{0}")]
    NotInstallable(String),
    /// A failure surfaced verbatim, matching Go's `return "", "", err` for a
    /// version-comparison failure (updater.go:359-362). It is not a fetch
    /// failure and must not be re-wrapped with a "failed to …" prefix.
    #[error("{0}")]
    Check(String),
}

impl From<String> for FetchError {
    fn from(message: String) -> Self {
        FetchError::NotInstallable(message)
    }
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        // Stricter than Go: Go deletes Authorization only on redirects that
        // leave GitHub hosts (updater.go:977-979), ureq drops it on every
        // redirect. Keeping the stricter setting — see prefs::is_github_host
        // for the pre-flight host check.
        .redirect_auth_headers(ureq::config::RedirectAuthHeaders::Never)
        // Status codes are inspected by hand so a 403/429 can be reported
        // rather than silently swallowed.
        .http_status_as_error(false)
        .build()
        .into()
}

/// Attach the Go API header set plus, only when the user opted in *and* the
/// URL is a GitHub host, `Authorization: Bearer <token>`
/// (Go `setGitHubAPIHeaders`, updater.go:241-246).
fn with_api_headers(
    req: ureq::RequestBuilder<ureq::typestate::WithoutBody>,
    url: &str,
    user_agent: &str,
) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
    let req = req
        .header("Accept", ACCEPT_HEADER)
        .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .header("User-Agent", user_agent);
    match github_auth_header(url) {
        Some(value) => req.header("Authorization", &value),
        None => req,
    }
}

fn decode_release_metadata(bytes: &[u8]) -> Result<Release, FetchError> {
    if bytes.len() > MAX_RELEASE_METADATA_BYTES {
        return Err(FetchError::TooLarge);
    }
    let mut de = serde_json::Deserializer::from_slice(bytes);
    let rel: Release =
        serde::Deserialize::deserialize(&mut de).map_err(|e| FetchError::Parse(e.to_string()))?;
    de.end().map_err(|e| FetchError::Parse(e.to_string()))?;
    Ok(rel)
}

fn latest_release_url() -> String {
    format!("https://api.github.com/repos/{REPO_OWNER}/{REPO_NAME}/releases/latest")
}

/// Fetch the latest release from the GitHub API (Go `GetLatestRelease`).
pub fn get_latest_release() -> Result<Release, FetchError> {
    let url = latest_release_url();
    let req = with_api_headers(agent(RELEASE_TIMEOUT).get(&url), &url, USER_AGENT);
    let resp = req.call().map_err(|e| FetchError::Network(e.to_string()))?;
    if resp.status().as_u16() != 200 {
        return Err(FetchError::Status(resp.status().to_string()));
    }
    let body = resp.into_body().into_reader();
    let mut bytes = Vec::new();
    use std::io::Read as _;
    body.take((MAX_RELEASE_METADATA_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| FetchError::Network(e.to_string()))?;
    let rel = decode_release_metadata(&bytes)?;
    validate_release_identity(&rel)?;
    Ok(rel)
}

/// Check whether a newer release exists. Returns `None` when up to date.
///
/// Every non-200 is an error (Go updater.go:342-345 returns
/// `github api returned status: %s` for *all* of them), so a rate limit can
/// no longer be mistaken for "you are up to date" — the previous 403/429 →
/// `Ok(None)` arm made `bvr --check-update` print "up to date" and exit 0
/// while rate-limited.
pub fn check_for_updates() -> Result<Option<UpdateInfo>, FetchError> {
    let url = latest_release_url();
    let req = with_api_headers(agent(CHECK_TIMEOUT).get(&url), &url, USER_AGENT_CHECK);
    let resp = req.call().map_err(|e| FetchError::Network(e.to_string()))?;
    if resp.status().as_u16() != 200 {
        return Err(FetchError::Status(resp.status().to_string()));
    }
    let body = resp.into_body().into_reader();
    let mut bytes = Vec::new();
    use std::io::Read as _;
    body.take((MAX_RELEASE_METADATA_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| FetchError::Network(e.to_string()))?;
    let rel = decode_release_metadata(&bytes)?;
    validate_release_identity(&rel)?;
    // Go propagates the compare error (updater.go:359-362); it is not a
    // silent "up to date". `is_newer_than_current` swallows it, so call the
    // fallible form.
    let newer =
        crate::version::check_newer_than_current(&rel.tag_name).map_err(FetchError::Check)?;
    if !newer {
        return Ok(None);
    }
    // Same guard Go applies before advertising a tag (updater.go:367-369),
    // minus the remote checksum-manifest fetch: a startup check must not
    // double the network cost. `perform_update` re-validates, including the
    // manifest, before anything is installed.
    validate_release_for_update(&rel).map_err(|e| {
        FetchError::NotInstallable(format!(
            "latest release {} is not installable: {e}",
            rel.tag_name
        ))
    })?;
    Ok(Some(UpdateInfo {
        new_version: rel.tag_name,
        release_url: rel.html_url,
    }))
}

/// A parsed URL broken into the parts `parseGitHubHTTPSURL` inspects.
struct ParsedUrl {
    scheme: String,
    host: String,
    port: String,
    user: String,
    path: String,
    query: String,
    fragment: String,
}

/// Minimal RFC-3986-ish parse covering exactly what Go's `url.Parse` +
/// `Hostname()` + `Port()` + `User` + `RawQuery` + `Fragment` + `Path`
/// expose for the release URLs this crate handles. The `url` crate is not a
/// workspace dependency.
fn parse_url(raw: &str) -> Option<ParsedUrl> {
    // A schemeless URL has no authority; Go treats it as a path only, which
    // then fails the https check anyway.
    let i = raw.find("://")?;
    let (scheme, rest) = (raw[..i].to_string(), &raw[i + 3..]);
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let tail = &rest[authority_end..];

    let (user, host_port) = match authority.rfind('@') {
        Some(i) => (authority[..i].to_string(), &authority[i + 1..]),
        None => (String::new(), authority),
    };

    let (host, port) = if let Some(after_bracket) = host_port.strip_prefix('[') {
        match after_bracket.find(']') {
            Some(i) => {
                let host = after_bracket[..i].to_string();
                let rest = &after_bracket[i + 1..];
                let port = rest.strip_prefix(':').unwrap_or("").to_string();
                (host, port)
            }
            None => return None,
        }
    } else {
        match host_port.find(':') {
            Some(i) => (host_port[..i].to_string(), host_port[i + 1..].to_string()),
            None => (host_port.to_string(), String::new()),
        }
    };
    if host.is_empty() {
        return None;
    }

    let (path, after_path) = match tail.find(['?', '#']) {
        Some(i) => (&tail[..i], &tail[i..]),
        None => (tail, ""),
    };
    // `after_path` starts at the first `?` or `#`, so the query slice must
    // start at 0 — slicing at `1..i` panicked on a bare "#frag" (a URL with a
    // fragment and no query), which is exactly the shape the
    // no-query/no-fragment check below is meant to reject.
    let (query, fragment) = if let Some(i) = after_path.find('#') {
        (
            after_path[..i].trim_start_matches('?').to_string(),
            after_path[i + 1..].to_string(),
        )
    } else {
        (
            after_path.trim_start_matches('?').to_string(),
            String::new(),
        )
    };

    Some(ParsedUrl {
        scheme,
        host: host.to_ascii_lowercase(),
        port,
        user,
        path: percent_decode(path),
        query,
        fragment,
    })
}

/// Decode the `%XX` escapes Go's `url.Path` resolves. Release tags and asset
/// names are ASCII, so this is belt-and-braces rather than load-bearing.
fn percent_decode(value: &str) -> String {
    if !value.contains('%') {
        return value.to_string();
    }
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Go `parseGitHubHTTPSURL` (updater.go:734-746): HTTPS on `github.com`, no
/// port, no credentials, no query, no fragment.
fn parse_github_https_url(raw_url: &str, field: &str) -> Result<ParsedUrl, String> {
    let parsed =
        parse_url(raw_url).ok_or_else(|| format!("invalid {field} URL: missing scheme or host"))?;
    if parsed.scheme != "https" || parsed.host != "github.com" || !parsed.port.is_empty() {
        return Err(format!("{field} URL must use HTTPS on github.com"));
    }
    if !parsed.user.is_empty() || !parsed.query.is_empty() || !parsed.fragment.is_empty() {
        return Err(format!(
            "{field} URL must not contain credentials, a query, or a fragment"
        ));
    }
    Ok(parsed)
}

/// Go `assetSHA256Digest` (updater.go:819-835): the `sha256:<64 hex>` prefix
/// GitHub publishes for every uploaded asset.
pub fn asset_sha256_digest(asset: &Asset) -> Result<String, String> {
    const PREFIX: &str = "sha256:";
    let Some(digest) = asset.digest.strip_prefix(PREFIX) else {
        return Err(format!(
            "asset {:?} digest must start with {PREFIX:?}",
            asset.name
        ));
    };
    let digest = digest.trim().to_ascii_lowercase();
    if digest.len() != 64 {
        return Err(format!(
            "asset {:?} has invalid sha256 digest length {}",
            asset.name,
            digest.len()
        ));
    }
    if !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("asset {:?} has invalid sha256 digest", asset.name));
    }
    Ok(digest)
}

/// Go `validateReleaseIdentity` (updater.go:748-780).
pub fn validate_release_identity(release: &Release) -> Result<(), String> {
    if release.tag_name != release.tag_name.trim() {
        return Err(format!(
            "release tag {:?} has surrounding whitespace",
            release.tag_name
        ));
    }
    if release.draft {
        return Err(format!("release {:?} is still a draft", release.tag_name));
    }
    if release.prerelease {
        return Err(format!(
            "release {:?} is marked as a prerelease",
            release.tag_name
        ));
    }
    let parsed = parse_version(&release.tag_name)
        .map_err(|e| format!("invalid release tag {:?}: {e}", release.tag_name))?;
    if !parsed.prerelease().is_empty() {
        return Err(format!(
            "release tag {:?} is a semantic-version prerelease",
            release.tag_name
        ));
    }
    if parsed.core_components() != 3 {
        return Err(format!(
            "release tag {:?} must use major.minor.patch",
            release.tag_name
        ));
    }
    let release_url = parse_github_https_url(&release.html_url, "release page")?;
    let expected_path = format!(
        "/{REPO_OWNER}/{REPO_NAME}/releases/tag/{}",
        release.tag_name
    );
    if release_url.path != expected_path {
        return Err(format!(
            "release page URL path {:?} does not match {:?}",
            release_url.path, expected_path
        ));
    }
    Ok(())
}

/// Go `validateReleaseAsset` (updater.go:782-817).
pub fn validate_release_asset(
    release: &Release,
    asset: &Asset,
    description: &str,
    max_size: i64,
) -> Result<(), String> {
    let asset_name = crate::update::safe_asset_name(&asset.name)
        .map_err(|e| format!("invalid {description}: {e}"))?
        .to_string();
    if asset.state != "uploaded" {
        return Err(format!(
            "{description} {:?} is not uploaded (state {:?})",
            asset.name, asset.state
        ));
    }
    if asset.size <= 0 {
        return Err(format!(
            "{description} {:?} has invalid size {}",
            asset.name, asset.size
        ));
    }
    if max_size <= 0 {
        return Err(format!("{description} has invalid maximum size {max_size}"));
    }
    if asset.size > max_size {
        return Err(format!(
            "{description} {:?} exceeds maximum size {}",
            asset.name, max_size
        ));
    }
    let asset_url = parse_github_https_url(&asset.browser_download_url, description)?;
    let expected_path = format!(
        "/{REPO_OWNER}/{REPO_NAME}/releases/download/{}/{asset_name}",
        release.tag_name
    );
    if asset_url.path != expected_path {
        return Err(format!(
            "{description} URL path {:?} does not match {:?}",
            asset_url.path, expected_path
        ));
    }
    asset_sha256_digest(asset).map_err(|e| format!("invalid {description} digest: {e}"))?;
    Ok(())
}

impl Release {
    /// Find the asset matching this platform, most specific name first
    /// (Go `FindPlatformAsset`, updater.go:690-702).
    pub fn find_platform_asset(&self) -> Option<&Asset> {
        platform_asset_names(&self.tag_name)
            .iter()
            .find_map(|name| self.assets.iter().find(|a| a.name == *name))
    }

    /// Find the platform asset that the checksum manifest actually covers
    /// (Go `findPlatformAssetWithChecksum`, updater.go:704-719). Falling back
    /// to any platform asset would install an archive the manifest cannot
    /// vouch for.
    pub fn find_platform_asset_with_checksum(
        &self,
        checksums: &std::collections::HashMap<String, String>,
    ) -> Option<&Asset> {
        platform_asset_names(&self.tag_name)
            .iter()
            .filter(|name| checksums.contains_key(*name))
            .find_map(|name| self.assets.iter().find(|a| a.name == *name))
    }

    /// Find the checksum manifest asset (Go `FindChecksumAsset`,
    /// updater.go:722-732). Go requires the literal `checksums.txt`; the Rust
    /// release workflow emits one `<archive>.sha256` sidecar per archive and
    /// no manifest at all (`.github/workflows/release.yml`, five matrix
    /// suffixes), so both shapes are accepted here.
    ///
    /// The sidecar is chosen by *platform*, not by "first `.sha256` wins":
    /// a sidecar names exactly one archive, so picking another platform's
    /// would download a manifest that does not cover this platform's archive
    /// and make `checked_platform_asset` refuse the install on every platform
    /// but one.
    pub fn find_checksum_asset(&self) -> Option<&Asset> {
        // Go's own name first — a `checksums.txt` covers every archive.
        if let Some(manifest) = self.assets.iter().find(|a| a.name == "checksums.txt") {
            return Some(manifest);
        }
        // Then the sidecar for this platform's archive, versioned name first,
        // then the unversioned legacy name.
        for archive in platform_asset_names(&self.tag_name) {
            let sidecar = format!("{archive}.sha256");
            if let Some(found) = self.assets.iter().find(|a| a.name == sidecar) {
                return Some(found);
            }
        }
        // Last resort: any sidecar. The install then fails loudly in
        // `checked_platform_asset` rather than proceeding unverified.
        self.assets.iter().find(|a| a.name.ends_with(".sha256"))
    }
}

/// Go `releaseAssetsForUpdate` (updater.go:837-856): the release must be
/// installable and must carry *both* a platform archive and a checksum
/// manifest. A release with no checksum asset is refused outright — Go's
/// message is `checksums.txt asset is missing`, and the previous Rust
/// installer silently skipped verification in that case.
pub fn release_assets_for_update(release: &Release) -> Result<(&Asset, &Asset), String> {
    validate_release_identity(release)?;
    let asset = release.find_platform_asset().ok_or_else(|| {
        format!(
            "no binary available for {}/{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })?;
    validate_release_asset(release, asset, "platform asset", MAX_DOWNLOAD_BYTES)?;
    let checksum_asset = release
        .find_checksum_asset()
        .ok_or_else(|| "checksums.txt asset is missing".to_string())?;
    validate_release_asset(
        release,
        checksum_asset,
        "checksum asset",
        MAX_CHECKSUM_MANIFEST_BYTES,
    )?;
    Ok((asset, checksum_asset))
}

/// Go `ValidateReleaseForUpdate` (updater.go:884-887).
pub fn validate_release_for_update(release: &Release) -> Result<(), String> {
    release_assets_for_update(release).map(|_| ())
}

/// Go `checkedPlatformAsset` (updater.go:858-878): the platform asset the
/// checksum manifest actually covers, together with the digest GitHub reports
/// for it. Both halves must agree before anything is downloaded — that is what
/// stops a release whose `checksums.txt` disagrees with the API metadata (or
/// which does not cover the archive at all) from being installed.
pub fn checked_platform_asset<'r>(
    release: &'r Release,
    checksums: &std::collections::HashMap<String, String>,
) -> Result<(&'r Asset, String), String> {
    let asset = release
        .find_platform_asset_with_checksum(checksums)
        .ok_or_else(|| {
            format!(
                "checksums.txt has no entry for a {}/{} release asset",
                std::env::consts::OS,
                std::env::consts::ARCH
            )
        })?;
    validate_release_asset(
        release,
        asset,
        "checksummed platform asset",
        MAX_DOWNLOAD_BYTES,
    )?;
    let expected = checksums
        .get(&asset.name)
        .ok_or_else(|| format!("no checksum found for {}", asset.name))?;
    let digest = asset_sha256_digest(asset)?;
    if expected != &digest {
        return Err(format!(
            "checksums.txt digest for {} disagrees with GitHub release metadata",
            asset.name
        ));
    }
    Ok((asset, digest))
}

/// Candidate asset names for `tag` on this platform, most specific first:
/// the versioned name, then the unversioned legacy name, then the old
/// Windows tar.gz variant (Go `platformAssetNames`, updater.go:675-687).
/// The Rust naming scheme is `bvr-…`, not Go's `bv_…`; see `lib.rs`.
pub fn platform_asset_names(tag: &str) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(stable) = crate::asset_name_for(tag) {
        names.push(stable);
    }
    // Legacy unversioned name, so releases published by an older
    // configuration still install.
    if let Some(platform) = crate::platform_suffix() {
        names.push(format!(
            "{}-{platform}{}",
            crate::BINARY_NAME,
            crate::archive_extension()
        ));
    }
    if cfg!(target_os = "windows") {
        if let Some(platform) = crate::platform_suffix() {
            names.push(format!("{}-{tag}-{platform}.tar.gz", crate::BINARY_NAME));
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(name: &str) -> Asset {
        Asset {
            name: name.to_string(),
            browser_download_url: format!(
                "https://github.com/{REPO_OWNER}/{REPO_NAME}/releases/download/v1.2.3/{name}"
            ),
            size: 1024,
            digest: format!("sha256:{}", "ab".repeat(32)),
            state: "uploaded".to_string(),
        }
    }

    fn release(tag: &str, assets: Vec<Asset>) -> Release {
        Release {
            tag_name: tag.to_string(),
            html_url: format!("https://github.com/{REPO_OWNER}/{REPO_NAME}/releases/tag/{tag}"),
            draft: false,
            prerelease: false,
            assets,
        }
    }

    #[test]
    fn finds_stable_asset() {
        let tag = "v1.2.3";
        let expected = crate::asset_name_for(tag).unwrap();
        let rel = release(tag, vec![asset(&expected)]);
        assert_eq!(rel.find_platform_asset().unwrap().name, expected);
    }

    #[test]
    fn metadata_size_limit() {
        let big = vec![b'x'; MAX_RELEASE_METADATA_BYTES + 2];
        assert!(matches!(
            decode_release_metadata(&big),
            Err(FetchError::TooLarge)
        ));
    }

    // --- gap 1: release identity -----------------------------------------

    #[test]
    fn release_identity_accepts_a_plain_stable_tag() {
        assert!(validate_release_identity(&release("v1.2.3", vec![])).is_ok());
    }

    #[test]
    fn release_identity_rejects_draft_prerelease_and_bad_tags() {
        let mut draft = release("v1.2.3", vec![]);
        draft.draft = true;
        assert!(validate_release_identity(&draft)
            .unwrap_err()
            .contains("still a draft"));

        let mut pre = release("v1.2.3", vec![]);
        pre.prerelease = true;
        assert!(validate_release_identity(&pre)
            .unwrap_err()
            .contains("prerelease"));

        // Surrounding whitespace.
        assert!(validate_release_identity(&release(" v1.2.3", vec![]))
            .unwrap_err()
            .contains("surrounding whitespace"));
        // Not three components.
        assert!(validate_release_identity(&release("v1.2", vec![]))
            .unwrap_err()
            .contains("major.minor.patch"));
        // Semver prerelease tag.
        assert!(validate_release_identity(&release("v1.2.3-rc1", vec![]))
            .unwrap_err()
            .contains("semantic-version prerelease"));
    }

    #[test]
    fn release_identity_pins_the_html_url_to_this_repo_and_tag() {
        let mut other_repo = release("v1.2.3", vec![]);
        other_repo.html_url = "https://github.com/attacker/beads_viewer/releases/tag/v1.2.3".into();
        assert!(validate_release_identity(&other_repo)
            .unwrap_err()
            .contains("does not match"));

        let mut other_host = release("v1.2.3", vec![]);
        other_host.html_url = "https://evil.example.com/a/b/releases/tag/v1.2.3".into();
        assert!(validate_release_identity(&other_host)
            .unwrap_err()
            .contains("must use HTTPS on github.com"));

        // Query/fragment/userinfo are refused.
        for suffix in ["?x=1", "#frag"] {
            let mut r = release("v1.2.3", vec![]);
            r.html_url.push_str(suffix);
            assert!(validate_release_identity(&r)
                .unwrap_err()
                .contains("query, or a fragment"));
        }
        let mut with_user = release("v1.2.3", vec![]);
        with_user.html_url = "https://user:pw@github.com/a/b/releases/tag/v1.2.3".into();
        assert!(validate_release_identity(&with_user)
            .unwrap_err()
            .contains("credentials"));
        // A port is refused.
        let mut with_port = release("v1.2.3", vec![]);
        with_port.html_url = "https://github.com:443/a/b/releases/tag/v1.2.3".into();
        assert!(validate_release_identity(&with_port)
            .unwrap_err()
            .contains("must use HTTPS on github.com"));
    }

    // --- gap 1 / 2: asset validation and the mandatory manifest -----------

    #[test]
    fn asset_digest_must_be_sha256_prefixed_hex() {
        let mut a = asset("x.tar.gz");
        a.digest = "md5:abc".into();
        assert!(asset_sha256_digest(&a).is_err());
        a.digest = "sha256:tooshort".into();
        assert!(asset_sha256_digest(&a)
            .unwrap_err()
            .contains("invalid sha256 digest length"));
        a.digest = "sha256:zz".repeat(32);
        assert!(asset_sha256_digest(&a)
            .unwrap_err()
            .contains("invalid sha256 digest"));
        a.digest = format!("sha256:{}", "AB".repeat(32));
        assert_eq!(asset_sha256_digest(&a).unwrap(), "ab".repeat(32));
    }

    #[test]
    fn asset_state_size_and_url_are_validated() {
        let rel = release("v1.2.3", vec![]);
        let mut a = asset("x.tar.gz");
        a.state = "new".into();
        assert!(
            validate_release_asset(&rel, &a, "platform asset", MAX_DOWNLOAD_BYTES)
                .unwrap_err()
                .contains("is not uploaded")
        );
        a.state = "uploaded".into();
        a.size = 0;
        assert!(
            validate_release_asset(&rel, &a, "platform asset", MAX_DOWNLOAD_BYTES)
                .unwrap_err()
                .contains("invalid size")
        );
        a.size = 10;
        assert!(validate_release_asset(&rel, &a, "platform asset", 5)
            .unwrap_err()
            .contains("exceeds maximum size"));
        a.size = 1024;
        a.browser_download_url = "https://evil.example.com/x.tar.gz".into();
        assert!(
            validate_release_asset(&rel, &a, "platform asset", MAX_DOWNLOAD_BYTES)
                .unwrap_err()
                .contains("must use HTTPS on github.com")
        );
    }

    #[test]
    fn missing_checksum_asset_is_refused() {
        // Gap 2: the manifest is mandatory. Go's message is
        // `checksums.txt asset is missing` (updater.go:849-851).
        let tag = "v1.2.3";
        let archive = crate::asset_name_for(tag).unwrap();
        let rel = release(tag, vec![asset(&archive)]);
        let err = release_assets_for_update(&rel).unwrap_err();
        assert_eq!(err, "checksums.txt asset is missing");
        assert!(validate_release_for_update(&rel).is_err());
    }

    #[test]
    fn a_complete_release_validates() {
        let tag = "v1.2.3";
        let archive = crate::asset_name_for(tag).unwrap();
        let rel = release(
            tag,
            vec![asset(&archive), asset(&format!("{archive}.sha256"))],
        );
        let (found, sum) = release_assets_for_update(&rel).unwrap();
        assert_eq!(found.name, archive);
        assert_eq!(sum.name, format!("{archive}.sha256"));
    }

    #[test]
    fn platform_asset_falls_back_to_the_unversioned_name() {
        let tag = "v1.2.3";
        let legacy = format!(
            "{}-{}.{}",
            crate::BINARY_NAME,
            crate::platform_suffix().unwrap(),
            crate::archive_extension().trim_start_matches('.')
        );
        let rel = release(tag, vec![asset(&legacy)]);
        assert_eq!(rel.find_platform_asset().unwrap().name, legacy);
    }

    #[test]
    fn checksummed_lookup_skips_an_uncovered_name() {
        let tag = "v1.2.3";
        let archive = crate::asset_name_for(tag).unwrap();
        let rel = release(tag, vec![asset(&archive)]);
        // A manifest that does not mention the archive yields no asset, which
        // is what makes the installer refuse rather than skip verification.
        let empty = std::collections::HashMap::new();
        assert!(rel.find_platform_asset_with_checksum(&empty).is_none());
        let mut covered = std::collections::HashMap::new();
        covered.insert(archive.clone(), "ab".repeat(32));
        assert_eq!(
            rel.find_platform_asset_with_checksum(&covered)
                .unwrap()
                .name,
            archive
        );
    }

    #[test]
    fn url_parsing_rejects_the_shapes_go_rejects() {
        assert!(parse_github_https_url("https://github.com/a", "f").is_ok());
        for bad in [
            "http://github.com/a",           // not https
            "https://GitHub.com/a",          // case: Go lowercases Hostname(), so this passes
            "https://api.github.com/a",      // not github.com
            "https://github.com:443/a",      // port
            "ftp://github.com/a",            // scheme
            "github.com/a",                  // no scheme
            "https:///a",                    // no host
            "https://github.com/a#frag",     // fragment, no query
            "https://github.com/a?q=1#frag", // query and fragment
        ] {
            let got = parse_github_https_url(bad, "f");
            if bad == "https://GitHub.com/a" {
                assert!(got.is_ok(), "{bad} should parse");
            } else {
                assert!(got.is_err(), "{bad} should be rejected");
            }
        }
    }

    // --- gap 17: the manifest entry must agree with GitHub's own digest ----

    /// A release whose archive digest is `ab…` (see `asset()`).
    fn digest_release() -> (Release, String) {
        let tag = "v1.2.3";
        let archive = crate::asset_name_for(tag).unwrap();
        (
            release(
                tag,
                vec![asset(&archive), asset(&format!("{archive}.sha256"))],
            ),
            archive,
        )
    }

    #[test]
    fn checked_platform_asset_requires_a_matching_manifest_entry() {
        let (rel, archive) = digest_release();
        let mut covered = std::collections::HashMap::new();
        covered.insert(archive.clone(), "ab".repeat(32));
        let (found, digest) = checked_platform_asset(&rel, &covered).unwrap();
        assert_eq!(found.name, archive);
        assert_eq!(digest, "ab".repeat(32));

        // A manifest that omits the archive yields the Go message rather than
        // silently falling back to an unverified asset.
        let empty = std::collections::HashMap::new();
        assert!(checked_platform_asset(&rel, &empty)
            .unwrap_err()
            .contains("has no entry for a"));
    }

    #[test]
    fn checked_platform_asset_rejects_a_manifest_that_disagrees_with_the_api() {
        let (rel, archive) = digest_release();
        let mut tampered = std::collections::HashMap::new();
        // A manifest an attacker could serve naming a different digest.
        tampered.insert(archive.clone(), "cd".repeat(32));
        assert!(checked_platform_asset(&rel, &tampered)
            .unwrap_err()
            .contains("disagrees with GitHub release metadata"));
    }
}
