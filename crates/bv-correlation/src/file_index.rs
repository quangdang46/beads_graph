//! File→bead reverse index — port of Go `pkg/correlation/file_index.go`.
//!
//! This is the public, full-fidelity port of Go's `FileLookup` and
//! `CoChangeMatrix`. `orphan.rs` previously carried a private, reduced copy of
//! the exact-match arm; that copy is gone and it now uses [`FileLookup`] from
//! here, so there is exactly one implementation of the index in this crate.
//!
//! Two Go behaviours are load-bearing and easy to lose in translation, so they
//! are called out at their definitions:
//! - [`normalize_path`] strips exactly one leading `./` and one trailing `/`
//!   (`strings.TrimPrefix`/`TrimSuffix`, not a repeat-strip), so `"a//"`
//!   normalizes to `"a/"`.
//! - [`CoChangeMatrix::get_related_files`] maps a non-positive threshold back
//!   to Go's `0.5` default rather than treating it as "include everything".

use crate::history::{self, HistoryReport};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};

/// Go `BeadReference` — links a bead to a file via commits.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct BeadReference {
    pub bead_id: String,
    pub title: String,
    /// open/in_progress/closed.
    pub status: String,
    /// Which commits linked this bead to this file.
    pub commit_shas: Vec<String>,
    /// Most recent commit timestamp (RFC3339, as stored in the report).
    pub last_touch: String,
    /// Sum of insertions + deletions across commits.
    pub total_changes: i64,
}

/// Go `FileIndexStats` — aggregate statistics about the file index.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct FileIndexStats {
    /// Number of unique files.
    pub total_files: usize,
    /// Sum of all bead references.
    pub total_bead_links: usize,
    /// Files touched by more than one bead.
    pub files_with_multiple_beads: usize,
}

/// Go `FileBeadLookupResult` — the result of looking up beads for a file.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FileBeadLookupResult {
    pub file_path: String,
    /// Currently open beads.
    pub open_beads: Vec<BeadReference>,
    /// Recently closed beads.
    pub closed_beads: Vec<BeadReference>,
    pub total_beads: usize,
}

/// Go `FileBeadIndex` — O(1) lookup from file path to the beads that touched it.
#[derive(Debug, Clone, Default, Serialize)]
pub struct FileBeadIndex {
    /// Normalized file path → beads that modified it.
    pub file_to_beads: BTreeMap<String, Vec<BeadReference>>,
    pub stats: FileIndexStats,
}

/// Go `FileLookup` — file-to-bead lookup, plus the co-change matrix it carries.
#[derive(Debug, Clone, Default)]
pub struct FileLookup {
    index: FileBeadIndex,
    /// BeadID → (title, status), refreshed at lookup time because a status can
    /// change after the index was built.
    beads: BTreeMap<String, (String, String)>,
    co_change: CoChangeMatrix,
}

/// Go `FileHotspot` — a file that has been touched by many beads.
///
/// Field order matches Go's declaration (file_index.go:378-383), which is the
/// wire order of each `--robot-file-hotspots` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileHotspot {
    pub file_path: String,
    pub total_beads: usize,
    pub open_beads: usize,
    pub closed_beads: usize,
}

/// Go `normalizePath`.
///
/// Strips exactly one leading `./` and exactly one trailing `/`. Go uses
/// `strings.TrimPrefix`/`strings.TrimSuffix`, so this is deliberately a single
/// strip rather than `trim_end_matches`.
pub fn normalize_path(path: &str) -> String {
    let slashed = path.replace('\\', "/");
    let no_prefix = slashed.strip_prefix("./").unwrap_or(&slashed);
    no_prefix.strip_suffix('/').unwrap_or(no_prefix).to_string()
}

/// Go `classifyBeadStatus` — `(bucket, skip)`.
///
/// `skip` marks a tombstone, which the index drops at build time so every
/// lookup surface (hotspots, per-file lookups, related work) agrees on the same
/// bead set (Go issue #184).
pub fn classify_bead_status(status: &str) -> (&'static str, bool) {
    match status.trim().to_lowercase().as_str() {
        "tombstone" => ("", true),
        "closed" => ("closed", false),
        _ => ("open", false),
    }
}

/// Go `normalizeStatus` (related.go) / the same normalization in orphan.go.
pub fn normalize_status(status: &str) -> String {
    status.trim().to_lowercase()
}

/// Go `isClosedHistoryStatus` (orphan.go:190).
pub fn is_closed_history_status(status: &str) -> bool {
    let normalized = normalize_status(status);
    normalized == "closed" || normalized == "tombstone"
}

/// Go `sortBeadRefs` — most recently touched first, bead ID breaking ties.
fn sort_bead_refs(refs: &mut [BeadReference]) {
    refs.sort_by(|a, b| {
        ts_cmp_str(&b.last_touch, &a.last_touch).then_with(|| a.bead_id.cmp(&b.bead_id))
    });
}

/// String-ordered timestamp comparison. RFC3339 instants compare correctly once
/// parsed; unparseable values fall back to lexical order so the sort stays
/// total.
fn ts_cmp_str(a: &str, b: &str) -> std::cmp::Ordering {
    match (history::parse_ts(a), history::parse_ts(b)) {
        (Some(a), Some(b)) => a.cmp(&b),
        _ => a.cmp(b),
    }
}

/// Go `appendUnique` (orphan.go:612).
fn append_unique(slice: &mut Vec<String>, s: &str) {
    if !slice.iter().any(|existing| existing == s) {
        slice.push(s.to_string());
    }
}

/// Go `accumulateBeadReference` (file_index.go:815) — merge a second sighting of
/// the same bead into the map built by the prefix/glob arms.
fn accumulate_bead_reference(
    refs: &mut BTreeMap<String, BeadReference>,
    mut reference: BeadReference,
) {
    let Some(existing) = refs.get_mut(&reference.bead_id) else {
        refs.insert(reference.bead_id.clone(), reference);
        return;
    };
    if !reference.title.is_empty() {
        existing.title = std::mem::take(&mut reference.title);
    }
    if !reference.status.is_empty() {
        existing.status = std::mem::take(&mut reference.status);
    }
    let shas = std::mem::take(&mut reference.commit_shas);
    for sha in &shas {
        append_unique(&mut existing.commit_shas, sha);
    }
    if ts_cmp_str(&reference.last_touch, &existing.last_touch) == std::cmp::Ordering::Greater {
        existing.last_touch = std::mem::take(&mut reference.last_touch);
    }
    existing.total_changes += reference.total_changes;
}

/// Go `beadReferencesFromMap` (file_index.go:839).
fn bead_references_from_map(refs: BTreeMap<String, BeadReference>) -> Vec<BeadReference> {
    refs.into_values()
        .map(|mut reference| {
            reference.commit_shas.sort();
            reference
        })
        .collect()
}

impl FileLookup {
    /// Go `NewFileLookup` — build the file index and the co-change matrix from a
    /// history report.
    pub fn new(report: &HistoryReport) -> Self {
        Self {
            index: build_file_index(report),
            beads: report
                .histories
                .iter()
                .map(|(id, h)| (id.clone(), (h.title.clone(), h.status.clone())))
                .collect(),
            co_change: CoChangeMatrix::build(report),
        }
    }

