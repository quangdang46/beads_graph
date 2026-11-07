//! Persistent vector index — port of Go `pkg/search/vector_index.go`.
//!
//! Binary format (`BVVI`, version 1, little-endian — byte-compatible with
//! the Go binary so indexes are interchangeable):
//! ```text
//! magic[4] = "BVVI" | version u16 (=1) | reserved u16 (=0)
//! dim u32 | count u32
//! per entry: id_len u16 | id bytes | sha256 content hash[32] | dim×f32 LE
//! ```
//! Validation mirrors Go exactly: magic/version/dim/count bounds, non-empty
//! IDs, duplicate-ID reject, `minEntryOverhead` capacity pre-check, trailing
//! data reject. Save is atomic via temp-file + rename (with Go's Windows
//! remove-then-rename fallback note — Rust `std::fs::rename` replaces on
//! Windows already, so no fallback needed).

use std::collections::BTreeMap;
use std::io::Read;

pub const VECTOR_INDEX_MAGIC: &[u8; 4] = b"BVVI";
pub const VECTOR_INDEX_VERSION: u16 = 1;
pub const VECTOR_INDEX_HEADER_SIZE: i64 = 16;
pub const MAX_VECTOR_DIM: u32 = 1 << 20;
pub const MAX_VECTOR_ENTRIES: u32 = 1 << 20;
pub const MAX_VECTOR_FILE_SIZE: i64 = 512 << 20;
/// id_len u16 + non-empty id (min 1) + sha256 content hash.
const MIN_ENTRY_OVERHEAD: i64 = 2 + 1 + 32;

/// sha256 content hash of a document (Go `ComputeContentHash` =
/// `sha256.Sum256`). Real sha256 via the workspace `sha2` version (already
/// used by bv-core for data_hash — same version, no new version decision);
/// byte-identical to Go so content-hash skips agree across binaries.
pub fn compute_content_hash(text: &str) -> [u8; 32] {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(text.as_bytes());
    hasher.finalize().into()
}

/// One indexed document: content hash + embedding vector.
#[derive(Debug, Clone)]
pub struct VectorEntry {
    pub content_hash: [u8; 32],
    pub vector: Vec<f32>,
}

/// In-memory vector index with sorted-ID iteration and top-K search.
#[derive(Debug, Default)]
pub struct VectorIndex {
    pub dim: usize,
    entries: BTreeMap<String, VectorEntry>,
}

impl VectorIndex {
    pub fn new(dim: usize) -> Self {
        VectorIndex {
            dim: if dim == 0 {
                crate::embedder::DEFAULT_DIM
            } else {
                dim
            },
            entries: BTreeMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, issue_id: &str) -> Option<&VectorEntry> {
        self.entries.get(issue_id)
    }

    /// Insert or replace (Go `Upsert`): rejects empty IDs, dim mismatches,
    /// and non-finite components.
    pub fn upsert(&mut self, issue_id: &str, hash: [u8; 32], vec: Vec<f32>) -> Result<(), String> {
        if issue_id.is_empty() {
            return Err("issue id cannot be empty".into());
        }
        if vec.len() != self.dim {
            return Err(format!(
                "vector dim mismatch: {} != {}",
                vec.len(),
                self.dim
            ));
        }
        validate_finite(&vec)?;
        self.entries.insert(
            issue_id.to_string(),
            VectorEntry {
                content_hash: hash,
                vector: vec,
            },
        );
        Ok(())
    }

    pub fn remove(&mut self, issue_id: &str) {
        self.entries.remove(issue_id);
    }

    /// Replace the whole entry map (index_sync's all-or-nothing publish —
    /// Go swaps `idx.entries` under one lock acquisition after staging).
    pub fn entries_replace(&mut self, entries: BTreeMap<String, VectorEntry>) {
        self.entries = entries;
    }

    /// Sorted IDs (Go `sortedIDs` — BTreeMap iterates sorted already).
    pub fn sorted_ids(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }

    /// Cosine-similarity top-K over the whole index (Go `SearchTopK`:
    /// dot product over L2-normalized vectors, deterministic ID-asc
    /// tiebreak, `k` clamped to index size, `k <= 0` → empty).
    pub fn search_top_k(&self, query: &[f32], k: usize) -> Result<Vec<SearchHit>, String> {
        if query.len() != self.dim {
            return Err(format!(
                "query dim mismatch: {} != {}",
                query.len(),
                self.dim
            ));
        }
        validate_finite(query)?;
        if k == 0 {
            return Ok(Vec::new());
        }
        let mut scored: Vec<(String, f64)> = self
            .entries
            .iter()
            .map(|(id, e)| (id.clone(), dot_f32(query, &e.vector)))
            .collect();
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        let k = k.min(scored.len());
        Ok(scored
            .into_iter()
            .take(k)
            .map(|(id, score)| SearchHit {
                issue_id: id,
                score,
            })
            .collect())
    }

    /// Load from a `.bvvi` file (Go `LoadVectorIndex` — same validation).
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let meta = std::fs::metadata(path).map_err(|e| format!("stat vector index: {e}"))?;
        if meta.len() < VECTOR_INDEX_HEADER_SIZE as u64 {
            return Err(format!("vector index too small: {} bytes", meta.len()));
        }
        if meta.len() > MAX_VECTOR_FILE_SIZE as u64 {
            return Err(format!(
                "vector index too large: {} bytes (max {MAX_VECTOR_FILE_SIZE})",
                meta.len()
            ));
        }
        let bytes = std::fs::read(path).map_err(|e| format!("read vector index: {e}"))?;
        Self::decode(&bytes)
    }

