//! Triage payload assembly — port of Go triage output shapes:
//! quick_ref (#165 strict semantics), project_health, commands.

use crate::impact::{compute_impact_scores, ImpactInputs};
use bv_core::model::{Issue, Status};
use bv_graph_core::DiGraph;
use serde::Serialize;
use std::collections::BTreeMap;

// === Triage scoring weights (Go triage.go:1203-1208) — DO NOT ALTER ===
/// Base score weight applied to the impact score.
pub const TRIAGE_BASE_WEIGHT: f64 = 0.70;
/// Maximum boost for unblock count.
pub const TRIAGE_UNBLOCK_BOOST: f64 = 0.15;
/// Maximum boost for quick-win potential.
pub const TRIAGE_QUICKWIN_BOOST: f64 = 0.15;
/// Minimum unblocks required for full unblock boost (normalization floor).
pub const TRIAGE_UNBLOCK_THRESHOLD: usize = 5;
/// Maximum blocker depth eligible for quick-win boost.
pub const TRIAGE_QUICKWIN_MAX_DEPTH: usize = 2;

/// Strict count semantics from #165.
#[derive(Debug, Default, Serialize)]
pub struct QuickRef {
    /// status exactly "open"
    pub open_count: usize,
    /// non-closed with zero open blocking dependencies
    pub actionable_count: usize,
    /// status exactly "blocked"
    pub blocked_count: usize,
    pub in_progress_count: usize,
    /// every non-closed issue
    pub not_closed_count: usize,
    /// non-closed blocked by open dependencies
    pub not_actionable_count: usize,
}

impl QuickRef {
    /// Partition invariant: not_closed == actionable + not_actionable.
    pub fn validate_invariant(&self) -> bool {
        self.not_closed_count == self.actionable_count + self.not_actionable_count
    }
}

#[derive(Debug, Default, Serialize)]
pub struct ProjectCounts {
    pub total: usize,
    pub open: usize,
    pub closed: usize,
    pub blocked: usize,
    pub actionable: usize,
    pub not_closed: usize,
    pub dependency_blocked: usize,
    /// Go golden key `dependency_blocked` alias for quick_ref.not_actionable.
    pub not_actionable_count: usize,
    pub by_status: BTreeMap<String, usize>,
    pub by_type: BTreeMap<String, usize>,
    #[serde(rename = "by_priority")]
    pub by_priority: BTreeMap<String, usize>,
}

pub fn compute_counts(
    issues: &[Issue],
    readiness: &Readiness<'_>,
    now: jiff::Timestamp,
) -> (ProjectCounts, QuickRef) {
    let mut c = ProjectCounts::default();
    let mut qr = QuickRef::default();
    c.total = issues.len();
    for i in issues {
        *c.by_status
            .entry(i.status.as_str().to_string())
            .or_default() += 1;
        *c.by_type.entry(i.issue_type.clone()).or_default() += 1;
        *c.by_priority.entry(i.priority.to_string()).or_default() += 1;

        if matches!(i.status, Status::Closed | Status::Tombstone) {
            c.closed += 1;
            continue;
        }
        // non-closed from here on
        c.not_closed += 1;

        // Go `ReadinessIndex.Ready` (readiness.go:175-179), reached through
        // `getActionableIssuesAfterCompletions`:
        //
        //     exists && isActionableStatus(status)
        //           && !isDeferredAt(now) && DependencyState == Satisfied
        //
        // `dependency_blocked` is the *complement within the non-closed set*,
        // not membership of a blocked set (triage.go:850-860). Counting
        // readiness-blocked ids instead made the two unrelated quantities: a
        // deferred or parked issue was in neither bucket, so
        // `not_closed == actionable + not_actionable` stopped being checkable.
        let ready = i.status.is_open()
            && !readiness.is_deferred_at(i.id.as_str(), now)
            && readiness.dependency_state(i.id.as_str()) == DepState::Satisfied;
        if ready {
            c.actionable += 1;
        } else {
            c.dependency_blocked += 1;
        }
    }
    // Strict status counts (Go triage.go:865-867, issue #165): Open and Blocked
    // are read back out of ByStatus rather than tallied in the loop, so
    // counts.open == by_status.open holds by construction.
    c.open = c.by_status.get("open").copied().unwrap_or(0);
    c.blocked = c.by_status.get("blocked").copied().unwrap_or(0);

    qr.open_count = c.open;
    qr.blocked_count = c.blocked;
    // Go `InProgressCount: counts.ByStatus["in_progress"]` (triage.go:722). The
    // field was declared on QuickRef and never assigned, so it reported 0 for
    // every repository — including one with work in flight.
    qr.in_progress_count = c.by_status.get("in_progress").copied().unwrap_or(0);
    qr.actionable_count = c.actionable;
    qr.not_closed_count = c.not_closed;
    // Go `NotActionableCount: counts.DependencyBlocked` (triage.go:724).
    qr.not_actionable_count = c.dependency_blocked;
    c.not_actionable_count = c.dependency_blocked;
    (c, qr)
}

/// Per-row triage decoration for the TUI list (Go `IssueItem` triage fields:
/// `IsQuickWin`, `IsBlocker`, `UnblocksCount` — populated in Go from
/// `triageResult.QuickWins` / `BlockersToClear` / `unblocksMap`).
#[derive(Debug, Clone, Default)]
pub struct RowTriage {
    pub is_quick_win: bool,
    pub is_blocker: bool,
    pub unblocks_count: usize,
}

/// Compute per-row triage decorations for every issue, in one pass.
///
/// Semantics (Go `buildUnblocksMap` + `buildQuickWins` + `buildBlockersToClear`):
/// - `unblocks_count[id]` = number of non-closed-like issues B for which `id`
///   is the ONLY remaining open blocker (direct blocking edge or
///   parent-propagated), and B would be actionable once `id` completes.
/// - `is_blocker` = appears in the blockers-to-clear set: any non-closed-like
///   issue with >= 1 unblock, sorted by unblock count desc, id asc. The TUI
///   keeps the full set (not Go's display `limit`) since it's a per-row flag,
///   not a top-N list.
/// - `is_quick_win` = claimable (open, non-epic, unassigned, zero open
///   blockers — Go's `isClaimableRecommendation` minus scheduler-deferral and
///   not-ready-labels, neither of which the TUI models) AND in the top 5 of
///   the quick-win ranking (`log2(unblocks+1)*0.4 + simplicity*0.4 +
///   priority_bonus*0.2`, score desc, id asc; Go `buildQuickWins` with
///   `BlokerRatioNorm` approximated by the raw blocker ratio — the normalized
///   value isn't available without the full impact-scoring pipeline).
///
/// Shared by the TUI row renderer and (future) CLI callers so the two never
/// drift apart — `crates/bv/src/main.rs`'s inline `quick_wins` /
/// `blockers_to_clear` builders are the golden-covered legacy copies and are
/// intentionally left untouched.
pub fn compute_row_triage(issues: &[Issue]) -> std::collections::HashMap<String, RowTriage> {
    use std::collections::{HashMap, HashSet};
    fn closed_like(s: Status) -> bool {
        matches!(s, Status::Closed | Status::Tombstone)
    }

    let by_id: HashMap<&str, &Issue> = issues.iter().map(|i| (i.id.as_str(), i)).collect();

    // Open blockers per issue (direct blocking edges + parent propagation),
    // via the shared blocker_chain helper (Go `getOpenBlockersInternal`).
    let open_blockers_of =
        |id: &str| -> Vec<String> { crate::blocker_chain::open_blockers(&by_id, id) };

    // Go `buildUnblocksMap`: B counts toward A iff A is B's ONLY open blocker
    // and B is actionable-after-completing-A (approximated here as:
    // non-closed-like and non-deferred — scheduler deferral has no Rust model
    // beyond `Status::Deferred`).
    let mut unblocks: HashMap<String, Vec<String>> = HashMap::new();
    for issue in issues {
        unblocks.entry(issue.id.clone()).or_default();
        if closed_like(issue.status) || issue.status == Status::Deferred {
            continue;
        }
        let blockers = open_blockers_of(&issue.id);
        if blockers.len() == 1 {
            unblocks
                .entry(blockers[0].clone())
                .or_default()
                .push(issue.id.clone());
        }
    }
    for list in unblocks.values_mut() {
        list.sort();
    }

    // Go `buildQuickWins` ranking over the claimable subset.
    let blocker_ratio_of = |id: &str| -> f64 {
        let Some(issue) = by_id.get(id) else {
            return 0.0;
        };
        let total = issues.len().max(1) as f64;
        let blocked_by_count = issues
            .iter()
            .filter(|o| {
                !closed_like(o.status)
                    && o.dependencies
                        .iter()
                        .any(|d| d.r#type.is_blocking() && d.effective_depends_on() == id)
            })
            .count() as f64;
        let _ = issue;
        blocked_by_count / total
    };
    let mut qw_candidates: Vec<(f64, &str)> = Vec::new();
    for issue in issues {
        // Claimable subset (Go `isClaimableRecommendation`, TUI-applicable
        // parts): open, non-epic, unassigned, zero open blockers.
        if issue.status != Status::Open {
            continue;
        }
        if issue.issue_type == "epic" {
            continue;
        }
        if !issue.assignee.is_empty() {
            continue;
        }
        if !open_blockers_of(&issue.id).is_empty() {
            continue;
        }
        let unblocks_count = unblocks.get(&issue.id).map(Vec::len).unwrap_or(0);
        // Go's `math.Log2`, not `f64::log2` — see `crate::go_math`. Only the
        // ranking depends on it here (`is_quick_win` is a bool), but the sort
        // has to be the same sort Go performs or a 1-ULP tie can swap two
        // rows and put the quick-win mark on the wrong issue.
        let unblock_impact = crate::go_math::go_log2((unblocks_count as f64) + 1.0);
        let ratio = blocker_ratio_of(&issue.id);
        let simplicity = if ratio < 0.2 {
            1.0
        } else if ratio < 0.4 {
            0.5
        } else {
            0.0
        };
        let priority_bonus = if issue.priority <= 1 { 0.5 } else { 0.0 };
        let qw_score = unblock_impact * 0.4 + simplicity * 0.4 + priority_bonus * 0.2;
        qw_candidates.push((qw_score, issue.id.as_str()));
    }
    qw_candidates.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.cmp(b.1))
    });
    let quick_win_set: HashSet<&str> = qw_candidates.iter().take(5).map(|(_, id)| *id).collect();

    let mut out: HashMap<String, RowTriage> = HashMap::new();
    for issue in issues {
        let unblocks_count = unblocks.get(&issue.id).map(Vec::len).unwrap_or(0);
        let is_blocker = !closed_like(issue.status) && unblocks_count > 0;
        out.insert(
            issue.id.clone(),
            RowTriage {
                is_quick_win: quick_win_set.contains(issue.id.as_str()),
                is_blocker,
                unblocks_count,
            },
        );
    }
    out
}