    /// Go `LookupByFile` — beads that touched `path`. `path` may be an exact
    /// file or a directory prefix.
    pub fn lookup_by_file(&self, path: &str) -> FileBeadLookupResult {
        let normalized_path = normalize_path(path);
        let mut result = FileBeadLookupResult {
            file_path: path.to_string(),
            open_beads: Vec::new(),
            closed_beads: Vec::new(),
            total_beads: 0,
        };

        // Exact match first.
        if let Some(refs) = self.index.file_to_beads.get(&normalized_path) {
            for reference in refs {
                let reference = self.refresh(reference);
                let (bucket, skip) = classify_bead_status(&reference.status);
                if skip {
                    continue;
                }
                if bucket == "closed" {
                    result.closed_beads.push(reference);
                } else {
                    result.open_beads.push(reference);
                }
            }
            sort_bead_refs(&mut result.open_beads);
            sort_bead_refs(&mut result.closed_beads);
            result.total_beads = result.open_beads.len() + result.closed_beads.len();
            return result;
        }

        // Prefix match, for directory lookups. `normalize_path` has already
        // folded backslashes to forward slashes, so only "/" needs checking.
        let mut open_refs: BTreeMap<String, BeadReference> = BTreeMap::new();
        let mut closed_refs: BTreeMap<String, BeadReference> = BTreeMap::new();
        let prefix = format!("{normalized_path}/");
        for (file_path, refs) in &self.index.file_to_beads {
            if !file_path.starts_with(&prefix) {
                continue;
            }
            for reference in refs {
                let reference = self.refresh(reference);
                let (bucket, skip) = classify_bead_status(&reference.status);
                if skip {
                    continue;
                }
                if bucket == "closed" {
                    accumulate_bead_reference(&mut closed_refs, reference);
                } else {
                    accumulate_bead_reference(&mut open_refs, reference);
                }
            }
        }

        result.open_beads = bead_references_from_map(open_refs);
        result.closed_beads = bead_references_from_map(closed_refs);
        sort_bead_refs(&mut result.open_beads);
        sort_bead_refs(&mut result.closed_beads);
        result.total_beads = result.open_beads.len() + result.closed_beads.len();
        result
    }

    /// Go `LookupByFileGlob` — beads for files matching a glob pattern.
    pub fn lookup_by_file_glob(&self, pattern: &str) -> FileBeadLookupResult {
        let mut open_refs: BTreeMap<String, BeadReference> = BTreeMap::new();
        let mut closed_refs: BTreeMap<String, BeadReference> = BTreeMap::new();

        for (file_path, refs) in &self.index.file_to_beads {
            if !glob_match(pattern, file_path) {
                continue;
            }
            for reference in refs {
                let reference = self.refresh(reference);
                let (bucket, skip) = classify_bead_status(&reference.status);
                if skip {
                    continue;
                }
                if bucket == "closed" {
                    accumulate_bead_reference(&mut closed_refs, reference);
                } else {
                    accumulate_bead_reference(&mut open_refs, reference);
                }
            }
        }

        let mut result = FileBeadLookupResult {
            file_path: pattern.to_string(),
            open_beads: bead_references_from_map(open_refs),
            closed_beads: bead_references_from_map(closed_refs),
            total_beads: 0,
        };
        sort_bead_refs(&mut result.open_beads);
        sort_bead_refs(&mut result.closed_beads);
        result.total_beads = result.open_beads.len() + result.closed_beads.len();
        result
    }

    /// Go `GetAllFiles` — every file in the index, sorted by path.
    pub fn all_files(&self) -> Vec<String> {
        self.index.file_to_beads.keys().cloned().collect()
    }

    /// Go `GetStats`.
    pub fn stats(&self) -> FileIndexStats {
        self.index.stats
    }

    /// Go `GetHotspots` — files touched by the most beads (conflict zones).
    ///
    /// Bead counting uses the same `classify_bead_status` on the *current*
    /// status from `beads` as [`Self::lookup_by_file`], so the three robot
    /// surfaces built on this index cannot disagree (Go #184). Tombstoned
    /// beads are not counted, and a file whose every bead is skipped is
    /// omitted entirely.
    ///
    /// `limit` is Go's `int`, kept signed so its guard is a literal port:
    /// `limit <= 0 || limit > len(counts)` means "every hotspot", so both 0
    /// and a negative value mean *all*, not none.
    pub fn get_hotspots(&self, limit: i64) -> Vec<FileHotspot> {
        let mut counts: Vec<FileHotspot> = Vec::new();
        for (path, refs) in &self.index.file_to_beads {
            let mut open_count = 0usize;
            let mut closed_count = 0usize;
            for reference in refs {
                // The status recorded at index time may be stale; the report is
                // the authority.
                let status = self
                    .beads
                    .get(&reference.bead_id)
                    .map_or(reference.status.as_str(), |(_, status)| status.as_str());
                let (bucket, skip) = classify_bead_status(status);
                if skip {
                    continue;
                }
                if bucket == "closed" {
                    closed_count += 1;
                } else {
                    open_count += 1;
                }
            }
            let total = open_count + closed_count;
            if total == 0 {
                continue;
            }
            counts.push(FileHotspot {
                file_path: path.clone(),
                total_beads: total,
                open_beads: open_count,
                closed_beads: closed_count,
            });
        }

        // Count descending; path breaks ties for deterministic output.
        counts.sort_by(|a, b| {
            b.total_beads
                .cmp(&a.total_beads)
                .then_with(|| a.file_path.cmp(&b.file_path))
        });

        let limit = if limit <= 0 || limit > counts.len() as i64 {
            counts.len()
        } else {
            limit as usize
        };
        counts.truncate(limit);
        counts
    }

    /// Go `GetRelatedFiles` — files that frequently co-change with `file_path`.
    /// `threshold` is the minimum correlation (defaulting to 0.5 when
    /// non-positive); `limit` caps the result (defaulting to 10).
    pub fn get_related_files(
        &self,
        file_path: &str,
        threshold: f64,
        limit: usize,
    ) -> CoChangeResult {
        self.co_change
            .get_related_files(file_path, threshold, limit)
    }

    /// Go `GetCoChangeMatrix` — the underlying matrix for advanced queries.
    pub fn co_change_matrix(&self) -> &CoChangeMatrix {
        &self.co_change
    }

    /// Go `ImpactAnalysis` — impact of modifying `files`, evaluated at the wall
    /// clock.
    pub fn impact_analysis(&self, files: &[String]) -> ImpactResult {
        self.impact_analysis_at(files, jiff::Timestamp::now())
    }

