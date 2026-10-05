//! Version resolution and comparison — port of Go `pkg/version` plus
//! `parseVersion` / `validateVersionIdentifiers` / `compareParsedVersions` /
//! `isDevelopmentVersion` / `isNewerVersion` in `pkg/updater/updater.go`
//! (lines 389-613).
//!
//! Go accepts one-to-three numeric core components for compatibility with
//! older bv tags, rejects leading zeros, caps the input at 128 characters, and
//! treats two unparseable versions as EQUAL so callers fail closed instead of
//! announcing a bogus update.

/// The **Go release this port tracks**, matching Go's hardcoded
/// `pkg/version` fallback (version.go:18).
///
/// This is a *compatibility* constant, not the port's own version. It is what
/// lands in the `version` field of every robot envelope, which the frozen Go
/// oracle byte-compares against: bumping it drops the golden gate from
/// PASS=64/DIFF_FAILS=0 to PASS=5/DIFF_FAILS=59, one byte per file, because
/// there is no Go bv v0.25.1 for a `v0.25.1` envelope to be compatible with.
/// It tracks the Go release even after a port release, so a v0.25.1 binary
/// still reports `v0.25.0` in its envelope.
///
/// For the port's own version see [`PORT_VERSION`].
pub const APP_VERSION: &str = "v0.25.0";

/// This port's own version — the GitHub tag and the release it was built from.
///
/// Kept separate from [`APP_VERSION`] so the two numbers can diverge without
/// either being wrong: the envelope advertises Go parity and is byte-pinned by
/// the goldens, while `--version`, `--check-update` and the TUI update modal
/// describe *this* binary. Collapsing them into one constant made
/// `--check-update` compare the running release against itself and always
/// report "already up to date", which is how v0.25.1 shipped with a dead
/// updater.
///
/// Deliberately not `CARGO_PKG_VERSION`: that would tie the reported version
/// to the workspace semver, which has no relationship to the release tag and
/// previously disagreed with it.
pub const PORT_VERSION: &str = "v0.25.1";

/// Current **port** version, normalized with a `v` prefix.
///
/// This is what the updater compares against the latest published release, so
/// it must be [`PORT_VERSION`] and not the Go-parity [`APP_VERSION`].
pub fn current_version() -> String {
    PORT_VERSION.to_string()
}

/// Markers Go treats as "local build, never advertise an update"
/// (updater.go:547, 558).
const DEVELOPMENT_MARKERS: [&str; 6] = ["dev", "dirty", "nightly", "local", "snapshot", "git"];

/// Go updater.go:398 — reject anything longer before doing any work.
const MAX_VERSION_LENGTH: usize = 128;

/// A parsed version: up to three zero-padded core components plus an optional
/// prerelease identifier list (Go `parsedVersion`, updater.go:389-393).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedVersion {
    core: [String; 3],
    core_components: usize,
    prerelease: Vec<String>,
}

impl Default for ParsedVersion {
    fn default() -> Self {
        Self {
            core: ["0".to_string(), "0".to_string(), "0".to_string()],
            core_components: 0,
            prerelease: Vec::new(),
        }
    }
}

impl ParsedVersion {
    /// Number of core components actually present in the source string
    /// (Go `parsedVersion.coreComponents`).
    pub fn core_components(&self) -> usize {
        self.core_components
    }

    /// Prerelease identifiers, empty for a plain release (Go
    /// `parsedVersion.prerelease`).
    pub fn prerelease(&self) -> &[String] {
        &self.prerelease
    }
}

/// Go `isNumericIdentifier` (updater.go:463-473): non-empty and all ASCII digits.
fn is_numeric_identifier(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit())
}

/// Go `validateVersionIdentifiers` (updater.go:445-461).
fn validate_version_identifiers(
    value: &str,
    reject_numeric_leading_zeros: bool,
) -> Result<(), String> {
    for identifier in value.split('.') {
        if identifier.is_empty() {
            return Err("identifier is empty".to_string());
        }
        for ch in identifier.chars() {
            let ok = ch.is_ascii_digit()
                || ch.is_ascii_uppercase()
                || ch.is_ascii_lowercase()
                || ch == '-';
            if !ok {
                return Err(format!(
                    "identifier {identifier:?} contains an invalid character"
                ));
            }
        }
        if reject_numeric_leading_zeros
            && identifier.len() > 1
            && identifier.starts_with('0')
            && is_numeric_identifier(identifier)
        {
            return Err(format!(
                "numeric identifier {identifier:?} has a leading zero"
            ));
        }
    }
    Ok(())
}

