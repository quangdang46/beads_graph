//! Byte-exact port of the Go v0.25.0 `fingerprintWriter` (pkg/analysis/cache.go:437-540).
//!
//! v0.25.0 replaced the v0.20.0 flat NUL-joined encoding with a length-prefixed,
//! type-tagged streaming encoder. The aggregate `data_hash` is now a full 64-char
//! SHA-256 hex digest (not the 16-char truncation v0.20.0 emitted).
//!
//! The writer streams through a fixed 256-byte buffer; this only batches writes.
//! Field bytes and SHA-256 are identical to hashing directly — matching Go.

use sha2::{Digest, Sha256};

const BUF_LEN: usize = 256;
/// Go `binary.MaxVarintLen64` — the reserve that forces a flush before a varint.
const MAX_VARINT_LEN_64: usize = 10;
/// Unix seconds of Go's zero `time.Time` (`0001-01-01T00:00:00Z`).
const GO_ZERO_UNIX: i64 = -62135596800;

/// Streaming, length-prefixed encoder matching Go's `fingerprintWriter`.
pub struct FingerprintWriter {
    hash: Sha256,
    buffer: [u8; BUF_LEN],
    used: usize,
}

impl Default for FingerprintWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl FingerprintWriter {
    pub fn new() -> Self {
        Self {
            hash: Sha256::new(),
            buffer: [0u8; BUF_LEN],
            used: 0,
        }
    }

    /// Go `reset()` — clears the hash and the pending buffer span.
    pub fn reset(&mut self) {
        self.hash = Sha256::new();
        self.used = 0;
    }

    /// Go `flush()` — drain the pending buffer into the hash.
    fn flush(&mut self) {
        if self.used > 0 {
            let chunk: Vec<u8> = self.buffer[..self.used].to_vec();
            self.hash.update(&chunk);
            self.used = 0;
        }
    }

    /// Go `writeByte()`.
    fn write_byte(&mut self, v: u8) {
        if self.used == BUF_LEN {
            self.flush();
        }
        self.buffer[self.used] = v;
        self.used += 1;
    }

    /// Go `sumHex()` — full SHA-256 hex, 64 lowercase chars.
    /// Clones the hasher so the writer stays reusable, matching Go's
    /// `hash.Sum(nil)` which never consumes the running hash.
    pub fn sum_hex(&mut self) -> String {
        self.flush();
        let digest = self.hash.clone().finalize();
        let mut out = String::with_capacity(64);
        for b in digest.iter() {
            out.push(char::from_digit((b >> 4) as u32, 16).unwrap());
            out.push(char::from_digit((b & 0x0f) as u32, 16).unwrap());
        }
        out
    }

    /// Go `writeStringHash()` — uvarint byte-length prefix, then raw bytes.
    pub fn write_string_hash(&mut self, v: &str) {
        self.write_uint_hash(v.len() as u64);
        let bytes = v.as_bytes();
        let mut rest = bytes;
        while !rest.is_empty() {
            if self.used == BUF_LEN {
                self.flush();
            }
            let n = rest.len().min(BUF_LEN - self.used);
            self.buffer[self.used..self.used + n].copy_from_slice(&rest[..n]);
            self.used += n;
            rest = &rest[n..];
        }
    }

    /// Go `writeStringPtrHash()` — nil tag 0, non-nil tag 1 then the string.
    pub fn write_string_ptr_hash(&mut self, v: Option<&str>) {
        match v {
            None => self.write_byte(0),
            Some(s) => {
                self.write_byte(1);
                self.write_string_hash(s);
            }
        }
    }

    /// Go `writeIntHash()` / `writeInt64Hash()` — varint (zigzag) encoding.
    pub fn write_int_hash(&mut self, v: i64) {
        if BUF_LEN - self.used < MAX_VARINT_LEN_64 {
            self.flush();
        }
        let mut buf = [0u8; MAX_VARINT_LEN_64];
        let n = put_varint(&mut buf, v);
        self.buffer[self.used..self.used + n].copy_from_slice(&buf[..n]);
        self.used += n;
    }