    /// Go `ImpactAnalysisAt` (file_index.go:595) — the same analysis evaluated
    /// at a caller-owned instant, which is what keeps the recency filter and
    /// the derived `relevance` scores deterministic for robot callers.
    pub fn impact_analysis_at(&self, files: &[String], now: jiff::Timestamp) -> ImpactResult {
        // Go file_index.go:596-603 — note the pre-seeded `Files: []string{}`
        // and `Warnings: []string{}`, which is why the early returns below emit
        // `[]` rather than `null`.
        let mut result = ImpactResult {
            files: Vec::new(),
            affected_beads: Vec::new(),
            risk_level: "low".to_string(),
            risk_score: 0.0,
            warnings: Vec::new(),
            summary: String::new(),
        };

        if files.is_empty() {
            result.summary = "No files to analyze".to_string();
            return result;
        }

        // Go file_index.go:610-621 — normalize, drop blank paths, dedupe while
        // preserving first-occurrence order. `normalizePath` runs BEFORE
        // `TrimSpace`, so a path that is only whitespace normalizes to "" and is
        // dropped; a path with a leading space keeps it until the trim.
        let mut seen: HashSet<String> = HashSet::new();
        let mut normalized_files: Vec<String> = Vec::with_capacity(files.len());
        for f in files {
            let norm = normalize_path(f).trim().to_string();
            if norm.is_empty() {
                continue;
            }
            if seen.insert(norm.clone()) {
                normalized_files.push(norm);
            }
        }

        if normalized_files.is_empty() {
            result.summary = "No valid files to analyze".to_string();
            return result;
        }

        result.files = normalized_files.clone();
        let mut bead_map: BTreeMap<String, AffectedBead> = BTreeMap::new();

        // Go file_index.go:630 does `now = now.UTC()`. It has no effect on what
        // follows — every remaining use of `now` is a `Sub` against another
        // instant — so it is deliberately not reproduced here.

        for file_path in &normalized_files {
            let lookup = self.lookup_by_file(file_path);

            for reference in lookup.open_beads {
                accumulate_affected_bead(&mut bead_map, &reference, file_path);
            }

            for reference in lookup.closed_beads {
                // Go file_index.go:656 — a closed bead drops out of the impact
                // analysis once it has been untouched for a week.
                if is_older_than(&reference.last_touch, now, SEVEN_DAYS) {
                    continue;
                }
                accumulate_affected_bead(&mut bead_map, &reference, file_path);
            }
        }

        let mut open_count = 0usize;
        let mut in_progress_count = 0usize;
        let mut recent_closed_count = 0usize;

        for ab in bead_map.values_mut() {
            // Go file_index.go:684-686 — recency over a 7-day window, clamped.
            let days_since = days_since(&ab.last_activity, now);
            let recency_score = (1.0 - (days_since / 7.0)).clamp(0.0, 1.0);
            let overlap_score = ab.overlap_count as f64 / normalized_files.len() as f64;
            // Go compares the status with `==` here (file_index.go:693-701) —
            // no trimming or lowercasing, unlike the sort priority below. That
            // asymmetry is preserved: anything that is not exactly "open" or
            // "in_progress" counts as recently closed.
            let status_multiplier = match ab.status.as_str() {
                "in_progress" => {
                    in_progress_count += 1;
                    1.0
                }
                "open" => {
                    open_count += 1;
                    0.8
                }
                _ => {
                    recent_closed_count += 1;
                    0.5
                }
            };
            ab.relevance = recency_score * 0.4 + overlap_score * 0.4 + status_multiplier * 0.2;
        }

        result.affected_beads = bead_map.into_values().collect();

        // Go file_index.go:706-715. The comparator is total — bead IDs are
        // unique — so the result does not depend on map iteration order, which
        // is what lets this port iterate a BTreeMap where Go ranges a map.
        result.affected_beads.sort_by(|a, b| {
            affected_bead_status_priority(&a.status)
                .cmp(&affected_bead_status_priority(&b.status))
                .then_with(|| {
                    b.relevance
                        .partial_cmp(&a.relevance)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| a.bead_id.cmp(&b.bead_id))
        });

        // Go file_index.go:717-723. The expression order is preserved exactly:
        // summing the three weighted counts left to right is what makes
        // `3 closed` land on 0.15000000000000002 rather than 0.15.
        result.risk_score = in_progress_count as f64 * 0.4
            + open_count as f64 * 0.2
            + recent_closed_count as f64 * 0.05;
        if normalized_files.len() > 3 {
            result.risk_score += 0.1;
        }
        if result.risk_score > 1.0 {
            result.risk_score = 1.0;
        }

        result.risk_level = if result.risk_score >= 0.7 {
            "critical"
        } else if result.risk_score >= 0.4 {
            "high"
        } else if result.risk_score >= 0.2 {
            "medium"
        } else {
            "low"
        }
        .to_string();

        if in_progress_count > 0 {
            result.warnings.push(
                "Active work in progress on these files - coordinate before making changes"
                    .to_string(),
            );
        }
        if open_count > 0 {
            result
                .warnings
                .push("Open beads touch these files - review before modifying".to_string());
        }

        result.summary = build_impact_summary(in_progress_count, open_count, recent_closed_count);
        result
    }

    /// Re-read a bead's title/status from the report, which may have moved on
    /// since the index was built (Go's inline `fl.beads[ref.BeadID]` refresh).
    fn refresh(&self, reference: &BeadReference) -> BeadReference {
        let Some((title, status)) = self.beads.get(&reference.bead_id) else {
            return reference.clone();
        };
        BeadReference {
            title: title.clone(),
            status: status.clone(),
            ..reference.clone()
        }
    }
}

/// Go `BuildFileIndex` — map every file a correlated commit touched to the beads
/// behind that commit.
///
/// Beads whose status classifies as skip (tombstone) are excluded at build time
/// so every consumer of this index agrees on the same bead set (Go #184).
pub fn build_file_index(report: &HistoryReport) -> FileBeadIndex {
    // file → beadID → reference, for deduplication.
    let mut file_bead_map: HashMap<String, BTreeMap<String, BeadReference>> = HashMap::new();

    for (bead_id, history) in &report.histories {
        if classify_bead_status(&history.status).1 {
            continue;
        }
        for commit in history.commits.iter().flatten() {
            for file in &commit.files {
                let entry = file_bead_map
                    .entry(normalize_path(&file.path))
                    .or_default()
                    .entry(bead_id.clone())
                    .or_insert_with(|| BeadReference {
                        bead_id: bead_id.clone(),
                        title: history.title.clone(),
                        status: history.status.clone(),
                        last_touch: commit.timestamp.clone(),
                        ..Default::default()
                    });
                append_unique(&mut entry.commit_shas, &commit.short_sha);
                if ts_cmp_str(&commit.timestamp, &entry.last_touch) == std::cmp::Ordering::Greater {
                    entry.last_touch = commit.timestamp.clone();
                }
                entry.total_changes += file.insertions + file.deletions;
            }
        }
    }

    let mut file_to_beads = BTreeMap::new();
    let mut total_links = 0usize;
    let mut multiple_beads_count = 0usize;

    for (file_path, bead_map) in file_bead_map {
        let mut refs: Vec<BeadReference> = bead_map.into_values().collect();
        for reference in &mut refs {
            reference.commit_shas.sort();
        }
        // Most recent first, bead ID breaking ties.
        sort_bead_refs(&mut refs);
        total_links += refs.len();
        if refs.len() > 1 {
            multiple_beads_count += 1;
        }
        file_to_beads.insert(file_path, refs);
    }

    let stats = FileIndexStats {
        total_files: file_to_beads.len(),
        total_bead_links: total_links,
        files_with_multiple_beads: multiple_beads_count,
    };
    FileBeadIndex {
        file_to_beads,
        stats,
    }
}

/// Go `ImpactResult` (file_index.go:567-574) — what beads might be affected if
/// the analyzed files are modified.
///
/// Field order matches Go's declaration, which is the wire order of the
/// library-level result. The Go handler re-projects these into its own struct
/// in a different order, so a robot payload built from this type is not
/// necessarily in the handler's order.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ImpactResult {
    pub files: Vec<String>,
    pub affected_beads: Vec<AffectedBead>,
    pub risk_level: String,
    pub risk_score: f64,
    pub warnings: Vec<String>,
    pub summary: String,
}

/// Go `AffectedBead` (file_index.go:577-586) — a bead that touches one or more
/// of the analyzed files.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AffectedBead {
    pub bead_id: String,
    pub title: String,
    pub status: String,
    pub overlap_files: Vec<String>,
    pub overlap_count: usize,
    /// Most recent `last_touch` across every overlapping file, as stored in
    /// the report (Go marshals its `time.Time` to the same RFC3339 text).
    pub last_activity: String,
    pub relevance: f64,
    /// Sum of the per-file `total_changes` of every overlapping file.
    pub total_changes: i64,
}

/// Go's `7*24*time.Hour` recency window in [`FileLookup::impact_analysis_at`].
const SEVEN_DAYS: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 60 * 60);

/// Nanoseconds from `then` to `now` — Go's `now.Sub(t)`, negative when `then`
/// is in the future.
fn age_ns(then: &str, now: jiff::Timestamp) -> Option<i128> {
    history::parse_ts(then).map(|t| now.as_nanosecond() - t.as_nanosecond())
}

/// Days between `then` and `now`, as Go's `now.Sub(t).Hours() / 24`.
///
/// The two divisions are kept separate because Go's `Duration.Hours()` is
/// `float64(d) / float64(time.Hour)` and the `/ 24` is a second, separate
/// division at the call site. Folding them into one `ns / 86_400e9` can land a
/// ULP away from Go's answer and change a printed `relevance`.
///
/// A timestamp that does not parse falls back to 0.0 — the most recent
/// possible position — so a malformed `last_touch` inflates rather than
/// suppresses impact. Go cannot reach this arm: it reads a `time.Time` that
/// git already parsed.
fn days_since(then: &str, now: jiff::Timestamp) -> f64 {
    match age_ns(then, now) {
        Some(age) => (age as f64 / 3_600e9) / 24.0,
        None => 0.0,
    }
}

/// Whether `then` is more than `window` older than `now`.
///
/// A `then` in the *future* is not older, so the bead is kept — Go's
/// `now.Sub(t)` goes negative there and a negative duration is never greater
/// than the window. Reading the age as unsigned would instead drop the bead,
/// which is the opposite of Go.
///
/// See [`days_since`] for the unparseable-timestamp policy.
fn is_older_than(then: &str, now: jiff::Timestamp, window: std::time::Duration) -> bool {
    match age_ns(then, now) {
        Some(age) => age > window.as_nanos() as i128,
        None => false,
    }
}