    fn decode(bytes: &[u8]) -> Result<Self, String> {
        let mut cur = std::io::Cursor::new(bytes);
        let mut magic = [0u8; 4];
        cur.read_exact(&mut magic)
            .map_err(|e| format!("read header: {e}"))?;
        if &magic != VECTOR_INDEX_MAGIC {
            return Err(format!("invalid magic {magic:?}"));
        }
        let mut u16b = [0u8; 2];
        let mut u32b = [0u8; 4];
        cur.read_exact(&mut u16b)
            .map_err(|e| format!("read version: {e}"))?;
        let version = u16::from_le_bytes(u16b);
        if version != VECTOR_INDEX_VERSION {
            return Err(format!("unsupported version {version}"));
        }
        cur.read_exact(&mut u16b)
            .map_err(|e| format!("read reserved: {e}"))?;
        cur.read_exact(&mut u32b)
            .map_err(|e| format!("read dim: {e}"))?;
        let dim = u32::from_le_bytes(u32b);
        cur.read_exact(&mut u32b)
            .map_err(|e| format!("read count: {e}"))?;
        let count = u32::from_le_bytes(u32b);
        if dim == 0 {
            return Err("invalid dim 0".into());
        }
        if dim > MAX_VECTOR_DIM {
            return Err(format!(
                "vector dimension {dim} exceeds maximum {MAX_VECTOR_DIM}"
            ));
        }
        if count > MAX_VECTOR_ENTRIES {
            return Err(format!(
                "vector entry count {count} exceeds maximum {MAX_VECTOR_ENTRIES}"
            ));
        }
        if count > 0 {
            let min_entry = MIN_ENTRY_OVERHEAD + dim as i64 * 4;
            let available = bytes.len() as i64 - VECTOR_INDEX_HEADER_SIZE;
            if count as i64 > available / min_entry {
                return Err(format!(
                    "vector index header declares {count} entries but file can contain at most {}",
                    available / min_entry
                ));
            }
        }
        let mut idx = VectorIndex::new(dim as usize);
        for _ in 0..count {
            cur.read_exact(&mut u16b)
                .map_err(|e| format!("read id len: {e}"))?;
            let id_len = u16::from_le_bytes(u16b);
            if id_len == 0 {
                return Err("empty issue id".into());
            }
            let mut id_bytes = vec![0u8; id_len as usize];
            cur.read_exact(&mut id_bytes)
                .map_err(|e| format!("read id: {e}"))?;
            let issue_id = String::from_utf8(id_bytes)
                .map_err(|_| "issue id is not valid UTF-8".to_string())?;
            if idx.entries.contains_key(&issue_id) {
                return Err(format!("duplicate issue id {issue_id:?}"));
            }
            let mut hash = [0u8; 32];
            cur.read_exact(&mut hash)
                .map_err(|e| format!("read content hash: {e}"))?;
            let mut vec = Vec::with_capacity(dim as usize);
            let mut f4 = [0u8; 4];
            for _ in 0..dim {
                cur.read_exact(&mut f4)
                    .map_err(|e| format!("read vector: {e}"))?;
                vec.push(f32::from_le_bytes(f4));
            }
            validate_finite(&vec).map_err(|e| format!("invalid vector for {issue_id:?}: {e}"))?;
            idx.entries.insert(
                issue_id,
                VectorEntry {
                    content_hash: hash,
                    vector: vec,
                },
            );
        }
        if cur.position() != bytes.len() as u64 {
            return Err("unexpected trailing data".into());
        }
        Ok(idx)
    }

