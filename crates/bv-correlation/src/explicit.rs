//! Explicit-ID correlation — port of Go `pkg/correlation/explicit.go`:
//! builtin ID patterns, match classification, confidence calculation.

use regex::Regex;
use std::collections::HashSet;
#[cfg(test)]
use std::sync::{Mutex, MutexGuard};
use std::sync::{OnceLock, RwLock};

/// Go `customIDPatterns` (explicit.go:34-39) — the package global the
/// `--id-pattern` flag writes to. Guarded by an RwLock per the shared-state
/// convention; writes happen once at CLI startup.
static CUSTOM_ID_PATTERNS: OnceLock<RwLock<Vec<Regex>>> = OnceLock::new();

fn custom_id_patterns_slot() -> &'static RwLock<Vec<Regex>> {
    CUSTOM_ID_PATTERNS.get_or_init(|| RwLock::new(Vec::new()))
}

/// Go `SetCustomIDPatterns` — registers extra bead ID patterns used alongside
/// the built-ins by every message-based ID matcher (explicit matching, orphan
/// detection), so trackers whose IDs carry no numeric suffix work (#188).
/// Patterns may capture the ID in group 1; a pattern with no capture group
/// matches the ID as the whole expression.
pub fn set_custom_id_patterns(patterns: Vec<Regex>) {
    *custom_id_patterns_slot()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = patterns;
}

/// Go `CustomIDPatterns` — the registered patterns in registration order, empty
/// when `--id-pattern` was not passed.
pub fn custom_id_patterns() -> Vec<Regex> {
    custom_id_patterns_slot()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// Serializes tests against the process-global registry above. The runner is
/// parallel, so without this a registration in one test leaks into a sibling
/// test's fixture. Hold it in any test that registers patterns, and in any test
/// that reaches them through a `custom_id_patterns()` caller.
///
/// The guard is a plain `Mutex` and so is not reentrant: a test must either
/// take this itself and call the registry readers directly, or use a helper
/// that takes it — never both on one call path.
#[cfg(test)]
pub(crate) fn custom_patterns_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Match classification (Go matchType strings).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchKind {
    Closes,
    Fixes,
    Resolves,
    Bracket,
    Refs,
    Bead,
    /// Go's `classifyMatch` "generic" — the raw match named no keyword and no
    /// project prefix, so it earns no confidence bonus.
    Generic,
}

/// Go: `CalculateConfidence` — exact bonus/penalty table.
pub fn calculate_confidence(match_kind: Option<MatchKind>, total_matches: usize) -> f64 {
    let mut base = 0.90f64;
    match match_kind {
        Some(MatchKind::Closes) | Some(MatchKind::Fixes) | Some(MatchKind::Resolves) => {
            base += 0.05
        }
        Some(MatchKind::Bracket) => base += 0.02,
        Some(MatchKind::Refs) => base += 0.01,
        Some(MatchKind::Bead) => base += 0.03,
        // `Generic` is Go's catch-all "generic" and, like `None`, earns no bonus.
        Some(MatchKind::Generic) | None => {}
    }
    if total_matches > 1 {
        base -= 0.02 * (total_matches - 1) as f64;
    }
    base.clamp(0.70, 0.99)
}

/// Builtin bead-ID patterns — Go's `builtinPatterns()`
/// (explicit.go:70-90) verbatim and in evaluation order, followed by the
/// `DefaultPatterns` custom tail (`--id-pattern`, #188).
///
/// Go has exactly ONE builtin set; it backs both `ExplicitMatcher` (this
/// file's port target) and the history walker's explicit-ID strategy
/// (`history::builtin_patterns` evaluates the same eight regexes). An earlier
/// revision of this struct carried a divergent combined-alternation action
/// regex and a `beads[-_]` (no `?`) legacy form, so "closed BV-1" scored
/// Closes/0.95 where Go scores generic/0.90 and "fixes#BV-1" was missed
/// entirely — both are fixed by evaluating Go's patterns literally.
pub struct IdPatterns {
    /// Ordered pattern list: the eight Go built-ins, then any custom
    /// `--id-pattern` registrations. Order decides which pattern claims an ID
    /// first — Go keeps the first match per normalized ID.
    patterns: Vec<Regex>,
}

impl Default for IdPatterns {
    fn default() -> Self {
        IdPatterns {
            patterns: vec![
                // Go explicit.go:73 — `[PREFIX-123]` bracket form.
                Regex::new(r"\[([A-Za-z]+-\d+)\]").unwrap(),
                // Go explicit.go:77 — `closes?` action verb.
                Regex::new(r"(?i)closes?:?\s*#?([A-Za-z]+-\d+)").unwrap(),
                // Go explicit.go:78 — `fix`/`fixes`/`fixed`.
                Regex::new(r"(?i)fix(?:es|ed)?:?\s*#?([A-Za-z]+-\d+)").unwrap(),
                // Go explicit.go:79 — `ref`/`refs`.
                Regex::new(r"(?i)refs?:?\s*#?([A-Za-z]+-\d+)").unwrap(),
                // Go explicit.go:80 — `resolve`/`resolves`/`resolved`.
                Regex::new(r"(?i)resolves?:?\s*#?([A-Za-z]+-\d+)").unwrap(),
                // Go explicit.go:85 — `bead-N`/`beads-N`/`bead_N`/`beads_N`.
                Regex::new(r"(?i)beads?[-_](\d+)\b").unwrap(),
                // Go explicit.go:86 — `bv-N`/`bv_N` project form.
                Regex::new(r"(?i)bv[-_](\d+)\b").unwrap(),
                // Go explicit.go:89 — generic UPPERCASE-PREFIX-N.
                Regex::new(r"\b([A-Z]{2,10}-\d+)\b").unwrap(),
            ],
        }
    }
}

impl IdPatterns {
    /// Go `NewExplicitMatcherWithPatterns` — the built-ins plus an explicit
    /// pattern list, which is what Go's `DefaultPatterns` yields once
    /// [`set_custom_id_patterns`] has run. `Default` deliberately stays
    /// built-ins-only: a `Default` that read the process-global would make every
    /// `IdPatterns::default()` caller depend on CLI flag state.
    pub fn with_custom(custom: Vec<Regex>) -> Self {
        let mut patterns = Self::default().patterns;
        patterns.extend(custom);
        Self { patterns }
    }
}

/// One detected ID mention in a commit message.
#[derive(Debug, Clone)]
pub struct IdMention {
    pub bead_id: String,
    pub kind: MatchKind,
}

/// Scan a commit message for bead-ID mentions — Go
/// `ExplicitMatcher.ExtractIDsFromMessage` (explicit.go:126-160): patterns are
/// evaluated in order, every builtin capture goes through
/// [`crate::history::normalize_bead_id`], and the FIRST pattern to name a
/// normalized ID claims it (later patterns may not reclassify it). The kind
/// is Go's `classifyMatch` applied to the whole match text, narrowed to
/// [`MatchKind`]. Output order is pattern order, not sorted.
pub fn find_mentions(message: &str, patterns: &IdPatterns) -> Vec<IdMention> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for re in &patterns.patterns {
        for caps in re.captures_iter(message) {
            let Some(whole) = caps.get(0) else {
                continue;
            };
            // Go prefers capture group 1 when the pattern defines one;
            // custom patterns without a capture group match the whole
            // expression as the ID (#188).
            let raw = caps
                .get(1)
                .map(|g| g.as_str())
                .filter(|g| !g.is_empty())
                .unwrap_or_else(|| whole.as_str());
            if raw.is_empty() {
                continue;
            }
            let bead_id = crate::history::normalize_bead_id(raw);
            if !seen.insert(bead_id.clone()) {
                continue;
            }
            out.push(IdMention {
                bead_id,
                kind: match_kind(crate::history::classify_match(whole.as_str())),
            });
        }
    }
    out
}

