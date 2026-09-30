//! Byte-exact port of Go `analysis.ComputeDataHash` (pkg/analysis/cache.go:142).
//!
//! Contract: the Rust output MUST equal the Go output for identical issue
//! sets. Verified against golden fixtures captured from commit 9ace029.

use crate::fingerprint::FingerprintWriter;
use crate::model::Issue;
use sha2::{Digest, Sha256};

/// Normalize an RFC3339 timestamp string to UTC RFC3339Nano form, matching
/// Go `t.UTC().Format(time.RFC3339Nano)`:
/// - nanosecond precision, trailing zeros REMOVED (Go RFC3339Nano layout)
/// - "Z" suffix for UTC
pub fn normalize_rfc3339_nano(raw: &str) -> Option<String> {
    let ts = raw.parse::<jiff::Timestamp>().ok()?;
    // jiff formats with as many subsecond digits as needed (Go RFC3339Nano
    // trims trailing zeros too) — but jiff prints "Z" for UTC by default.
    Some(ts.to_string())
}

struct HashIssue<'a> {
    id: &'a str,
    title: &'a str,
    description: &'a str,
    notes: &'a str,
    design: &'a str,
    acceptance_criteria: &'a str,
    assignee: &'a str,
    source_repo: &'a str,
    external_ref: Option<&'a str>,
    status: &'a str,
    issue_type: &'a str,
    priority: i32,
    estimated_minutes: Option<i64>,
    created_at: Option<String>,
    updated_at: Option<String>,
    closed_at: Option<String>,
    labels: Vec<String>,
    deps: Vec<DepKey>,
    comments: Vec<CommentKey>,
}

#[derive(PartialEq)]
struct DepKey {
    depends_on: String,
    dep_type: String,
    created_at: String,
    created_by: String,
}

#[derive(PartialEq)]
struct CommentKey {
    id: String,
    author: String,
    text: String,
    created_at: String,
}

impl<'a> HashIssue<'a> {
    fn from_issue(issue: &'a Issue) -> Option<Self> {
        let mut labels = issue.labels.clone();
        labels.sort();

        let mut deps: Vec<DepKey> = issue
            .dependencies
            .iter()
            .map(|d| DepKey {
                depends_on: d.effective_depends_on().to_string(),
                // Go hashes `string(dep.Type)` — the raw type string. Going
                // through `as_str` instead of re-matching the enum here keeps
                // the two in lockstep, so adding a dependency type can never
                // silently desync the hash from the wire format.
                dep_type: d.r#type.as_str().to_string(),
                // Go: Dependency.CreatedAt is a non-pointer time.Time; absent
                // JSON field decodes to zero time which formats as the
                // constant below (verified via instrumented upstream).
                created_at: d
                    .created_at
                    .as_deref()
                    .and_then(normalize_rfc3339_nano)
                    .unwrap_or_else(|| GO_ZERO_TIME.to_string()),
                created_by: d.created_by.clone(),
            })
            .collect();
        deps.sort_by(|a, b| {
            a.depends_on
                .cmp(&b.depends_on)
                .then(a.dep_type.cmp(&b.dep_type))
                .then(a.created_at.cmp(&b.created_at))
                .then(a.created_by.cmp(&b.created_by))
        });

        let mut comments: Vec<CommentKey> = issue
            .comments
            .iter()
            .map(|c| CommentKey {
                id: c.id.clone(),
                author: c.author.clone(),
                text: c.text.clone(),
                created_at: c
                    .created_at
                    .as_deref()
                    .and_then(normalize_rfc3339_nano)
                    .unwrap_or_else(|| GO_ZERO_TIME.to_string()),
            })
            .collect();
        comments.sort_by(|a, b| {
            a.id.cmp(&b.id)
                .then(a.created_at.cmp(&b.created_at))
                .then(a.author.cmp(&b.author))
                .then(a.text.cmp(&b.text))
        });

        Some(HashIssue {
            id: &issue.id,
            title: &issue.title,
            description: &issue.description,
            notes: &issue.notes,
            design: &issue.design,
            acceptance_criteria: &issue.acceptance_criteria,
            assignee: &issue.assignee,
            source_repo: &issue.source_repo,
            external_ref: issue.external_ref.as_deref(),
            status: issue.status.as_str(),
            issue_type: &issue.issue_type,
            priority: issue.priority,
            estimated_minutes: issue.estimated_minutes,
            created_at: issue.created_at.as_deref().and_then(normalize_rfc3339_nano),
            updated_at: issue.updated_at.as_deref().and_then(normalize_rfc3339_nano),
            closed_at: issue.closed_at.as_deref().and_then(normalize_rfc3339_nano),
            labels,
            deps,
            comments,
        })
    }
}