/// Go `parseVersion` (updater.go:395-443).
pub fn parse_version(raw: &str) -> Result<ParsedVersion, String> {
    let mut parsed = ParsedVersion::default();
    let raw = raw.trim();
    if raw.len() > MAX_VERSION_LENGTH {
        return Err("version is too long".to_string());
    }
    let raw = raw.strip_prefix('v').unwrap_or(raw);
    if raw.is_empty() {
        return Err("version is empty".to_string());
    }

    // Build metadata is validated then discarded (Go: SemVer §10).
    let version_and_build: Vec<&str> = raw.split('+').collect();
    if version_and_build.len() > 2 {
        return Err(format!(
            "version {raw:?} contains multiple build metadata separators"
        ));
    }
    if version_and_build.len() == 2 {
        validate_version_identifiers(version_and_build[1], false)
            .map_err(|e| format!("invalid build metadata in version {raw:?}: {e}"))?;
    }
    let raw = version_and_build[0];

    let version_and_pre: Vec<&str> = raw.splitn(2, '-').collect();
    if version_and_pre.len() == 2 {
        validate_version_identifiers(version_and_pre[1], true)
            .map_err(|e| format!("invalid prerelease in version {raw:?}: {e}"))?;
        parsed.prerelease = version_and_pre[1]
            .split('.')
            .map(|s| s.to_string())
            .collect();
    }

    let core: Vec<&str> = version_and_pre[0].split('.').collect();
    if core.is_empty() || core.len() > parsed.core.len() {
        return Err(format!(
            "version {raw:?} must have one to three numeric components"
        ));
    }
    for (i, component) in core.iter().enumerate() {
        if !is_numeric_identifier(component) {
            return Err(format!(
                "version {raw:?} has non-numeric core component {component:?}"
            ));
        }
        if component.len() > 1 && component.starts_with('0') {
            return Err(format!(
                "version {raw:?} has a leading zero in core component {component:?}"
            ));
        }
        parsed.core[i] = (*component).to_string();
    }
    parsed.core_components = core.len();
    Ok(parsed)
}

/// Go `compareNumericIdentifiers` (updater.go:475-483): compare by length first,
/// then lexicographically — so `9` < `10` without parsing into an integer.
fn compare_numeric_identifiers(v1: &str, v2: &str) -> i32 {
    match v1.len().cmp(&v2.len()) {
        std::cmp::Ordering::Greater => 1,
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => match v1.cmp(v2) {
            std::cmp::Ordering::Greater => 1,
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
        },
    }
}

/// Go `compareVersionCore` (updater.go:485-492): all three zero-padded components.
fn compare_version_core(v1: &ParsedVersion, v2: &ParsedVersion) -> i32 {
    for i in 0..v1.core.len() {
        let result = compare_numeric_identifiers(&v1.core[i], &v2.core[i]);
        if result != 0 {
            return result;
        }
    }
    0
}

/// Go `compareParsedVersions` (updater.go:494-539).
fn compare_parsed_versions(v1: &ParsedVersion, v2: &ParsedVersion) -> i32 {
    let result = compare_version_core(v1, v2);
    if result != 0 {
        return result;
    }
    if v1.prerelease.is_empty() && v2.prerelease.is_empty() {
        return 0;
    }
    // A release outranks any prerelease of the same core version.
    if v1.prerelease.is_empty() {
        return 1;
    }
    if v2.prerelease.is_empty() {
        return -1;
    }

    let limit = v1.prerelease.len().min(v2.prerelease.len());
    for i in 0..limit {
        let part1 = &v1.prerelease[i];
        let part2 = &v2.prerelease[i];
        let numeric1 = is_numeric_identifier(part1);
        let numeric2 = is_numeric_identifier(part2);
        match (numeric1, numeric2) {
            (true, true) => {
                let result = compare_numeric_identifiers(part1, part2);
                if result != 0 {
                    return result;
                }
            }
            // SemVer: numeric identifiers sort below alphanumeric ones.
            (true, false) => return -1,
            (false, true) => return 1,
            (false, false) => {
                let result = match part1.cmp(part2) {
                    std::cmp::Ordering::Greater => 1,
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                };
                if result != 0 {
                    return result;
                }
            }
        }
    }
    match v1.prerelease.len().cmp(&v2.prerelease.len()) {
        std::cmp::Ordering::Greater => 1,
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
    }
}