// === Readiness authority — port of Go `pkg/model/readiness.go` ===
//
// Go's `model.ReadinessIndex` is the ONE dependency authority behind every
// triage surface that asks "can this be worked on":
//
//   * `computeCountsWithContext` (triage.go:888) -> `ctx.IsActionable` ->
//     `getActionableIssuesAfterCompletions(nil)` -> `Readiness().Ready`
//   * `buildUnblocksMap` (triage.go:811) -> `getOpenBlockersInternal` ->
//     `Readiness().Blockers`
//   * `buildUnblocksMap`'s readiness gate (triage.go:819) ->
//     `isActionableAfterCompletions` -> `Readiness().ReadyAfter`
//
// Porting it once, here, keeps all three answering identically. The previous
// Rust code answered the first two with two independent approximations that
// disagreed with each other and with Go.

/// Go `model.DependencyState` (readiness.go:14-17).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DepState {
    Satisfied,
    Unsatisfied,
    Unknown,
}

/// Go `combineDependencyState` (readiness.go:99-107). `Unknown` dominates
/// `Unsatisfied` dominates `Satisfied` — the join is commutative and
/// associative, which is why Go's map-order queue below is deterministic.
fn combine(a: DepState, b: DepState) -> DepState {
    if a == DepState::Unknown || b == DepState::Unknown {
        DepState::Unknown
    } else if a == DepState::Unsatisfied || b == DepState::Unsatisfied {
        DepState::Unsatisfied
    } else {
        DepState::Satisfied
    }
}

/// Go `closedForReadiness` (readiness.go:79) — NOTE: this is `closed` +
/// `tombstone`, not Go's narrower `Issue.IsClosed()`.
fn closed_for_readiness(status: Status) -> bool {
    matches!(status, Status::Closed | Status::Tombstone)
}

/// Go `computeUnblocks` (plan.go:86-113) — the issues that become *ready* once
/// `id` lands, sorted.
///
/// This is not "everything that depends on `id`". Go walks the graph's
/// successors and keeps only those passing `isActionableAfterCompletions`,
/// which requires the dependent to have no unresolved blocker left once `id`
/// is complete (plan.go:105-108). A dependent that is still gated on something
/// else, or is itself closed or deferred, is not unblocked by this issue and
/// must not be counted. Go returns nil outright when the blocker is already
/// closed (plan.go:93-97): completing it changes no readiness.
///
/// Go's `IsCandidate` is the output-eligibility scope, and the CLI only sets a
/// candidate set for `--repo`/`--label-scope`, so an unscoped run accepts every
/// loaded issue and the test is omitted here.
pub fn compute_unblocks(
    readiness: &Readiness<'_>,
    g: &bv_graph_core::DiGraph,
    id: &str,
    now: jiff::Timestamp,
) -> Vec<String> {
    let Some(idx) = g.node_idx(id) else {
        return Vec::new();
    };
    if readiness
        .issues
        .get(id)
        .is_none_or(|i| closed_for_readiness(i.status))
    {
        return Vec::new();
    }
    let completed: std::collections::BTreeSet<String> = std::iter::once(id.to_string()).collect();
    let mut unblocks: Vec<String> = g
        // An edge runs dependent -> dependency (`build_graph`,
        // analyzer.rs:777-780), so the issues waiting on `id` are its
        // in-neighbours — the same set Go reaches through `g.To` on the
        // reversed adjacency it keeps.
        .predecessors_slice(idx)
        .iter()
        .filter_map(|&n| g.node_id(n))
        .filter(|dependent| readiness.ready_after(dependent, now, Some(&completed)))
        .collect();
    unblocks.sort();
    unblocks
}

/// Go `readinessIssue` (readiness.go:32-39) — the decision inputs only.
struct ReadinessIssue<'a> {
    status: Status,
    defer_until: Option<jiff::Timestamp>,
    /// Go's `ReadinessIssue` carries `Assignee` and `IssueType` for one reason:
    /// `Claimable` (readiness.go:181-185) tests them. Nothing else in the
    /// index reads either field.
    assignee: &'a str,
    issue_type: &'a str,
    /// `(depends_on_id, type)`, pre-folded from the legacy field names.
    deps: Vec<(&'a str, bv_core::model::DependencyType)>,
}

impl ReadinessIssue<'_> {
    /// Go `readinessIssue.isDeferredAt` (readiness.go:42-44) —
    /// `DeferUntil != nil && DeferUntil.After(now)`.
    fn is_deferred_at(&self, now: jiff::Timestamp) -> bool {
        self.defer_until.is_some_and(|d| d > now)
    }
}

/// Go `model.ReadinessIndex` (readiness.go:26-30).
pub struct Readiness<'a> {
    issues: std::collections::HashMap<&'a str, ReadinessIssue<'a>>,
    states: std::collections::HashMap<&'a str, DepState>,
    children: std::collections::HashMap<&'a str, Vec<&'a str>>,
}