/// Go zero time formatted as RFC3339Nano — used when timestamps are absent
/// because Go hashes `t.UTC().Format(RFC3339Nano)` unconditionally.
const GO_ZERO_TIME: &str = "0001-01-01T00:00:00Z";

fn write_field(h: &mut Sha256, field: &[u8]) {
    h.update(field);
    h.update([0u8]);
}

/// Go v0.25.0 per-issue content fingerprint (pkg/analysis/cache.go:305).
/// Covers everything that is not a dependency edge.
fn compute_issue_content_hash(w: &mut FingerprintWriter, issue: &Issue) -> String {
    w.reset();

    w.write_string_hash(&issue.title);
    w.write_string_hash(&issue.description);
    w.write_string_hash(&issue.design);
    w.write_string_hash(&issue.acceptance_criteria);
    w.write_string_hash(&issue.notes);
    w.write_string_hash(&issue.assignee);
    w.write_string_hash(&issue.source_repo);
    w.write_string_ptr_hash(issue.external_ref.as_deref());

    w.write_string_hash(issue.status.as_str());
    w.write_string_hash(&issue.issue_type);
    w.write_int_hash(issue.priority as i64);
    w.write_int_ptr_hash(issue.estimated_minutes);
    w.write_time_hash(issue.created_at.as_deref());
    w.write_time_hash(issue.updated_at.as_deref());
    w.write_time_ptr_hash(issue.due_date.as_deref());
    w.write_time_ptr_hash(issue.defer_until.as_deref());
    w.write_time_ptr_hash(issue.closed_at.as_deref());

    w.write_int_hash(issue.compaction_level);
    w.write_time_ptr_hash(issue.compacted_at.as_deref());
    w.write_string_ptr_hash(issue.compacted_at_commit.as_deref());
    w.write_int_hash(issue.original_size);

    if issue.labels.is_empty() {
        w.write_uint_hash(0);
    } else {
        let mut labels = issue.labels.clone();
        labels.sort();
        w.write_uint_hash(labels.len() as u64);
        for label in &labels {
            w.write_string_hash(label);
        }
    }

    let mut comments: Vec<&crate::model::Comment> = issue.comments.iter().collect();
    comments.sort_by(|a, b| {
        a.id.cmp(&b.id)
            .then_with(|| a.created_at.cmp(&b.created_at))
            .then_with(|| a.issue_id.cmp(&b.issue_id))
            .then_with(|| a.author.cmp(&b.author))
            .then_with(|| a.text.cmp(&b.text))
    });
    w.write_uint_hash(comments.len() as u64);
    for comment in comments {
        w.write_string_hash(&comment.id);
        w.write_string_hash(&comment.issue_id);
        w.write_string_hash(&comment.author);
        w.write_string_hash(&comment.text);
        w.write_time_hash(comment.created_at.as_deref());
    }

    w.sum_hex()
}