/// Go `hasDevelopmentMarker` (updater.go:567-576): the marker must be the
/// whole value, a `marker-` prefix, or a `marker<digit>` prefix. This is why
/// Go rejects `v0.1.8-devil` as a dev build where a naive `contains` matches.
fn has_development_marker(value: &str, marker: &str) -> bool {
    if value == marker || value.starts_with(&format!("{marker}-")) {
        return true;
    }
    if value.len() <= marker.len() || !value.starts_with(marker) {
        return false;
    }
    let next = value.as_bytes()[marker.len()];
    next.is_ascii_digit()
}

/// Go `isDevelopmentVersion` (updater.go:541-553): trims, drops one leading
/// `v`/`V`, lowercases, then looks for a marker.
pub fn is_development_version(raw: &str) -> bool {
    let trimmed = raw.trim();
    let trimmed = trimmed
        .strip_prefix('v')
        .or_else(|| trimmed.strip_prefix('V'))
        .unwrap_or(trimmed);
    let raw = trimmed.trim().to_ascii_lowercase();
    DEVELOPMENT_MARKERS
        .iter()
        .any(|marker| has_development_marker(&raw, marker))
}

/// Go `isDevelopmentPrerelease` (updater.go:555-565).
fn is_development_prerelease(parsed: &ParsedVersion) -> bool {
    parsed.prerelease.iter().any(|identifier| {
        let identifier = identifier.to_ascii_lowercase();
        DEVELOPMENT_MARKERS
            .iter()
            .any(|marker| has_development_marker(&identifier, marker))
    })
}

/// True when the version string marks a development build (`dev`, `dirty`,
/// `nightly`, `local`, `snapshot`, `git`, ...).
pub fn is_dev_version(v: &str) -> bool {
    is_development_version(v)
}

/// Go `isNewerVersion` (updater.go:578-594): an unparseable candidate is a
/// hard error; an unparseable current version fails closed (no update) when it
/// carries a development marker, and is a hard error otherwise.
pub fn is_newer_version(candidate: &str, current: &str) -> Result<bool, String> {
    let parsed_candidate = parse_version(candidate)
        .map_err(|e| format!("invalid candidate version {candidate:?}: {e}"))?;
    let parsed_current = match parse_version(current) {
        Ok(p) => p,
        Err(e) => {
            if is_development_version(current) {
                return Ok(false);
            }
            return Err(format!("invalid current version {current:?}: {e}"));
        }
    };
    // A dev prerelease on the same core is never upgraded.
    if is_development_prerelease(&parsed_current)
        && compare_version_core(&parsed_candidate, &parsed_current) <= 0
    {
        return Ok(false);
    }
    Ok(compare_parsed_versions(&parsed_candidate, &parsed_current) > 0)
}

/// Compare two semver-ish strings with optional leading `v` and optional
/// pre-release suffix. Returns 1 if `a>b`, -1 if `a<b`, 0 if equal.
/// Faithful port of Go `compareVersions` (updater.go:600-613).
pub fn compare_versions(a: &str, b: &str) -> i32 {
    let p1 = parse_version(a);
    let p2 = parse_version(b);
    if p2.is_err() && is_development_version(b) {
        return -1;
    }
    let (Ok(p1), Ok(p2)) = (p1, p2) else {
        // Invalid inputs compare EQUAL so callers fail closed instead of
        // announcing an update (Go updater.go:606-608).
        return 0;
    };
    if is_development_prerelease(&p2) && compare_version_core(&p1, &p2) <= 0 {
        return -1;
    }
    compare_parsed_versions(&p1, &p2)
}

/// Go `CheckNewerThanCurrent` (updater.go:385-387): reports whether
/// `candidate` is newer, surfacing the parse error for a malformed candidate
/// or a non-development current version.
pub fn check_newer_than_current(candidate: &str) -> Result<bool, String> {
    is_newer_version(candidate, &current_version())
}