impl<'a> Readiness<'a> {
    /// Go `NewReadinessIndex` (readiness.go:46-77) followed by `compute()`.
    pub fn new(issues: &'a [Issue]) -> Self {
        let mut r = Readiness {
            issues: std::collections::HashMap::new(),
            states: std::collections::HashMap::new(),
            children: std::collections::HashMap::new(),
        };
        for issue in issues {
            let deps: Vec<(&'a str, bv_core::model::DependencyType)> = issue
                .dependencies
                .iter()
                .map(|d| (d.effective_depends_on(), d.r#type.clone()))
                .collect();
            // readiness.go:114-116 — parent-child is a rollup edge; record
            // the reverse index here so the queue phase can walk children.
            for (target, ty) in &deps {
                if *ty == bv_core::model::DependencyType::ParentChild {
                    r.children
                        .entry(target)
                        .or_default()
                        .push(issue.id.as_str());
                }
            }
            r.issues.insert(
                issue.id.as_str(),
                ReadinessIssue {
                    status: issue.status,
                    defer_until: bv_core::model::parse_ts(&issue.defer_until),
                    assignee: issue.assignee.as_str(),
                    issue_type: issue.issue_type.as_str(),
                    deps,
                },
            );
        }
        r.compute();
        r
    }

    /// Go `ReadinessIndex.HasOpenChildren` (readiness.go:187-194).
    pub fn has_open_children(&self, id: &str) -> bool {
        self.children.get(id).is_some_and(|kids| {
            kids.iter().any(|k| {
                self.issues
                    .get(*k)
                    .is_some_and(|c| !closed_for_readiness(c.status))
            })
        })
    }

    /// Go `ReadinessIndex.Claimable` (readiness.go:181-185):
    ///
    /// ```go
    /// return exists && r.Ready(id, now) && issue.Status == StatusOpen &&
    ///     issue.Assignee == "" && issue.IssueType != TypeEpic && !r.HasOpenChildren(id)
    /// ```
    ///
    /// This is the gate behind the export's "Atomically claim" command block
    /// (`generateIssueCommands`, pkg/export/markdown.go:667-673), which
    /// reports an issue claimable only when the source authority is complete,
    /// a readiness index is present, and this returns true.
    pub fn claimable(&self, id: &str, now: jiff::Timestamp) -> bool {
        let Some(issue) = self.issues.get(id) else {
            return false;
        };
        self.ready(id, now)
            && issue.status == Status::Open
            && issue.assignee.is_empty()
            && issue.issue_type != "epic"
            && !self.has_open_children(id)
    }

    /// Go `ReadinessIndex.Ready` (readiness.go:175-179), the `defer_until`
    /// gate on its own: `DeferUntil != nil && DeferUntil.After(now)`.
    ///
    /// Go's `Ready` is `open|in_progress AND !deferred AND satisfied`. The
    /// status gate and the readiness gate both have Rust equivalents that were
    /// already applied; this defer gate was not, so an issue parked until next
    /// quarter still counted as actionable work.
    pub fn is_deferred_at(&self, id: &str, now: jiff::Timestamp) -> bool {
        self.issues
            .get(id)
            .is_some_and(|issue| issue.is_deferred_at(now))
    }

    /// Go `ReadinessIndex.compute` (readiness.go:109-161).
    ///
    /// Blocking edges are decided in one pass; parent-child edges defer their
    /// verdict until the parent has been resolved, then propagate. Note there
    /// is NO self-edge exclusion: a `blocks` dependency pointing at the issue
    /// itself finds `other` present and open, so the issue lands in
    /// `Unsatisfied` and is never actionable. The gonum graph the metrics run
    /// on rejects self-edges, but readiness never consults that graph.
    fn compute(&mut self) {
        let mut pending: std::collections::HashMap<&'a str, i64> = std::collections::HashMap::new();
        for (&id, issue) in self.issues.iter() {
            self.states.insert(id, DepState::Satisfied);
            let closed = closed_for_readiness(issue.status);
            for (target, ty) in &issue.deps {
                if closed {
                    continue;
                }
                let other = self.issues.get(*target);
                if ty.is_blocking() {
                    match other {
                        None => {
                            self.states.insert(id, DepState::Unknown);
                        }
                        Some(o) if !closed_for_readiness(o.status) => {
                            let cur = self.states[&id];
                            self.states.insert(id, combine(cur, DepState::Unsatisfied));
                        }
                        Some(_) => {}
                    }
                } else if *ty == bv_core::model::DependencyType::ParentChild {
                    match other {
                        None => {
                            self.states.insert(id, DepState::Unknown);
                        }
                        Some(o) if !closed_for_readiness(o.status) => {
                            *pending.entry(id).or_insert(0) += 1;
                        }
                        Some(_) => {}
                    }
                }
            }
        }

        // readiness.go:141-157 — process parents before children. There is no
        // depth cutoff, and a parent cycle (and its descendants) stays
        // unresolved.
        let mut queue: Vec<&'a str> = Vec::new();
        for (&id, issue) in self.issues.iter() {
            if !closed_for_readiness(issue.status) && pending.get(id).copied().unwrap_or(0) == 0 {
                queue.push(id);
            }
        }
        let mut head = 0usize;
        while head < queue.len() {
            let id = queue[head];
            head += 1;
            let kids = self.children.get(id).cloned().unwrap_or_default();
            let parent_state = self.states[id];
            for child in kids {
                if self
                    .issues
                    .get(child)
                    .is_some_and(|c| closed_for_readiness(c.status))
                {
                    continue;
                }
                let cur = self.states[child];
                self.states.insert(child, combine(cur, parent_state));
                let e = pending.entry(child).or_insert(0);
                *e -= 1;
                if *e == 0 {
                    queue.push(child);
                }
            }
        }
        for (&id, &remaining) in pending.iter() {
            if remaining > 0 {
                self.states.insert(id, DepState::Unknown);
            }
        }
    }

    /// Go `ReadinessIndex.DependencyState` (readiness.go:165-170) — an ID the
    /// index never saw is `Unknown`, not `Satisfied`.
    fn dependency_state(&self, id: &str) -> DepState {
        self.states.get(id).copied().unwrap_or(DepState::Unknown)
    }

    /// Go `ReadinessIndex.Blockers` (readiness.go:199-216) — the IDs that
    /// explain why an item was withheld, including blocker IDs that are absent
    /// from the source entirely. A `parent-child` edge counts only when the
    /// parent's state is not `Satisfied`, so an open unblocked parent does not
    /// gate its child.
    fn blockers(&self, id: &str) -> Vec<String> {
        let Some(issue) = self.issues.get(id) else {
            return Vec::new();
        };
        if closed_for_readiness(issue.status) {
            return Vec::new();
        }
        let mut set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for (target, ty) in &issue.deps {
            let unresolved_blocking = ty.is_blocking()
                && self
                    .issues
                    .get(*target)
                    .is_none_or(|o| !closed_for_readiness(o.status));
            let unresolved_parent = *ty == bv_core::model::DependencyType::ParentChild
                && self.dependency_state(target) != DepState::Satisfied;
            if unresolved_blocking || unresolved_parent {
                set.insert((*target).to_string());
            }
        }
        set.into_iter().collect()
    }

    /// Go `ReadinessIndex.Ready` (readiness.go:175-179).
    fn ready(&self, id: &str, now: jiff::Timestamp) -> bool {
        let Some(issue) = self.issues.get(id) else {
            return false;
        };
        issue.status.is_open()
            && !issue.is_deferred_at(now)
            && self.dependency_state(id) == DepState::Satisfied
    }

    /// Go `ReadinessIndex.ReadyAfter` (readiness.go:223-262) — the bounded
    /// what-if frontier. `completed == None` (Go's empty map) defers to
    /// [`Readiness::ready`].
    pub fn ready_after(
        &self,
        id: &str,
        now: jiff::Timestamp,
        completed: Option<&std::collections::BTreeSet<String>>,
    ) -> bool {
        let Some(completed) = completed else {
            return self.ready(id, now);
        };
        let Some((&key, issue)) = self.issues.get_key_value(id) else {
            return false;
        };
        if completed.contains(id) || !issue.status.is_open() || issue.is_deferred_at(now) {
            return false;
        }
        let mut states = std::collections::HashMap::new();
        let mut visiting = std::collections::BTreeSet::new();
        self.visit(key, completed, &mut states, &mut visiting) == DepState::Satisfied
    }

    /// Go's inner `visit` closure (readiness.go:234-261). Recursion depth is
    /// bounded by the parent-child chain; a cycle resolves to `Unknown`, which
    /// withholds readiness rather than looping.
    fn visit(
        &self,
        id: &'a str,
        completed: &std::collections::BTreeSet<String>,
        states: &mut std::collections::HashMap<&'a str, DepState>,
        visiting: &mut std::collections::BTreeSet<&'a str>,
    ) -> DepState {
        let Some(issue) = self.issues.get(id) else {
            return DepState::Unknown;
        };
        if visiting.contains(id) {
            return DepState::Unknown;
        }
        if completed.contains(id) || closed_for_readiness(issue.status) {
            return DepState::Satisfied;
        }
        if let Some(&s) = states.get(id) {
            return s;
        }
        visiting.insert(id);
        let mut state = DepState::Satisfied;
        for (target, ty) in &issue.deps {
            if ty.is_blocking() {
                match self.issues.get(*target) {
                    None => state = DepState::Unknown,
                    Some(o) if !completed.contains(*target) && !closed_for_readiness(o.status) => {
                        state = combine(state, DepState::Unsatisfied)
                    }
                    Some(_) => {}
                }
            } else if *ty == bv_core::model::DependencyType::ParentChild {
                state = combine(state, self.visit(target, completed, states, visiting));
            }
        }
        visiting.remove(id);
        states.insert(id, state);
        state
    }
}

/// Compute the set of issue IDs that are not dependency-ready.
///
/// Go parity (`br ready`/`br blocked`): the authority is
/// `ReadinessIndex.DependencyState`, not a per-edge "does this have an open
/// blocker" scan. Three behaviours only the index reproduces:
///
///   * a self-blocking dependency — `blocks` pointing at the issue itself —
///     leaves the target present and open, so the issue is `Unsatisfied`
///     (readiness.go:120-127 has no self-edge exclusion);
///   * a `parent-child` parent that is itself blocked, or whose own parent
///     chain never resolves, withholds the child;
///   * a blocking dependency naming an issue absent from the source leaves the
///     state `Unknown`, which also withholds readiness.
pub fn compute_blocked_set(issues: &[Issue]) -> std::collections::HashSet<String> {
    let readiness = Readiness::new(issues);
    issues
        .iter()
        .filter(|i| {
            !closed_for_readiness(i.status)
                && readiness.dependency_state(i.id.as_str()) != DepState::Satisfied
        })
        .map(|i| i.id.clone())
        .collect()
}

/// Assemble the full triage recommendation list (ranked) + quick_ref +
/// project_health counts. Command templates built by caller with real IDs.
pub struct TriageOutput {
    pub recommendations: Vec<crate::impact::IssueImpact>,
    pub counts: ProjectCounts,
    pub quick_ref: QuickRef,
    pub velocity: Option<serde_json::Value>,
    /// Go TriageConfig parity: PageRank computed, Betweenness computed with
    /// reason "approximate" (sample recorded only when the approximator
    /// actually sampled), every other Phase-2 metric skipped.
    pub metric_status: crate::analyzer::MetricStatus,
}

/// Parse YYYY-MM-DDTHH:MM:SS from an RFC3339 timestamp string.
fn parse_ymd(s: &str) -> Option<(i32, u32, u32)> {
    let y: i32 = s[0..4].parse().ok()?;
    let m: u32 = s[5..7].parse().ok()?;
    let d: u32 = s[8..10].parse().ok()?;
    Some((y, m, d))
}

/// Convert year/month/day to ISO year + ISO week number (Monday-start).
/// Uses Julian Day Number arithmetic for ISO week computation.
fn ymd_to_iso_week(y: i32, m: u32, d: u32) -> (i32, u32) {
    fn julian_day(y: i32, m: i32, d: i32) -> i64 {
        let a = (14 - m) / 12;
        let yy = y + 4800 - a;
        let mm = m + 12 * a - 3;
        (d + (153 * mm + 2) / 5 + 365 * yy + yy / 4 - yy / 100 + yy / 400 - 32045) as i64
    }
    let jd = julian_day(y, m as i32, d as i32);
    let dow = (jd % 7 + 7) % 7; // 0=Monday
    let day_of_year = jd - julian_day(y, 1, 1) + 1;
    let week_num = ((day_of_year - dow + 10) / 7) as u32;

    if week_num == 0 {
        // Belongs to previous year's last week.
        let prev_y = y - 1;
        let prev_jan1_dow = (julian_day(prev_y, 1, 1) % 7 + 7) % 7;
        let is_leap = prev_y % 4 == 0 && (prev_y % 100 != 0 || prev_y % 400 == 0);
        let total_days = 365 + i64::from(is_leap);
        let weeks_in_prev = ((total_days - prev_jan1_dow + 6) / 7) as u32;
        (prev_y, weeks_in_prev)
    } else if week_num > 52 {
        // Check if it belongs to next year's week 1.
        let dec31_dow = (julian_day(y, 12, 31) % 7 + 7) % 7;
        if dow >= 4 - dec31_dow {
            (y + 1, 1)
        } else {
            (y, week_num)
        }
    } else {
        (y, week_num)
    }
}

/// Velocity snapshot — port of Go `ComputeProjectVelocity` (triage.go:215).
/// Computes closure velocity from issue timestamps, including weekly buckets.
pub fn compute_project_velocity(
    issues: &[Issue],
    now: jiff::Timestamp,
) -> Option<serde_json::Value> {
    let week_ago = now - jiff::SignedDuration::from_secs(7 * 86400);
    let month_ago = now - jiff::SignedDuration::from_secs(30 * 86400);

    let mut closed_last_7 = 0usize;
    let mut closed_last_30 = 0usize;
    let mut total_close_secs = 0.0f64;
    let mut close_samples = 0usize;
    let mut estimated = false;

    // Weekly buckets: key = (iso_year, iso_week), value = count.
    // Go uses 8 weeks, Monday-start ISO weeks.
    use std::collections::BTreeMap;
    let mut week_buckets: BTreeMap<i64, usize> = BTreeMap::new();

    for issue in issues {
        if !matches!(issue.status, Status::Closed | Status::Tombstone) {
            continue;
        }
        // Determine closure time (Go parity: closed_at > updated_at > now).
        let closed_at = issue
            .closed_at
            .as_deref()
            .and_then(|s| s.parse::<jiff::Timestamp>().ok())
            .or_else(|| {
                estimated = true;
                issue
                    .updated_at
                    .as_deref()
                    .and_then(|s| s.parse::<jiff::Timestamp>().ok())
            })
            .unwrap_or(now);

        if closed_at >= week_ago {
            closed_last_7 += 1;
        }
        if closed_at >= month_ago {
            closed_last_30 += 1;
        }
        // Bucket by the Monday of the closure's ISO week.
        // Go (triage.go:299-304) keys `weekBuckets` by `isoWeekStart(year,
        // week)` — a time.Time Monday — and emits it as an RFC3339 date, not
        // an ISO week label. Keying by (year, week) and formatting as
        // "YYYY-Www" produced "2026-W34" where Go emits "2026-08-17T00:00:00Z".
        if let Some(ts_str) = issue.closed_at.as_deref().or(issue.updated_at.as_deref()) {
            if let Some((y, m, d)) = parse_ymd(ts_str) {
                let (iso_year, iso_week) = ymd_to_iso_week(y, m, d);
                if let Some(monday) = iso_week_start(iso_year, iso_week) {
                    *week_buckets.entry(monday).or_insert(0) += 1;
                }
            }
        }

        // Average time-to-close.
        if let Some(created) = issue
            .created_at
            .as_deref()
            .and_then(|s| s.parse::<jiff::Timestamp>().ok())
        {
            let dur = closed_at - created;
            total_close_secs += dur.total(jiff::Unit::Second).unwrap_or(0.0);
            close_samples += 1;
        }
    }

    let avg_days = if close_samples > 0 {
        total_close_secs / 86400.0 / close_samples as f64
    } else {
        0.0
    };

    // Build weekly array (newest first, 8 weeks). Go parity: VelocityWeek.
    // Go (triage.go:314-321) starts at `truncateToMonday(now)` and steps back
    // 7 days at a time, emitting each cursor as an RFC3339 timestamp.
    let mut weekly: Vec<serde_json::Value> = Vec::new();
    if let Some((ny, nm, nd)) = parse_ymd(&now.to_string()) {
        let (cur_y, cur_w) = ymd_to_iso_week(ny, nm, nd);
        if let Some(mut cursor) = iso_week_start(cur_y, cur_w) {
            for _ in 0..8 {
                let count = week_buckets.get(&cursor).copied().unwrap_or(0);
                let ts = jiff::Timestamp::from_second(cursor * 86400)
                    .map(|t| format!("{}T00:00:00Z", t.strftime("%Y-%m-%d")))
                    .unwrap_or_default();
                weekly.push(serde_json::json!({
                    "week_start": ts,
                    "closed": count,
                }));
                cursor -= 7;
            }
        }
    }

    // Go's `Estimated` is omitempty (triage.go:235), so a false value is
    // absent rather than present-as-false.
    let mut vel = serde_json::json!({
        "closed_last_7_days": closed_last_7,
        "closed_last_30_days": closed_last_30,
        "avg_days_to_close": avg_days,
        "weekly": weekly,
    });
    if estimated {
        vel["estimated"] = serde_json::json!(true);
    }
    Some(vel)
}

/// Days since the Unix epoch for the Monday that starts the given ISO week.
/// Go `isoWeekStart` (triage.go) reconstructs the Monday from the ISO
/// year/week pair; the four-day rule puts the reference in that same week.
fn iso_week_start(iso_year: i32, iso_week: u32) -> Option<i64> {
    // Jan 4 is always in ISO week 1.
    let jan4 = days_from_civil(iso_year, 1, 4);
    // Weekday of Jan 4, 0=Monday.
    let jan4_dow = (jan4 + 3).rem_euclid(7);
    let week1_monday = jan4 - jan4_dow;
    Some(week1_monday + (iso_week as i64 - 1) * 7)
}

/// Days since 1970-01-01 for a proleptic-Gregorian y/m/d (Howard Hinnant's
/// `days_from_civil`), matching Go's `time.Date(...).Unix()` / 86400.
fn days_from_civil(y: i32, m: i32, d: i32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y } as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = ((m + 9) % 12) as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

pub fn build_triage(
    issues: &[Issue],
    g: &DiGraph,
    now: jiff::Timestamp,
    // True when the caller is time-travelling (--as-of). Go runs that path
    // without timing the analysis phases, and statusEntry serialises ms only
    // when Elapsed is non-zero (graph.go:131-147), so those payloads carry no
    // per-metric timing at all.
    time_travelling: bool,
) -> TriageOutput {
    build_triage_inner(issues, g, now, true, time_travelling, None, None, None)
}

/// Go parity for the three triage entrypoints that pass
/// `TriageOptions.Weights` from `loadRobotFeedback()`
/// (robot_registry.go:920, :2208, :2470). The weights reach scoring exactly
/// as Go's `analyzer.SetWeights` does — before the composite is built, not
/// after — so the ranking and the `feedback.applied` report agree with each
/// other.
pub fn build_triage_with_weights(
    issues: &[Issue],
    g: &DiGraph,
    now: jiff::Timestamp,
    time_travelling: bool,
    weights: Option<crate::scoring::Weights>,
) -> TriageOutput {
    build_triage_inner(issues, g, now, true, time_travelling, weights, None, None)
}

/// Go `ComputeTriageWithOptionsAndTime` (pkg/analysis/triage.go:466-467):
///
/// ```go
/// if opts.Readiness == nil {
///     opts.Readiness = model.NewReadinessIndex(issues)
/// }
/// analyzer.SetReadinessScope(opts.Readiness, opts.CandidateIDs)
/// ```
///
/// The index is built over `issues` only when the caller did not supply one —
/// and the robot handlers always do. `buildRobotContext` builds its readiness
/// over the whole loaded source and then narrows `ctx.Issues` to the recipe's
/// output (main.go:4904-4920), so by the time triage runs, `issues` is the
/// recipe's short list while the authority still knows every issue in the
/// repository.
///
/// Rebuilding the index from the narrowed list instead loses the blockers that
/// the recipe filtered away, and a blocking dependency naming an absent issue
/// resolves to `Unknown` rather than `Satisfied` (readiness.go, `visit`). Under
/// `--recipe triage` on the ultraworkers set — 20 epics, whose children are
/// all outside the recipe — that withheld readiness from 2 of the 20 and
/// reported `actionable_count` 18 where Go reports 20.
pub fn build_triage_with_readiness(
    issues: &[Issue],
    g: &DiGraph,
    now: jiff::Timestamp,
    time_travelling: bool,
    weights: Option<crate::scoring::Weights>,
    readiness: Option<&Readiness<'_>>,
) -> TriageOutput {
    build_triage_inner(
        issues,
        g,
        now,
        true,
        time_travelling,
        weights,
        readiness,
        None,
    )
}

/// Go `ComputeTriageScores` (pkg/analysis/triage.go:1316-1333), reduced to the
/// shape `--recipe` consumes: an id -> triage-score map.
///
/// Go builds it as `ComputeImpactScores` → `computeTriageScoresFromImpact` →
/// `computeSingleTriageScore`, whose composite is
///
/// ```text
/// triageScore = base.Score * BaseScoreWeight + UnblockBoost + QuickWinBoost
/// (triage.go:1409; label health, claim penalty and attention are listed
///  as pending, not applied — :1398-1407)
/// ```
///
/// which is exactly what `build_triage` already assembles at triage.rs:1215.
/// Reusing it rather than restating the formula means a recipe sorted by
/// `triage` ranks identically to the triage payload — which is the point of
/// Go sending both through `TriageScoringOptions`.
///
/// The caller is the `--recipe` scope in `apply_recipe_scope`
/// (main.go:4955-4963), which only builds this when the recipe actually sorts
/// by that field.
/// Go `analysis.ComputeTriageScores` (pkg/analysis/triage.go:1316-1332) — the
/// score source the `--recipe` sort chain reads (`cmd/bv/main.go:4957`).
///
/// This is *not* the `ComputeTriageWithOptions` path the `--robot-triage`
/// command takes. Go builds a bare analyzer here and calls
/// `ComputeImpactScores`, which is `a.Analyze()` — synchronous and therefore
/// complete. So this path has real PageRank and betweenness in hand even though
/// no `WaitForPhase2` option is set anywhere near it. Passing `false` to
/// [`build_triage`] left `pr` and `bw` empty (see `build_triage_inner`), which
/// silently drops the graph terms from every score and reorders the `triage`
/// recipe: it selected `epic-g7mp`/`epic-jhz5` where Go picks
/// `m1-w21-021`/`m6-gap-m6-12-081`.
pub fn compute_triage_scores(
    issues: &[Issue],
    g: &DiGraph,
    now: jiff::Timestamp,
) -> std::collections::BTreeMap<String, f64> {
    // Go: `NewAnalyzer(issues)` leaves `config` nil, so `Analyze()` resolves
    // `ConfigForSize(nodeCount, edgeCount)` (graph.go:1583-1590,
    // config.go:98). `analyze_with_profile` needs its own graph handle because
    // it takes ownership; `g` stays for the impact scorer. `build_graph`
    // assigns indices in sorted-id order, so the two graphs are isomorphic and
    // the float results agree.
    let config = crate::analyzer::config_for_size(g.len(), g.edge_count(), 0.0);
    let owned = std::sync::Arc::new(crate::analyzer::build_graph(issues));
    let (analysis, _) = crate::analyzer::analyze_with_profile(owned, &config);
    let pagerank = analysis.page_rank.clone().unwrap_or_default();
    let betweenness = analysis.betweenness.clone().unwrap_or_default();
    let metrics = ScoreMetrics {
        pagerank: &pagerank,
        betweenness: &betweenness,
        critical_path: analysis.critical_path_score.as_ref(),
    };
    build_triage_inner(
        issues,
        g,
        now,
        // `Analyze()` waits for phase 2 before returning, so the maps are
        // populated even though no `WaitForPhase2` option is in play here.
        true,
        false,
        None,
        None,
        Some(metrics),
    )
    .recommendations
    .iter()
    .map(|r| (r.id.clone(), r.score))
    .collect()
}

/// Go parity variant for `cmd/bv/main.go:4013` (`--emit-script`), the one
/// triage caller that omits `TriageOptions.WaitForPhase2` (triage.go:400).
/// The `opts.WaitForPhase2 && hasOpenIssues` guard at triage.go:517-519
/// therefore never fires, so `ComputeImpactScoresFromStats` reads a
/// `GraphStats` whose `pageRank`/`betweenness` maps are still the empty
/// `make(map[string]float64)` from graph.go:1740. `findMax` over an empty map
/// is 0 (priority.go:445-453) and `normalize(v, 0)` is 0 (priority.go:429-434),
/// so `prNorm`/`bwNorm` are 0 for every node and both terms drop out of the
/// composite. This is deliberate Go parity, not an oversight.
/// Go `--emit-script` (cmd/bv/main.go:4013) calls
/// `ComputeTriageWithOptions`, whose analyzer never had phase 2 awaited, so its
/// PageRank and betweenness inputs are EMPTY and the ranking comes from
/// staleness, priority, effort and the unblock boost alone. The triage registry
/// path awaits phase 2 (robot_registry.go:930-932) and does use them, which is
/// why the two rank the same issues differently — with graph metrics TREE-1
/// scores 0.542 and leads nothing; without them it scores 0.355 and leads, which
/// is what the oracle's script prints.
pub fn build_triage_phase2_pending(
    issues: &[Issue],
    g: &DiGraph,
    now: jiff::Timestamp,
) -> TriageOutput {
    build_triage_inner(issues, g, now, false, false, None, None, None)
}

/// Graph metrics to score against, when the caller already has them.
///
/// `build_triage_inner` otherwise derives PageRank and betweenness from the
/// *triage fast config* (betweenness approximate, sample 50, critical path
/// disabled — Go `TriageConfig`, triage.go:400). Go's
/// `analysis.ComputeTriageScores` is not on that path: it builds a bare
/// `NewAnalyzer(issues)` whose `config` field is nil, so `Analyze()` picks
/// `ConfigForSize` (config.go:98), and for any graph under 500 nodes that is
/// **exact** betweenness with `ComputeCriticalPath: true`
/// (config.go:134-153). The two disagree enough to reorder the `triage`
/// recipe — on the 133-node reference workspace Rust's scores collapsed to 13
/// distinct values and selected `epic-g7mp`/`epic-jhz5` where Go selects
/// `m1-w21-021`/`m6-gap-m6-12-081`.
///
/// This is deliberately an override rather than a second scoring function: the
/// unblocks map, blocker depths and the `base*0.70 + boosts` composition below
/// are shared, and only the three graph maps differ.
#[derive(Clone, Copy)]
pub struct ScoreMetrics<'a> {
    pub pagerank: &'a BTreeMap<String, f64>,
    pub betweenness: &'a BTreeMap<String, f64>,
    pub critical_path: Option<&'a BTreeMap<String, f64>>,
}

// One parameter per Go `ComputeTriageWithOptions` input plus the two
// caller-supplied overrides the recipe path needs. Bundling them into an
// options struct would hide which combinations Go actually reaches, and every
// caller here passes one fixed combination.
#[allow(clippy::too_many_arguments)]
fn build_triage_inner(
    issues: &[Issue],
    g: &DiGraph,
    now: jiff::Timestamp,
    phase2_awaited: bool,
    time_travelling: bool,
    weights: Option<crate::scoring::Weights>,
    supplied_readiness: Option<&Readiness<'_>>,
    supplied_metrics: Option<ScoreMetrics<'_>>,
) -> TriageOutput {
    // Go TriageConfig (fast config) parity: PageRank exact, Betweenness
    // approximate with sample 50 (falling back to exact inside the
    // approximator when sample >= n), all other Phase-2 metrics skipped.
    const TRIAGE_BETWEENNESS_SAMPLE: usize = 50;

    let n = g.len();
    // When the caller supplies the metrics, the fast-config ones below are
    // dead: they are read only through `inputs`, which the override replaces.
    // Computing them anyway cost a full PageRank plus a betweenness pass per
    // `--recipe` invocation — and the betweenness branch is O(n·m) whenever the
    // graph has fewer than 50 nodes, or exact-by-fallback above it. The
    // `metric_status` timings go to zero with them, which is invisible: the
    // recipe path reads `recommendations[..].score` and nothing else.
    let (pr, bw, pr_ms, bw_ms, actual_sample) = match supplied_metrics {
        Some(_) => (BTreeMap::new(), BTreeMap::new(), 0.0, 0.0, 0),
        None => {
            let t0 = std::time::Instant::now();
            let pagerank = crate::algorithms::pagerank::pagerank_default(g);
            let pr_ms = t0.elapsed().as_secs_f64() * 1000.0;

            let t1 = std::time::Instant::now();
            let (betweenness, actual_sample) = if TRIAGE_BETWEENNESS_SAMPLE >= n {
                // Go ApproxBetweenness falls back to exact when sample >= n;
                // the config-mode reason stays "approximate" but no sample is
                // reported.
                (crate::algorithms::betweenness::betweenness(g), 0)
            } else {
                (
                    crate::algorithms::betweenness::betweenness_approx(
                        g,
                        TRIAGE_BETWEENNESS_SAMPLE,
                        Some(1),
                    ),
                    TRIAGE_BETWEENNESS_SAMPLE,
                )
            };
            let bw_ms = t1.elapsed().as_secs_f64() * 1000.0;

            // Go TriageConfig skips ComputeCriticalPath: no time-to-impact
            // component.
            let pr: BTreeMap<String, f64> = if phase2_awaited {
                pagerank
                    .into_iter()
                    .enumerate()
                    .map(|(i, v)| (g.node_id(i).unwrap_or_default().to_string(), v))
                    .collect()
            } else {
                BTreeMap::new()
            };
            let bw: BTreeMap<String, f64> = if phase2_awaited {
                betweenness
                    .into_iter()
                    .enumerate()
                    .map(|(i, v)| (g.node_id(i).unwrap_or_default().to_string(), v))
                    .collect()
            } else {
                BTreeMap::new()
            };
            (pr, bw, pr_ms, bw_ms, actual_sample)
        }
    };

    // Go graph.go:1913-1916 marks every metric "skipped" on an empty graph
    // (`emptyGraphMetricStatus`). Everything else in the result is still
    // computed — the velocity block in particular still gets its eight zeroed
    // weekly buckets — so only the status is special-cased, not the output.
    let empty_graph = g.node_count() == 0;
    let all_skipped = crate::analyzer::StatusEntry::skipped("");
    let empty_graph_status = crate::analyzer::MetricStatus {
        page_rank: all_skipped.clone(),
        betweenness: all_skipped.clone(),
        eigenvector: all_skipped.clone(),
        hits: all_skipped.clone(),
        critical: all_skipped.clone(),
        cycles: all_skipped.clone(),
        kcore: all_skipped.clone(),
        articulation: all_skipped.clone(),
        slack: all_skipped,
    };

    // Go's bv-perf shortcut (triage.go:492-508): "Phase 2 metrics (PageRank,
    // Betweenness) are only used for scoring open issues. If all issues are
    // closed, we can skip Phase 2 entirely." When no issue is open it analyses
    // under `NoPhase2Config`, and `GraphStats.analyzePhase2` then reports every
    // metric as `skipped` with the reason "all phase 2 disabled"
    // (graph.go:1779-1790).
    //
    // The graph is not empty here — a fully-closed workspace still has nodes —
    // so `empty_graph` does not cover it. Without this gate the triage status
    // claimed PageRank and Betweenness were `computed` on a workspace where
    // the oracle reports them as never attempted, and `triage.status` is what a
    // caller reads to decide whether a metric actually ran.
    let has_open_issues = issues.iter().any(|i| !i.status.is_closed());
    let no_phase2_status = || {
        let s = crate::analyzer::StatusEntry::skipped("all phase 2 disabled");
        crate::analyzer::MetricStatus {
            page_rank: s.clone(),
            betweenness: s.clone(),
            eigenvector: s.clone(),
            hits: s.clone(),
            critical: s.clone(),
            cycles: s.clone(),
            kcore: s.clone(),
            articulation: s.clone(),
            slack: s,
        }
    };

    // Go's statusEntry serialises `ms` only when Elapsed is non-zero
    // (graph.go:131-147, `json:"ms,omitempty"`), and the time-travel path
    // leaves Elapsed at zero — so `--as-of` payloads carry no per-metric timing
    // at all. Measuring the phases here and reporting them unconditionally
    // added an `ms` key the oracle never emits.
    let timed = |ms: f64| {
        if time_travelling {
            0.0
        } else {
            ms
        }
    };
    let pr_ms = timed(pr_ms);
    let bw_ms = timed(bw_ms);

    let skipped = crate::analyzer::StatusEntry::skipped("disabled by triage fast config");
    let metric_status = if empty_graph {
        empty_graph_status
    } else if !has_open_issues {
        no_phase2_status()
    } else {
        crate::analyzer::MetricStatus {
            page_rank: crate::analyzer::StatusEntry::computed(pr_ms),
            betweenness: {
                let mut e = crate::analyzer::StatusEntry::computed(bw_ms);
                e.reason = "approximate".into();
                e.sample = actual_sample;
                e
            },
            eigenvector: skipped.clone(),
            hits: skipped.clone(),
            critical: skipped.clone(),
            cycles: skipped.clone(),
            kcore: skipped.clone(),
            articulation: skipped.clone(),
            slack: skipped.clone(),
        }
    };

    let inputs = ImpactInputs {
        issues,
        pagerank: supplied_metrics.map_or(&pr, |m| m.pagerank),
        betweenness: supplied_metrics.map_or(&bw, |m| m.betweenness),
        // Go TriageConfig skips ComputeCriticalPath; the `ConfigForSize` path
        // behind `ComputeTriageScores` enables it, and the caller says which.
        critical_path: supplied_metrics.and_then(|m| m.critical_path),
        g,
        now,
    };
    // Go `TriageOptions.Weights` (triage.go:413-415, :483-484) hands the
    // feedback-adjusted factor weights to the analyzer before scoring, so the
    // composite is built with them. `None` is Go's nil and scores with
    // `DefaultWeights`.
    let mut recommendations = match weights {
        Some(w) => crate::impact::compute_impact_scores_with_weights(&inputs, w),
        None => compute_impact_scores(&inputs),
    };

    // === Go triage scoring (triage.go:1267-1311) ===
    // triageScore = baseScore * 0.70 + unblockBoost + quickwinBoost
    // This transforms raw impact scores into triage-prioritized scores.

    // 1. Compute the unblocks map (Go `buildUnblocksMap`, triage.go:797-831).
    //    Reverse map: blocker_id -> the issues it would unblock. Three
    //    conditions, all load-bearing:
    //      * the candidate is neither closed-like nor deferred (triage.go:802);
    //      * its open-blocker set has EXACTLY ONE member (triage.go:815) —
    //        completing one of two blockers leaves the other holding it up, so
    //        Go credits nobody;
    //      * completing that single blocker actually makes it actionable
    //        (triage.go:819 -> `isActionableAfterCompletions`), which withholds
    //        parked statuses, unresolved parent chains and output eligibility.
    //    The blocker set is `Readiness().Blockers`, not a raw in-degree scan:
    //    it deduplicates, keeps blockers absent from the source, and rolls up
    //    `parent-child` parents that are themselves unresolved.
    let owned_readiness;
    let readiness = match supplied_readiness {
        Some(r) => r,
        None => {
            owned_readiness = Readiness::new(issues);
            &owned_readiness
        }
    };
    let mut unblocks_map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for issue in issues {
        if closed_for_readiness(issue.status)
            || readiness
                .issues
                .get(issue.id.as_str())
                .is_some_and(|r| r.is_deferred_at(now))
        {
            continue;
        }
        unblocks_map.entry(issue.id.clone()).or_default();
        let open_blockers = readiness.blockers(issue.id.as_str());
        if open_blockers.len() == 1 {
            let blocker_id = &open_blockers[0];
            let mut completed: std::collections::BTreeSet<String> = Default::default();
            completed.insert(blocker_id.clone());
            // Go's `IsCandidate` is the output-eligibility scope; the CLI only
            // sets a candidate set for `--repo`/`--label-scope`, so an
            // unscoped `--robot-triage` accepts every loaded issue.
            if readiness.ready_after(issue.id.as_str(), now, Some(&completed)) {
                unblocks_map
                    .entry(blocker_id.clone())
                    .or_default()
                    .push(issue.id.clone());
            }
        }
    }
    for list in unblocks_map.values_mut() {
        list.sort();
    }
    let max_unblocks = unblocks_map.values().map(|v| v.len()).max().unwrap_or(0);

    /// Go `getBlockerDepthRecursive` (triage.go:1440-1470): the length of the
    /// longest chain of open blockers above this issue. Returns -1 on a cycle, and
    /// memoizes the result so a wide graph stays linear.
    fn blocker_depth_recursive(
        issue_id: &str,
        issues: &[Issue],
        visited: &mut std::collections::BTreeSet<String>,
        memo: &mut BTreeMap<String, i64>,
    ) -> i64 {
        if let Some(val) = memo.get(issue_id) {
            return *val;
        }
        if !visited.insert(issue_id.to_string()) {
            return -1; // cycle
        }
        let open_blockers: Vec<String> = issues
            .iter()
            .find(|i| i.id == issue_id)
            .map(|i| {
                i.dependencies
                    .iter()
                    .filter(|d| d.r#type.is_blocking())
                    .map(|d| d.effective_depends_on().to_string())
                    .filter(|bid| {
                        !bid.is_empty() && issues.iter().any(|o| o.id == *bid && o.status.is_open())
                    })
                    .collect()
            })
            .unwrap_or_default();
        if open_blockers.is_empty() {
            visited.remove(issue_id);
            memo.insert(issue_id.to_string(), 0);
            return 0;
        }
        let mut max_chain = 0i64;
        for blocker_id in &open_blockers {
            let depth = blocker_depth_recursive(blocker_id, issues, visited, memo);
            if depth == -1 {
                visited.remove(issue_id);
                return -1;
            }
            max_chain = max_chain.max(depth);
        }
        visited.remove(issue_id);
        memo.insert(issue_id.to_string(), max_chain + 1);
        max_chain + 1
    }

    // 2. Compute blocker depths. Go `getBlockerDepthRecursive` (triage.go:1440)
    //    walks the whole open-blocker CHAIN and returns its maximum length, not
    //    the number of direct blockers. Counting direct open blockers gave every
    //    mid-chain issue a depth of 1, so the quick-win boost (gated at
    //    depth <= 2) fired far too often and every score past the first link
    //    came out too high.
    // The map holds Go's signed depth, including the -1 that marks a cycle.
    // Dropping the -1 entries and defaulting the lookup to 0 made every node in
    // a cycle look like a root: the quick-win gate is `depth <= max && depth >=
    // 0` (triage.go:1379), so -1 is what excludes a cycle from the boost, and
    // substituting 0 granted it. On large_cyclic_600 that inflated Cyc-2 by
    // exactly base_score * 0.15 and reordered the whole list.
    let mut blocker_depths: BTreeMap<String, i64> = BTreeMap::new();
    for issue in issues {
        if !issue.status.is_open() {
            continue;
        }
        let mut visited: std::collections::BTreeSet<String> = Default::default();
        let mut memo: BTreeMap<String, i64> = BTreeMap::new();
        let depth = blocker_depth_recursive(&issue.id, issues, &mut visited, &mut memo);
        blocker_depths.insert(issue.id.clone(), depth);
    }

    // Go builds the recommendation's graph context here: `UnblocksIDs` from the
    // unblocks map (triage.go:947) and `BlockedBy` from the context's open
    // blockers, set only when non-empty (triage.go:949). Both are omitted
    // when empty, so the field order is reasons, unblocks_ids, blocked_by.
    // `BlockedBy` is `ctx.OpenBlockers` (triage_context.go:228) — the same
    // `Readiness().Blockers` the unblocks map keys off, so a single blocker
    // set answers both. `blocker_chain::open_blockers` is a near-miss of that
    // authority: it drops blocking targets missing from the source and treats
    // a tombstone parent as still blocking, both of which Go counts.
    for rec in recommendations.iter_mut() {
        rec.unblocks_ids = unblocks_map.get(&rec.id).cloned().unwrap_or_default();
        rec.blocked_by = readiness.blockers(&rec.id);
        // Go's claimable gate requires zero open blockers
        // (isClaimableRecommendation, triage.go:1191). The impact scorer
        // stamps a preliminary verdict before it can see the dependency graph,
        // so refine it here where blocked_by is known.
        if !rec.blocked_by.is_empty() {
            rec.claimable = false;
        }
        let blocker_depth = *blocker_depths.get(&rec.id).unwrap_or(&-1);
        let unblocks = rec.unblocks_ids.len();
        let base_score = rec.score;

        // Unblock boost: normalized unblocks * weight
        let unblock_boost = if unblocks > 0 {
            let norm = unblocks as f64 / (max_unblocks.max(TRIAGE_UNBLOCK_THRESHOLD) as f64);
            norm.min(1.0) * TRIAGE_UNBLOCK_BOOST
        } else {
            0.0
        };

        // Quick-win boost: depth-based factor * base score * weight. Go's gate
        // is exactly `blockerDepth <= QuickWinMaxDepth && blockerDepth >= 0`
        // (triage.go:1386) and has NO status term. An earlier version here also
        // excluded InProgress, so an in-progress issue lost a boost Go still
        // gave it — the `>= 0` half is what a `usize` depth cannot express and
        // is spelled out below, but the status half was never in the oracle.
        // Go gates the quick-win boost on status too
        // (triage.go:1385): .
        // A previous commit in this file removed that condition after
        // misreading the gate as depth-only; the published fixture sets happen
        // not to contain any in-progress issue, so nothing caught it.
        let quickwin_boost = if rec.status != Status::InProgress.as_str()
            && (0..=TRIAGE_QUICKWIN_MAX_DEPTH as i64).contains(&blocker_depth)
        {
            let depth_factor =
                1.0 - blocker_depth as f64 / (TRIAGE_QUICKWIN_MAX_DEPTH as f64 + 1.0);
            (depth_factor * base_score * TRIAGE_QUICKWIN_BOOST).min(TRIAGE_QUICKWIN_BOOST)
        } else {
            0.0
        };

        rec.score = base_score * TRIAGE_BASE_WEIGHT + unblock_boost + quickwin_boost;
        // Go marks a quick win by `QuickWinBoost > 0.05` (triage.go:1759) and
        // rewrites the action hint for a genuinely startable bead
        // (triage.go:1605-1622). The boost is only known here, so the hint is
        // stamped in this layer rather than where the base action is derived.
        if quickwin_boost > 0.05 {
            // Go emits the quick-win reason immediately BEFORE the
            // claim-status reason (triage.go:1605, then 1625) and rewrites
            // the action hint from the same predicate, but only for a bead
            // that is genuinely startable (no open blockers).
            const QUICK_WIN_REASON: &str = "⚡ Low effort, high impact - good starting point";
            if let Some(pos) = rec
                .reasons
                .iter()
                .position(|r| r.starts_with("✅ Currently unclaimed"))
            {
                rec.reasons.insert(pos, QUICK_WIN_REASON.to_string());
            } else {
                rec.reasons.push(QUICK_WIN_REASON.to_string());
            }
            if rec.blocked_by.is_empty() {
                rec.action = "Quick win - start here for fast progress".to_string();
            }
        }
    }

    // Re-sort by triage score descending, ID ascending (Go tie-break).
    recommendations.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.id.cmp(&b.id))
    });