/// Go v0.25.0 per-issue dependency fingerprint (pkg/analysis/cache.go:378).
/// Returns the literal "none" when the issue has no dependency edges.
fn compute_issue_dependency_hash(w: &mut FingerprintWriter, issue: &Issue) -> String {
    if issue.dependencies.is_empty() {
        return "none".to_string();
    }
    struct DepKey {
        issue_id: String,
        depends_on: String,
        dep_type: String,
        created_at: String,
        created_by: String,
    }
    let mut deps: Vec<DepKey> = issue
        .dependencies
        .iter()
        .map(|dep| DepKey {
            issue_id: dep.issue_id.clone(),
            depends_on: dep.depends_on_id.clone(),
            dep_type: dep.r#type.as_str().to_string(),
            // Go: dep.CreatedAt.UTC().Format(RFC3339Nano) — a non-pointer time,
            // so an absent value formats as the Go zero time.
            created_at: dep
                .created_at
                .as_deref()
                .map(normalize_time_for_hash)
                .unwrap_or_else(|| GO_ZERO_TIME.to_string()),
            created_by: dep.created_by.clone(),
        })
        .collect();
    if deps.is_empty() {
        return "none".to_string();
    }
    deps.sort_by(|a, b| {
        a.issue_id
            .cmp(&b.issue_id)
            .then_with(|| a.depends_on.cmp(&b.depends_on))
            .then_with(|| a.dep_type.cmp(&b.dep_type))
            .then_with(|| a.created_at.cmp(&b.created_at))
            .then_with(|| a.created_by.cmp(&b.created_by))
    });

    w.reset();
    w.write_uint_hash(deps.len() as u64);
    for dep in &deps {
        w.write_string_hash(&dep.issue_id);
        w.write_string_hash(&dep.depends_on);
        w.write_string_hash(&dep.dep_type);
        w.write_string_hash(&dep.created_at);
        w.write_string_hash(&dep.created_by);
    }
    w.sum_hex()
}

/// Dependency timestamps go through the same UTC-`Z` normalization as issue
/// timestamps so both sides of the hash agree with Go's Format output.
fn normalize_time_for_hash(raw: &str) -> String {
    crate::fingerprint::normalize_time_public(raw)
}

/// Compute the aggregate data hash. Empty input -> "empty".
///
/// Go v0.25.0 (pkg/analysis/cache.go:168) assembles a per-issue
/// (ID, ContentHash, DependencyHash) fingerprint list, sorts it by ID
/// (encounter order breaks duplicate-ID ties), and hashes the whole list.
/// The result is a full 64-char SHA-256 hex digest.
pub fn compute_data_hash(issues: &[Issue]) -> String {
    let borrowed: Vec<&Issue> = issues.iter().collect();
    compute_data_hash_refs(&borrowed)
}

/// Go `sourceIssuesHash` (cmd/bv/main.go:7354-7356).
///
/// The envelope publishes two digests over the same load, and they are meant
/// to differ: the top-level one names the set that was *analysed*, while
/// `source_authority.sources[].data_hash` names the file that was *read*. Go
/// keeps them apart by hashing the visible issues together with a bare
/// `{id, status: tombstone}` stub for every deleted record — `issuesWithTombstones`,
/// main.go:7340-7352 — so the authority's digest still accounts for every id
/// the file holds.
///
/// Hashing the visible set alone made the two digests identical, which erased
/// the distinction the split exists for and changed `authority_hash` (and
/// through it `scope_hash`) on any repository that had deleted a bead.
///
/// Stub order follows `tombstone_ids`, which the loader collects in file order
/// — the same order Go appends them in, and `compute_data_hash` sorts by ID
/// anyway.
pub fn source_issues_hash(issues: &[Issue], tombstone_ids: &[String]) -> String {
    let mut all: Vec<Issue> = issues.to_vec();
    let seen: std::collections::HashSet<String> = all.iter().map(|i| i.id.clone()).collect();
    for id in tombstone_ids {
        if !seen.contains(id) {
            all.push(Issue {
                id: id.clone(),
                content_hash: String::new(),
                title: String::new(),
                description: String::new(),
                design: String::new(),
                acceptance_criteria: String::new(),
                notes: String::new(),
                status: crate::model::Status::Tombstone,
                priority: 0,
                issue_type: String::new(),
                assignee: String::new(),
                estimated_minutes: None,
                created_at: None,
                updated_at: None,
                due_date: None,
                defer_until: None,
                closed_at: None,
                external_ref: None,
                compaction_level: 0,
                compacted_at: None,
                compacted_at_commit: None,
                original_size: 0,
                labels: vec![],
                dependencies: vec![],
                comments: vec![],
                source_repo: String::new(),
            });
        }
    }
    compute_data_hash(&all)
}