/// Go `classifyMatch`'s string, narrowed to [`MatchKind`].
fn match_kind(match_type: &str) -> MatchKind {
    match match_type {
        "closes" => MatchKind::Closes,
        "fixes" => MatchKind::Fixes,
        "resolves" => MatchKind::Resolves,
        "bracket" => MatchKind::Bracket,
        "refs" => MatchKind::Refs,
        "bead" => MatchKind::Bead,
        _ => MatchKind::Generic,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn confidence_base_and_clamps() {
        assert!(close(calculate_confidence(None, 1), 0.90));
        assert!(close(calculate_confidence(Some(MatchKind::Fixes), 1), 0.95));
        assert!(close(
            calculate_confidence(Some(MatchKind::Bracket), 1),
            0.92
        ));
        assert!(close(calculate_confidence(Some(MatchKind::Refs), 1), 0.91));
        assert!(close(calculate_confidence(Some(MatchKind::Bead), 1), 0.93));
        // multi-ID penalty: 0.95 - 0.02 = 0.93
        assert!(close(calculate_confidence(Some(MatchKind::Fixes), 2), 0.93));
        // clamp low bound: base .90 - .02*14 = .62 -> clamped to .70
        assert_eq!(calculate_confidence(None, 15), 0.70);
    }

    #[test]
    fn finds_action_keywords() {
        let p = IdPatterns::default();
        let mentions = find_mentions("fixes BV-123 and closes AUTH-45", &p);
        // Builtin captures are normalized like the custom-pattern pass, so
        // case variants of the same ID collapse to one mention.
        assert!(mentions.iter().any(|m| m.bead_id == "bv-123"));
        assert!(mentions.iter().any(|m| m.bead_id == "auth-45"));
    }

    #[test]
    fn finds_bracket_and_generic_forms() {
        let p = IdPatterns::default();
        let mentions = find_mentions("[CORE-7] refactor per PROJ-42", &p);
        assert!(mentions
            .iter()
            .any(|m| m.bead_id == "core-7" && m.kind == MatchKind::Bracket));
        assert!(mentions.iter().any(|m| m.bead_id == "proj-42"));
    }

    #[test]
    fn finds_legacy_beads_form() {
        let p = IdPatterns::default();
        let mentions = find_mentions("ported from beads-123 and beads_456", &p);
        assert!(mentions
            .iter()
            .any(|m| m.bead_id == "bv-123" && m.kind == MatchKind::Bead));
        assert!(mentions
            .iter()
            .any(|m| m.bead_id == "bv-456" && m.kind == MatchKind::Bead));
    }

    #[test]
    fn builtin_mentions_normalize_case() {
        let p = IdPatterns::default();
        let mentions = find_mentions("Fixes BV-123, see bv-123", &p);
        assert_eq!(mentions.len(), 1);
        assert_eq!(mentions[0].bead_id, "bv-123");
        assert_eq!(mentions[0].kind, MatchKind::Fixes);
    }

    #[test]
    fn no_mentions_clean_message() {
        let p = IdPatterns::default();
        assert!(find_mentions("just a regular commit", &p).is_empty());
    }

    #[test]
    fn generic_kind_earns_no_confidence_bonus() {
        assert!(close(
            calculate_confidence(Some(MatchKind::Generic), 1),
            0.90
        ));
        assert_eq!(match_kind("generic"), MatchKind::Generic);
        assert_eq!(match_kind("closes"), MatchKind::Closes);
        assert_eq!(match_kind("fixes"), MatchKind::Fixes);
        assert_eq!(match_kind("resolves"), MatchKind::Resolves);
        assert_eq!(match_kind("bracket"), MatchKind::Bracket);
        assert_eq!(match_kind("refs"), MatchKind::Refs);
        assert_eq!(match_kind("bead"), MatchKind::Bead);
    }

    /// Go `TestCustomIDPatterns_NoCaptureGroup` (#188) — no capture group, so
    /// the whole match is the ID, lowercased by `normalizeBeadID`.
    #[test]
    fn custom_pattern_without_capture_group_matches_whole_expression() {
        let p = IdPatterns::with_custom(vec![Regex::new(r"\bzzq-[a-z0-9]{5}\b").unwrap()]);
        let mentions = find_mentions("fix flush ordering (zzq-a1b2c)", &p);
        let hit = mentions
            .iter()
            .find(|m| m.bead_id == "zzq-a1b2c")
            .expect("custom id");
        // `zzq-a1b2c` trips none of the classifyMatch keyword/prefix rules, so
        // it lands on "generic" — no confidence bonus, exactly as in Go.
        assert_eq!(hit.kind, MatchKind::Generic);
        assert!(close(calculate_confidence(Some(hit.kind), 1), 0.90));
    }

    /// Go `TestCustomIDPatterns_WithCaptureGroup` (#188) — capture group 1 is the
    /// ID even when the surrounding match text is longer.
    #[test]
    fn custom_pattern_prefers_capture_group_one() {
        let p =
            IdPatterns::with_custom(vec![Regex::new(r"(?i)ticket\s+(zzt-[a-z]{3})\b").unwrap()]);
        let mentions = find_mentions("board polish, ticket ZZT-PBB done", &p);
        assert!(mentions.iter().any(|m| m.bead_id == "zzt-pbb"));
    }

    /// Custom patterns close out the list, so they may add an ID but never
    /// reclassify one a built-in already claimed — Go keeps the first ID it
    /// finds for a value, and the built-ins are evaluated first.
    #[test]
    fn custom_pattern_does_not_reclassify_a_builtin_id() {
        let p = IdPatterns::with_custom(vec![Regex::new(r"\bzzq-[a-z0-9]{5}\b").unwrap()]);
        let mentions = find_mentions("refs zzq-a1b2c then closes zzq-a1b2c", &p);
        assert_eq!(mentions.len(), 1);
        assert_eq!(mentions[0].bead_id, "zzq-a1b2c");
    }

    /// Go `TestCustomIDPatterns_DefaultsUnaffectedWhenEmpty` (#188) — the
    /// built-ins and the empty-registration case are unchanged.
    #[test]
    fn registry_round_trips_and_defaults_to_empty() {
        let _serialized = custom_patterns_lock();
        set_custom_id_patterns(Vec::new());
        assert!(custom_id_patterns().is_empty());
        let bare = find_mentions("fix flush ordering (zzq-a1b2c)", &IdPatterns::default());
        assert!(bare.is_empty(), "no registration, no custom-form ID");

        set_custom_id_patterns(vec![Regex::new(r"x-[0-9a-f]{4}").unwrap()]);
        assert_eq!(custom_id_patterns().len(), 1);
        set_custom_id_patterns(Vec::new());
        assert!(custom_id_patterns().is_empty());
    }
}