    // Go reuses one readiness index for both the unblocks map above and the
    // counts here — `opts.Readiness` is resolved once at triage.go:466 and
    // threaded through. When the caller supplied the full-source index, the
    // narrowed `issues` are counted against it; otherwise this is the same
    // locally-built index as above.
    let (counts, quick_ref) = compute_counts(issues, readiness, now);
    let velocity = compute_project_velocity(issues, now);
    TriageOutput {
        recommendations,
        counts,
        quick_ref,
        velocity,
        metric_status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_issues(name: &str) -> Vec<Issue> {
        let path = format!(
            "{}/../../tests/fixtures/{}/.beads/issues.jsonl",
            env!("CARGO_MANIFEST_DIR"),
            name
        );
        let raw = std::fs::read_to_string(path).unwrap();
        raw.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str::<Issue>(l).unwrap())
            .filter(|i| i.validate().is_ok())
            .collect()
    }

    /// Minimal dependency-free open issue, for graph/readiness unit tests.
    fn bare_issue(id: &str) -> Issue {
        bare_issue_with(id, Status::Open, None)
    }

    fn bare_issue_with(id: &str, status: Status, defer_until: Option<&str>) -> Issue {
        Issue {
            id: id.to_string(),
            content_hash: String::new(),
            title: id.to_string(),
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
            defer_until: defer_until.map(str::to_string),
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

    /// The `--recipe` sort path scores against `ConfigForSize`, not the triage
    /// fast config. That difference is invisible in `--robot-triage` (which
    /// really does use the fast config and matches the oracle byte for byte) and
    /// only shows up as a reshuffled `triage` recipe export, so it needs its own
    /// guard: assert the supplied metrics actually reach the composite.
    ///
    /// The fixture is a two-node chain, `A -> B`, where `A` is the only node
    /// with any PageRank to speak of. Supplying a PageRank map that ranks `B`
    /// above `A` must move `B`'s score up; with the override dropped on the
    /// floor the two calls would be identical.
    #[test]
    fn supplied_metrics_reach_the_triage_composite() {
        let now = jiff::Timestamp::now();
        let mut a = bare_issue("A");
        let b = bare_issue("B");
        a.dependencies.push(bv_core::model::Dependency {
            issue_id: "A".into(),
            depends_on_id: "B".into(),
            depends_on_legacy: String::new(),
            target_id_legacy: String::new(),
            r#type: bv_core::model::DependencyType::Blocks,
            created_at: None,
            created_by: String::new(),
        });
        let issues = vec![a, b];
        let g = crate::analyzer::build_graph(&issues);

        let default_run =
            build_triage_inner(&issues, &g, now, true, false, None, None, None).recommendations;
        let default_scores: BTreeMap<String, f64> = default_run
            .iter()
            .map(|r| (r.id.clone(), r.score))
            .collect();

        // Invert the ranking: give `B` all the PageRank.
        let mut pr = BTreeMap::new();
        pr.insert("A".to_string(), 0.0);
        pr.insert("B".to_string(), 1.0);
        let empty: BTreeMap<String, f64> = BTreeMap::new();
        let override_run = build_triage_inner(
            &issues,
            &g,
            now,
            true,
            false,
            None,
            None,
            Some(ScoreMetrics {
                pagerank: &pr,
                betweenness: &empty,
                critical_path: None,
            }),
        )
        .recommendations;
        let override_scores: BTreeMap<String, f64> = override_run
            .iter()
            .map(|r| (r.id.clone(), r.score))
            .collect();

        // `A` is the dependent, so its score is the one the graph terms move:
        // zeroing its PageRank drops it from 0.1777 to 0.0826. `B` is the
        // blocker and its score is unchanged by construction, so asserting on
        // it would have passed even with the override dropped on the floor.
        assert_ne!(
            default_scores.get("A"),
            override_scores.get("A"),
            "supplied PageRank did not change A's triage score — the override is not reaching ImpactInputs"
        );
    }

    /// Go `ReadinessIndex.Claimable` (readiness.go:181-185) decides whether the
    /// export prints an "Atomically claim" command for an issue. Each conjunct
    /// is a separate rejection, and this walks them one at a time so a future
    /// edit cannot quietly drop one and only show up as a missing line in a
    /// markdown export nobody diffs.
    #[test]
    fn claimable_rejects_each_go_conjunct() {
        let now = jiff::Timestamp::now();

        // Baseline: open, unassigned, non-epic, no blockers, no children.
        let plain = bare_issue("A");
        let r = Readiness::new(std::slice::from_ref(&plain));
        assert!(r.claimable("A", now), "a plain open task is claimable");

        // `issue.Status == StatusOpen` — in_progress is Ready but not Open.
        let in_progress = bare_issue_with("A", Status::InProgress, None);
        assert!(!Readiness::new(std::slice::from_ref(&in_progress)).claimable("A", now));
        // ...and so is a closed one.
        let closed = bare_issue_with("A", Status::Closed, None);
        assert!(!Readiness::new(std::slice::from_ref(&closed)).claimable("A", now));

        // `issue.Assignee == ""`
        let mut assigned = bare_issue("A");
        assigned.assignee = "someone".into();
        assert!(!Readiness::new(std::slice::from_ref(&assigned)).claimable("A", now));

        // `issue.IssueType != TypeEpic`
        let mut epic = bare_issue("A");
        epic.issue_type = "epic".into();
        assert!(!Readiness::new(std::slice::from_ref(&epic)).claimable("A", now));

        // `!r.HasOpenChildren(id)` — a closed child does not withhold.
        let parent_child = |child_status: Status| {
            let pc = |target: &str| bv_core::model::Dependency {
                issue_id: String::new(),
                depends_on_id: target.to_string(),
                depends_on_legacy: String::new(),
                target_id_legacy: String::new(),
                r#type: bv_core::model::DependencyType::ParentChild,
                created_at: None,
                created_by: String::new(),
            };
            let mut parent = bare_issue("P");
            let mut child = bare_issue_with("C", child_status, None);
            child.dependencies = vec![pc("P")];
            parent.dependencies = vec![pc("C")];
            vec![parent, child]
        };
        assert!(
            !Readiness::new(&parent_child(Status::Open)).claimable("P", now),
            "an open child withholds the parent"
        );
        assert!(
            Readiness::new(&parent_child(Status::Closed)).claimable("P", now),
            "a closed child does not withhold the parent"
        );

        // `exists` — an id the index never saw is not claimable.
        assert!(!r.claimable("nope", now));
    }

    /// `quick_ref.in_progress_count` is `ByStatus["in_progress"]`
    /// (Go triage.go:722). It was declared on the struct and never assigned,
    /// so every repository reported 0 — including one with work in flight,
    /// which also turned the in-progress bead's recommendation reason into
    /// "Currently unclaimed - available for work".
    #[test]
    fn in_progress_count_reports_work_in_flight() {
        let issues = vec![
            bare_issue("A-1"),
            bare_issue_with("A-2", Status::InProgress, None),
            bare_issue_with("A-3", Status::InProgress, None),
            bare_issue_with("A-4", Status::Deferred, None),
        ];
        let readiness = Readiness::new(&issues);
        let (counts, qr) = compute_counts(&issues, &readiness, jiff::Timestamp::UNIX_EPOCH);

        assert_eq!(qr.in_progress_count, 2);
        assert_eq!(counts.by_status.get("in_progress"), Some(&2));
        assert_eq!(
            qr.not_closed_count, 4,
            "open + in_progress + deferred are all non-closed"
        );
    }

    /// Go `ReadinessIndex.Ready` (readiness.go:175-179) gates on
    /// `DeferUntil.After(now)`. Without that gate an issue parked until next
    /// quarter counted as actionable work, and — because
    /// `dependency_blocked` is the complement within the non-closed set
    /// (triage.go:850-860) — it was counted in neither bucket.
    #[test]
    fn a_deferred_issue_is_neither_actionable_nor_ignored() {
        let parked = jiff::Timestamp::now() + jiff::SignedDuration::from_hours(24 * 30);
        let issues = vec![
            bare_issue("A-1"),
            bare_issue_with("A-2", Status::Open, Some(&parked.to_string())),
            // A defer_until already in the past does not park anything.
            bare_issue_with(
                "A-3",
                Status::Open,
                Some(
                    &(jiff::Timestamp::UNIX_EPOCH + jiff::SignedDuration::from_hours(1))
                        .to_string(),
                ),
            ),
        ];
        let readiness = Readiness::new(&issues);
        let now = jiff::Timestamp::now();
        let (counts, qr) = compute_counts(&issues, &readiness, now);

        assert_eq!(qr.actionable_count, 2, "A-1 and the expired A-3");
        assert_eq!(
            qr.not_actionable_count, 1,
            "A-2 is parked, so it belongs to the complement — not nowhere"
        );
        assert_eq!(counts.dependency_blocked, 1);
        assert_eq!(
            qr.not_closed_count,
            qr.actionable_count + qr.not_actionable_count,
            "the partition invariant has to hold for the numbers to mean anything"
        );
        assert!(qr.validate_invariant());
    }

    #[test]
    fn small_chain_quick_ref_matches_golden() {
        // Golden: open=12 actionable=1 blocked=0 in_progress=0 not_closed=12
        //         not_actionable=11
        let issues = fixture_issues("small_chain");
        let g = crate::analyzer::build_graph(&issues);
        let out = build_triage(&issues, &g, jiff::Timestamp::now(), false);
        assert_eq!(out.quick_ref.open_count, 12);
        assert_eq!(out.quick_ref.actionable_count, 1);
        assert_eq!(out.quick_ref.blocked_count, 0);
        assert_eq!(out.quick_ref.not_closed_count, 12);
        assert_eq!(out.quick_ref.not_actionable_count, 11);
        assert!(out.quick_ref.validate_invariant());
        // counts block matches golden too
        assert_eq!(out.counts.total, 12);
        assert_eq!(out.counts.dependency_blocked, 11);
    }

    #[test]
    fn recommendations_ranked_desc_with_id_tiebreak() {
        let issues = fixture_issues("medium_tree");
        let g = crate::analyzer::build_graph(&issues);
        let out = build_triage(&issues, &g, jiff::Timestamp::now(), false);
        for w in out.recommendations.windows(2) {
            let ok = w[0].score > w[1].score || (w[0].score == w[1].score && w[0].id <= w[1].id);
            assert!(
                ok,
                "{:?} vs {:?}",
                (&w[0].id, w[0].score),
                (&w[1].id, w[1].score)
            );
        }
    }

    /// Go `ReadinessIndex.compute` (readiness.go:120-127) has no self-edge
    /// exclusion: a `blocks` dependency naming the issue itself finds the
    /// target present and open, so the issue is `Unsatisfied` and never
    /// actionable. `large_cyclic_600` carries exactly one such node (Cyc-33)
    /// and the Go golden puts actionable at 149, not 150.
    #[test]
    fn self_blocking_dependency_is_dependency_blocked() {
        use bv_core::model::Dependency;
        let mut self_loop = fixture_issues("small_chain")
            .into_iter()
            .find(|i| i.status == Status::Open)
            .expect("small_chain has an open issue");
        let id = self_loop.id.clone();
        self_loop.dependencies.push(Dependency {
            issue_id: id.clone(),
            depends_on_id: id.clone(),
            depends_on_legacy: String::new(),
            target_id_legacy: String::new(),
            r#type: bv_core::model::DependencyType::Blocks,
            created_at: None,
            created_by: String::new(),
        });
        let issues = vec![self_loop];
        let blocked = compute_blocked_set(&issues);
        assert!(
            blocked.contains(&id),
            "a self-blocking dependency must leave the issue dependency-blocked"
        );
        let readiness = Readiness::new(&issues);
        assert_eq!(
            readiness.dependency_state(id.as_str()),
            DepState::Unsatisfied
        );
    }

    /// The same fixture the gap above came from: 600 issues, one of them
    /// (Cyc-33) self-blocking. The Go golden's
    /// `project_health.counts` is actionable 149 / dependency_blocked 451.
    #[test]
    fn large_cyclic_counts_match_go_golden() {
        let issues = fixture_issues("large_cyclic_600");
        let blocked = compute_blocked_set(&issues);
        let readiness = Readiness::new(&issues);
        let (counts, _) = compute_counts(&issues, &readiness, jiff::Timestamp::UNIX_EPOCH);
        assert_eq!(counts.total, 600);
        assert_eq!(counts.actionable, 149);
        assert_eq!(counts.dependency_blocked, 451);
        assert!(blocked.contains("Cyc-33"), "Cyc-33 is the only self-loop");
    }

    /// Go `buildUnblocksMap` (triage.go:797-831): a blocker is credited only
    /// for issues it is the SOLE open blocker of, and only when completing it
    /// makes the issue actionable. Two open blockers mean neither of them is
    /// credited — the `Cyc-354`/`Cyc-417` case the Go golden settles at
    /// `Cyc-354 -> [Cyc-355]`.
    #[test]
    fn unblocks_credits_only_the_sole_open_blocker() {
        use bv_core::model::Dependency;
        let blocks = |issue_id: &str, target: &str| Dependency {
            issue_id: issue_id.to_string(),
            depends_on_id: target.to_string(),
            depends_on_legacy: String::new(),
            target_id_legacy: String::new(),
            r#type: bv_core::model::DependencyType::Blocks,
            created_at: None,
            created_by: String::new(),
        };
        // A-1 blocks A-2 only. B-1 and B-2 both block B-3, so neither is
        // B-3's sole blocker.
        let a1 = bare_issue("A-1");
        let mut a2 = bare_issue("A-2");
        a2.dependencies.push(blocks("A-2", "A-1"));
        let b1 = bare_issue("B-1");
        let b2 = bare_issue("B-2");
        let mut b3 = bare_issue("B-3");
        b3.dependencies.push(blocks("B-3", "B-1"));
        b3.dependencies.push(blocks("B-3", "B-2"));
        let issues = vec![a1, a2, b1, b2, b3];
        let readiness = Readiness::new(&issues);
        assert_eq!(readiness.blockers("A-2"), vec!["A-1".to_string()]);
        assert_eq!(
            readiness.blockers("B-3"),
            vec!["B-1".to_string(), "B-2".to_string()],
            "Go sorts and dedupes the blocker set (readiness.go:212-214)"
        );
        let g = crate::analyzer::build_graph(&issues);
        let out = build_triage(&issues, &g, jiff::Timestamp::now(), false);
        let unblocks_of = |id: &str| {
            out.recommendations
                .iter()
                .find(|r| r.id == id)
                .map(|r| r.unblocks_ids.clone())
                .unwrap_or_default()
        };
        assert_eq!(unblocks_of("A-1"), vec!["A-2".to_string()]);
        assert!(
            unblocks_of("B-1").is_empty() && unblocks_of("B-2").is_empty(),
            "an issue with two open blockers is credited to neither"
        );
    }
}