/// [`compute_data_hash`] over borrowed issues.
///
/// Byte-for-byte identical output: the encoder below only ever reads through
/// the `&Issue`, and `position` — the only input-order-sensitive value — is
/// the slice index, which a `&[&Issue]` in the same order reproduces
/// exactly. The TUI keeps its issues in a `HashMap<String, Issue>` and was
/// deep-cloning the whole map (megabytes of prose) just to throw the copy
/// away after hashing it.
pub fn compute_data_hash_refs(issues: &[&Issue]) -> String {
    if issues.is_empty() {
        return "empty".to_string();
    }

    struct OrderedFingerprint {
        id: String,
        content_hash: String,
        dependency_hash: String,
        position: usize,
    }
    let mut w = FingerprintWriter::new();
    let mut fingerprints: Vec<OrderedFingerprint> = issues
        .iter()
        .enumerate()
        .map(|(i, issue)| OrderedFingerprint {
            id: issue.id.clone(),
            content_hash: compute_issue_content_hash(&mut w, issue),
            dependency_hash: compute_issue_dependency_hash(&mut w, issue),
            position: i,
        })
        .collect();
    fingerprints.sort_by(|a, b| {
        if a.id != b.id {
            a.id.cmp(&b.id)
        } else {
            a.position.cmp(&b.position)
        }
    });

    w.reset();
    w.write_uint_hash(fingerprints.len() as u64);
    for fp in &fingerprints {
        w.write_string_hash(&fp.id);
        w.write_string_hash(&fp.content_hash);
        w.write_string_hash(&fp.dependency_hash);
    }
    w.sum_hex()
}

/// v0.20.0 data hash — retained so the historical goldens stay reproducible and
/// so drift against the old oracle remains testable. Not used on the CLI path.
#[deprecated(note = "v0.20.0 encoding; use compute_data_hash for the v0.25.0 oracle")]
pub fn compute_data_hash_v020(issues: &[Issue]) -> String {
    if issues.is_empty() {
        return "empty".to_string();
    }

    let mut sorted: Vec<&Issue> = issues.iter().collect();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));

    let mut h = Sha256::new();
    for issue in sorted {
        let hi = match HashIssue::from_issue(issue) {
            Some(x) => x,
            None => continue,
        };
        write_field(&mut h, hi.id.as_bytes());
        write_field(&mut h, hi.title.as_bytes());
        write_field(&mut h, hi.description.as_bytes());
        write_field(&mut h, hi.notes.as_bytes());
        write_field(&mut h, hi.design.as_bytes());
        write_field(&mut h, hi.acceptance_criteria.as_bytes());
        write_field(&mut h, hi.assignee.as_bytes());
        write_field(&mut h, hi.source_repo.as_bytes());
        if let Some(ext) = hi.external_ref {
            h.update(ext.as_bytes());
        }
        h.update([0u8]);

        write_field(&mut h, hi.status.as_bytes());
        write_field(&mut h, hi.issue_type.as_bytes());

        write_field(&mut h, hi.priority.to_string().as_bytes());
        if let Some(est) = hi.estimated_minutes {
            h.update(est.to_string().as_bytes());
        }
        h.update([0u8]);
        // Created/Updated are non-pointer time.Time in Go: ALWAYS written
        // (zero time when absent). ClosedAt is a POINTER: written only when set.
        let c = hi.created_at.as_deref().unwrap_or(GO_ZERO_TIME);
        h.update(c.as_bytes());
        h.update([0u8]);
        let u = hi.updated_at.as_deref().unwrap_or(GO_ZERO_TIME);
        h.update(u.as_bytes());
        h.update([0u8]);
        if let Some(cl) = &hi.closed_at {
            h.update(cl.as_bytes());
        }
        h.update([0u8]);

        for lbl in &hi.labels {
            h.update(lbl.as_bytes());
            h.update([0u8]);
        }
        h.update([0u8]); // end-of-labels separator

        for d in &hi.deps {
            write_field(&mut h, d.depends_on.as_bytes());
            write_field(&mut h, d.dep_type.as_bytes());
            write_field(&mut h, d.created_at.as_bytes());
            write_field(&mut h, d.created_by.as_bytes());
        }
        h.update([0u8]); // end-of-deps separator

        for c in &hi.comments {
            write_field(&mut h, c.id.as_bytes());
            write_field(&mut h, c.author.as_bytes());
            write_field(&mut h, c.text.as_bytes());
            write_field(&mut h, c.created_at.as_bytes());
        }
        // NOTE: Go has NO comments-end separator (verified byte-stream vs
        // instrumented upstream). Comments flow straight into {1}.

        h.update([1u8]); // issue separator
    }

    let digest = h.finalize();
    hex_encode_16(&digest)
}