    /// Go `writeIntPtrHash()` — nil tag 0, non-nil tag 1 then the int.
    pub fn write_int_ptr_hash(&mut self, v: Option<i64>) {
        match v {
            None => self.write_byte(0),
            Some(x) => {
                self.write_byte(1);
                self.write_int_hash(x);
            }
        }
    }

    /// Go `writeUintHash()` — uvarint encoding.
    pub fn write_uint_hash(&mut self, v: u64) {
        if BUF_LEN - self.used < MAX_VARINT_LEN_64 {
            self.flush();
        }
        let mut buf = [0u8; MAX_VARINT_LEN_64];
        let n = put_uvarint(&mut buf, v);
        self.buffer[self.used..self.used + n].copy_from_slice(&buf[..n]);
        self.used += n;
    }

    /// Go `writeTimeHash()` — zero time becomes the empty string; otherwise
    /// `t.UTC().Format(time.RFC3339Nano)`. Rust timestamps are already stored
    /// in that normalized form by the loader, so we normalize defensively.
    pub fn write_time_hash(&mut self, t: Option<&str>) {
        let formatted = match t {
            None => String::new(),
            Some(raw) => {
                if raw.is_empty() {
                    String::new()
                } else {
                    match parse_go_timestamp(raw) {
                        // Go checks `t.IsZero()` BEFORE formatting: an explicit
                        // "0001-01-01T00:00:00Z" decodes to `time.Time{}` and
                        // hashes as "" exactly like an absent field. The
                        // dependency hash is the one caller that formats
                        // unconditionally — it goes through
                        // `normalize_time_public`, not this path.
                        Some((secs, nanos)) if secs == GO_ZERO_UNIX && nanos == 0 => String::new(),
                        _ => normalize_time(raw),
                    }
                }
            }
        };
        self.write_string_hash(&formatted);
    }

    /// Go `writeTimePtrHash()` — nil tag 0, non-nil tag 1 then the time.
    pub fn write_time_ptr_hash(&mut self, t: Option<&str>) {
        match t {
            None => self.write_byte(0),
            Some(_) => {
                self.write_byte(1);
                self.write_time_hash(t);
            }
        }
    }
}

/// Go `t.UTC().Format(time.RFC3339Nano)`. Renders in UTC with a `Z` suffix
/// (Go's RFC3339Nano layout uses "Z07:00", printing "Z" for UTC), so an input
/// like `...+00:00` is normalized to `...Z` — a raw pass-through would change
/// the hash bytes.
///
/// Only strings Go's `time.Time` JSON decoder would have accepted reach this
/// point on the JSONL path (the loader rejects the record otherwise — see
/// `loader::process_line`), so parsing is the strict Go grammar, not jiff's
/// more lenient ISO-8601 reader.
fn normalize_time(raw: &str) -> String {
    match parse_go_timestamp(raw) {
        Some((secs, nanos)) => format_go_utc(secs, nanos),
        // Unreachable for loader-validated issues; a programmatically built
        // `Issue` can still carry an arbitrary string, and Go has no state for
        // it — hash the stored bytes rather than invent a normalized value.
        None => raw.to_string(),
    }
}

