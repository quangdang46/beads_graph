//! Index sync — port of Go `pkg/search/index_sync.go`.
//!
//! Path convention: `<project>/.bv/semantic/index-<provider>-<dim>.bvvi`
//! (Go `DefaultIndexPath`; provider here is always `"hash"` — the hash
//! embedder is the only embedder this crate ships). Sync is incremental:
//! documents whose sha256 content hash matches the stored entry are skipped,
//! changed/new ones re-embedded in deterministic (sorted-ID, batch-32)
//! order, removed ones dropped. Corrupt files are backed up to
//! `<path>.corrupt-<nanos>` and rebuilt (Go `LoadOrNewVectorIndex`).

use crate::embedder::DEFAULT_DIM;
use crate::vector_index::{compute_content_hash, VectorIndex};

/// Sync statistics (Go `IndexSyncStats` — same JSON field names).
#[derive(Debug, Clone, Default)]
pub struct IndexSyncStats {
    pub total: usize,
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    pub skipped: usize,
    pub embedded: usize,
}

impl IndexSyncStats {
    pub fn changed(&self) -> bool {
        self.added + self.updated + self.removed > 0
    }
}

/// Default index path for a project dir (Go `DefaultIndexPath` with the
/// hash provider): `<project>/.bv/semantic/index-hash-<dim>.bvvi`.
pub fn default_index_path(project_dir: &std::path::Path, dim: usize) -> std::path::PathBuf {
    project_dir
        .join(".bv")
        .join("semantic")
        .join(format!("index-hash-{dim}.bvvi"))
}

/// Load an existing index or start fresh (Go `LoadOrNewVectorIndex`).
/// Returns `(index, loaded_from_disk)`. Go's rule is `os.IsNotExist(err)`:
/// a missing file starts fresh silently, while *every* other load failure —
/// decode/validation AND I/O — is treated as corruption: the file is renamed
/// aside to `<path>.corrupt-<unix_nanos>` and rebuilt empty (dim mismatches
/// are synthesized into the same path). Go propagates the load error only
/// when the backup rename itself fails; this signature cannot return an
/// error (callers are fixed), so a failed rename still yields a fresh index.
///
/// The not-found case is detected by probing the path rather than matching
/// `VectorIndex::load`'s error text, which is OS/locale dependent.
pub fn load_or_new(path: &std::path::Path, dim: usize) -> (VectorIndex, bool) {
    let dim = if dim == 0 { DEFAULT_DIM } else { dim };
    let missing = matches!(
        std::fs::metadata(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound
    );
    match VectorIndex::load(path) {
        Ok(idx) if idx.dim == dim => (idx, true),
        Ok(idx) => {
            let _ = dim;
            backup_corrupt(path);
            let _ = idx;
            (VectorIndex::new(dim), false)
        }
        Err(_) if missing => (VectorIndex::new(dim), false),
        Err(_) => {
            backup_corrupt(path);
            (VectorIndex::new(dim), false)
        }
    }
}

fn backup_corrupt(path: &std::path::Path) {
    // Go appends (not replaces): `<path>.corrupt-<unix_nanos>`.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let backup_path = std::path::PathBuf::from(format!("{}.corrupt-{nanos}", path.display()));
    let _ = std::fs::rename(path, backup_path);
}

/// Sync `idx` to match `docs` (issue ID → embed text), embedding only
/// changed items (Go `SyncVectorIndex`, batch size 32, sorted-ID order).
/// `embed` maps a batch of texts to vectors (the hash embedder in practice).
pub fn sync_index(
    idx: &mut VectorIndex,
    docs: &std::collections::BTreeMap<String, String>,
    embed: impl Fn(&[String]) -> Vec<Vec<f32>>,
) -> Result<IndexSyncStats, String> {
    const BATCH: usize = 32;
    let mut stats = IndexSyncStats {
        total: docs.len(),
        ..Default::default()
    };
    // Deterministic order (BTreeMap iterates sorted already).
    let mut to_embed_ids: Vec<String> = Vec::new();
    let mut to_embed_texts: Vec<String> = Vec::new();
    let mut to_embed_hashes: Vec<[u8; 32]> = Vec::new();
    let mut staged: std::collections::BTreeMap<String, crate::vector_index::VectorEntry> =
        std::collections::BTreeMap::new();

    for (id, text) in docs {
        if id.is_empty() {
            return Err("issue id cannot be empty".into());
        }
        let hash = compute_content_hash(text);
        match idx.get(id) {
            Some(existing) if existing.content_hash == hash => {
                stats.skipped += 1;
                staged.insert(id.clone(), existing.clone());
            }
            Some(_) => {
                stats.updated += 1;
                to_embed_ids.push(id.clone());
                to_embed_texts.push(text.clone());
                to_embed_hashes.push(hash);
            }
            None => {
                stats.added += 1;
                to_embed_ids.push(id.clone());
                to_embed_texts.push(text.clone());
                to_embed_hashes.push(hash);
            }
        }
    }
    stats.removed = idx.len().saturating_sub(stats.skipped + stats.updated);

    if !stats.changed() {
        return Ok(stats);
    }
    for (start, chunk) in to_embed_texts.chunks(BATCH).enumerate() {
        let base = start * BATCH;
        let vecs = embed(chunk);
        if vecs.len() != chunk.len() {
            return Err(format!(
                "embedder returned {} vectors for {} texts",
                vecs.len(),
                chunk.len()
            ));
        }
        for (i, vec) in vecs.into_iter().enumerate() {
            let id = &to_embed_ids[base + i];
            if vec.len() != idx.dim {
                return Err(format!(
                    "vector dim mismatch for {id}: {} != {}",
                    vec.len(),
                    idx.dim
                ));
            }
            if vec.iter().any(|v| !v.is_finite()) {
                return Err(format!("invalid vector for {id}"));
            }
            staged.insert(
                id.clone(),
                crate::vector_index::VectorEntry {
                    content_hash: to_embed_hashes[base + i],
                    vector: vec,
                },
            );
            stats.embedded += 1;
        }
    }
    // Publish all-or-nothing (Go publishes under one lock acquisition).
    idx.entries_replace(staged);
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedder::hash_embed;

    fn test_docs() -> std::collections::BTreeMap<String, String> {
        [("A-1", "fix login bug"), ("B-2", "refactor db layer")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn sync_adds_then_skips_unchanged_then_updates() {
        let mut idx = VectorIndex::new(8);
        let docs = test_docs();
        let embed = |texts: &[String]| texts.iter().map(|t| hash_embed(t, 8)).collect();
        let s1 = sync_index(&mut idx, &docs, embed).unwrap();
        assert_eq!((s1.added, s1.embedded, s1.skipped), (2, 2, 0));
        assert!(s1.changed());
        let s2 = sync_index(&mut idx, &docs, embed).unwrap();
        assert_eq!((s2.added, s2.updated, s2.skipped), (0, 0, 2));
        assert!(!s2.changed());
        assert_eq!(s2.embedded, 0);

        let mut changed = docs.clone();
        changed.insert("A-1".into(), "fix login bug urgently".into());
        changed.remove("B-2");
        let s3 = sync_index(&mut idx, &changed, embed).unwrap();
        assert_eq!((s3.updated, s3.removed, s3.embedded), (1, 1, 1));
        assert_eq!(idx.len(), 1);
    }

    #[test]
    fn default_index_path_matches_go_convention() {
        let p = default_index_path(std::path::Path::new("/proj"), 384);
        assert_eq!(
            p,
            std::path::Path::new("/proj/.bv/semantic/index-hash-384.bvvi")
        );
    }
}
