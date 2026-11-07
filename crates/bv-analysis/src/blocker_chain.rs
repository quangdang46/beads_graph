//! Blocker-chain analysis — port of Go `pkg/analysis/graph.go`
//! `Analyzer.GetBlockerChain` + `pkg/analysis/triage_context.go`
//! `OpenBlockers`/`IsActionable` (backing `robot-blocker-chain`).

use bv_core::model::{Issue, Status};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

/// Go `closedForReadiness` (readiness.go:79) — `closed` + `tombstone`, NOT
/// the narrower `Issue.IsClosed()` (which is `closed` only). A tombstoned
/// blocker is satisfied work: it must not count as an open blocker anywhere
/// in this module. The previous `is_closed()` checks let a tombstone keep
/// gating its dependents.
fn closed_for_readiness(status: Status) -> bool {
    matches!(status, Status::Closed | Status::Tombstone)
}

/// Direct-predecessor open blockers for `id`, plus transitive parent-child
/// propagation: an open parent-child parent counts as a blocker of its
/// child when the parent is itself (transitively) blocked.
///
/// Go parity: `TriageContext.OpenBlockers`/`getOpenBlockersInternal`
/// (triage_context.go:194-228) delegates to `ReadinessIndex.Blockers`
/// (readiness.go:199-216), so this function does too. That authority lists a
/// blocking dependency whose target is ABSENT from the source (state
/// `Unknown` withholds readiness) and a parent-child parent whose state is
/// not `Satisfied` — including missing parents and parent cycles — where the
/// previous hand-rolled walk silently dropped both.
pub fn open_blockers(by_id: &HashMap<&str, &Issue>, id: &str) -> Vec<String> {
    crate::triage::Readiness::new_borrowed(by_id.values().copied()).blockers(id)
}

/// Same as [`open_blockers`] but against a prebuilt index — for callers
/// walking many issues so the O(n) index build happens once, not per call.
pub(crate) fn open_blockers_in(readiness: &crate::triage::Readiness<'_>, id: &str) -> Vec<String> {
    readiness.blockers(id)
}

/// True when the issue is non-closed, non-deferred, and has no open
/// blockers (direct or parent-propagated). Approximates Go's `IsActionable`
/// (Go additionally checks scheduler deferral, which has no Rust model
/// equivalent beyond `Status::Deferred`).
pub fn is_actionable(by_id: &HashMap<&str, &Issue>, id: &str) -> bool {
    let readiness = crate::triage::Readiness::new_borrowed(by_id.values().copied());
    is_actionable_in(&readiness, by_id, id)
}

/// Same as [`is_actionable`] against a prebuilt index — see
/// [`open_blockers_in`].
pub(crate) fn is_actionable_in(
    readiness: &crate::triage::Readiness<'_>,
    by_id: &HashMap<&str, &Issue>,
    id: &str,
) -> bool {
    let Some(issue) = by_id.get(id) else {
        return false;
    };
    if closed_for_readiness(issue.status) || issue.status == Status::Deferred {
        return false;
    }
    readiness.blockers(id).is_empty()
}

