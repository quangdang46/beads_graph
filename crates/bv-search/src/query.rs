//! Query heuristics — port of Go `pkg/search/query_adjust.go` +
//! `lexical_boost.go`.
//!
//! Short queries (≤2 tokens or ≤12 runes) favor literal text matching:
//! weights get a text-relevance floor (0.55), the hybrid candidate pool
//! grows (300 vs 200), and literal token matches earn a +0.35 boost.

/// Short-query thresholds (Go constants).
pub const SHORT_QUERY_TOKEN_LIMIT: usize = 2;
pub const SHORT_QUERY_RUNE_LIMIT: usize = 12;
pub const SHORT_QUERY_MIN_TEXT_WEIGHT: f64 = 0.55;
pub const SHORT_QUERY_DOC_BOOST: f64 = 0.35;
pub const HYBRID_CANDIDATE_MIN: usize = 200;
pub const HYBRID_CANDIDATE_MIN_SHORT: usize = 300;
pub const HYBRID_CANDIDATE_DEFAULT_LIMIT: usize = 10;

/// Lightweight query stats (Go `QueryStats`).
pub struct QueryStats {
    pub tokens: usize,
    pub length: usize,
    pub is_short: bool,
}

/// Analyze a query (Go `AnalyzeQuery`): token count via letter/digit runs,
/// length in runes, short when tokens ≤ 2 or runes ≤ 12 (empty → short).
pub fn analyze_query(query: &str) -> QueryStats {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return QueryStats {
            tokens: 0,
            length: 0,
            is_short: true,
        };
    }
    let tokens = count_tokens(trimmed);
    let length = trimmed.chars().count();
    QueryStats {
        tokens,
        length,
        is_short: tokens <= SHORT_QUERY_TOKEN_LIMIT || length <= SHORT_QUERY_RUNE_LIMIT,
    }
}

fn count_tokens(s: &str) -> usize {
    lexical_tokens(s).len()
}

/// Short-query predicate (Go `IsShortQuery`).
pub fn is_short_query(query: &str) -> bool {
    analyze_query(query).is_short
}

/// Boost text relevance to the 0.55 floor for short queries, rescaling the
/// rest proportionally (Go `AdjustWeightsForQuery`).
pub fn adjust_weights_for_query(
    weights: crate::hybrid::Weights,
    query: &str,
) -> crate::hybrid::Weights {
    use crate::hybrid::Weights;
    if !is_short_query(query) {
        return weights;
    }
    if weights.text_relevance >= SHORT_QUERY_MIN_TEXT_WEIGHT {
        return weights;
    }
    let target = SHORT_QUERY_MIN_TEXT_WEIGHT;
    let remaining = weights.text_relevance
        + weights.pagerank
        + weights.status
        + weights.impact
        + weights.priority
        + weights.recency
        - weights.text_relevance;
    if remaining <= 0.0 {
        return Weights {
            text_relevance: 1.0,
            pagerank: 0.0,
            status: 0.0,
            impact: 0.0,
            priority: 0.0,
            recency: 0.0,
        };
    }
    let scale = (1.0 - target) / remaining;
    Weights {
        text_relevance: target,
        pagerank: weights.pagerank * scale,
        status: weights.status * scale,
        impact: weights.impact * scale,
        priority: weights.priority * scale,
        recency: weights.recency * scale,
    }
}

/// Hybrid candidate pool size (Go `HybridCandidateLimit`): 3× limit clamped
/// to a 200 floor (300 for short queries).
pub fn hybrid_candidate_limit(limit: usize, total: usize, query: &str) -> usize {
    let limit = if limit == 0 {
        HYBRID_CANDIDATE_DEFAULT_LIMIT
    } else {
        limit
    };
    if total == 0 {
        return 0;
    }
    let base = limit.saturating_mul(3);
    let min = if is_short_query(query) {
        HYBRID_CANDIDATE_MIN_SHORT
    } else {
        HYBRID_CANDIDATE_MIN
    };
    base.max(min).min(total)
}

/// Lexical tokens: maximal letter/digit runs (Go `lexicalTokens`).
pub fn lexical_tokens(s: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in s.char_indices() {
        if c.is_alphanumeric() {
            if start.is_none() {
                start = Some(i);
            }
        } else if let Some(st) = start.take() {
            tokens.push(s[st..i].to_string());
        }
    }
    if let Some(st) = start {
        tokens.push(s[st..].to_string());
    }
    tokens
}

