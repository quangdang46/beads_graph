//! Character classes matching Go's `unicode` package predicates, for the
//! letter/digit token runs in `hash_embedder.go` (`hashEmbedInto`) and
//! `lexical_boost.go` (`lexicalTokens`/`countTokens`).
//!
//! Neither `char::is_alphanumeric()` nor a plain `is_alphabetic() ||
//! is_numeric()` reproduces `unicode.IsLetter(r) || unicode.IsDigit(r)`:
//! - Go `IsDigit` is general category **Nd** only; Rust `is_numeric` covers
//!   all of N* — Nd plus Nl (letter numbers, e.g. Roman numerals) and No
//!   (other numbers, e.g. superscripts `²`, fractions `½`). Accepting those
//!   merges tokens Go keeps apart.
//! - Go `IsLetter` is categories L* only; Rust `is_alphabetic` is the
//!   derived *Alphabetic* property, which additionally includes Nl and
//!   `Other_Alphabetic` marks. `&& !is_numeric()` removes the Nl/No part of
//!   that (Nl ⊂ N*); the residual divergence is a handful of
//!   `Other_Alphabetic` combining marks — the same approximation
//!   `bv-tui`'s `byte_is_letter` already documents for `IsLetter`.
//!
//! Verified against Go 1.26 (`go/types`-level `unicode` tables): `Ⅷ` (Nl),
//! `½`/`²` (No) are rejected; `٣` (Arabic-Indic digit, Nd) is accepted.

use std::cmp::Ordering;

/// `unicode.IsLetter(c) || unicode.IsDigit(c)` — the token-character class
/// used by Go's hash embedder and lexical tokenizer.
pub(crate) fn is_letter_or_digit(c: char) -> bool {
    // L* letters: Alphabetic minus the numeric categories (drops Nl/No
    // false-positives; see module docs for the residual Other_Alphabetic
    // approximation).
    (c.is_alphabetic() && !c.is_numeric()) || is_decimal_digit(c)
}

/// `unicode.IsDigit(c)` — general category Nd (decimal digit) only.
///
/// Rust std exposes no Nd check (`is_numeric` is all of N*), so the Nd
/// ranges are tabulated explicitly. Table generated from UnicodeData
/// (Unicode 16.0); Nd membership is frozen once a block is assigned, so
/// only brand-new blocks can drift vs Go's unicode version.
fn is_decimal_digit(c: char) -> bool {
    let v = c as u32;
    ND_RANGES
        .binary_search_by(|&(lo, hi)| {
            if v < lo {
                Ordering::Greater
            } else if v > hi {
                Ordering::Less
            } else {
                Ordering::Equal
            }
        })
        .is_ok()
}

/// All Unicode Nd (decimal digit) ranges, sorted and disjoint.
const ND_RANGES: &[(u32, u32)] = &[
    (0x0030, 0x0039),
    (0x0660, 0x0669),
    (0x06F0, 0x06F9),
    (0x07C0, 0x07C9),
    (0x0966, 0x096F),
    (0x09E6, 0x09EF),
    (0x0A66, 0x0A6F),
    (0x0AE6, 0x0AEF),
    (0x0B66, 0x0B6F),
    (0x0BE6, 0x0BEF),
    (0x0C66, 0x0C6F),
    (0x0CE6, 0x0CEF),
    (0x0D66, 0x0D6F),
    (0x0DE6, 0x0DEF),
    (0x0E50, 0x0E59),
    (0x0ED0, 0x0ED9),
    (0x0F20, 0x0F29),
    (0x1040, 0x1049),
    (0x1090, 0x1099),
    (0x17E0, 0x17E9),
    (0x1810, 0x1819),
    (0x1946, 0x194F),
    (0x19D0, 0x19D9),
    (0x1A80, 0x1A89),
    (0x1A90, 0x1A99),
    (0x1B50, 0x1B59),
    (0x1BB0, 0x1BB9),
    (0x1C40, 0x1C49),
    (0x1C50, 0x1C59),
    (0xA620, 0xA629),
    (0xA8D0, 0xA8D9),
    (0xA900, 0xA909),
    (0xA9D0, 0xA9D9),
    (0xA9F0, 0xA9F9),
    (0xAA50, 0xAA59),
    (0xABF0, 0xABF9),
    (0xFF10, 0xFF19),
    (0x104A0, 0x104A9),
    (0x10D30, 0x10D39),
    (0x10D40, 0x10D49),
    (0x11066, 0x1106F),
    (0x110F0, 0x110F9),
    (0x11136, 0x1113F),
    (0x111D0, 0x111D9),
    (0x112F0, 0x112F9),
    (0x11450, 0x11459),
    (0x114D0, 0x114D9),
    (0x11650, 0x11659),
    (0x116C0, 0x116C9),
    (0x116D0, 0x116E3),
    (0x11730, 0x11739),
    (0x118E0, 0x118E9),
    (0x11950, 0x11959),
    (0x11BF0, 0x11BF9),
    (0x11C50, 0x11C59),
    (0x11D50, 0x11D59),
    (0x11DA0, 0x11DA9),
    (0x11F50, 0x11F59),
    (0x16130, 0x16139),
    (0x16A60, 0x16A69),
    (0x16AC0, 0x16AC9),
    (0x16B50, 0x16B59),
    (0x16D70, 0x16D79),
    (0x1CCF0, 0x1CCF9),
    (0x1D7CE, 0x1D7FF),
    (0x1E140, 0x1E149),
    (0x1E2F0, 0x1E2F9),
    (0x1E4F0, 0x1E4F9),
    (0x1E5F1, 0x1E5FA),
    (0x1E950, 0x1E959),
    (0x1FBF0, 0x1FBF9),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Cases verified against Go 1.26 `unicode.IsLetter || unicode.IsDigit`.
    #[test]
    fn matches_go_letter_or_digit() {
        // ASCII letters/digits.
        assert!(is_letter_or_digit('a'));
        assert!(is_letter_or_digit('Z'));
        assert!(is_letter_or_digit('0'));
        assert!(is_letter_or_digit('9'));
        // Unicode letters (L*).
        assert!(is_letter_or_digit('é'));
        assert!(is_letter_or_digit('界'));
        // Nd digits beyond ASCII.
        assert!(is_letter_or_digit('٣')); // Arabic-Indic digit three
        assert!(is_letter_or_digit('५')); // Devanagari digit five
        assert!(is_letter_or_digit('９')); // fullwidth digit nine
                                           // Nl/No numerics Go rejects.
        assert!(!is_letter_or_digit('Ⅷ')); // Roman numeral (Nl)
        assert!(!is_letter_or_digit('½')); // vulgar fraction (No)
        assert!(!is_letter_or_digit('²')); // superscript two (No)
                                           // Punctuation/marks/separators.
        assert!(!is_letter_or_digit('_'));
        assert!(!is_letter_or_digit('-'));
        assert!(!is_letter_or_digit(' '));
    }
}
