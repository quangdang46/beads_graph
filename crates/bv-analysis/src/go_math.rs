//! Go's `math.Log2`, ported because the platform libm is not the same function.
//!
//! `f64::log2` binds to the C library's `log2`, which is correctly rounded on
//! most platforms. Go's `math.Log2` is not that: when there is no assembly
//! override it composes `Log(frac)*(1/Ln2) + exp` from Go's own fdlibm-derived
//! `Log`, and that composition rounds differently. The two disagree by one ULP
//! often enough to matter here — `log2(3.0) * 0.4` is `0.6339850002884625` in
//! Rust and `0.6339850002884626` in Go, which is the entire remaining
//! difference in the `--robot-triage` `quick_wins` scores against the frozen
//! goldens.
//!
//! Reproducing the arithmetic exactly is the point; substituting a better
//! implementation would reintroduce the drift.

// The constants below are transcribed from Go's `src/math/log.go` and
// `src/math/const.go`. Two lints fire on them and both are wrong here:
//
// * `approx_constant` sees `LN2` and recognises `f64::consts::LN_2`. Go's
//   `Ln2` and Rust's `LN_2` are the same correctly-rounded double, so using
//   the stdlib constant would be numerically identical — but the literal is
//   what lets a reader check this file against the Go source line by line,
//   which is the entire reason it is spelled out.
// * `excessive_precision` wants the trailing digits dropped. Rust truncates
//   the literal at parse time regardless, so the value is unchanged; the
//   digits are Go's, and Go's own source carries them in the same form.
#![allow(clippy::approx_constant, clippy::excessive_precision)]

/// Go `math.Sqrt2 / 2`, the threshold in `Log`'s argument reduction.
const SQRT2_OVER_2: f64 = std::f64::consts::FRAC_1_SQRT_2;

/// Go `math.Ln2` (src/math/const.go:21). Used for the `1/Ln2` in `Log2`; `Log`
/// itself uses the split Ln2Hi/Ln2Lo pair below.
const LN2: f64 = 0.693147180559945309417232121458176568075500134360255254120680009;

/// Go `Log` (src/math/log.go:88-129).
///
/// The coefficients are Go's, verbatim, including the trailing digits that
/// differ from the "exact" values — they are fitted constants, and rounding
/// them to fifteen digits changes the result.
pub fn go_log(x: f64) -> f64 {
    const LN2_HI: f64 = 6.93147180369123816490e-01;
    const LN2_LO: f64 = 1.90821492927058770002e-10;
    const L1: f64 = 6.666666666666735130e-01;
    const L2: f64 = 3.999999999940941908e-01;
    const L3: f64 = 2.857142874366239149e-01;
    const L4: f64 = 2.222219843214978396e-01;
    const L5: f64 = 1.818357216161805012e-01;
    const L6: f64 = 1.531383769920937332e-01;
    const L7: f64 = 1.479819860511658591e-01;

    // Special cases (log.go:102-109).
    if x.is_nan() || x == f64::INFINITY {
        return x;
    }
    if x < 0.0 {
        return f64::NAN;
    }
    if x == 0.0 {
        return f64::NEG_INFINITY;
    }

    // Reduce (log.go:112-118).
    let (mut f1, mut ki) = frexp_go(x);
    if f1 < SQRT2_OVER_2 {
        f1 *= 2.0;
        ki -= 1;
    }
    let f = f1 - 1.0;
    let k = ki as f64;

    // Compute (log.go:120-127).
    let s = f / (2.0 + f);
    let s2 = s * s;
    let s4 = s2 * s2;
    let t1 = s2 * (L1 + s4 * (L3 + s4 * (L5 + s4 * L7)));
    let t2 = s4 * (L2 + s4 * (L4 + s4 * L6));
    let r = t1 + t2;
    let hfsq = 0.5 * f * f;
    k * LN2_HI - ((hfsq - (s * (hfsq + r) + k * LN2_LO)) - f)
}

/// Go `Log2` (src/math/log10.go:29-37).
pub fn go_log2(x: f64) -> f64 {
    let (frac, exp) = frexp_go(x);
    // Make sure exact powers of two give an exact answer. Don't depend on
    // Log(0.5)*(1/Ln2)+exp being exactly exp-1.
    if frac == 0.5 {
        return (exp - 1) as f64;
    }
    go_log(frac) * (1.0 / LN2) + exp as f64
}