fn hex_encode_16(digest: &[u8]) -> String {
    let mut out = String::with_capacity(16);
    for b in digest.iter().take(8) {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Dependency, DependencyType, Issue, Status};

    fn issue(id: &str) -> Issue {
        Issue {
            id: id.into(),
            content_hash: String::new(),
            title: format!("Title {}", id),
            description: String::new(),
            design: String::new(),
            acceptance_criteria: String::new(),
            notes: String::new(),
            status: Status::Open,
            priority: 2,
            issue_type: "task".into(),
            assignee: String::new(),
            estimated_minutes: None,
            created_at: Some("2026-01-01T00:00:00Z".into()),
            updated_at: Some("2026-01-01T01:00:00Z".into()),
            due_date: None,
            defer_until: None,
            closed_at: None,
            external_ref: None,
            compaction_level: 0,
            compacted_at: None,
            compacted_at_commit: None,
            original_size: 0,
            labels: vec![],
            dependencies: vec![],
            comments: vec![],
            source_repo: String::new(),
        }
    }

    #[test]
    fn empty_input_yields_empty_sentinel() {
        assert_eq!(compute_data_hash(&[]), "empty");
        assert_eq!(compute_data_hash_refs(&[]), "empty");
    }

    /// The borrowed entry point exists so callers holding a `HashMap` of
    /// issues can hash without deep-cloning first. It is only safe because
    /// it feeds the encoder exactly the same bytes in exactly the same order;
    /// this pins that, including the duplicate-ID encounter-order tiebreak
    /// that is the one place slice order can reach the digest.
    #[test]
    fn borrowed_variant_matches_the_owned_one() {
        let issues: Vec<Issue> = ["B", "A", "C", "A"].into_iter().map(issue).collect();
        let borrowed: Vec<&Issue> = issues.iter().collect();
        assert_eq!(
            compute_data_hash(&issues),
            compute_data_hash_refs(&borrowed)
        );

        let mut dupes = vec![issue("A"), issue("A"), issue("A")];
        dupes[0].title = "first".into();
        dupes[1].title = "second".into();
        dupes[2].title = "third".into();
        let borrowed_dupes: Vec<&Issue> = dupes.iter().collect();
        assert_eq!(
            compute_data_hash(&dupes),
            compute_data_hash_refs(&borrowed_dupes),
            "duplicate IDs must still be ordered by encounter position"
        );
    }

    #[test]
    fn order_independent() {
        let a = issue("A");
        let b = issue("B");
        let h1 = compute_data_hash(&[a.clone(), b.clone()]);
        let h2 = compute_data_hash(&[b, a]);
        assert_eq!(h1, h2);
        // v0.25.0 returns the full SHA-256 digest as 64 lowercase hex chars
        // (`fmt.Sprintf("%x", sha256.Sum256(raw))`). v0.20.0 emitted a
        // 16-char truncation, so this assertion changed with the algorithm.
        assert_eq!(h1.len(), 64);
        assert!(h1
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn content_change_changes_hash() {
        let mut a = issue("A");
        let h1 = compute_data_hash(std::slice::from_ref(&a));
        a.title = "Changed".into();
        let h2 = compute_data_hash(std::slice::from_ref(&a));
        assert_ne!(h1, h2);
    }

    #[test]
    fn dependency_affects_hash() {
        let mut a = issue("A");
        let h1 = compute_data_hash(std::slice::from_ref(&a));
        a.dependencies.push(Dependency {
            issue_id: "A".into(),
            depends_on_id: "B".into(),
            depends_on_legacy: String::new(),
            target_id_legacy: String::new(),
            r#type: DependencyType::Blocks,
            created_at: Some("2026-01-01T00:00:00Z".into()),
            created_by: "test".into(),
        });
        let h2 = compute_data_hash(std::slice::from_ref(&a));
        assert_ne!(h1, h2);
    }
}