/// Literal-match boost for short queries (Go `ShortQueryLexicalBoost`):
/// +0.35 when every query token matches a doc token (exact for ≤2-rune
/// tokens, prefix otherwise). Operates on the same document text used for
/// indexing — the caller passes the already-built doc.
pub fn short_query_lexical_boost(query: &str, doc: &str) -> f64 {
    LexicalMatchDoc::new(doc).boost(query)
}

/// A document prepared once for repeated [`short_query_lexical_boost`]
/// scoring.
///
/// Go's `ShortQueryLexicalBoost` lowercases the *whole* document before
/// tokenizing it. That is not a detail a caller can skip or reimplement: the
/// lowercased form is what the tokens are cut from, and Unicode case folding
/// is context-sensitive (Greek final sigma), so lowering token-by-token is
/// not the same function. Which leaves the allocation — a fresh copy of the
/// document on every call. Scoring one document per issue per keystroke means
/// re-lowercasing the entire corpus for every character typed, ~15 ms on a
/// 3.4 MB dataset. Prepare once, score many.
#[derive(Clone)]
pub struct LexicalMatchDoc {
    lowered: String,
}

impl LexicalMatchDoc {
    /// Pre-lowers `doc` for [`Self::boost`].
    pub fn new(doc: &str) -> Self {
        Self {
            lowered: doc.to_lowercase(),
        }
    }

    /// Go `ShortQueryLexicalBoost`, minus the per-call lowercase.
    pub fn boost(&self, query: &str) -> f64 {
        if !is_short_query(query) {
            return 0.0;
        }
        let needle = query.trim().to_lowercase();
        if needle.is_empty() || self.lowered.is_empty() {
            return 0.0;
        }
        if short_query_matches_document(&needle, &self.lowered) {
            SHORT_QUERY_DOC_BOOST
        } else {
            0.0
        }
    }
}

/// Allocation-free twin of [`lexical_tokens`]: hands each maximal
/// letter/digit run to `f`, in order, as a borrow of `s`.
///
/// Same token boundaries as `lexical_tokens` — that function is exactly this
/// loop with `tokens.push(s[st..i].to_string())` in place of `f(&s[st..i])`.
/// Scoring a megabyte-scale document cannot afford one `String` per token,
/// which for a 3 MB corpus is roughly 500k allocations per call.
fn for_each_token(s: &str, mut f: impl FnMut(&str)) {
    let mut start: Option<usize> = None;
    for (i, c) in s.char_indices() {
        if c.is_alphanumeric() {
            if start.is_none() {
                start = Some(i);
            }
        } else if let Some(st) = start.take() {
            f(&s[st..i]);
        }
    }
    if let Some(st) = start {
        f(&s[st..]);
    }
}