/// Go `math.Frexp` (src/math/frexp.go) — `x == frac * 2^exp` with `frac` in
/// `[0.5, 1)` and the sign preserved.
///
/// Rust's stdlib has no `f64::frexp`, so this reads the IEEE-754 layout
/// directly. Subnormals need the shift loop: they carry no implicit leading
/// bit, so the exponent is found by shifting the mantissa up until one
/// appears.
fn frexp_go(x: f64) -> (f64, i32) {
    // Go returns these unchanged (frexp.go:12-16).
    if x == 0.0 || x.is_infinite() || x.is_nan() {
        return (x, 0);
    }
    let bits = x.to_bits();
    let biased = ((bits >> 52) & 0x7FF) as i32;
    let mantissa = bits & 0x000F_FFFF_FFFF_FFFF;

    // `x == (m / 2^52) * 2^unbiased` after this.
    let (unbiased, m) = if biased == 0 {
        let mut m = mantissa;
        let mut e = -1022i32;
        while m & (1u64 << 52) == 0 {
            m <<= 1;
            e -= 1;
        }
        (e, m)
    } else {
        (biased - 1023, mantissa | (1u64 << 52))
    };

    // `m / 2^52` is the significand in [1, 2). `m` is a raw mantissa with the
    // implicit bit set, not a float — reading it with `from_bits` would put
    // those bits in the exponent field. Build the value with an explicit
    // exponent of 1023 (1.0's pattern) and the mantissa's own 52 bits.
    let significand = f64::from_bits(0x3FF0_0000_0000_0000 | (m & 0x000F_FFFF_FFFF_FFFF));
    // Go divides the significand down by one binade and bumps the exponent to
    // compensate, which is what puts `frac` in [0.5, 1) rather than [1, 2).
    let frac = significand * 0.5;
    (if x < 0.0 { -frac } else { frac }, unbiased + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The values the frozen `robot_triage` goldens carry. If this drifts back
    /// to the platform libm the goldens stop matching on `quick_wins`.
    ///
    /// `large_cyclic_600____robot_triage.json` records quick-win scores
    /// 0.6339850002884626 (unblocks 2) and 0.6000000000000001 (unblocks 1),
    /// and `xl_2500____robot_triage.json` records 1.0339850002884625
    /// (unblocks 5) and 0.9287712379549449 (unblocks 4) — all of which are
    /// `log2(unblocks + 1) * 0.4` under Go's arithmetic, before the simplicity
    /// and priority terms.
    #[test]
    fn log2_matches_go_on_the_golden_inputs() {
        // (unblocks, the log2 term the goldens' scores are built from)
        const GOLDEN: &[(usize, f64)] = &[
            (0, 0.0),
            (1, 0.4),
            (2, 0.6339850002884626),
            (3, 0.8),
            (4, 0.9287712379549449),
            (5, 1.0339850002884625),
        ];
        for (unblocks, expected) in GOLDEN {
            let got = go_log2((*unblocks as f64) + 1.0) * 0.4;
            assert_eq!(
                got.to_bits(),
                expected.to_bits(),
                "quick-win score for unblocks={unblocks}: got {got:.17}, golden {expected:.17}"
            );
        }
    }

    #[test]
    fn exact_powers_of_two_are_exact() {
        for e in -10i32..10 {
            let x = (2.0f64).powi(e);
            assert_eq!(go_log2(x), e as f64, "log2(2^{e}) should be exact");
        }
    }

    #[test]
    fn log2_differs_from_platform_libm_where_go_does() {
        // If a future platform rounds log2 the way Go does, this stops being a
        // difference. It is a canary, not a requirement: the point is that the
        // port is exercised.
        let go = go_log2(3.0) * 0.4;
        assert_eq!(go.to_bits(), 0.63398500028846261_f64.to_bits());
    }

    #[test]
    fn special_cases_match_go() {
        assert!(go_log2(0.0).is_infinite() && go_log2(0.0).is_sign_negative());
        assert!(go_log2(f64::NAN).is_nan());
        assert_eq!(go_log2(f64::INFINITY), f64::INFINITY);
        assert!(go_log(f64::NEG_INFINITY).is_nan());
    }
}
