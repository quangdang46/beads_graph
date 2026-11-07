//! Byte-exact port of Go's `math/rand` seeded generator.
//!
//! Go's approximate betweenness samples pivots with
//! `rand.New(rand.NewSource(1))` and a Fisher-Yates shuffle
//! (`pkg/analysis/betweenness_approx.go:402`). Rust used a hand-rolled LCG
//! with the same seed, so the two selected **different pivots** — and
//! approximate betweenness is a function of the pivots. That is why
//! `xl_2500` and `large_cyclic_600` betweenness diverged even though exact
//! Brandes matches byte-for-byte on small graphs.
//!
//! Reproducing `Intn` means reproducing the lagged-Fibonacci generator, the
//! Lehmer seeding pass, and `Int31n`'s rejection sampling. Any shortcut
//! yields a different stream, so the seed table is transcribed rather than
//! derived.

include!("rng_cooked.rs");

const RNG_LEN: usize = 607;
const RNG_TAP: usize = 273;
const RNG_MASK: u64 = (1u64 << 63) - 1;
const INT32_MAX: i32 = i32::MAX;

/// Go `seedrand` ($GOROOT/src/math/rand/rng.go:187) — the Lehmer/Park–Miller
/// step used only while seeding. Distinct from the generator proper, which
/// is additive lagged Fibonacci.
fn seedrand(x: i32) -> i32 {
    const A: i32 = 48271;
    const Q: i32 = 44488;
    const R: i32 = 3399;
    let hi = x / Q;
    let lo = x % Q;
    let mut x = A.wrapping_mul(lo).wrapping_sub(R.wrapping_mul(hi));
    if x < 0 {
        x = x.wrapping_add(INT32_MAX);
    }
    x
}

/// Go `rngSource` ($GOROOT/src/math/rand/rng.go).
#[derive(Clone)]
pub struct GoRand {
    vec: [i64; RNG_LEN],
    tap: usize,
    feed: usize,
}

impl GoRand {
    /// Go `newSource(seed)` ($GOROOT/src/math/rand/rand.go:55).
    pub fn new(seed: i64) -> Self {
        let mut rng = GoRand {
            vec: [0; RNG_LEN],
            tap: 0,
            feed: 0,
        };
        rng.seed(seed);
        rng
    }

    /// Go `(*rngSource).Seed` ($GOROOT/src/math/rand/rng.go:210).
    fn seed(&mut self, seed: i64) {
        self.tap = 0;
        self.feed = RNG_LEN - RNG_TAP;
        // Go rng.go Seed: `seed = seed % int32max` is computed in int64 on
        // the full seed BEFORE narrowing to int32 — truncating to i32 first
        // would corrupt seeds outside the int32 range or whose low 32 bits
        // differ from the true remainder. Negatives are then mapped back
        // into range and zero is substituted with a constant.
        let int32max = i64::from(INT32_MAX);
        let mut seed = seed % int32max;
        if seed < 0 {
            seed += int32max;
        }
        if seed == 0 {
            seed = 89482311;
        }
        let mut x = seed as i32;
        for i in -20i32..RNG_LEN as i32 {
            x = seedrand(x);
            if i >= 0 {
                let mut u = (x as i64) << 40;
                x = seedrand(x);
                u ^= (x as i64) << 20;
                x = seedrand(x);
                u ^= x as i64;
                u ^= RNG_COOKED[i as usize];
                self.vec[i as usize] = u;
            }
        }
    }

    /// Go `(*rngSource).Uint64` (rng.go:238) — additive lagged Fibonacci.
    fn next_u64(&mut self) -> u64 {
        self.tap = if self.tap == 0 {
            RNG_LEN - 1
        } else {
            self.tap - 1
        };
        self.feed = if self.feed == 0 {
            RNG_LEN - 1
        } else {
            self.feed - 1
        };
        let x = self.vec[self.feed].wrapping_add(self.vec[self.tap]);
        self.vec[self.feed] = x;
        x as u64
    }

    /// Go `(*rngSource).Int63` (rng.go:233).
    fn int63(&mut self) -> i64 {
        (self.next_u64() & RNG_MASK) as i64
    }

    /// Go `(*Rand).Int31` (rand.go:110) — the high 32 bits of `Int63`.
    fn int31(&mut self) -> i32 {
        (self.int63() >> 32) as i32
    }

