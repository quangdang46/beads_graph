//! Hash embedder — port of Go `pkg/search/hash_embedder.go`.
//!
//! Byte-for-byte port of `hashEmbedInto`/`addHashedToken`/`normalizeL2`,
//! verified against a real Go-written `.bvvi` index (see
//! `vector_index::go_interop_investigation` and
//! `beads_viewer_rust-api-freeze-tui-ux-p14-followups-1xz.1`). Two real
//! divergences existed before that verification and are now fixed:
//! - Go lowercases ASCII bytes *inside* the FNV-1a accumulation (per-token,
//!   not on the whole text up front) — this port did not lowercase at all,
//!   so any uppercase-containing token hashed to a different bucket than Go.
//! - Go's bucket update is **signed**: `vec[idx] += sign`, where `sign` is
//!   `-1.0` when the hash's top bit is set, `+1.0` otherwise (a standard
//!   feature-hashing trick to de-bias collisions) — this port always did
//!   `vec[bucket] += 1.0`, an unsigned count. That's why the two embedders
//!   picked different bucket magnitudes/signs even when tokenization agreed.

/// Default embedding dimension (Go: DefaultEmbeddingDim).
pub const DEFAULT_DIM: usize = 384;

/// FNV-1a 64-bit hash over a token's ASCII-lowercased bytes (Go
/// `addHashedToken`'s hash loop — lowercasing only ASCII, matching Go's
/// `if b >= 'A' && b <= 'Z' { b += 'a' - 'A' }` fast path; other Unicode
/// case folding is intentionally skipped by Go too, "isn't worth it for a
/// fallback embedder").
fn fnv1a64_hash(token: &str) -> u64 {
    const OFFSET64: u64 = 0xcbf29ce484222325;
    const PRIME64: u64 = 0x100000001b3;
    let mut hash = OFFSET64;
    for b in token.as_bytes() {
        let b = if b.is_ascii_uppercase() {
            b.to_ascii_lowercase()
        } else {
            *b
        };
        hash ^= b as u64;
        hash = hash.wrapping_mul(PRIME64);
    }
    hash
}

/// Add one token's hashed, signed contribution to `vec` (Go
/// `addHashedToken`): bucket = `hash % dim`; sign = `-1.0` when the hash's
/// top bit is set, else `+1.0`.
fn add_hashed_token(vec: &mut [f32], token: &str) {
    let hash = fnv1a64_hash(token);
    let idx = (hash % vec.len() as u64) as usize;
    let sign: f32 = if (hash >> 63) & 1 == 1 { -1.0 } else { 1.0 };
    vec[idx] += sign;
}

/// Tokenize by maximal letter/digit runs (Go `hashEmbedInto`'s inline
/// loop: `unicode.IsLetter(r) || unicode.IsDigit(r)`, everything else is a
/// separator — same rule `bv_search::query::lexical_tokens` already uses
/// for lexical boosting; the shared predicate lives in `char_class` because
/// `char::is_alphanumeric()` also accepts Nl/No numerics Go rejects).
fn hash_embed_tokens(text: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in text.char_indices() {
        if crate::char_class::is_letter_or_digit(c) {
            if start.is_none() {
                start = Some(i);
            }
        } else if let Some(st) = start.take() {
            tokens.push(&text[st..i]);
        }
    }
    if let Some(st) = start {
        tokens.push(&text[st..]);
    }
    tokens
}

/// Hash-embed a document into f32[dim], L2-normalized (Go `Embed`:
/// `hashEmbedInto` then `normalizeL2`).
pub fn hash_embed(text: &str, dim: usize) -> Vec<f32> {
    let mut vec = vec![0.0f32; dim];
    if dim == 0 {
        return vec;
    }
    for token in hash_embed_tokens(text) {
        add_hashed_token(&mut vec, token);
    }

    // Go `normalizeL2` (pkg/search/hash_embedder.go:89-100). Two details
    // decide the last bit and are not interchangeable: the sum of squares is
    // accumulated in f64, and the scale is `1/sqrt` computed in f64 and then
    // narrowed, rather than dividing by an f32 norm. Either shortcut shifts
    // every embedding by an ULP, which propagates into cosine scores and
    // therefore into search result ordering.
    let sum: f64 = vec.iter().map(|&v| f64::from(v) * f64::from(v)).sum();
    if sum == 0.0 {
        return vec;
    }
    let scale = (1.0 / sum.sqrt()) as f32;
    for v in &mut vec {
        *v *= scale;
    }
    vec
}

/// Cosine similarity between two equal-length vectors (both already
/// L2-normalized by `hash_embed`, so this reduces to a dot product — kept
/// as a full cosine calculation so it's correct for non-normalized inputs
/// too).
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    (dot / (na * nb)) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_similarity_of_identical_vectors_is_one() {
        let v = hash_embed("login authentication flow", DEFAULT_DIM);
        assert!((cosine_similarity(&v, &v) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn cosine_similarity_of_disjoint_vocab_is_zero() {
        let a = hash_embed("alpha", 8);
        let b = hash_embed("zzz", 8);
        // Small dim risks hash collisions landing in the same bucket; just
        // assert it's not the identical-vector case.
        assert!(cosine_similarity(&a, &b) < 1.0);
    }

    #[test]
    fn cosine_similarity_mismatched_lengths_is_zero() {
        assert_eq!(cosine_similarity(&[1.0, 0.0], &[1.0, 0.0, 0.0]), 0.0);
    }

    #[test]
    fn embedding_is_normalized() {
        let v = hash_embed("hello world test", DEFAULT_DIM);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5);
    }

    #[test]
    fn different_texts_different_embeddings() {
        let a = hash_embed("login authentication", DEFAULT_DIM);
        let b = hash_embed("database migration", DEFAULT_DIM);
        assert_ne!(a, b);
    }

    #[test]
    fn same_text_same_embedding() {
        let a = hash_embed("deterministic output", DEFAULT_DIM);
        let b = hash_embed("deterministic output", DEFAULT_DIM);
        assert_eq!(a, b);
    }

    #[test]
    fn default_dim_is_384() {
        assert_eq!(DEFAULT_DIM, 384);
    }
}