/// Go's per-file `ab.OverlapFiles = append(...)` / `TotalChanges +=` block
/// (file_index.go:636-652), shared by the open and closed arms.
///
/// The first sighting seeds title, status and `last_activity`; later sightings
/// only widen the overlap, add up changes, and push `last_activity` forward.
fn accumulate_affected_bead(
    bead_map: &mut BTreeMap<String, AffectedBead>,
    reference: &BeadReference,
    file_path: &str,
) {
    let ab = bead_map
        .entry(reference.bead_id.clone())
        .or_insert_with(|| AffectedBead {
            bead_id: reference.bead_id.clone(),
            title: reference.title.clone(),
            status: reference.status.clone(),
            overlap_files: Vec::new(),
            overlap_count: 0,
            last_activity: reference.last_touch.clone(),
            relevance: 0.0,
            total_changes: 0,
        });
    ab.overlap_files.push(file_path.to_string());
    ab.overlap_count = ab.overlap_files.len();
    ab.total_changes += reference.total_changes;
    if ts_cmp_str(&reference.last_touch, &ab.last_activity) == std::cmp::Ordering::Greater {
        ab.last_activity = reference.last_touch.clone();
    }
}

/// Go `affectedBeadStatusPriority` (file_index.go:795-804) — sort key, lower
/// first: in-progress, then everything else, then closed.
///
/// Unlike the status *multiplier* in `impact_analysis_at`, this one does
/// normalize case and whitespace.
pub fn affected_bead_status_priority(status: &str) -> u8 {
    match status.trim().to_lowercase().as_str() {
        "in_progress" => 0,
        "closed" => 2,
        _ => 1,
    }
}

/// Go `pluralize` (file_index.go:849-854).
fn pluralize(count: usize, singular: &str) -> String {
    if count == 1 {
        singular.to_string()
    } else {
        format!("{singular}s")
    }
}

/// Go's summary assembly (file_index.go:743-762), lifted out of the handler so
/// it is unit-testable on its own.
fn build_impact_summary(
    in_progress_count: usize,
    open_count: usize,
    recent_closed_count: usize,
) -> String {
    if in_progress_count + open_count + recent_closed_count == 0 {
        return "No beads found touching these files - safe to proceed".to_string();
    }
    let mut parts: Vec<String> = Vec::new();
    if in_progress_count > 0 {
        parts.push(format!(
            "{in_progress_count} {} in progress",
            pluralize(in_progress_count, "bead")
        ));
    }
    if open_count > 0 {
        parts.push(format!(
            "{open_count} open {}",
            pluralize(open_count, "bead")
        ));
    }
    if recent_closed_count > 0 {
        parts.push(format!(
            "{recent_closed_count} recently closed {}",
            pluralize(recent_closed_count, "bead")
        ));
    }
    let prefix = if in_progress_count > 0 {
        "⚠️ Conflict risk: "
    } else {
        "Found "
    };
    format!("{prefix}{} touching these files", parts.join(", "))
}

/// Go `CoChangeEntry` — a file that frequently co-changes with another file.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CoChangeEntry {
    /// The related file.
    pub file_path: String,
    /// Commits where both files changed.
    pub co_change_count: usize,
    /// Total commits touching the source file.
    pub total_commits: usize,
    /// `co_change_count / total_commits`, in 0.0-1.0.
    pub correlation: f64,
    /// Up to 3 sample full commit SHAs, sorted.
    pub sample_commits: Vec<String>,
}

/// Go `CoChangeResult`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CoChangeResult {
    /// The queried file.
    pub file_path: String,
    /// Total commits touching this file.
    pub total_commits: usize,
    /// Files that co-change, sorted by correlation descending.
    pub related_files: Vec<CoChangeEntry>,
    /// The effective minimum correlation threshold used.
    pub threshold: f64,
}

/// Go `CoChangeMatrix` — which files frequently change together.
#[derive(Debug, Clone, Default)]
pub struct CoChangeMatrix {
    /// file → related file → count of commits where both changed.
    matrix: BTreeMap<String, BTreeMap<String, usize>>,
    /// file → total commits touching that file.
    file_commit_counts: BTreeMap<String, usize>,
    /// Full commit SHA → files changed in that commit, for sample selection.
    commit_files: BTreeMap<String, Vec<String>>,
}

impl CoChangeMatrix {
    /// Go `GetRelatedFiles` — files that co-change with `file_path`.
    ///
    /// A non-positive `threshold` maps back to Go's `0.5` default; a zero
    /// `limit` maps to 10.
    pub fn get_related_files(
        &self,
        file_path: &str,
        threshold: f64,
        limit: usize,
    ) -> CoChangeResult {
        let threshold = if threshold <= 0.0 { 0.5 } else { threshold };
        let limit = if limit == 0 { 10 } else { limit };
        let normalized_path = normalize_path(file_path);

        let total_commits = self
            .file_commit_counts
            .get(&normalized_path)
            .copied()
            .unwrap_or(0);
        let mut result = CoChangeResult {
            file_path: file_path.to_string(),
            total_commits,
            related_files: Vec::new(),
            threshold,
        };
        if total_commits == 0 {
            // File not found in history.
            return result;
        }
        let Some(related) = self.matrix.get(&normalized_path) else {
            // No co-changes found.
            return result;
        };

        let mut entries: Vec<CoChangeEntry> = related
            .iter()
            .filter_map(|(related_file, &count)| {
                let correlation = count as f64 / total_commits as f64;
                if correlation < threshold {
                    return None;
                }
                Some(CoChangeEntry {
                    file_path: related_file.clone(),
                    co_change_count: count,
                    total_commits,
                    correlation,
                    sample_commits: self.sample_commits(&normalized_path, related_file),
                })
            })
            .collect();

        // Correlation descending, path breaking ties. The float comparison is
        // exact: every value here is count/total for one shared denominator.
        entries.sort_by(|a, b| {
            b.correlation
                .partial_cmp(&a.correlation)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.file_path.cmp(&b.file_path))
        });
        entries.truncate(limit);
        result.related_files = entries;
        result
    }

    /// Total commits touching `file_path` (normalized).
    pub fn total_commits(&self, file_path: &str) -> usize {
        self.file_commit_counts
            .get(&normalize_path(file_path))
            .copied()
            .unwrap_or(0)
    }

    /// Commits where both files changed. Collected then sorted — taking the
    /// first three straight from a map made robot output vary between runs.
    fn sample_commits(&self, normalized_path: &str, related_file: &str) -> Vec<String> {
        let mut sample_commits: Vec<String> = self
            .commit_files
            .iter()
            .filter(|(_, files)| {
                files.iter().any(|f| f == normalized_path)
                    && files.iter().any(|f| f == related_file)
            })
            .map(|(sha, _)| sha.clone())
            .collect();
        sample_commits.sort();
        sample_commits.truncate(3);
        sample_commits
    }
}

/// Go `BuildCoChangeMatrix` — analyze which files frequently change together in
/// the same commits.
///
/// Note the deliberate asymmetry with [`build_file_index`]: Go filters
/// tombstones there (file_index.go:70-72) but not here (file_index.go:430-432),
/// so a tombstoned bead's commits still count toward co-change. This port keeps
/// that difference rather than "fixing" it, because co-change is a property of
/// commit history rather than of current bead membership.
pub fn build_co_change_matrix(report: &HistoryReport) -> CoChangeMatrix {
    CoChangeMatrix::build(report)
}

impl CoChangeMatrix {
    fn build(report: &HistoryReport) -> Self {
        let mut matrix = CoChangeMatrix::default();
        // A commit can appear in several bead histories; count it once.
        let mut processed_commits: HashSet<&str> = HashSet::new();

        for history in report.histories.values() {
            for commit in history.commits.iter().flatten() {
                if !processed_commits.insert(commit.sha.as_str()) {
                    continue;
                }

                // Normalize, dedupe, then sort this commit's file list.
                let mut files: Vec<String> = Vec::new();
                let mut seen_files: HashSet<String> = HashSet::new();
                for file in &commit.files {
                    let normalized = normalize_path(&file.path);
                    if normalized.is_empty() {
                        continue;
                    }
                    if !seen_files.insert(normalized.clone()) {
                        continue;
                    }
                    files.push(normalized);
                }
                files.sort();

                // Keyed by the full SHA: seven-character prefixes are not unique
                // and can otherwise overwrite one another in long histories.
                matrix
                    .commit_files
                    .insert(commit.sha.clone(), files.clone());

                for file in &files {
                    *matrix.file_commit_counts.entry(file.clone()).or_insert(0) += 1;
                }

                // All ordered pairs; the self-relationship is skipped.
                for file_a in &files {
                    let row = matrix.matrix.entry(file_a.clone()).or_default();
                    for file_b in &files {
                        if file_a == file_b {
                            continue;
                        }
                        *row.entry(file_b.clone()).or_insert(0) += 1;
                    }
                }
            }
        }

        matrix
    }
}