    /// Save atomically via temp-file + rename (Go `Save`).
    pub fn save(&self, path: &std::path::Path) -> Result<(), String> {
        if self.dim == 0 || self.dim as u64 > u32::MAX as u64 {
            return Err(format!(
                "index dim {} is outside serializable uint32 range",
                self.dim
            ));
        }
        if self.entries.len() as u64 > u32::MAX as u64 {
            return Err(format!(
                "index entry count {} exceeds serializable uint32 range",
                self.entries.len()
            ));
        }
        if self.dim as u32 > MAX_VECTOR_DIM {
            return Err(format!(
                "vector dimension {} exceeds supported range 1..={MAX_VECTOR_DIM}",
                self.dim
            ));
        }
        if self.entries.len() as u32 > MAX_VECTOR_ENTRIES {
            return Err(format!(
                "vector entry count {} exceeds maximum {MAX_VECTOR_ENTRIES}",
                self.entries.len()
            ));
        }
        let mut encoded_size: i64 = VECTOR_INDEX_HEADER_SIZE;
        for (id, entry) in &self.entries {
            if id.len() > u16::MAX as usize {
                return Err(format!("issue id too long: {}", id.len()));
            }
            if entry.vector.len() != self.dim {
                return Err(format!(
                    "vector dim mismatch for {id}: {} != {}",
                    entry.vector.len(),
                    self.dim
                ));
            }
            // Go's Save does not re-validate components (it trusts Upsert /
            // the sync staging checks), but `entries_replace` is an
            // unvalidated bulk swap: without this guard a NaN/inf vector
            // would be persisted and then rejected by `decode`'s
            // `validate_finite` on the next load, bricking the index.
            validate_finite(&entry.vector)
                .map_err(|e| format!("invalid vector for {id:?}: {e}"))?;
            encoded_size += 2 + id.len() as i64 + 32 + self.dim as i64 * 4;
            if encoded_size > MAX_VECTOR_FILE_SIZE {
                return Err(format!(
                    "encoded vector index exceeds maximum size {MAX_VECTOR_FILE_SIZE}"
                ));
            }
        }

        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)
                    .map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
            }
        }
        let mut buf: Vec<u8> = Vec::with_capacity(encoded_size as usize);
        buf.extend_from_slice(VECTOR_INDEX_MAGIC);
        buf.extend_from_slice(&VECTOR_INDEX_VERSION.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&(self.dim as u32).to_le_bytes());
        buf.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());
        for (id, entry) in &self.entries {
            buf.extend_from_slice(&(id.len() as u16).to_le_bytes());
            buf.extend_from_slice(id.as_bytes());
            buf.extend_from_slice(&entry.content_hash);
            for v in &entry.vector {
                buf.extend_from_slice(&v.to_le_bytes());
            }
        }
        // Atomic write: temp file in the same dir + rename.
        let dir = path.parent().filter(|d| !d.as_os_str().is_empty());
        // Go uses `os.CreateTemp(dir, "bvvi-*.tmp")`, whose random suffix
        // cannot collide; a bare timestamp can (coarse clocks, two saves in
        // the same tick). pid + nanos + a process-local counter gives the
        // same uniqueness guarantee without pulling in `rand`.
        let tmp = match dir {
            Some(d) => tempfile_in(d)?,
            None => tempfile_in(std::env::temp_dir())?,
        };
        // Go `defer`s `os.Remove(tmpPath)`: a failed write or rename must
        // not leak the temp file.
        if let Err(e) = std::fs::write(&tmp, &buf) {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("write temp index: {e}"));
        }
        if let Err(e) = std::fs::rename(&tmp, path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("rename index: {e}"));
        }
        Ok(())
    }
}

/// Temp file path helper without a new dependency (pid + nanos + counter
/// uniqueness — mirrors `os.CreateTemp`'s collision-free suffix).
fn tempfile_in(dir: impl AsRef<std::path::Path>) -> Result<std::path::PathBuf, String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let name = format!("bvvi-{}-{}-{}.tmp", std::process::id(), nanos, seq);
    Ok(dir.as_ref().join(name))
}

