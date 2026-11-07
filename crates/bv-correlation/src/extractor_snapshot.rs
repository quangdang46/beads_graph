//! Snapshot-path event extraction — port of Go
//! `pkg/correlation/extractor_snapshot.go`'s `extractViaSnapshots`.
//!
//! Go reconstructs lifecycle events from per-commit JSONL *snapshot*
//! differences rather than from git's textual patch. `git log --raw --follow`
//! gives each commit's old/new blob id for the followed beads file; each unique
//! blob is read once through a long-lived `git cat-file --batch`; and the
//! per-commit record diff is computed as a **multiset** difference of the two
//! blobs' record lines.
//!
//! The multiset semantics are the observable point: a commit that only *reorders*
//! records (a tracker "sync/export" commit) has an identical multiset before and
//! after, so it produces an empty diff and **no events** — even though
//! `git log -p` renders it as a long remove/add patch. The legacy patch path
//! cannot see that difference, which is why Go routes large blobs here and why
//! `extract` dispatches on blob size (see `SNAPSHOT_BLOB_SIZE_THRESHOLD`).

use crate::causality::{is_closed_lifecycle_status, normalize_status};
use crate::extractor::{
    determine_status_event, parse_bead_json, parse_diff_text, BeadEvent, BeadSnapshot,
    CommitHeader, EventType, ExtractOptions,
};
use crate::readiness::{
    dep_type_is_blocking, dep_type_is_valid, DependencyState, ReadinessIndex, ReadinessIssue,
    DEP_PARENT_CHILD,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

/// The followed file's current (HEAD) blob size at or above which `extract`
/// takes this path. Go's `snapshotBlobSizeThreshold`.
pub const SNAPSHOT_BLOB_SIZE_THRESHOLD: i64 = 64 * 1024;

/// Git's null blob id — an add or a delete, not a real parent/child blob.
const NULL_BLOB_SHA: &str = "0000000000000000000000000000000000000000";

/// Go `preferSnapshotPath` — one cheap `git cat-file -s HEAD:<file>`. An
/// unreadable size (no history yet, untracked file) also selects this path, so
/// the degenerate case degrades to "no commits" rather than a slower native
/// diff.
pub fn prefer_snapshot_path(repo: &Path, beads_rel: &str) -> bool {
    let out = Command::new("git")
        .args(["cat-file", "-s", &format!("HEAD:{beads_rel}")])
        .current_dir(repo)
        .output();
    let Ok(out) = out else { return true };
    if !out.status.success() {
        return true;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    match text.trim().parse::<i64>() {
        Ok(size) => size >= SNAPSHOT_BLOB_SIZE_THRESHOLD,
        Err(_) => true,
    }
}

/// Go `snapshotCommit` — commit metadata plus the followed file's two blob ids.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SnapshotCommit {
    header: CommitHeader,
    old_sha: String,
    new_sha: String,
    /// The committer date (`%cI`). Only the causal walk requests it; the event
    /// walk leaves it empty because nothing downstream reads it there.
    committed_at: String,
}

/// Go `snapshotCommits` + `parseSnapshotLog` + `parseRawDiffLines`: one
/// metadata-only `git log --raw --no-abbrev --follow` yields, per commit, the
/// header and the first raw diff entry for the followed file.
fn snapshot_commits(
    repo: &Path,
    opts: &ExtractOptions,
    beads_rel: &str,
) -> Result<Vec<SnapshotCommit>, String> {
    let mut args: Vec<String> = vec![
        "log".into(),
        "--raw".into(),
        "--no-abbrev".into(),
        "--follow".into(),
        "--no-color".into(),
        format!("--format={}", crate::extractor::GIT_LOG_HEADER_FORMAT),
    ];
    crate::extractor::append_history_filters(&mut args, opts);
    args.push("--".to_string());
    args.push(beads_rel.to_string());

    let out = Command::new("git")
        .args(&args)
        .current_dir(repo)
        .output()
        .map_err(|e| format!("spawning git log --raw: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git log --raw failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(parse_snapshot_log(&out.stdout))
}

fn parse_snapshot_log(out: &[u8]) -> Vec<SnapshotCommit> {
    let mut commits = Vec::new();
    for chunk in commit_chunks(out) {
        let Some(nl) = chunk.iter().position(|b| *b == b'\n') else {
            continue;
        };
        let Some(header) = CommitHeader::parse(&chunk[..nl]) else {
            continue;
        };
        let mut sc = SnapshotCommit {
            header,
            old_sha: String::new(),
            new_sha: String::new(),
            committed_at: String::new(),
        };
        if parse_raw_diff_lines(&chunk[nl + 1..], &mut sc) {
            commits.push(sc);
        }
    }
    commits
}

/// Go's `commitPattern.FindAllIndex` + `forEachCommitChunk` boundary detection:
/// a commit header starts at a hex run — 40 chars for SHA-1 repos, 64 for
/// SHA-256 repos — immediately followed by NUL. The run must be *exactly* the
/// SHA width; a longer hex run is message/author text, not a header. This is
/// the same scan `history::for_each_commit_chunk` performs.
fn commit_chunks(out: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0usize;
    while i < out.len() {
        if !out[i].is_ascii_hexdigit() {
            i += 1;
            continue;
        }
        let mut run_end = i;
        while run_end < out.len() && out[run_end].is_ascii_hexdigit() {
            run_end += 1;
        }
        let run_len = run_end - i;
        if (run_len == 40 || run_len == 64) && out.get(run_end) == Some(&0) {
            starts.push(i);
        }
        i = run_end.max(i + 1);
    }
    starts
        .iter()
        .enumerate()
        .map(|(idx, &start)| {
            let end = starts.get(idx + 1).copied().unwrap_or(out.len());
            &out[start..end]
        })
        .collect()
}

/// Go `parseRawDiffLines` — the first `:`-prefixed raw line, whose metadata
/// before the TAB is `:<oldmode> <newmode> <oldsha> <newsha> <status>`. A pure
/// deletion (new == null blob) carries no records and is skipped entirely.
fn parse_raw_diff_lines(payload: &[u8], sc: &mut SnapshotCommit) -> bool {
    for line in payload.split(|b| *b == b'\n') {
        if line.first() != Some(&b':') {
            continue;
        }
        let meta_end = line.iter().position(|b| *b == b'\t').unwrap_or(line.len());
        let meta = String::from_utf8_lossy(&line[..meta_end]);
        let fields: Vec<&str> = meta.split_whitespace().collect();
        if fields.len() < 5 {
            continue;
        }
        if fields[2] != NULL_BLOB_SHA {
            sc.old_sha = fields[2].to_string();
        }
        if fields[3] != NULL_BLOB_SHA {
            sc.new_sha = fields[3].to_string();
        }
        return !sc.new_sha.is_empty();
    }
    false
}

/// One entry of a blob's record-line multiset: how many times the line occurs
/// plus a representative copy (needed to emit the synthesized diff).
#[derive(Debug, Clone, Default)]
struct RecordLineEntry {
    count: usize,
    text: Vec<u8>,
}

/// A blob's record-line multiset. Go keys this by a 64-bit hash of the line for
/// memory; the line bytes themselves are already in hand here (they are the
/// representative that gets emitted), so keying by content is both exact — no
/// hash-collision risk — and one hash instead of two.
type RecordLineSet = HashMap<Vec<u8>, RecordLineEntry>;

/// Go `scanRecordLineDescriptors` + the set assembly in
/// `buildRecordLineSnapshot`: only lines whose first byte is `{` are records,
/// and a final record without a trailing newline still counts.
fn build_record_line_set(blob: &[u8]) -> RecordLineSet {
    let mut set: RecordLineSet = HashMap::new();
    for line in blob.split(|b| *b == b'\n') {
        // split() yields a trailing empty segment for a newline-terminated blob.
        if line.is_empty() || line[0] != b'{' {
            continue;
        }
        let entry = set.entry(line.to_vec()).or_default();
        if entry.count == 0 {
            entry.text = line.to_vec();
        }
        entry.count += 1;
    }
    set
}

/// Go `synthesizeRecordDiff` — emit `-{line}` once per old occurrence the new
/// side does not account for, then `+{line}` once per new occurrence the old
/// side does not account for. `parse_diff_text` only consumes `+{`/`-{` lines
/// and sorts the affected bead IDs, so the traversal order here is not
/// observable.
fn synthesize_record_diff(old: &RecordLineSet, new: &RecordLineSet) -> Vec<u8> {
    let mut buf: Vec<u8> = Vec::new();
    for (line, oe) in old {
        let new_count = new.get(line).map_or(0, |e| e.count);
        for _ in 0..oe.count.saturating_sub(new_count) {
            buf.push(b'-');
            buf.extend_from_slice(&oe.text);
            buf.push(b'\n');
        }
    }
    for (line, ne) in new {
        let old_count = old.get(line).map_or(0, |e| e.count);
        for _ in 0..ne.count.saturating_sub(old_count) {
            buf.push(b'+');
            buf.extend_from_slice(&ne.text);
            buf.push(b'\n');
        }
    }
    buf
}

/// A long-lived `git cat-file --batch`: one object id in, one blob out, so N
/// blobs cost one process instead of N forks.
struct BlobReader {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl BlobReader {
    fn open(repo: &Path) -> Result<Self, String> {
        let mut child = Command::new("git")
            .arg("cat-file")
            .arg("--batch")
            .current_dir(repo)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| format!("spawning git cat-file: {e}"))?;
        let stdin = child.stdin.take().ok_or("git cat-file stdin unavailable")?;
        let stdout = child
            .stdout
            .take()
            .ok_or("git cat-file stdout unavailable")?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::with_capacity(64 * 1024, stdout),
        })
    }

    fn read_blob(&mut self, sha: &str) -> Result<Vec<u8>, String> {
        Ok(self.read_blob_opt(sha)?.unwrap_or_default())
    }

    /// `None` for a blob git does not have. Go's `read` maps a missing blob to
    /// `nil`, and the causal path treats that differently from a genuinely
    /// empty blob: missing means the state is *unknown*, empty means the file
    /// existed with no records.
    fn read_blob_opt(&mut self, sha: &str) -> Result<Option<Vec<u8>>, String> {
        writeln!(self.stdin, "{sha}").map_err(|e| e.to_string())?;
        self.stdin.flush().map_err(|e| e.to_string())?;
        let mut header = String::new();
        self.stdout
            .read_line(&mut header)
            .map_err(|e| e.to_string())?;
        let fields: Vec<&str> = header.split_whitespace().collect();
        if fields.len() < 3 {
            // "<oid> missing" — an absent blob reads as empty, like Go's
            // readBlobs mapping it to nil.
            return Ok(None);
        }
        let size: usize = fields[2]
            .parse()
            .map_err(|e: std::num::ParseIntError| e.to_string())?;
        let mut buf = vec![0u8; size];
        self.stdout
            .read_exact(&mut buf)
            .map_err(|e| e.to_string())?;
        // cat-file terminates each object with a newline.
        let mut nl = [0u8; 1];
        let _ = self.stdout.read_exact(&mut nl);
        Ok(Some(buf))
    }
}

impl Drop for BlobReader {
    fn drop(&mut self) {
        // Closing stdin makes cat-file exit; reap it so no zombie survives.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Go `extractViaSnapshots` — the snapshot extraction, chronological.
///
/// Blobs are read at most once each and only the record-line sets are retained.
/// `last_use` records the final commit index that needs each blob so a set is
/// dropped as soon as no later commit references it: consecutive commits in a
/// followed chain share their boundary blob, so the live window is small even
/// when a tracker rewrites the whole file every mutation.
pub fn extract_via_snapshots(
    repo: &Path,
    opts: &ExtractOptions,
    beads_rel: &str,
) -> Result<Vec<BeadEvent>, String> {
    let commits = snapshot_commits(repo, opts, beads_rel)?;
    if commits.is_empty() {
        return Ok(Vec::new());
    }

    let mut last_use: HashMap<String, usize> = HashMap::new();
    for (i, c) in commits.iter().enumerate() {
        if !c.old_sha.is_empty() {
            last_use
                .entry(c.old_sha.clone())
                .and_modify(|last| *last = (*last).max(i))
                .or_insert(i);
        }
        if !c.new_sha.is_empty() {
            last_use
                .entry(c.new_sha.clone())
                .and_modify(|last| *last = (*last).max(i))
                .or_insert(i);
        }
    }

    let mut reader = BlobReader::open(repo)?;
    let mut live: HashMap<String, RecordLineSet> = HashMap::new();
    let mut events: Vec<BeadEvent> = Vec::new();

    // git log emits newest-first; the per-commit events are concatenated in that
    // order and the whole stream reversed at the end, matching the legacy
    // Extract contract (chronological).
    for (i, c) in commits.iter().enumerate() {
        let new_set = load_set(&mut reader, &mut live, &c.new_sha)?;
        let old_set = load_set(&mut reader, &mut live, &c.old_sha)?;
        let diff = synthesize_record_diff(&old_set, &new_set);
        if !diff.is_empty() {
            let text = String::from_utf8_lossy(&diff).to_string();
            events.extend(parse_diff_text(&text, &c.header, opts.bead_id.as_deref()));
        }
        // Evict every blob whose last referencing commit is at or before i.
        live.retain(|sha, _| last_use.get(sha).is_some_and(|last| *last > i));
    }
    events.reverse();
    Ok(events)
}

fn load_set(
    reader: &mut BlobReader,
    live: &mut HashMap<String, RecordLineSet>,
    sha: &str,
) -> Result<RecordLineSet, String> {
    if sha.is_empty() {
        // No parent blob: every record reads as an addition.
        return Ok(RecordLineSet::new());
    }
    if let Some(set) = live.get(sha) {
        return Ok(set.clone());
    }
    let blob = reader.read_blob(sha)?;
    let set = build_record_line_set(&blob);
    live.insert(sha.to_string(), set.clone());
    Ok(set)
}

// ---------------------------------------------------------------------------
// Causal history (Go `extractCausalHistory` + `causalSnapshotState`)
// ---------------------------------------------------------------------------

/// Go `extractCausalHistory` — follow one first-parent history for `target`,
/// retaining the full source while evaluating each target state.
///
/// Two deliberate differences from `extract_via_snapshots`:
///
/// * it does **not** apply the target's `-G` filter. An unchanged target can
///   become unblocked because *another* record changed, so filtering to commits
///   that touch the target would drop the release event.
/// * it parses whole records rather than diffing record-line multisets, because
///   a dependency edge inside an unchanged record is still evidence.
///
/// Only two parsed snapshots are resident at a time; the retained artifact
/// holds compact target observations, not every issue in every commit.
pub fn extract_causal_history(
    repo: &Path,
    target: &str,
    opts: &ExtractOptions,
    beads_rel: &str,
) -> Result<crate::causality::CausalHistory, String> {
    let mut args: Vec<String> = vec![
        "log".into(),
        "--first-parent".into(),
        "--diff-merges=first-parent".into(),
        "--raw".into(),
        "--no-abbrev".into(),
        "--follow".into(),
        "--no-color".into(),
        format!(
            "--format={}%x00%cI",
            crate::extractor::GIT_LOG_HEADER_FORMAT
        ),
    ];
    crate::extractor::append_history_filters(&mut args, opts);
    args.push("--".to_string());
    args.push(beads_rel.to_string());

    let out = Command::new("git")
        .args(&args)
        .current_dir(repo)
        .output()
        .map_err(|e| format!("spawning causal git log: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "causal git log: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }

    let commits = parse_causal_log(&out.stdout);
    let mut result = crate::causality::CausalHistory {
        bead_id: target.to_string(),
        observations: Vec::new(),
        until: None,
        revision: String::new(),
        reference_time: None,
        reference_committed_at: None,
    };

    if !opts.revision.is_empty() {
        // The revision may not itself change the beads file. Its own clocks —
        // not the last file change, and not today's clock — bound an ongoing
        // wait.
        let out = Command::new("git")
            .args(["show", "-s", "--format=%aI%x00%cI", &opts.revision])
            .current_dir(repo)
            .output()
            .map_err(|e| format!("spawning causal reference commit: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "causal reference commit: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let mut parts = text.trim().split('\0');
        let (Some(authored), Some(committed)) = (parts.next(), parts.next()) else {
            return Err("incomplete causal reference timestamps".to_string());
        };
        result.reference_time = Some(parse_go_time(
            authored,
            "causal reference author timestamp",
        )?);
        result.reference_committed_at = Some(parse_go_time(
            committed,
            "causal reference committer timestamp",
        )?);
        result.revision = opts.revision.clone();
    }

    if let Some(until) = &opts.until {
        result.until = Some(parse_go_time(until, "causal cutoff")?);
    }

    let mut reader = BlobReader::open(repo)?;
    // Go caches the previous commit's snapshot and reuses it when the current
    // commit's parent blob is that same blob, so consecutive commits in a
    // followed chain parse the boundary file once.
    let mut previous: Option<(String, bool, BTreeMap<String, BeadSnapshot>)> = None;

    // git log emits newest-first; Go walks the slice backwards, so the
    // observations end up in chronological order.
    for c in commits.iter().rev() {
        let (before, valid_before) = match &previous {
            Some((sha, valid, records)) if *sha == c.old_sha => (records.clone(), *valid),
            _ => {
                let (records, valid) = read_records(&mut reader, &c.old_sha)?;
                (records, valid)
            }
        };
        let (after, valid_after) = read_records(&mut reader, &c.new_sha)?;

        let (old_state, mut old_relevant) = causal_snapshot_state(&before, valid_before, target);
        let (new_state, new_relevant) = causal_snapshot_state(&after, valid_after, target);
        for (id, _) in new_relevant {
            old_relevant.insert(id, ());
        }
        let mut ids: Vec<&String> = old_relevant.keys().collect();
        ids.sort();

        let mut changes: Vec<BeadEvent> = Vec::new();
        for id in ids {
            let old = before.get(id);
            let current = after.get(id);
            if old.is_some() == current.is_some()
                && match (old, current) {
                    (Some(a), Some(b)) => equal_historical_record(a, b),
                    _ => true,
                }
            {
                continue;
            }
            let event_type = match (old, current) {
                (None, _) => EventType::Created,
                (_, None) => EventType::Deleted,
                (Some(a), Some(b)) if a.status != b.status => {
                    determine_status_event(&a.status, &b.status)
                }
                _ => EventType::Modified,
            };
            changes.push(BeadEvent {
                bead_id: id.clone(),
                commit_sha: c.header.sha.clone(),
                timestamp: c.header.timestamp.clone(),
                commit_msg: c.header.message.clone(),
                author: c.header.author.clone(),
                author_email: c.header.author_email.clone(),
                event_type,
                before: old.map(BeadSnapshot::historical_state),
                after: current.map(BeadSnapshot::historical_state),
                transition_observed: valid_before && valid_after,
            });
        }

        result
            .observations
            .push(crate::causality::CausalObservation {
                commit_sha: c.header.sha.clone(),
                timestamp: parse_go_time(&c.header.timestamp, "causal commit timestamp")?,
                committed_at: parse_go_time(&c.committed_at, "causal committer timestamp")?,
                before: old_state,
                after: new_state,
                changes,
            });
        previous = Some((c.new_sha.clone(), valid_after, after));
    }

    Ok(result)
}

fn parse_go_time(raw: &str, what: &str) -> Result<crate::causality::GoTime, String> {
    crate::causality::GoTime::parse(raw).map_err(|e| format!("{what}: {e}"))
}

/// Go's `read` closure — parse one blob into records, and report whether every
/// record in it was well formed and unique. A malformed or duplicated record
/// does not abort the walk; it makes the state *unknown*, which withholds the
/// conclusions that would otherwise follow.
fn read_records(
    reader: &mut BlobReader,
    sha: &str,
) -> Result<(BTreeMap<String, BeadSnapshot>, bool), String> {
    if sha.is_empty() {
        // No parent blob: the file did not exist yet, which is a known-empty
        // state rather than a missing one.
        return Ok((BTreeMap::new(), true));
    }
    let Some(data) = reader.read_blob_opt(sha)? else {
        return Ok((BTreeMap::new(), false));
    };
    let mut records = BTreeMap::new();
    let mut valid = true;
    for line in data.split(|b| *b == b'\n') {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let Some(record) = parse_bead_json(&String::from_utf8_lossy(line)) else {
            valid = false;
            continue;
        };
        let id = record.id.clone();
        if records.insert(id, record).is_some() {
            // A repeated id means the file cannot be read as one record per
            // bead, so nothing derived from it is trustworthy.
            valid = false;
        }
    }
    Ok((records, valid))
}

/// Go `equalHistoricalRecord` — identity by the fields causality reasons about.
fn equal_historical_record(a: &BeadSnapshot, b: &BeadSnapshot) -> bool {
    a.id == b.id && a.title == b.title && a.status == b.status && a.dependencies == b.dependencies
}

/// Go `causalSnapshotState` — the target's constraint set in one committed
/// snapshot, plus every record id that snapshot makes relevant.
///
/// A blocker is a dependency that resolves to a live issue, or to no issue at
/// all. A `parent-child` edge instead *transitively* withholds readiness, so
/// it is walked rather than counted, and cycles terminate on the visited set
/// rather than recursing forever.
fn causal_snapshot_state(
    records: &BTreeMap<String, BeadSnapshot>,
    valid: bool,
    target: &str,
) -> (crate::causality::CausalState, BTreeMap<String, ()>) {
    let mut state = crate::causality::CausalState {
        issue: None,
        known: valid,
        dependency_state: DependencyState::Unknown,
        blockers: Vec::new(),
        reason: String::new(),
    };
    let mut relevant: BTreeMap<String, ()> = BTreeMap::new();
    relevant.insert(target.to_string(), ());

    let mut issues: Vec<ReadinessIssue> = Vec::with_capacity(records.len());
    for record in records.values() {
        let status = normalize_status(&record.status);
        if status.trim().is_empty() {
            // Go `Status.IsValid` accepts any nonblank status, so a blank one
            // is the only invalid case.
            state.known = false;
        }
        for dep in &record.dependencies {
            // Go: `(dep.Type != "" && !model.DependencyType(dep.Type).IsValid())
            // || dep.DependsOnID == ""`. The empty type is exempt — it is the
            // legacy blocking default, not malformed data. Only a *blank but
            // non-empty* type (e.g. "   ") invalidates the snapshot, because
            // `IsValid` is a nonblank test.
            if (!dep.dep_type.is_empty() && !dep_type_is_valid(&dep.dep_type))
                || dep.depends_on_id.is_empty()
            {
                state.known = false;
            }
        }
        issues.push(ReadinessIssue {
            id: record.id.clone(),
            status: status.clone(),
            dependencies: record
                .dependencies
                .iter()
                .map(|d| (d.depends_on_id.clone(), d.dep_type.clone()))
                .collect(),
        });
    }

    if let Some(record) = records.get(target) {
        state.issue = Some(record.historical_state());
        state.dependency_state = ReadinessIndex::new(&issues).dependency_state(target);
    }

    let mut blockers: BTreeSet<String> = BTreeSet::new();
    let mut visited: BTreeSet<String> = BTreeSet::new();
    visit_parents(target, records, &mut visited, &mut relevant, &mut blockers);
    state.blockers = blockers.into_iter().collect();

    if !state.known {
        state.reason = "malformed, duplicate, or invalid historical records".to_string();
    } else if state.issue.is_some() && state.dependency_state == DependencyState::Unknown {
        state.reason = "missing dependency or unresolved parent cycle".to_string();
    }
    (state, relevant)
}

/// Go's inner `visit` closure — walk `parent-child` edges transitively,
/// collecting direct blockers on the way.
fn visit_parents(
    id: &str,
    records: &BTreeMap<String, BeadSnapshot>,
    visited: &mut BTreeSet<String>,
    relevant: &mut BTreeMap<String, ()>,
    blockers: &mut BTreeSet<String>,
) {
    if !visited.insert(id.to_string()) {
        return;
    }
    let Some(record) = records.get(id) else {
        return;
    };
    if is_closed_lifecycle_status(&normalize_status(&record.status)) {
        return;
    }
    for dep in &record.dependencies {
        let blocking = dep_type_is_blocking(&dep.dep_type);
        if !blocking && dep.dep_type != DEP_PARENT_CHILD {
            continue;
        }
        relevant.insert(dep.depends_on_id.clone(), ());
        let Some(other) = records.get(&dep.depends_on_id) else {
            // An unresolvable edge blocks, and the missing id is itself worth
            // reporting.
            blockers.insert(dep.depends_on_id.clone());
            continue;
        };
        if is_closed_lifecycle_status(&normalize_status(&other.status)) {
            continue;
        }
        if blocking {
            blockers.insert(dep.depends_on_id.clone());
        } else {
            visit_parents(&dep.depends_on_id, records, visited, relevant, blockers);
        }
    }
}

/// Go's `parseSnapshotLog` for the causal walk: the header carries a sixth
/// NUL-separated field, the committer date, which the event path does not need.
fn parse_causal_log(out: &[u8]) -> Vec<SnapshotCommit> {
    let mut commits = Vec::new();
    for chunk in commit_chunks(out) {
        let Some(nl) = chunk.iter().position(|b| *b == b'\n') else {
            continue;
        };
        // The committer date is the field after the last NUL, so the header
        // proper is everything before it.
        let Some(last) = chunk[..nl].iter().rposition(|b| *b == 0) else {
            continue;
        };
        let Some(header) = CommitHeader::parse(&chunk[..last]) else {
            continue;
        };
        let Ok(committed_at) = String::from_utf8(chunk[last + 1..nl].to_vec()) else {
            continue;
        };
        let mut sc = SnapshotCommit {
            header,
            old_sha: String::new(),
            new_sha: String::new(),
            committed_at,
        };
        if parse_raw_diff_lines(&chunk[nl + 1..], &mut sc) {
            commits.push(sc);
        }
    }
    commits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(lines: &[&str]) -> RecordLineSet {
        build_record_line_set(lines.join("\n").as_bytes())
    }

    fn diff_text(old: &RecordLineSet, new: &RecordLineSet) -> String {
        String::from_utf8(synthesize_record_diff(old, new)).expect("utf8")
    }

    #[test]
    fn pure_reorder_yields_no_diff() {
        // The whole point of the snapshot path: a tracker "sync/export" commit
        // that only reorders records has an identical multiset before and after.
        let old = set(&[
            r#"{"id":"A","status":"open"}"#,
            r#"{"id":"B","status":"open"}"#,
        ]);
        let new = set(&[
            r#"{"id":"B","status":"open"}"#,
            r#"{"id":"A","status":"open"}"#,
        ]);
        assert_eq!(diff_text(&old, &new), "");
    }

    #[test]
    fn changed_record_emits_removal_then_addition() {
        let old = set(&[r#"{"id":"A","status":"open"}"#]);
        let new = set(&[r#"{"id":"A","status":"closed"}"#]);
        let diff = diff_text(&old, &new);
        assert!(
            diff.contains("-{\"id\":\"A\",\"status\":\"open\"}\n"),
            "{diff}"
        );
        assert!(
            diff.contains("+{\"id\":\"A\",\"status\":\"closed\"}\n"),
            "{diff}"
        );
    }

    #[test]
    fn added_and_removed_records_are_one_sided() {
        let old = set(&[r#"{"id":"A","status":"open"}"#]);
        let new = set(&[r#"{"id":"B","status":"open"}"#]);
        let diff = diff_text(&old, &new);
        assert!(diff.contains("-{\"id\":\"A\""), "{diff}");
        assert!(!diff.contains("+{\"id\":\"A\""), "{diff}");
        assert!(diff.contains("+{\"id\":\"B\""), "{diff}");
        assert!(!diff.contains("-{\"id\":\"B\""), "{diff}");
    }

    #[test]
    fn duplicate_lines_are_counted_as_a_multiset() {
        // Two identical records, one removed: exactly one removal, not two.
        let line = r#"{"id":"A","status":"open"}"#;
        let old = build_record_line_set(format!("{line}\n{line}\n").as_bytes());
        let new = build_record_line_set(format!("{line}\n").as_bytes());
        let diff = diff_text(&old, &new);
        assert_eq!(diff.matches('\n').count(), 1);
        assert!(diff.starts_with('-'), "{diff}");
    }

    #[test]
    fn non_record_lines_are_ignored() {
        let blob = b"not a record\n{\"id\":\"A\"}\n\n";
        let set = build_record_line_set(blob);
        assert_eq!(set.len(), 1);
        assert!(set.contains_key(b"{\"id\":\"A\"}".as_slice()));
    }

    #[test]
    fn final_record_without_newline_is_counted() {
        let set = build_record_line_set(b"{\"id\":\"A\"}");
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn raw_diff_line_fills_blob_ids_and_skips_deletions() {
        let mut sc = SnapshotCommit {
            header: CommitHeader {
                sha: "s".into(),
                timestamp: "t".into(),
                author: "a".into(),
                author_email: "e".into(),
                message: "m".into(),
            },
            old_sha: String::new(),
            new_sha: String::new(),
            committed_at: String::new(),
        };
        let payload = b"\n:100644 100644 aaa bbb M\t.beads/issues.jsonl\n";
        assert!(parse_raw_diff_lines(payload, &mut sc));
        assert_eq!(sc.old_sha, "aaa");
        assert_eq!(sc.new_sha, "bbb");

        let mut del = SnapshotCommit {
            header: sc.header.clone(),
            old_sha: String::new(),
            new_sha: String::new(),
            committed_at: String::new(),
        };
        let del_payload =
            b":100644 000000 aaa 0000000000000000000000000000000000000000 D\t.beads/issues.jsonl\n";
        assert!(!parse_raw_diff_lines(del_payload, &mut del));
    }

    #[test]
    fn raw_add_leaves_old_sha_empty() {
        let mut sc = SnapshotCommit {
            header: CommitHeader {
                sha: "s".into(),
                timestamp: "t".into(),
                author: "a".into(),
                author_email: "e".into(),
                message: "m".into(),
            },
            old_sha: String::new(),
            new_sha: String::new(),
            committed_at: String::new(),
        };
        let payload =
            b":000000 100644 0000000000000000000000000000000000000000 bbb A\t.beads/issues.jsonl\n";
        assert!(parse_raw_diff_lines(payload, &mut sc));
        assert!(sc.old_sha.is_empty(), "null old blob stays empty");
        assert_eq!(sc.new_sha, "bbb");
    }

    #[test]
    fn commit_chunks_split_on_header_pattern() {
        let sha1 = "a".repeat(40);
        let out = format!("{sha1}\x00rest\npayload\nsecond-header");
        let chunks = commit_chunks(out.as_bytes());
        assert_eq!(chunks.len(), 1, "only the 40-hex + NUL run is a header");
        assert!(chunks[0].starts_with(sha1.as_bytes()));
    }

    /// The hex run must be *exactly* the SHA width: a 42-hex run followed by
    /// NUL is message text, not a header (same rule as
    /// `history::for_each_commit_chunk`).
    #[test]
    fn commit_chunks_reject_overlong_hex_runs() {
        let mut out = "c".repeat(42).into_bytes();
        out.push(0);
        out.extend_from_slice(b"not a header\n");
        assert!(commit_chunks(&out).is_empty());
    }

    /// SHA-256 repos emit 64-hex commit headers; both widths must split.
    #[test]
    fn commit_chunks_accept_sha256_width() {
        let sha256 = "b".repeat(64);
        let mut out = Vec::new();
        out.extend_from_slice(&b"a".repeat(40));
        out.push(0);
        out.extend_from_slice(b"header-one\npayload\n");
        out.extend_from_slice(sha256.as_bytes());
        out.push(0);
        out.extend_from_slice(b"header-two\npayload\n");
        let chunks = commit_chunks(&out);
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].starts_with(&b"a".repeat(40)));
        assert!(chunks[1].starts_with(sha256.as_bytes()));
    }
}