/// Strict port of Go's `time.Time` RFC3339 decode, probed against Go 1.26:
///
/// - `YYYY-MM-DD` `T` `HH:MM:SS` — capital `T` only (lowercase `t` and a space
///   separator are rejected), seconds required (`HH:MM` alone fails), year is
///   exactly four digits (0000–9999).
/// - Optional fraction: `.` or `,` followed by one or more digits, truncated
///   (never rounded) to nanosecond precision — `.1234567895` → `.123456789`.
/// - Zone: `Z` or `±HH:MM`; the offset hour must be ≤ 24 and the minute ≤ 60
///   (`+24:60` parses, `+25:00` and `+05:61` do not).
/// - Calendar fields are range-checked on the proleptic Gregorian calendar
///   (`2026-02-30`, `T24:00:00`, `T00:00:60` all fail).
///
/// Returns `(unix_seconds, nanoseconds)` for the instant, or `None` when the
/// input is not a Go-parseable timestamp.
pub(crate) fn parse_go_timestamp(raw: &str) -> Option<(i64, u32)> {
    let b = raw.as_bytes();
    let len = b.len();
    let mut i = 0usize;

    fn digits(b: &[u8], i: &mut usize, n: usize) -> Option<u32> {
        if *i + n > b.len() {
            return None;
        }
        let mut v: u32 = 0;
        for _ in 0..n {
            let c = b[*i];
            if !c.is_ascii_digit() {
                return None;
            }
            v = v * 10 + u32::from(c - b'0');
            *i += 1;
        }
        Some(v)
    }
    fn expect(b: &[u8], i: &mut usize, c: u8) -> Option<()> {
        if b.get(*i) == Some(&c) {
            *i += 1;
            Some(())
        } else {
            None
        }
    }

    let year = digits(b, &mut i, 4)? as i64;
    expect(b, &mut i, b'-')?;
    let month = digits(b, &mut i, 2)?;
    expect(b, &mut i, b'-')?;
    let day = digits(b, &mut i, 2)?;
    expect(b, &mut i, b'T')?;
    let hour = digits(b, &mut i, 2)?;
    expect(b, &mut i, b':')?;
    let min = digits(b, &mut i, 2)?;
    expect(b, &mut i, b':')?;
    let sec = digits(b, &mut i, 2)?;

    // Fraction: `.` or `,` plus ≥1 digit; keep the first nine digits and
    // right-pad with zeros — Go truncates extra digits away.
    let mut nanos = 0u32;
    if matches!(b.get(i), Some(b'.') | Some(b',')) {
        i += 1;
        let start = i;
        let mut kept = 0usize;
        while i < len && b[i].is_ascii_digit() {
            if kept < 9 {
                nanos = nanos * 10 + u32::from(b[i] - b'0');
                kept += 1;
            }
            i += 1;
        }
        if i == start {
            return None; // decimal point with no digits ("...00.Z")
        }
        for _ in kept..9 {
            nanos *= 10;
        }
    }

    // Zone: `Z` or sign + two-digit hour + ':' + two-digit minute.
    let offset_secs: i64 = match b.get(i) {
        Some(b'Z') => {
            i += 1;
            0
        }
        Some(sign @ (b'+' | b'-')) => {
            i += 1;
            let oh = digits(b, &mut i, 2)?;
            expect(b, &mut i, b':')?;
            let om = digits(b, &mut i, 2)?;
            // Go's zone-offset bounds, probed: hour ≤ 24, minute ≤ 60.
            if oh > 24 || om > 60 {
                return None;
            }
            let mag = i64::from(oh) * 3600 + i64::from(om) * 60;
            if *sign == b'-' {
                -mag
            } else {
                mag
            }
        }
        _ => return None,
    };
    if i != len {
        return None;
    }

    // Calendar validation (proleptic Gregorian, same rules `time.Date` uses).
    if !(1..=12).contains(&month) || hour > 23 || min > 59 || sec > 59 {
        return None;
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let dim = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    if day == 0 || day > dim {
        return None;
    }

    // days-from-civil (Howard Hinnant's algorithm; valid for year 0 and
    // negative years, matching Go's proleptic Gregorian `time.Time`).
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;

    let local = days * 86400 + i64::from(hour) * 3600 + i64::from(min) * 60 + i64::from(sec);
    Some((local - offset_secs, nanos))
}

/// `time.Time.UTC().Format(time.RFC3339Nano)` for a `(unix_seconds, nanos)`
/// instant: civil date in UTC, `Z` suffix, fraction printed only when nonzero
/// with trailing zeros stripped. Years outside 0000–9999 (reachable only via
/// extreme offsets) print unpadded like Go (`-0001`, `10000`).
fn format_go_utc(secs: i64, nanos: u32) -> String {
    // civil-from-days (inverse of the algorithm above).
    let z = secs.div_euclid(86_400) + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };

    let rem = secs.rem_euclid(86_400);
    let (hh, mm, ss) = (rem / 3600, rem % 3600 / 60, rem % 60);

    let mut out = if year < 0 {
        format!("-{:04}", -year)
    } else {
        format!("{year:04}")
    };
    out.push_str(&format!(
        "-{:02}-{:02}T{:02}:{:02}:{:02}",
        month, day, hh, mm, ss
    ));
    if nanos > 0 {
        let frac = format!("{nanos:09}");
        out.push('.');
        out.push_str(frac.trim_end_matches('0'));
    }
    out.push('Z');
    out
}