/// True when `candidate` is newer than this binary's version.
pub fn is_newer_than_current(candidate: &str) -> bool {
    check_newer_than_current(candidate).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The updater must compare against the **port's** version. When both
    /// constants were one value, `--check-update` compared the running release
    /// against itself and always reported "already up to date" — v0.25.1
    /// shipped with a dead updater because of it.
    #[test]
    fn current_version_is_the_port_version_not_the_go_parity_version() {
        assert_eq!(current_version(), PORT_VERSION);
    }

    /// Both must stay well-formed, since `compare_versions` parses them.
    #[test]
    fn both_versions_are_v_prefixed_semver() {
        for v in [APP_VERSION, PORT_VERSION] {
            assert!(v.starts_with('v'), "{v} must start with v");
            assert!(
                compare_versions(v, "v0.0.1") > 0,
                "{v} must parse and order above v0.0.1"
            );
        }
    }

    /// The envelope constant is a *compatibility* pin, so it must not drift
    /// ahead of a Go bv release that does not exist. Bumping it to match a
    /// port-only release takes the golden gate from PASS=64/DIFF_FAILS=0 to
    /// PASS=5/DIFF_FAILS=59 — one byte per file, the `version` field.
    #[test]
    fn go_parity_version_stays_on_a_real_go_bv_release() {
        assert_eq!(
            APP_VERSION, "v0.25.0",
            "the envelope version is pinned to the Go release this port tracks; \
             raising it breaks byte-parity with the frozen goldens"
        );
    }

    #[test]
    fn basic_ordering() {
        assert_eq!(compare_versions("v0.1.8", "v0.1.7"), 1);
        assert_eq!(compare_versions("v0.1.7", "v0.1.8"), -1);
        assert_eq!(compare_versions("v0.1.7", "v0.1.7"), 0);
        assert_eq!(compare_versions("0.1.8", "v0.1.7"), 1);
    }

    #[test]
    fn prerelease_lower_than_release() {
        assert_eq!(compare_versions("v1.2.3-alpha", "v1.2.3"), -1);
        assert_eq!(compare_versions("v1.2.3", "v1.2.3-alpha"), 1);
        assert_eq!(compare_versions("v1.2.3-alpha.1", "v1.2.3-alpha.2"), -1);
        assert_eq!(compare_versions("v1.2.3-rc.1", "v1.2.3-rc.1"), 0);
    }

    #[test]
    fn dev_build_never_prompts() {
        // Unparseable current carrying a bare marker: isDevelopmentVersion.
        assert_eq!(compare_versions("v0.1.8", "dev"), -1);
        assert_eq!(compare_versions("v9.9.9", "  local  "), -1);
        assert_eq!(compare_versions("v9.9.9", "vgit7"), -1);
        // Parseable current with a dev prerelease: isDevelopmentPrerelease
        // suppresses the update only when the candidate core is not higher.
        assert_eq!(compare_versions("v0.1.8", "v0.1.8-dirty"), -1);
        assert_eq!(compare_versions("v9.9.9", "v9.9.9-dev2"), -1);
        // A genuinely higher release still wins over a dev prerelease.
        assert_eq!(compare_versions("v9.9.9", "0.1.8-dev2"), 1);
    }

    // --- Regressions the pre-port implementation got wrong ---------------

    #[test]
    fn four_component_core_is_rejected_not_truncated() {
        // Go: "version %q must have one to three numeric components".
        let err = parse_version("1.2.3.4").unwrap_err();
        assert!(err.contains("one to three numeric components"), "{err}");
        // The old `.take(3)` parser accepted this as 1.2.3.
        assert!(parse_version("1.2").is_ok());
    }

    #[test]
    fn leading_zeros_are_rejected() {
        assert!(parse_version("01.2.3").is_err());
        assert!(parse_version("1.02.3").is_err());
        assert!(parse_version("1.2.03").is_err());
        // A single "0" component is fine; "0" is not a leading zero.
        assert!(parse_version("0.0.0").is_ok());
        // Prerelease numeric identifiers reject leading zeros too.
        assert!(parse_version("1.2.3-01").is_err());
    }

    #[test]
    fn version_length_is_capped_at_128() {
        // Build metadata is the only field that can grow without tripping the
        // leading-zero rule on a core component.
        let ok = format!("1.0.0+{}", "a".repeat(122));
        assert_eq!(ok.len(), 128);
        assert!(parse_version(&ok).is_ok());
        let too_long = format!("1.0.0+{}", "a".repeat(123));
        assert_eq!(too_long.len(), 129);
        assert_eq!(parse_version(&too_long).unwrap_err(), "version is too long");
    }

    #[test]
    fn invalid_identifier_characters_are_rejected() {
        assert!(parse_version("1.2.3-alpha_beta").is_err());
        assert!(parse_version("1.2.3-al pha").is_err());
        assert!(parse_version("1.2.3+bu ild").is_err());
        assert!(parse_version("1.2.3-a..b").is_err());
        assert!(parse_version("1.2.3+a+b").is_err());
    }

    #[test]
    fn build_metadata_is_validated_then_ignored() {
        assert_eq!(compare_versions("v1.2.3+build1", "v1.2.3+build2"), 0);
        assert_eq!(compare_versions("v1.2.3+build1", "v1.2.3"), 0);
    }

    #[test]
    fn two_unparseable_versions_compare_equal() {
        // Go updater.go:606-608 — fail closed, never a lexicographic compare.
        assert_eq!(compare_versions("garbage", "zzz"), 0);
        assert_eq!(compare_versions("1.2.3.4", "not-a-version"), 0);
        // An unparseable candidate is never announced as an update, whatever
        // the current version is.
        assert!(!is_newer_than_current("1.2.3.4"));
        assert!(!is_newer_than_current("garbage"));
        // ...but a well-formed one that really is newer still is.
        assert!(check_newer_than_current("v999.0.0").unwrap());
    }

    #[test]
    fn development_marker_matching_is_exact_not_substring() {
        // Go hasDevelopmentMarker: the marker must be the whole value, a
        // `marker-` prefix, or a `marker<digit>` prefix. isDevelopmentVersion
        // runs on the whole string with one leading `v`/`V` stripped, so a
        // version-shaped local build is matched by isDevelopmentPrerelease
        // instead — see dev_build_never_prompts.
        for dev in [
            "dev",
            "vdev",
            "Vdev",
            "dev-rc1",
            "dev2",
            "  dirty  ",
            "git7",
            "local",
        ] {
            assert!(
                is_dev_version(dev),
                "{dev:?} should be a development version"
            );
        }
        // "devil" merely CONTAINS "dev"; Go says it is an ordinary version.
        // This is the exact case the pre-port `.contains()` check got wrong.
        for real in [
            "v0.1.8-devil",
            "0.1.8-dirty",
            "V0.1.8-DEV-rc1",
            "0.1.8-git7",
            "0.1.8-snapshot",
            "v0.1.8",
            "1.2.3",
        ] {
            assert!(
                !is_dev_version(real),
                "{real:?} should NOT be a development version"
            );
        }
    }

    #[test]
    fn dev_prerelease_blocks_same_core_update() {
        // Go isNewerVersion: a dev prerelease on the same or newer core wins.
        assert!(!is_newer_version("v0.1.8", "v0.1.8-dev1").unwrap());
        assert!(!is_newer_version("v0.1.7", "v0.1.8-dev1").unwrap());
        // A genuinely higher release still wins.
        assert!(is_newer_version("v0.2.0", "v0.1.8-dev1").unwrap());
    }

    #[test]
    fn is_newer_version_reports_malformed_input() {
        assert!(is_newer_version("nonsense", "v0.1.8").is_err());
        // Non-development malformed current is a hard error, not a silent skip.
        assert!(is_newer_version("v9.9.9", "nonsense").is_err());
        // A dev current fails closed without erroring.
        assert!(!is_newer_version("v9.9.9", "dev-build").unwrap());
    }

    #[test]
    fn numeric_core_compares_by_length_then_value() {
        // Go compares identifiers as strings, by length first: 9 < 10.
        assert_eq!(compare_versions("v1.2.9", "v1.2.10"), -1);
        assert_eq!(compare_versions("v1.2.100", "v1.2.99"), 1);
        assert_eq!(compare_versions("v10.0.0", "v9.0.0"), 1);
    }

    #[test]
    fn shorter_cores_are_zero_padded_like_go() {
        assert_eq!(compare_versions("v1.2", "v1.2.0"), 0);
        assert_eq!(compare_versions("v1.3", "v1.2.9"), 1);
    }

    #[test]
    fn parsed_exposes_core_component_count() {
        assert_eq!(parse_version("v1.2.3").unwrap().core_components(), 3);
        assert_eq!(parse_version("v1.2").unwrap().core_components(), 2);
    }

    #[test]
    fn current_version_has_a_v_prefix() {
        assert!(current_version().starts_with('v'));
    }
}