/// Dot product (Go `dotFloat32` — f64 accumulator).
fn dot_f32(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    a.iter()
        .zip(b)
        .map(|(x, y)| f64::from(*x) * f64::from(*y))
        .sum()
}

fn validate_finite(vec: &[f32]) -> Result<(), String> {
    for (i, v) in vec.iter().enumerate() {
        if !v.is_finite() {
            return Err(format!("vector component {i} must be finite"));
        }
    }
    Ok(())
}

/// Top-K search hit.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub issue_id: String,
    pub score: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory unique to this call.
    ///
    /// Keying only on `SystemTime::now().as_nanos()` is not unique: the clock
    /// is coarse enough that two tests running in parallel can read the same
    /// nanosecond, land in the same directory, and corrupt each other's
    /// index. That showed up as an intermittent
    /// `load_rejects_bad_magic_version_and_trailing_data` failure under
    /// `cargo test --workspace`. The counter guarantees uniqueness within the
    /// process; the timestamp keeps separate runs from colliding.
    fn unique_test_dir() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("bvvi-test-{nanos}-{seq}"))
    }

    fn sample_index() -> VectorIndex {
        let mut idx = VectorIndex::new(4);
        idx.upsert("B-2", [1u8; 32], vec![0.0, 1.0, 0.0, 0.0])
            .unwrap();
        idx.upsert("A-1", [2u8; 32], vec![1.0, 0.0, 0.0, 0.0])
            .unwrap();
        idx.upsert("C-3", [3u8; 32], vec![0.5, 0.5, 0.0, 0.0])
            .unwrap();
        idx
    }

    #[test]
    fn save_load_round_trip_preserves_entries() {
        let idx = sample_index();
        let dir = unique_test_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("index.bvvi");
        idx.save(&path).unwrap();
        let loaded = VectorIndex::load(&path).unwrap();
        assert_eq!(loaded.dim, 4);
        assert_eq!(loaded.len(), 3);
        for id in ["A-1", "B-2", "C-3"] {
            let a = idx.get(id).unwrap();
            let b = loaded.get(id).unwrap();
            assert_eq!(a.content_hash, b.content_hash);
            assert_eq!(a.vector, b.vector);
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_rejects_bad_magic_version_and_trailing_data() {
        let idx = sample_index();
        let dir = unique_test_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("index.bvvi");
        idx.save(&path).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();

        let mut bad = bytes.clone();
        bad[0] = b'X';
        assert!(VectorIndex::decode(&bad).is_err());

        let mut bad = bytes.clone();
        bad[4] = 0xFF;
        assert!(VectorIndex::decode(&bad).is_err());

        bytes.push(0x00);
        assert!(VectorIndex::decode(&bytes).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn search_top_k_ranks_by_dot_product_with_id_tiebreak() {
        let idx = sample_index();
        let hits = idx.search_top_k(&[1.0, 0.0, 0.0, 0.0], 10).unwrap();
        assert_eq!(hits[0].issue_id, "A-1");
        assert_eq!(hits[1].issue_id, "C-3");
        assert_eq!(hits[2].issue_id, "B-2");
        let hits = idx.search_top_k(&[1.0, 0.0, 0.0, 0.0], 2).unwrap();
        assert_eq!(hits.len(), 2);
        assert!(idx.search_top_k(&[1.0, 0.0], 2).is_err());
    }

    #[test]
    fn upsert_rejects_empty_id_dim_mismatch_and_nonfinite() {
        let mut idx = VectorIndex::new(2);
        assert!(idx.upsert("", [0u8; 32], vec![1.0, 0.0]).is_err());
        assert!(idx.upsert("X", [0u8; 32], vec![1.0]).is_err());
        assert!(idx.upsert("X", [0u8; 32], vec![f32::NAN, 0.0]).is_err());
    }

    /// `entries_replace` is the one unvalidated bulk swap; `save` must not
    /// persist a vector that `decode` would reject on the next load.
    #[test]
    fn save_rejects_nonfinite_entries_from_entries_replace() {
        let mut idx = VectorIndex::new(2);
        let mut entries = std::collections::BTreeMap::new();
        entries.insert(
            "X".to_string(),
            VectorEntry {
                content_hash: [0u8; 32],
                vector: vec![f32::NAN, 0.0],
            },
        );
        idx.entries_replace(entries);
        let dir = unique_test_dir();
        std::fs::create_dir_all(&dir).unwrap();
        assert!(idx.save(&dir.join("index.bvvi")).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