/// Public wrapper so `data_hash` normalizes dependency timestamps identically.
pub fn normalize_time_public(raw: &str) -> String {
    normalize_time(raw)
}

/// Go `t.UTC().Format(time.RFC3339Nano)` for a `(unix_seconds, nanos)` pair.
///
/// The SQLite reader parses timestamp spellings Go's JSON decoder never sees
/// (space-separated `2006-01-02 15:04:05`, offset-less forms), so it needs the
/// formatter directly rather than going through [`normalize_time`]'s
/// RFC3339-only parser.
pub(crate) fn format_go_instant(secs: i64, nanos: u32) -> String {
    format_go_utc(secs, nanos)
}

/// Go `binary.PutUvarint` — LEB128, low 7 bits per byte, high bit = continuation.
fn put_uvarint(buf: &mut [u8], mut v: u64) -> usize {
    let mut i = 0;
    while v >= 0x80 {
        buf[i] = (v as u8) | 0x80;
        v >>= 7;
        i += 1;
    }
    buf[i] = v as u8;
    i + 1
}

/// Go `binary.PutVarint` — zigzag then uvarint.
fn put_varint(buf: &mut [u8], v: i64) -> usize {
    let zz = ((v << 1) ^ (v >> 63)) as u64;
    put_uvarint(buf, zz)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uvarint_matches_go_encoding() {
        // Go: binary.PutUvarint(0)=1 byte 0x00; 300 -> 0xac 0x02.
        let mut b = [0u8; 10];
        assert_eq!(put_uvarint(&mut b, 0), 1);
        assert_eq!(b[0], 0x00);
        assert_eq!(put_uvarint(&mut b, 300), 2);
        assert_eq!(b[0], 0xac);
        assert_eq!(b[1], 0x02);
    }

    #[test]
    fn varint_is_zigzag() {
        // Go: binary.PutVarint(-1) -> 0x01, (0)->0x00, (1)->0x02.
        let mut b = [0u8; 10];
        assert_eq!(put_varint(&mut b, -1), 1);
        assert_eq!(b[0], 0x01);
        assert_eq!(put_varint(&mut b, 0), 1);
        assert_eq!(b[0], 0x00);
        assert_eq!(put_varint(&mut b, 1), 1);
        assert_eq!(b[0], 0x02);
    }

    #[test]
    fn sum_hex_is_64_chars() {
        let mut w = FingerprintWriter::new();
        assert_eq!(w.sum_hex().len(), 64);
    }

    #[test]
    fn string_ptr_nil_vs_empty_differ() {
        let mut a = FingerprintWriter::new();
        a.write_string_ptr_hash(None);
        let mut b = FingerprintWriter::new();
        b.write_string_ptr_hash(Some(""));
        assert_ne!(a.sum_hex(), b.sum_hex());
    }

    /// Every expectation below was probed against Go 1.26
    /// `time.Time` JSON decoding + `UTC().Format(RFC3339Nano)`.
    #[test]
    fn go_timestamp_strict_grammar() {
        let ok = |raw: &str, want: &str| {
            assert_eq!(normalize_time(raw), want, "normalize_time({raw:?})");
        };
        let bad = |raw: &str| assert!(parse_go_timestamp(raw).is_none(), "{raw:?}");

        // Accepted forms.
        ok("2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z");
        ok("2026-01-01T00:00:00.5Z", "2026-01-01T00:00:00.5Z");
        ok("2026-01-01T00:00:00.500000000Z", "2026-01-01T00:00:00.5Z");
        ok("2026-01-01T00:00:00+00:00", "2026-01-01T00:00:00Z");
        ok("2026-01-01T00:00:00-05:30", "2026-01-01T05:30:00Z");
        ok("0001-01-01T00:00:00Z", "0001-01-01T00:00:00Z");
        ok("0000-01-01T00:00:00Z", "0000-01-01T00:00:00Z");
        ok("9999-12-31T23:59:59Z", "9999-12-31T23:59:59Z");
        // Go truncates (never rounds) fractions past nanoseconds.
        ok(
            "2026-01-01T00:00:00.123456789123Z",
            "2026-01-01T00:00:00.123456789Z",
        );
        ok("2026-01-01T00:00:00.0000000009Z", "2026-01-01T00:00:00Z");
        ok(
            "2026-01-01T00:00:00.9999999999Z",
            "2026-01-01T00:00:00.999999999Z",
        );
        // Comma fraction separator is legal Go RFC3339.
        ok("2026-01-01T00:00:00,5Z", "2026-01-01T00:00:00.5Z");
        ok(
            "2026-01-01T00:00:00,123456789123Z",
            "2026-01-01T00:00:00.123456789Z",
        );
        // Go's offset bounds: hour ≤ 24, minute ≤ 60.
        ok("2026-01-01T00:00:00+24:00", "2025-12-31T00:00:00Z");
        ok("2026-01-01T00:00:00+24:60", "2025-12-30T23:00:00Z");
        ok("2026-01-01T00:00:00+05:60", "2025-12-31T18:00:00Z");
        ok("2026-01-01T00:00:00-00:00", "2026-01-01T00:00:00Z");
        // Extreme offsets can shift the printed year outside 0000–9999.
        ok("0000-01-01T00:00:00+24:00", "-0001-12-31T00:00:00Z");
        ok("9999-12-31T23:59:59-24:00", "10000-01-01T23:59:59Z");

        // Rejected forms (Go `json.Unmarshal` into `time.Time` errors).
        bad("2026-01-01t00:00:00z"); // lowercase t/z
        bad("2026-01-01 00:00:00Z"); // space separator
        bad("2026-01-01T00:00Z"); // seconds required
        bad("2026-01-01T00:00:00"); // zone required
        bad("2026-01-01T00:00:00+05"); // offset needs :MM
        bad("2026-01-01T00:00:00+0530");
        bad("2026-01-01T00:00:00+05:30:15"); // trailing junk
        bad("2026-01-01T00:00:00+25:00"); // offset hour > 24
        bad("2026-01-01T00:00:00+05:61"); // offset minute > 60
        bad("2026-01-01T00:00:00.Z"); // fraction needs digits
        bad("2026-01-01T00:00:00z"); // lowercase z
        bad("2026-1-1T0:0:0Z"); // field widths
        bad("10000-01-01T00:00:00Z"); // 5-digit year
        bad(" 2026-01-01T00:00:00Z"); // leading space
        bad("");
        bad("not-a-time");
        bad("2026-02-30T00:00:00Z"); // day out of range
        bad("2026-01-01T24:00:00Z"); // hour out of range
        bad("2026-01-01T00:00:60Z"); // second out of range
                                     // jiff accepts these but Go does not — the strict port must not.
        bad("2026-01-01T00:00Z");
    }

    /// Go `writeTimeHash` checks `t.IsZero()` before formatting, so an
    /// explicit zero timestamp hashes identically to an absent one — while
    /// the dependency hash (`normalize_time_public`) formats unconditionally
    /// and keeps the `0001-01-01T00:00:00Z` spelling.
    #[test]
    fn zero_time_collapses_only_on_the_write_time_path() {
        let mut explicit = FingerprintWriter::new();
        explicit.write_time_hash(Some("0001-01-01T00:00:00Z"));
        let mut absent = FingerprintWriter::new();
        absent.write_time_hash(None);
        assert_eq!(explicit.sum_hex(), absent.sum_hex());

        // A non-zero instant one second later must NOT collapse.
        assert_ne!(normalize_time("0001-01-01T00:00:01Z"), String::new());
        assert_eq!(
            normalize_time_public("0001-01-01T00:00:00Z"),
            "0001-01-01T00:00:00Z"
        );
    }
}