/// A query token and the rule that decides whether it matched
/// (Go `hasMatchingDocumentToken`).
#[derive(Clone, Copy, PartialEq)]
enum Needle<'a> {
    /// ≤2 runes: the document must contain this token exactly.
    Exact(&'a str),
    /// Longer: any document token with this prefix counts.
    Prefix(&'a str),
}

fn short_query_matches_document(query: &str, doc: &str) -> bool {
    let query_tokens = lexical_tokens(query);
    if query_tokens.is_empty() {
        return false;
    }
    // Classify each query token once. Deciding "exact or prefix" inside the
    // document loop re-ran `chars().count()` once per *document* token, and
    // a 3 MB corpus has half a million of those per keystroke.
    let mut needles: Vec<(Needle<'_>, bool)> = Vec::with_capacity(query_tokens.len());
    for qt in &query_tokens {
        let qt = qt.as_str();
        if qt.chars().count() <= 2 {
            needles.push((Needle::Exact(qt), false));
        } else if doc.contains(qt) {
            needles.push((Needle::Prefix(qt), false));
        }
        // A `Prefix` needle whose text does not occur in the document at all
        // cannot match: a token can only start with it if the text is
        // somewhere in the document.
    }
    if needles.len() != query_tokens.len() {
        // Some query token is dead, so Go's `all()` fails and there is
        // nothing to scan. This is also the path that skips the scan for
        // documents not mentioning the query at all — the common case, and
        // the difference between re-scanning every byte of the corpus per
        // keystroke and not.
        return false;
    }
    let mut remaining = needles.len();
    // Go checks every query token against every document token and requires
    // all of them to match (`queryTokens.All(...)`), so one document token
    // may satisfy several query tokens — "fix fix" matches a document holding
    // a single "fix". A found-flag per query token preserves that; stopping
    // at the first match would not.
    for_each_token(doc, |dt| {
        if remaining == 0 {
            return;
        }
        for (needle, matched) in needles.iter_mut() {
            if *matched {
                continue;
            }
            let hit = match *needle {
                Needle::Exact(qt) => dt == qt,
                Needle::Prefix(qt) => dt.starts_with(qt),
            };
            if hit {
                *matched = true;
                remaining -= 1;
            }
        }
    });
    // A document with no tokens leaves every query token unmatched, which is
    // Go's `len(docTokens) == 0 → false` without materialising the tokens.
    remaining == 0
}

#[cfg(test)]
fn has_matching_document_token(query_token: &str, doc_tokens: &[String]) -> bool {
    for dt in doc_tokens {
        if query_token.chars().count() <= 2 {
            if dt == query_token {
                return true;
            }
            continue;
        }
        if dt.starts_with(query_token) {
            return true;
        }
    }
    false
}

/// The pre-cache matcher, kept verbatim as an oracle for the streaming one.
#[cfg(test)]
fn short_query_matches_document_naive(query: &str, doc: &str) -> bool {
    let query_tokens = lexical_tokens(query);
    if query_tokens.is_empty() {
        return false;
    }
    let doc_tokens = lexical_tokens(doc);
    if doc_tokens.is_empty() {
        return false;
    }
    query_tokens
        .iter()
        .all(|qt| has_matching_document_token(qt, &doc_tokens))
}

/// The pre-cache public entry point, kept verbatim as an oracle.
#[cfg(test)]
fn short_query_lexical_boost_naive(query: &str, doc: &str) -> f64 {
    if !is_short_query(query) {
        return 0.0;
    }
    let needle = query.trim().to_lowercase();
    if needle.is_empty() || doc.is_empty() {
        return 0.0;
    }
    if short_query_matches_document_naive(&needle, &doc.to_lowercase()) {
        SHORT_QUERY_DOC_BOOST
    } else {
        0.0
    }
}

/// Issue document text for indexing (Go `IssueDocument`): ID ×3, title ×2,
/// labels ×1, description ×1, newline-joined, empty parts skipped.
pub fn issue_document(id: &str, title: &str, labels: &[String], description: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    let id = id.trim();
    if !id.is_empty() {
        parts.extend([id.to_string(), id.to_string(), id.to_string()]);
    }
    let title = title.trim();
    if !title.is_empty() {
        parts.extend([title.to_string(), title.to_string()]);
    }
    let labels_joined = labels
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if !labels_joined.is_empty() {
        parts.push(labels_joined);
    }
    let desc = description.trim();
    if !desc.is_empty() {
        parts.push(desc.to_string());
    }
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_query_detection_matches_go_rules() {
        assert!(is_short_query(""));
        assert!(is_short_query("fix"));
        assert!(is_short_query("fix login")); // 2 tokens
        assert!(!is_short_query("fix the login bug now please sir")); // 6 tokens, long
        assert!(is_short_query("abcdefghijkl")); // 12 runes
                                                 // 13 runes but 1 token: Go's rule is tokens ≤ 2 OR runes ≤ 12,
                                                 // so a single long token is still short (token rule dominates).
        assert!(is_short_query("abcdefghijklm"));
    }

    #[test]
    fn thirteen_runes_single_token_is_still_short() {
        // tokens=1 ≤ 2 dominates: Go's rule is OR, not AND.
        assert!(is_short_query("abcdefghijklm"));
    }

    #[test]
    fn adjust_weights_floors_text_at_point_five_five() {
        let w = crate::hybrid::get_preset("impact-first").unwrap();
        assert!(w.text_relevance < 0.55);
        let adj = adjust_weights_for_query(w, "fix");
        assert!((adj.text_relevance - 0.55).abs() < 1e-9);
        let total = adj.text_relevance
            + adj.pagerank
            + adj.status
            + adj.impact
            + adj.priority
            + adj.recency;
        assert!((total - 1.0).abs() < 1e-9);
        // Long query → untouched.
        let same = adjust_weights_for_query(w, "fix the login bug now please sir");
        assert_eq!(
            (same.text_relevance, w.text_relevance),
            (w.text_relevance, w.text_relevance)
        );
    }

    #[test]
    fn lexical_boost_matches_go_prefix_rules() {
        assert_eq!(short_query_lexical_boost("fix", "fix login bug"), 0.35);
        assert_eq!(short_query_lexical_boost("log", "fix login bug"), 0.35); // prefix
        assert_eq!(short_query_lexical_boost("og", "fix login bug"), 0.0); // not a prefix
        assert_eq!(short_query_lexical_boost("is", "this is it"), 0.35); // ≤2 runes: exact
        assert_eq!(short_query_lexical_boost("is", "issue list"), 0.0); // ≤2 runes: no prefix rule
        assert_eq!(
            short_query_lexical_boost("fix the login bug now please sir", "fix login"),
            0.0
        ); // long query → no boost
    }

    #[test]
    fn issue_document_boosts_id_and_title() {
        let doc = issue_document("A-1", "Fix bug", &["ui".into()], "desc here");
        assert_eq!(doc.matches("A-1").count(), 3);
        assert_eq!(doc.matches("Fix bug").count(), 2);
        assert!(doc.contains("ui"));
        assert!(doc.contains("desc here"));
    }

    /// The streaming matcher replaced a `Vec<String>`-materialising one so a
    /// keystroke stops allocating ~500k tokens per document. It must agree
    /// with the old code on every input, not just the curated cases above —
    /// this walks the query/document cross-product plus the awkward shapes:
    /// repeated query tokens satisfied by a single doc token, 1- and 2-rune
    /// exact-vs-prefix rules, mixed case, CJK (non-ASCII `char_indices`
    /// boundaries) and empty documents.
    #[test]
    fn streaming_matcher_agrees_with_the_materialising_one() {
        let docs = [
            "",
            "   ",
            "!!!",
            "fix login bug",
            "Fix Login Bug",
            "fix",
            "fi",
            "f",
            "界 世界 rendering",
            "a-b-c-d-e",
            "prefixverylongtokenhere short",
            "database database",
            "tab\tand\nnewline, punctuation!;",
            "Ångström ÄÖÜ ß ﬁ",
        ];
        let queries = [
            "",
            " ",
            "fix",
            "fi",
            "f",
            "lo",
            "log",
            "bug",
            "界",
            "世界",
            "界世",
            "a-b-c-d-e",
            "abcde",
            "prefixverylongtokenhere",
            "prefix",
            "database",
            "tab",
            "newline",
            "ÅNGSTRÖM",
            "ångström",
            "fi2",
            "zzzzz",
            // Multi-token queries, including the shape that a
            // "does this text occur anywhere at all" pre-filter gets wrong:
            // one token present, one absent. `all()` must still fail.
            "graph render",
            "graph render tree",
            "fi bug",
            "界 rendering",
            "prefix short",
            "a bug",
        ];
        for doc in docs {
            for query in queries {
                assert_eq!(
                    short_query_matches_document(query, doc),
                    short_query_matches_document_naive(query, doc),
                    "disagreement on query={query:?} doc={doc:?}"
                );
                assert_eq!(
                    short_query_lexical_boost(query, doc),
                    short_query_lexical_boost_naive(query, doc),
                    "boost disagreement on query={query:?} doc={doc:?}"
                );
                assert_eq!(
                    LexicalMatchDoc::new(doc).boost(query),
                    short_query_lexical_boost_naive(query, doc),
                    "prepared-doc disagreement on query={query:?} doc={doc:?}"
                );
            }
        }
    }

    /// Go requires *all* query tokens to match, and lets one document token
    /// satisfy more than one of them. A per-query-token found-flag is what
    /// preserves that; bailing out on the first hit would not.
    #[test]
    fn one_document_token_can_satisfy_repeated_query_tokens() {
        assert!(short_query_matches_document("fix fix", "only one fix here"));
        assert!(short_query_matches_document(
            "graph render",
            "rendering graphs quickly"
        ));
    }

    /// A query token whose text does not occur anywhere in the document can
    /// never match, and Go's `all()` then fails — even when every other
    /// token matches. Dropping the dead token from the worklist is only a
    /// speed-up; treating the survivors as sufficient is a wrong answer.
    #[test]
    fn one_absent_query_token_fails_the_whole_match() {
        assert!(!short_query_matches_document("graph render", "graph only"));
        assert!(!short_query_matches_document("fi bug", "a single fix"));
        assert!(!short_query_matches_document("graph render tree", "graph"));
        // Present as a substring, but no document token starts with it. The
        // "does it occur at all" pre-filter must not turn this into a match.
        assert!(!short_query_matches_document(
            "erin",
            "the rendering pipeline"
        ));
    }
}