/// Go `filepath.Match` — shell-style glob over `/`-separated path segments.
///
/// `*` does not cross `/` and `?` matches exactly one non-`/` character, as in
/// Go. Returns false for a malformed pattern, mirroring Go's `(false, err)` arm
/// at file_index.go:260.
fn glob_match(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    glob_match_inner(&pattern, &name)
}

fn glob_match_inner(pattern: &[char], name: &[char]) -> bool {
    match pattern.first() {
        None => name.is_empty(),
        Some('*') => {
            // `*` matches any run of non-separator characters.
            for skip in 0..=name.len() {
                if skip > 0 && name[skip - 1] == '/' {
                    break;
                }
                if glob_match_inner(&pattern[1..], &name[skip..]) {
                    return true;
                }
            }
            false
        }
        Some('?') => {
            !name.is_empty() && name[0] != '/' && glob_match_inner(&pattern[1..], &name[1..])
        }
        Some('[') => {
            match match_class(pattern, name) {
                Some((consumed, matched)) => {
                    // A class consumes one name character; an empty name can
                    // never satisfy it (this also guards the `name[1..]`
                    // slice below — a negated class would otherwise report a
                    // match against nothing).
                    !name.is_empty()
                        && matched
                        && glob_match_inner(&pattern[consumed..], &name[1..])
                }
                // Unterminated class: Go treats the '[' as a literal.
                None => {
                    !name.is_empty()
                        && name[0] == '['
                        && glob_match_inner(&pattern[1..], &name[1..])
                }
            }
        }
        Some(c) => !name.is_empty() && name[0] == *c && glob_match_inner(&pattern[1..], &name[1..]),
    }
}