fn count_blocked_by(by_id: &HashMap<&str, &Issue>, issue_id: &str) -> i64 {
    let mut count = 0i64;
    for issue in by_id.values() {
        if closed_for_readiness(issue.status) {
            continue;
        }
        for dep in &issue.dependencies {
            if dep.r#type.is_blocking() && dep.effective_depends_on() == issue_id {
                count += 1;
                break;
            }
        }
    }
    count
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct BlockerChainEntry {
    // NOTE: Clone is used when pushing an entry into both `chain` and
    // `root_blockers`.
    pub id: String,
    pub title: String,
    pub status: Status,
    pub priority: i64,
    pub depth: i64,
    pub is_root: bool,
    pub actionable: bool,
    pub blocks_count: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct BlockerChainResult {
    pub target_id: String,
    pub target_title: String,
    pub is_blocked: bool,
    pub chain_length: i64,
    pub root_blockers: Vec<BlockerChainEntry>,
    pub chain: Vec<BlockerChainEntry>,
    pub has_cycle: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cycle_ids: Vec<String>,
}

/// Returns `None` when `issue_id` doesn't exist (Go returns `nil`).
pub fn get_blocker_chain(issues: &[Issue], issue_id: &str) -> Option<BlockerChainResult> {
    let by_id: HashMap<&str, &Issue> = issues.iter().map(|i| (i.id.as_str(), i)).collect();
    let issue = *by_id.get(issue_id)?;
    // One readiness index for the whole walk — Go's TriageContext caches
    // OpenBlockers/IsActionable the same way (triage_context.go:37-53).
    let readiness = crate::triage::Readiness::new(issues);

    let mut result = BlockerChainResult {
        target_id: issue_id.to_string(),
        target_title: issue.title.clone(),
        is_blocked: false,
        chain_length: 0,
        root_blockers: Vec::new(),
        chain: Vec::new(),
        has_cycle: false,
        cycle_ids: Vec::new(),
    };

    let target_open_blockers = open_blockers_in(&readiness, issue_id);
    let mut target_entry = BlockerChainEntry {
        id: issue_id.to_string(),
        title: issue.title.clone(),
        status: issue.status,
        priority: issue.priority,
        depth: 0,
        is_root: false,
        actionable: is_actionable_in(&readiness, &by_id, issue_id),
        blocks_count: count_blocked_by(&by_id, issue_id),
    };

    if target_open_blockers.is_empty() {
        target_entry.is_root = true;
        result.chain.push(target_entry);
        return Some(result);
    }
    result.is_blocked = true;
    result.chain.push(target_entry);

    /// Mutable walk state bundled so `dfs` stays under the argument limit.
    struct Walk<'a> {
        visited: HashSet<String>,
        visiting: HashSet<String>,
        result: &'a mut BlockerChainResult,
    }

    /// Bound on `dfs` recursion. Go's goroutine stacks grow dynamically so the
    /// upstream walk has no explicit cap; Rust runs on fixed-size thread
    /// stacks, and an adversarially deep blocker chain would overflow before
    /// finishing. 4096 levels is far beyond any realistic chain; past it the
    /// walk simply stops descending (the chain shown is truncated, never
    /// wrong about the nodes it did visit).
    const DFS_MAX_DEPTH: i64 = 4096;

    fn dfs<'r>(
        readiness: &crate::triage::Readiness<'r>,
        by_id: &HashMap<&str, &Issue>,
        id: &str,
        depth: i64,
        target_id: &str,
        walk: &mut Walk<'_>,
    ) {
        if depth > DFS_MAX_DEPTH {
            return;
        }
        if walk.visiting.contains(id) {
            walk.result.has_cycle = true;
            walk.result.cycle_ids.push(id.to_string());
            return;
        }
        if walk.visited.contains(id) {
            return;
        }
        walk.visiting.insert(id.to_string());

        let Some(blocker) = by_id.get(id) else {
            walk.visiting.remove(id);
            walk.visited.insert(id.to_string());
            return;
        };

        let blocker_open = open_blockers_in(readiness, id);
        let is_root = blocker_open.is_empty();

        if id != target_id {
            let entry = BlockerChainEntry {
                id: id.to_string(),
                title: blocker.title.clone(),
                status: blocker.status,
                priority: blocker.priority,
                depth,
                is_root,
                actionable: is_actionable_in(readiness, by_id, id),
                blocks_count: count_blocked_by(by_id, id),
            };
            walk.result.chain.push(entry.clone());
            if is_root {
                walk.result.root_blockers.push(entry);
            }
        }

        for next_id in &blocker_open {
            dfs(readiness, by_id, next_id, depth + 1, target_id, walk);
        }

        walk.visiting.remove(id);
        walk.visited.insert(id.to_string());
    }

    dfs(
        &readiness,
        &by_id,
        issue_id,
        0,
        issue_id,
        &mut Walk {
            visited: HashSet::new(),
            visiting: HashSet::new(),
            result: &mut result,
        },
    );

    result.chain_length = result.chain.len() as i64 - 1;
    result
        .root_blockers
        .sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.id.cmp(&b.id)));

    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bv_core::model::{Dependency, DependencyType};

    fn issue(id: &str, status: Status, title: &str) -> Issue {
        Issue {
            id: id.to_string(),
            content_hash: String::new(),
            title: title.to_string(),
            description: String::new(),
            design: String::new(),
            acceptance_criteria: String::new(),
            notes: String::new(),
            status,
            priority: 2,
            issue_type: "task".into(),
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
        }
    }

    fn blocks(issue_id: &str, depends_on_id: &str) -> Dependency {
        Dependency {
            issue_id: issue_id.to_string(),
            depends_on_id: depends_on_id.to_string(),
            depends_on_legacy: String::new(),
            target_id_legacy: String::new(),
            r#type: DependencyType::Blocks,
            created_at: None,
            created_by: String::new(),
        }
    }

    fn parent_child(issue_id: &str, depends_on_id: &str) -> Dependency {
        Dependency {
            issue_id: issue_id.to_string(),
            depends_on_id: depends_on_id.to_string(),
            depends_on_legacy: String::new(),
            target_id_legacy: String::new(),
            r#type: DependencyType::ParentChild,
            created_at: None,
            created_by: String::new(),
        }
    }

    fn by_id_map(issues: &[Issue]) -> HashMap<&str, &Issue> {
        issues.iter().map(|i| (i.id.as_str(), i)).collect()
    }

    #[test]
    fn unblocked_issue_is_its_own_root() {
        let issues = vec![issue("A-1", Status::Open, "solo")];
        let result = get_blocker_chain(&issues, "A-1").unwrap();
        assert!(!result.is_blocked);
        assert_eq!(result.chain.len(), 1);
        assert!(result.chain[0].is_root);
    }

    #[test]
    fn missing_issue_returns_none() {
        let issues = vec![issue("A-1", Status::Open, "solo")];
        assert!(get_blocker_chain(&issues, "nope").is_none());
    }

    #[test]
    fn simple_chain_finds_root_blocker() {
        let mut target = issue("A-2", Status::Open, "target");
        target.dependencies.push(blocks("A-2", "A-1"));
        let root = issue("A-1", Status::Open, "root");
        let issues = vec![root, target];
        let result = get_blocker_chain(&issues, "A-2").unwrap();
        assert!(result.is_blocked);
        assert_eq!(result.chain_length, 1);
        assert_eq!(result.root_blockers.len(), 1);
        assert_eq!(result.root_blockers[0].id, "A-1");
    }

    #[test]
    fn closed_blocker_does_not_block() {
        let mut target = issue("A-2", Status::Open, "target");
        target.dependencies.push(blocks("A-2", "A-1"));
        let root = issue("A-1", Status::Closed, "root");
        let issues = vec![root, target];
        let result = get_blocker_chain(&issues, "A-2").unwrap();
        assert!(
            !result.is_blocked,
            "closed blocker must not count as blocking"
        );
    }

    /// Go `closedForReadiness` (readiness.go:79) is `closed` + `tombstone`,
    /// not `IsClosed()` — a tombstoned blocker is satisfied work and must not
    /// gate its dependents.
    #[test]
    fn tombstone_blocker_does_not_block() {
        let mut target = issue("A-2", Status::Open, "target");
        target.dependencies.push(blocks("A-2", "A-1"));
        let root = issue("A-1", Status::Tombstone, "tombstoned");
        let issues = vec![root, target];
        let result = get_blocker_chain(&issues, "A-2").unwrap();
        assert!(
            !result.is_blocked,
            "tombstone blocker must not count as blocking"
        );
        let by_id = by_id_map(&issues);
        assert!(open_blockers(&by_id, "A-2").is_empty());
        assert!(is_actionable(&by_id, "A-2"));
    }

    /// The tombstone rule applies inside the transitive parent-child walk too:
    /// an open parent whose only blocker is a tombstone is itself unblocked,
    /// so it must not gate its child.
    #[test]
    fn tombstone_grandparent_blocker_does_not_gate_child() {
        let mut parent = issue("EP-1", Status::Open, "parent");
        parent.dependencies.push(blocks("EP-1", "EP-0"));
        let grandparent = issue("EP-0", Status::Tombstone, "tombstoned");
        let mut child = issue("TASK-1", Status::Open, "child");
        child.dependencies.push(parent_child("TASK-1", "EP-1"));
        let issues = vec![grandparent, parent, child];
        let by_id = by_id_map(&issues);
        assert!(
            open_blockers(&by_id, "TASK-1").is_empty(),
            "tombstoned grandparent resolves the whole chain"
        );
        assert!(is_actionable(&by_id, "TASK-1"));
    }

    /// Issue #2: a task with only a parent-child edge to a blocked epic
    /// inherits the ancestor's blocked state (br ready/blocked parity).
    #[test]
    fn child_inherits_blocked_ancestor_epic() {
        // EP1 blocked on open EP0; TASK-1 has only a parent-child link to EP1.
        let ep0 = issue("EP0", Status::Open, "phase 0");
        let mut ep1 = issue("EP1", Status::Open, "phase 1");
        ep1.dependencies.push(blocks("EP1", "EP0"));
        let mut task = issue("TASK-1", Status::Open, "task under ep1");
        task.dependencies.push(parent_child("TASK-1", "EP1"));
        let issues = vec![ep0, ep1, task];
        let by_id = by_id_map(&issues);
        assert_eq!(open_blockers(&by_id, "TASK-1"), vec!["EP1".to_string()]);
        assert!(!is_actionable(&by_id, "TASK-1"));
        assert!(is_actionable(&by_id, "EP0"));
        assert!(!crate::triage::compute_blocked_set(&issues).is_disjoint(
            &["TASK-1".to_string(), "EP1".to_string()]
                .into_iter()
                .collect()
        ));
    }

    /// Issue #2: an open, unblocked parent does NOT gate its child.
    #[test]
    fn child_of_open_unblocked_parent_stays_actionable() {
        let ep0 = issue("EP0", Status::Open, "phase 0");
        let mut task = issue("TASK-0", Status::Open, "task under ep0");
        task.dependencies.push(parent_child("TASK-0", "EP0"));
        let issues = vec![ep0, task];
        let by_id = by_id_map(&issues);
        assert!(open_blockers(&by_id, "TASK-0").is_empty());
        assert!(is_actionable(&by_id, "TASK-0"));
        assert!(!crate::triage::compute_blocked_set(&issues).contains("TASK-0"));
    }

    #[test]
    fn cycle_is_detected_without_infinite_loop() {
        let mut a = issue("A-1", Status::Open, "a");
        a.dependencies.push(blocks("A-1", "A-2"));
        let mut b = issue("A-2", Status::Open, "b");
        b.dependencies.push(blocks("A-2", "A-1"));
        let issues = vec![a, b];
        let result = get_blocker_chain(&issues, "A-1").unwrap();
        assert!(result.has_cycle);
    }

    /// Go `ReadinessIndex.Blockers` (readiness.go:199-216) lists a blocking
    /// dependency whose target is absent from the source — `Unknown` withholds
    /// readiness — and `TriageContext.OpenBlockers` surfaces it verbatim. The
    /// previous hand-rolled walk dropped the edge entirely, reporting the
    /// issue unblocked.
    #[test]
    fn missing_blocker_target_still_blocks() {
        let mut task = issue("TASK-1", Status::Open, "task");
        task.dependencies.push(blocks("TASK-1", "GHOST-9"));
        let issues = vec![task];
        let by_id = by_id_map(&issues);
        assert_eq!(open_blockers(&by_id, "TASK-1"), vec!["GHOST-9".to_string()]);
        assert!(!is_actionable(&by_id, "TASK-1"));
        // The chain walk visits the absent ID and skips it, exactly like Go's
        // `blocker, exists := a.issueMap[id]; if !exists { ... return }`.
        let result = get_blocker_chain(&issues, "TASK-1").unwrap();
        assert!(result.is_blocked);
        // The absent ID contributes no chain entry (Go appends entries only
        // for IDs present in issueMap), so ChainLength stays 0 — but the
        // target is still reported blocked.
        assert_eq!(result.chain_length, 0);
        assert!(result.root_blockers.is_empty());
    }

    /// Same authority for parent-child: a parent ID the index never saw has
    /// state `Unknown` (!= Satisfied), so Go lists it as a blocker.
    #[test]
    fn missing_parent_target_still_blocks() {
        let mut task = issue("TASK-2", Status::Open, "task");
        task.dependencies.push(parent_child("TASK-2", "GHOST-P"));
        let issues = vec![task];
        let by_id = by_id_map(&issues);
        assert_eq!(open_blockers(&by_id, "TASK-2"), vec!["GHOST-P".to_string()]);
        assert!(!is_actionable(&by_id, "TASK-2"));
    }
}