    /// Go `(*Rand).Int31n` (rand.go:137), including the power-of-two mask
    /// shortcut and the rejection loop. Go's `Intn` routes here whenever
    /// `n <= 1<<31 - 1`, which always holds for a graph's node count.
    pub fn intn(&mut self, n: i32) -> i32 {
        assert!(n > 0, "invalid argument to Intn");
        if n & (n - 1) == 0 {
            return self.int31() & (n - 1);
        }
        // Go: max := int32((1 << 31) - 1 - (1<<31)%uint32(n)). The literal
        // (1<<31) is a uint32 there, so the modulo is done in u32 and only
        // then subtracted from the int32 maximum.
        let two31_mod_n = ((1u64 << 31) % n as u64) as i32;
        let max = INT32_MAX - two31_mod_n;
        let mut v = self.int31();
        while v > max {
            v = self.int31();
        }
        v % n
    }
}

/// Go `sampleIndices(n, k, seed)` (pkg/analysis/betweenness_approx.go:402) —
/// Fisher-Yates over the first `k` indices, drawn from a Go `*rand.Rand`.
///
/// Go special-cases `k >= n` by returning the identity permutation, which is
/// why a graph smaller than the sample size falls back to exact betweenness.
pub fn sample_indices(n: usize, k: usize, seed: i64) -> Vec<usize> {
    if k >= n {
        return (0..n).collect();
    }
    let mut shuffled: Vec<usize> = (0..n).collect();
    let mut rng = GoRand::new(seed);
    for i in 0..k {
        let j = i + rng.intn((n - i) as i32) as usize;
        shuffled.swap(i, j);
    }
    shuffled.truncate(k);
    shuffled
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference values from running Go's own `sampleIndices` in a standalone
    /// program built on `rand.New(rand.NewSource(1))` with the same
    /// Fisher-Yates loop as `pkg/analysis/betweenness_approx.go:402`.
    #[test]
    fn matches_go_pivot_stream() {
        assert_eq!(
            &sample_indices(2500, 200, 1)[..20],
            &[
                581, 1102, 1967, 897, 965, 2113, 1033, 1329, 1468, 544, 1614, 522, 2430, 585, 1214,
                1349, 1267, 894, 1817, 912
            ]
        );
        assert_eq!(
            &sample_indices(600, 100, 1)[..20],
            &[
                281, 520, 65, 398, 33, 583, 13, 66, 320, 363, 224, 598, 534, 76, 136, 284, 379,
                176, 35, 353
            ]
        );
        assert_eq!(
            &sample_indices(1000, 200, 1)[..20],
            &[
                81, 637, 831, 498, 341, 88, 261, 984, 816, 948, 564, 354, 766, 975, 850, 294, 331,
                834, 227, 276
            ]
        );
    }

    /// `k >= n` is Go's identity special case, which is what makes a graph
    /// smaller than the sample size fall back to exact betweenness.
    #[test]
    fn k_at_or_above_n_is_identity() {
        assert_eq!(sample_indices(39, 50, 1), (0..39).collect::<Vec<_>>());
        assert_eq!(sample_indices(10, 10, 1), (0..10).collect::<Vec<_>>());
    }

    #[test]
    fn sample_is_a_distinct_subset() {
        let s = sample_indices(2500, 200, 1);
        assert_eq!(s.len(), 200);
        assert!(s.iter().all(|&i| i < 2500));
        let mut sorted = s.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 200, "indices must be distinct");
    }

    /// A collapsed stream would make every graph sample the same pivots.
    #[test]
    fn different_seeds_diverge() {
        assert_ne!(sample_indices(2500, 200, 1), sample_indices(2500, 200, 2));
    }

    /// Go `rngSource.Seed` reduces the int64 seed with `seed % int32max`
    /// BEFORE narrowing to int32. The old port truncated to i32 first, so
    /// seeds outside int32 range (or whose low 32 bits alias a different
    /// remainder) produced a different stream. Reference values below were
    /// captured from Go 1.26.4 `rand.New(rand.NewSource(seed)).Intn(1000)`.
    #[test]
    fn large_and_negative_seeds_match_go() {
        let draw = |seed: i64| -> Vec<i32> {
            let mut r = GoRand::new(seed);
            (0..10).map(|_| r.intn(1000)).collect()
        };
        assert_eq!(
            draw((1i64 << 40) + 12345),
            vec![162, 554, 381, 8, 246, 422, 381, 479, 571, 421]
        );
        assert_eq!(
            draw(-(1i64 << 40) - 7),
            vec![602, 40, 933, 51, 351, 53, 72, 437, 857, 380]
        );
        // Sanity: the small-seed stream is unchanged.
        assert_eq!(draw(1), vec![81, 887, 847, 59, 81, 318, 425, 540, 456, 300]);
    }

    /// Int31n's power-of-two shortcut and its rejection path must both work.
    #[test]
    fn intn_stays_in_range() {
        for n in [2, 3, 8, 9, 17, 2500] {
            let mut r = GoRand::new(1);
            for _ in 0..64 {
                let v = r.intn(n);
                assert!((0..n).contains(&v), "intn({n}) produced {v}");
            }
        }
    }
}