/// Match a `[...]` class. Returns the pattern length consumed and whether the
/// first name character matched, or `None` when the class is unterminated.
fn match_class(pattern: &[char], name: &[char]) -> Option<(usize, bool)> {
    let mut i = 1usize; // past '['
                        // Go `filepath.Match` negates a class only with '^'; '!' is a literal
                        // member, so `[!b]` matches '!' or 'b' and `[^b]` matches anything but 'b'.
    let negated = pattern.get(i) == Some(&'^');
    if negated {
        i += 1;
    }
    let mut matched = false;
    let mut first = true;
    // A `]` immediately after the (optional) `^` is a literal.
    while i < pattern.len() {
        if pattern[i] == ']' && !first {
            i += 1;
            return Some((i, matched != negated));
        }
        first = false;
        let lo = pattern[i];
        if pattern.get(i + 1) == Some(&'-') && pattern.get(i + 2).is_some_and(|c| *c != ']') {
            let hi = pattern[i + 2];
            if !name.is_empty() && name[0] >= lo && name[0] <= hi {
                matched = true;
            }
            i += 3;
        } else {
            if !name.is_empty() && name[0] == lo {
                matched = true;
            }
            i += 1;
        }
    }
    None // unterminated class
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::HistoryReport;
    use crate::history::{
        BeadHistory, BeadMilestones, FileChange, HistoryCommit, HistoryStats, HistoryWindow,
    };
    use std::collections::BTreeMap;

    fn commit(sha: &str, ts: &str, files: &[(&str, i64, i64)]) -> HistoryCommit {
        HistoryCommit {
            bead_id: String::new(),
            sha: sha.to_string(),
            short_sha: sha.chars().take(7).collect(),
            message: String::new(),
            author: String::new(),
            author_email: String::new(),
            timestamp: ts.to_string(),
            files: files
                .iter()
                .map(|(path, ins, del)| FileChange {
                    path: path.to_string(),
                    action: "M".into(),
                    insertions: *ins,
                    deletions: *del,
                })
                .collect(),
            method: "explicit_id",
            methods: vec!["explicit_id".into()],
            confidence: 1.0,
            reason: String::new(),
            confirmed: false,
        }
    }

    fn history(
        bead_id: &str,
        title: &str,
        status: &str,
        commits: Vec<HistoryCommit>,
    ) -> BeadHistory {
        BeadHistory {
            bead_id: bead_id.into(),
            title: title.into(),
            status: status.into(),
            events: Vec::new(),
            milestones: BeadMilestones::default(),
            commits: Some(commits),
            cycle_time: None,
            last_author: String::new(),
        }
    }

    fn report(histories: BTreeMap<String, BeadHistory>) -> HistoryReport {
        HistoryReport {
            generated_at: "2026-01-01T00:00:00Z".into(),
            data_hash: String::new(),
            git_range: String::new(),
            latest_commit_sha: String::new(),
            window: HistoryWindow {
                revision: String::new(),
                limit: 0,
                since: None,
                until: None,
                commits: 0,
            },
            stats: HistoryStats {
                total_beads: 0,
                beads_with_commits: 0,
                total_commits: 0,
                unique_authors: 0,
                avg_commits_per_bead: 0.0,
                avg_cycle_time_days: None,
                method_distribution: BTreeMap::new(),
                strategies: None,
                feedback_applied: None,
            },
            commit_index: crate::history::build_commit_index(&histories),
            histories,
            causal_history: None,
        }
    }

    #[test]
    fn normalize_path_strips_one_prefix_and_one_suffix() {
        // Go uses TrimPrefix/TrimSuffix, so a doubled slash leaves one behind.
        assert_eq!(normalize_path("./a/b.rs"), "a/b.rs");
        assert_eq!(normalize_path(".\\a\\b.rs"), "a/b.rs");
        assert_eq!(normalize_path("a/b/"), "a/b");
        assert_eq!(normalize_path("a//"), "a/");
        // Only one leading "./" is removed, even when doubled up.
        assert_eq!(normalize_path("././/a"), ".//a");
    }

    #[test]
    fn classify_bead_status_matches_go() {
        assert_eq!(classify_bead_status("tombstone"), ("", true));
        assert_eq!(classify_bead_status("  TOMBSTONE "), ("", true));
        assert_eq!(classify_bead_status("closed"), ("closed", false));
        assert_eq!(classify_bead_status("open"), ("open", false));
        assert_eq!(classify_bead_status("in_progress"), ("open", false));
        assert_eq!(classify_bead_status(""), ("open", false));
    }

    /// bd-1 (closed) touches a.rs in two commits and b.rs in one; bd-2 (open)
    /// touches b.rs. SHAs are 8 chars so their 7-char prefixes stay distinct.
    fn sample_report() -> HistoryReport {
        let mut histories = BTreeMap::new();
        histories.insert(
            "bd-1".into(),
            history(
                "bd-1",
                "first",
                "closed",
                vec![
                    commit(
                        "aaaaaa11",
                        "2026-01-01T00:00:00Z",
                        &[("src/a.rs", 10, 2), ("src/b.rs", 5, 1)],
                    ),
                    commit("aaaaaa22", "2026-01-02T00:00:00Z", &[("src/a.rs", 3, 0)]),
                ],
            ),
        );
        histories.insert(
            "bd-2".into(),
            history(
                "bd-2",
                "second",
                "open",
                vec![commit(
                    "bbbbbb11",
                    "2026-01-03T00:00:00Z",
                    &[("src/b.rs", 7, 3)],
                )],
            ),
        );
        report(histories)
    }

    /// `sample_report` plus a tombstoned bead that also touched a.rs, for
    /// testing the two different tombstone policies.
    fn tombstoned_report() -> HistoryReport {
        let mut histories = BTreeMap::new();
        histories.insert(
            "bd-1".into(),
            history(
                "bd-1",
                "first",
                "closed",
                vec![commit(
                    "aaaaaa11",
                    "2026-01-01T00:00:00Z",
                    &[("src/a.rs", 10, 2)],
                )],
            ),
        );
        histories.insert(
            "bd-2".into(),
            history(
                "bd-2",
                "gone",
                "tombstone",
                vec![commit(
                    "cccccc11",
                    "2026-01-04T00:00:00Z",
                    &[("src/a.rs", 1, 1)],
                )],
            ),
        );
        report(histories)
    }

    #[test]
    fn build_file_index_accumulates_changes_and_keeps_distinct_shas() {
        let index = build_file_index(&sample_report());
        assert_eq!(index.stats.total_files, 2);
        assert_eq!(index.stats.total_bead_links, 3, "a.rs×1 + b.rs×2");
        assert_eq!(
            index.stats.files_with_multiple_beads, 1,
            "only b.rs has two beads"
        );
        let a_refs = &index.file_to_beads["src/a.rs"];
        assert_eq!(a_refs.len(), 1);
        assert_eq!(a_refs[0].bead_id, "bd-1");
        // Short SHAs, deduped and sorted.
        assert_eq!(a_refs[0].commit_shas, vec!["aaaaaa1", "aaaaaa2"]);
        assert_eq!(a_refs[0].total_changes, 10 + 2 + 3);
        assert_eq!(a_refs[0].last_touch, "2026-01-02T00:00:00Z");
    }

    #[test]
    fn build_file_index_excludes_tombstoned_beads() {
        let index = build_file_index(&tombstoned_report());
        let a_refs = &index.file_to_beads["src/a.rs"];
        assert_eq!(a_refs.len(), 1, "tombstoned bd-2 must not reach the index");
        assert_eq!(a_refs[0].bead_id, "bd-1");
    }

    #[test]
    fn co_change_matrix_keeps_tombstoned_beads() {
        // The mirror image of the test above: Go filters tombstones when
        // building the file index but not when building the co-change matrix,
        // so bd-2's commit still counts toward a.rs.
        let lookup = FileLookup::new(&tombstoned_report());
        assert_eq!(lookup.co_change_matrix().total_commits("src/a.rs"), 2);
        assert_eq!(
            build_file_index(&tombstoned_report())
                .stats
                .total_bead_links,
            1
        );
    }

    #[test]
    fn lookup_by_file_splits_open_and_closed_and_counts_total() {
        let lookup = FileLookup::new(&sample_report());
        let result = lookup.lookup_by_file("src/b.rs");
        assert_eq!(result.file_path, "src/b.rs");
        assert_eq!(result.total_beads, 2);
        assert_eq!(
            result
                .open_beads
                .iter()
                .map(|r| r.bead_id.as_str())
                .collect::<Vec<_>>(),
            vec!["bd-2"]
        );
        assert_eq!(
            result
                .closed_beads
                .iter()
                .map(|r| r.bead_id.as_str())
                .collect::<Vec<_>>(),
            vec!["bd-1"]
        );
    }

    #[test]
    fn lookup_by_file_prefix_merges_across_a_directory() {
        let lookup = FileLookup::new(&sample_report());
        // The exact arm misses, so the prefix arm runs and accumulates bd-1
        // across both of its files instead of returning it twice.
        let result = lookup.lookup_by_file("src");
        assert_eq!(result.total_beads, 2);
        let closed = &result.closed_beads[0];
        assert_eq!(closed.bead_id, "bd-1");
        assert_eq!(closed.total_changes, 10 + 2 + 5 + 1 + 3);
        assert_eq!(closed.commit_shas, vec!["aaaaaa1", "aaaaaa2"]);
    }

    #[test]
    fn lookup_by_file_unknown_path_is_empty() {
        let lookup = FileLookup::new(&sample_report());
        let result = lookup.lookup_by_file("nope.rs");
        assert_eq!(result.total_beads, 0);
        assert!(result.open_beads.is_empty());
        assert!(result.closed_beads.is_empty());
    }

    #[test]
    fn co_change_matrix_counts_a_commit_once() {
        let lookup = FileLookup::new(&sample_report());
        let matrix = lookup.co_change_matrix();
        // a.rs is in aaaaaa11 and aaaaaa22; b.rs in aaaaaa11 and bbbbbb11.
        assert_eq!(matrix.total_commits("src/a.rs"), 2);
        assert_eq!(matrix.total_commits("src/b.rs"), 2);
    }

    #[test]
    fn related_files_default_threshold_is_half_not_zero() {
        let lookup = FileLookup::new(&sample_report());
        // threshold 0.0 maps back to Go's 0.5 default: b.rs co-changed with
        // a.rs in 1 of the 2 commits touching a.rs, so it is exactly at 0.5.
        // A threshold of 0.0 must NOT be read as "include everything".
        let at_default = lookup.get_related_files("src/a.rs", 0.0, 10);
        assert_eq!(at_default.threshold, 0.5);
        assert_eq!(at_default.total_commits, 2);
        assert_eq!(at_default.related_files.len(), 1);
        assert_eq!(at_default.related_files[0].file_path, "src/b.rs");
        assert_eq!(at_default.related_files[0].co_change_count, 1);
        assert_eq!(at_default.related_files[0].correlation, 0.5);
        assert_eq!(at_default.related_files[0].sample_commits, vec!["aaaaaa11"]);
    }

    #[test]
    fn related_files_threshold_above_correlation_filters_everything() {
        let lookup = FileLookup::new(&sample_report());
        let strict = lookup.get_related_files("src/a.rs", 0.99, 10);
        assert!(strict.related_files.is_empty());
        assert_eq!(strict.threshold, 0.99);
        // The queried file itself is still reported.
        assert_eq!(strict.total_commits, 2);
    }

    #[test]
    fn related_files_limit_truncates() {
        let lookup = FileLookup::new(&sample_report());
        let capped = lookup.get_related_files("src/a.rs", 0.1, 0);
        assert_eq!(
            capped.related_files.len(),
            1,
            "only b.rs co-changes with a.rs"
        );
    }

    #[test]
    fn related_files_unknown_file_reports_zero_commits() {
        let lookup = FileLookup::new(&sample_report());
        let result = lookup.get_related_files("nope.rs", 0.5, 10);
        assert_eq!(result.total_commits, 0);
        assert!(result.related_files.is_empty());
    }

    #[test]
    fn glob_star_does_not_cross_separators() {
        assert!(glob_match("src/*.rs", "src/a.rs"));
        assert!(!glob_match("src/*.rs", "src/nested/a.rs"));
        assert!(glob_match("src/*", "src/nested"));
        assert!(glob_match("*.rs", "a.rs"));
        assert!(glob_match("[ab].rs", "b.rs"));
        // Go `filepath.Match` negates only with '^'; '!' is a literal member,
        // so `[!ab]` matches '!', 'a' or 'b'.
        assert!(glob_match("[!ab].rs", "b.rs"));
        assert!(glob_match("[!ab].rs", "!.rs"));
        assert!(!glob_match("[!ab].rs", "c.rs"));
        assert!(!glob_match("[^ab].rs", "b.rs"));
        assert!(glob_match("[^ab].rs", "c.rs"));
        assert!(glob_match("[a-c].rs", "b.rs"));
    }

    /// A character class needs a name character to consume: an empty name
    /// must not match — and must not panic on the `name[1..]` slice.
    #[test]
    fn glob_class_never_matches_empty_name() {
        assert!(!glob_match("[^a]", ""));
        assert!(!glob_match("[!a]", ""));
        assert!(!glob_match("[a]", ""));
        assert!(!glob_match("a[!b]", "a"));
    }

    // -----------------------------------------------------------------------
    // Impact analysis (Go file_index.go:566-765, 795-804, 849-854)
    // -----------------------------------------------------------------------

    fn ts(s: &str) -> jiff::Timestamp {
        s.parse::<jiff::Timestamp>().expect("fixture timestamp")
    }

    /// A report with one bead per status, so each branch of the relevance
    /// multiplier and of the risk-score weighting is hit at least once.
    ///
    /// `now` below is 2026-01-10T00:00:00Z; every commit is inside the 7-day
    /// recency window, so no closed bead is dropped.
    fn impact_report() -> HistoryReport {
        let mut histories = BTreeMap::new();
        histories.insert(
            "bd-closed".into(),
            history(
                "bd-closed",
                "closed bead",
                "closed",
                vec![commit(
                    "cc000011",
                    "2026-01-09T00:00:00Z",
                    &[("src/a.rs", 10, 2), ("src/b.rs", 4, 3)],
                )],
            ),
        );
        histories.insert(
            "bd-open".into(),
            history(
                "bd-open",
                "open bead",
                "open",
                vec![commit(
                    "oo000011",
                    "2026-01-08T00:00:00Z",
                    &[("src/a.rs", 1, 1)],
                )],
            ),
        );
        histories.insert(
            "bd-wip".into(),
            history(
                "bd-wip",
                "in-progress bead",
                "in_progress",
                vec![commit(
                    "ii000011",
                    "2026-01-08T00:00:00Z",
                    &[("src/a.rs", 2, 0), ("src/b.rs", 5, 5)],
                )],
            ),
        );
        report(histories)
    }

    #[test]
    fn impact_analysis_empty_input_reports_go_summary() {
        let lookup = FileLookup::new(&impact_report());
        let result = lookup.impact_analysis_at(&[], ts("2026-01-10T00:00:00Z"));
        assert_eq!(result.summary, "No files to analyze");
        // Go seeds these before the early return, so they are `[]` not `null`.
        assert!(result.files.is_empty());
        assert!(result.affected_beads.is_empty());
        assert!(result.warnings.is_empty());
        assert_eq!(result.risk_score, 0.0);
        assert_eq!(result.risk_level, "low");
    }

    #[test]
    fn impact_analysis_blank_paths_reports_no_valid_files() {
        let lookup = FileLookup::new(&impact_report());
        let result = lookup.impact_analysis_at(
            &["".to_string(), "   ".to_string(), "./".to_string()],
            ts("2026-01-10T00:00:00Z"),
        );
        assert_eq!(result.summary, "No valid files to analyze");
        assert!(result.files.is_empty());
    }

    #[test]
    fn impact_analysis_normalizes_and_dedupes_input_paths() {
        let lookup = FileLookup::new(&impact_report());
        let result = lookup.impact_analysis_at(
            &[
                "./src/a.rs".to_string(),
                "src/a.rs".to_string(), // duplicate after normalization
                " src/b.rs ".to_string(),
                String::new(),
            ],
            ts("2026-01-10T00:00:00Z"),
        );
        assert_eq!(result.files, vec!["src/a.rs", "src/b.rs"]);
    }

    #[test]
    fn impact_analysis_sorts_in_progress_first_then_relevance_then_id() {
        let lookup = FileLookup::new(&impact_report());
        let result =
            lookup.impact_analysis_at(&["src/a.rs".to_string()], ts("2026-01-10T00:00:00Z"));
        let ids: Vec<&str> = result
            .affected_beads
            .iter()
            .map(|b| b.bead_id.as_str())
            .collect();
        // bd-wip is in_progress → priority 0. bd-open is not closed and not
        // in_progress → priority 1. bd-closed → priority 2.
        assert_eq!(ids, vec!["bd-wip", "bd-open", "bd-closed"]);
    }

    #[test]
    fn impact_analysis_weights_status_into_risk_score() {
        let lookup = FileLookup::new(&impact_report());
        let result =
            lookup.impact_analysis_at(&["src/a.rs".to_string()], ts("2026-01-10T00:00:00Z"));
        // 1 in progress, 1 open, 1 closed, and only one file so no +0.1 for
        // breadth. The order of the additions is Go's and is load-bearing:
        // 3 * 0.05 alone is not 0.15 in binary floating point.
        assert_eq!(result.risk_score, 0.4 + 0.2 + 0.05);
        assert_eq!(result.risk_level, "high");
        assert_eq!(
            result.warnings,
            vec![
                "Active work in progress on these files - coordinate before making changes",
                "Open beads touch these files - review before modifying",
            ]
        );
        assert_eq!(
            result.summary,
            // The prefix is the conflict-risk one, not "Found": bd-wip is
            // in progress, and file_index.go:757-761 replaces the whole prefix
            // rather than appending to it.
            "⚠️ Conflict risk: 1 bead in progress, 1 open bead, 1 recently closed bead touching these files"
        );
    }

    #[test]
    fn impact_analysis_conflict_prefix_when_work_is_in_progress() {
        let lookup = FileLookup::new(&impact_report());
        let result =
            lookup.impact_analysis_at(&["src/b.rs".to_string()], ts("2026-01-10T00:00:00Z"));
        assert!(result
            .summary
            .starts_with("⚠️ Conflict risk: 1 bead in progress"));
    }

    #[test]
    fn impact_analysis_empty_overlap_is_safe_to_proceed() {
        let lookup = FileLookup::new(&impact_report());
        let result =
            lookup.impact_analysis_at(&["src/absent.rs".to_string()], ts("2026-01-10T00:00:00Z"));
        assert_eq!(
            result.summary,
            "No beads found touching these files - safe to proceed"
        );
        assert!(result.affected_beads.is_empty());
        assert!(result.warnings.is_empty());
        assert_eq!(result.risk_score, 0.0);
    }

    #[test]
    fn impact_analysis_drops_closed_beads_older_than_a_week() {
        // bd-closed's only commit is 2026-01-01, which is 30 days before `now`.
        let mut histories = BTreeMap::new();
        histories.insert(
            "bd-old".into(),
            history(
                "bd-old",
                "ancient",
                "closed",
                vec![commit(
                    "cc000011",
                    "2026-01-01T00:00:00Z",
                    &[("src/a.rs", 6, 4)],
                )],
            ),
        );
        let lookup = FileLookup::new(&report(histories));

        // Nine days out: filtered, so the file reads as untouched.
        let stale =
            lookup.impact_analysis_at(&["src/a.rs".to_string()], ts("2026-01-10T00:00:00Z"));
        assert!(stale.affected_beads.is_empty());
        assert_eq!(stale.risk_score, 0.0);

        // Six days out: inside the window, so it contributes 0.05.
        let fresh =
            lookup.impact_analysis_at(&["src/a.rs".to_string()], ts("2026-01-07T00:00:00Z"));
        assert_eq!(fresh.affected_beads.len(), 1);
        assert_eq!(fresh.risk_score, 0.05);
    }

    #[test]
    fn impact_analysis_keeps_a_closed_bead_with_a_future_timestamp() {
        // Clock skew or a rebased commit can date a commit after `now`. Go's
        // `now.Sub(t)` is then negative, and a negative duration is not greater
        // than the 7-day window, so the bead stays in the analysis.
        let mut histories = BTreeMap::new();
        histories.insert(
            "bd-future".into(),
            history(
                "bd-future",
                "dated ahead",
                "closed",
                vec![commit(
                    "ff000011",
                    "2027-01-01T00:00:00Z",
                    &[("src/a.rs", 3, 1)],
                )],
            ),
        );
        let lookup = FileLookup::new(&report(histories));
        let result =
            lookup.impact_analysis_at(&["src/a.rs".to_string()], ts("2026-01-10T00:00:00Z"));
        assert_eq!(result.affected_beads.len(), 1);
        assert_eq!(result.risk_score, 0.05);
        // Recency is negative, so it clamps at 1 rather than going negative.
        assert_eq!(result.affected_beads[0].relevance, 0.4 + 0.4 + 0.5 * 0.2);
    }

    #[test]
    fn impact_analysis_never_drops_open_beads_by_age() {
        // The recency window is a closed-bead-only rule (file_index.go:655-658).
        let mut histories = BTreeMap::new();
        histories.insert(
            "bd-stale-open".into(),
            history(
                "bd-stale-open",
                "ancient but open",
                "open",
                vec![commit(
                    "oo000011",
                    "2020-01-01T00:00:00Z",
                    &[("src/a.rs", 1, 0)],
                )],
            ),
        );
        let lookup = FileLookup::new(&report(histories));
        let result =
            lookup.impact_analysis_at(&["src/a.rs".to_string()], ts("2026-01-10T00:00:00Z"));
        assert_eq!(result.affected_beads.len(), 1);
        assert_eq!(result.affected_beads[0].relevance, 0.0 + 0.4 + 0.8 * 0.2);
    }

    #[test]
    fn impact_analysis_relevance_blends_recency_overlap_and_status() {
        let lookup = FileLookup::new(&impact_report());
        // now − 2026-01-09T00:00:00Z is exactly one day, so
        // recency = 1 − 1/7; overlap = 1/1; status multiplier 0.5.
        let result =
            lookup.impact_analysis_at(&["src/a.rs".to_string()], ts("2026-01-10T00:00:00Z"));
        let closed = result
            .affected_beads
            .iter()
            .find(|b| b.bead_id == "bd-closed")
            .expect("bd-closed is in the result");
        let expected = (1.0 - 1.0 / 7.0) * 0.4 + 1.0 * 0.4 + 0.5 * 0.2;
        assert_eq!(closed.relevance, expected);
    }

    #[test]
    fn impact_analysis_status_multiplier_is_1_0_for_wip_and_0_8_for_open() {
        // The two non-default multipliers are pinned per status. bd-open and
        // bd-wip share a timestamp, so their recency and overlap terms are
        // identical and the only thing separating their relevance is the
        // status multiplier.
        let lookup = FileLookup::new(&impact_report());
        let result =
            lookup.impact_analysis_at(&["src/a.rs".to_string()], ts("2026-01-10T00:00:00Z"));
        let recency_overlap = (1.0 - 2.0 / 7.0) * 0.4 + 1.0 * 0.4;
        let open = result
            .affected_beads
            .iter()
            .find(|b| b.bead_id == "bd-open")
            .expect("bd-open is in the result");
        let wip = result
            .affected_beads
            .iter()
            .find(|b| b.bead_id == "bd-wip")
            .expect("bd-wip is in the result");
        assert_eq!(open.relevance, recency_overlap + 0.8 * 0.2);
        assert_eq!(wip.relevance, recency_overlap + 1.0 * 0.2);
    }

    #[test]
    fn impact_analysis_accumulates_overlap_and_changes_across_files() {
        // bd-wip touches both files. Its per-file `total_changes` are 2 and 10,
        // and each is looked up through its own `lookup_by_file` call, so both
        // overlap entries and both change counts must land on one bead.
        let lookup = FileLookup::new(&impact_report());
        let result = lookup.impact_analysis_at(
            &["src/a.rs".to_string(), "src/b.rs".to_string()],
            ts("2026-01-10T00:00:00Z"),
        );
        let wip = result
            .affected_beads
            .iter()
            .find(|b| b.bead_id == "bd-wip")
            .expect("bd-wip is in the result");
        assert_eq!(wip.overlap_files, vec!["src/a.rs", "src/b.rs"]);
        assert_eq!(wip.overlap_count, 2);
        assert_eq!(wip.total_changes, 2 + 10);
        assert_eq!(wip.title, "in-progress bead");
        assert_eq!(wip.status, "in_progress");
        // Last activity is the later of the two commits.
        assert_eq!(wip.last_activity, "2026-01-08T00:00:00Z");
    }

    #[test]
    fn impact_analysis_adds_breadth_penalty_past_three_files() {
        let lookup = FileLookup::new(&impact_report());
        let narrow =
            lookup.impact_analysis_at(&["src/a.rs".to_string()], ts("2026-01-10T00:00:00Z"));
        let wide = lookup.impact_analysis_at(
            &[
                "src/a.rs".to_string(),
                "src/b.rs".to_string(),
                "src/c.rs".to_string(),
                "src/d.rs".to_string(),
            ],
            ts("2026-01-10T00:00:00Z"),
        );
        // Same three beads either way; the fourth file only moves the score by
        // the +0.1 breadth term, which is enough to cross into "critical".
        assert_eq!(narrow.risk_score, 0.4 + 0.2 + 0.05);
        assert_eq!(narrow.risk_level, "high");
        assert_eq!(wide.risk_score, 0.4 + 0.2 + 0.05 + 0.1);
        assert_eq!(wide.risk_level, "critical");
    }

    #[test]
    fn impact_analysis_caps_risk_score_at_one() {
        // 20 in-progress beads: 20 * 0.4 = 8.0, clamped to 1.0 → critical.
        let mut histories = BTreeMap::new();
        for i in 0..20 {
            let id = format!("bd-wip-{i:02}");
            histories.insert(
                id.clone(),
                history(
                    &id,
                    "wip",
                    "in_progress",
                    vec![commit(
                        &format!("ii{i:06}11"),
                        "2026-01-09T00:00:00Z",
                        &[("src/a.rs", 1, 0)],
                    )],
                ),
            );
        }
        let lookup = FileLookup::new(&report(histories));
        let result =
            lookup.impact_analysis_at(&["src/a.rs".to_string()], ts("2026-01-10T00:00:00Z"));
        assert_eq!(result.risk_score, 1.0);
        assert_eq!(result.risk_level, "critical");
    }

    #[test]
    fn affected_bead_status_priority_normalizes_but_multiplier_does_not() {
        assert_eq!(affected_bead_status_priority("IN_PROGRESS"), 0);
        assert_eq!(affected_bead_status_priority(" closed "), 2);
        assert_eq!(affected_bead_status_priority("open"), 1);
        assert_eq!(affected_bead_status_priority(""), 1);

        // A status with stray casing sorts as in-progress but is weighted as
        // closed, because file_index.go:693 compares with `==` and does not
        // normalize. Both halves of that asymmetry are asserted here so a
        // "cleanup" that normalizes one of them fails.
        let mut histories = BTreeMap::new();
        histories.insert(
            "bd-odd".into(),
            history(
                "bd-odd",
                "odd casing",
                "In_Progress",
                vec![commit(
                    "xx000011",
                    "2026-01-09T00:00:00Z",
                    &[("src/a.rs", 1, 0)],
                )],
            ),
        );
        let lookup = FileLookup::new(&report(histories));
        let result =
            lookup.impact_analysis_at(&["src/a.rs".to_string()], ts("2026-01-10T00:00:00Z"));
        assert_eq!(
            result.affected_beads[0].relevance,
            (1.0 - 1.0 / 7.0) * 0.4 + 0.4 + 0.5 * 0.2
        );
        // Weighted as closed (0.05), and the in-progress warning is absent.
        assert_eq!(result.risk_score, 0.05);
        assert!(result.warnings.is_empty());
        assert_eq!(
            result.summary,
            "Found 1 recently closed bead touching these files"
        );
    }

    #[test]
    fn impact_summary_pluralizes_singletons() {
        assert_eq!(
            build_impact_summary(1, 0, 0),
            "⚠️ Conflict risk: 1 bead in progress touching these files"
        );
        assert_eq!(
            build_impact_summary(0, 2, 0),
            "Found 2 open beads touching these files"
        );
        assert_eq!(
            build_impact_summary(0, 0, 0),
            "No beads found touching these files - safe to proceed"
        );
    }

    // -----------------------------------------------------------------------
    // The two queries the CLI could not answer before: --robot-file-beads and
    // --robot-file-relations both go through this index, and both need the
    // per-commit numstat that only a HistoryReport carries.
    // -----------------------------------------------------------------------

    #[test]
    fn lookup_by_file_sums_per_commit_numstat_into_total_changes() {
        // This is the field --robot-file-beads reports as `total_changes`, and
        // the reason the handler could not answer the query off a sha→commits
        // map: the insertions/deletions live in FileChange, not in the map.
        let lookup = FileLookup::new(&impact_report());
        let result = lookup.lookup_by_file("src/a.rs");
        let closed = &result.closed_beads[0];
        assert_eq!(closed.bead_id, "bd-closed");
        assert_eq!(closed.total_changes, 12);
        assert_eq!(closed.commit_shas, vec!["cc00001"]);
        assert_eq!(result.total_beads, 3);
        assert_eq!(result.open_beads.len(), 2, "bd-open and bd-wip");
    }

    #[test]
    fn get_related_files_answers_the_relations_query() {
        // a.rs is in cc000011, oo000011 and ii000011; b.rs in cc000011 and
        // ii000011. So b.rs co-changes with a.rs in 2 of 3 commits = 0.667,
        // which clears neither the 0.5 default nor a 0.0 request differently.
        let lookup = FileLookup::new(&impact_report());
        let result = lookup.get_related_files("src/a.rs", 0.0, 10);
        assert_eq!(result.file_path, "src/a.rs");
        assert_eq!(result.total_commits, 3);
        assert_eq!(result.threshold, 0.5);
        assert_eq!(result.related_files.len(), 1);
        let entry = &result.related_files[0];
        assert_eq!(entry.file_path, "src/b.rs");
        assert_eq!(entry.co_change_count, 2);
        assert_eq!(entry.total_commits, 3);
        assert_eq!(entry.sample_commits, vec!["cc000011", "ii000011"]);
    }
}
