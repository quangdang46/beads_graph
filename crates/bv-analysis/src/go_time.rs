//! Go's `time.Duration` arithmetic, reproduced where the oracle's float
//! output depends on it.
//!
//! Go's `Duration.Hours()` is NOT `nanoseconds / 3.6e12`:
//!
//! ```go
//! func (d Duration) Hours() float64 {
//!     h := d / Hour
//!     return float64(h) + float64(d%Hour)/float64(Hour)
//! }
//! ```
//!
//! The integer hours plus the fractional remainder rounds differently, and
//! every score that divides a duration by 24 inherits the difference. The
//! port computed `seconds / 86400`, which is a third rounding again, and the
//! two disagreed in the last digits of `avg_days_to_close`,
//! `avg_days_since_update` and the staleness norm.

/// The exact nanosecond difference between two instants — Go's
/// `time.Time.Sub` returns a `time.Duration`, an exact nanosecond count.
pub fn duration_nanos(later: jiff::Timestamp, earlier: jiff::Timestamp) -> i128 {
    (later.as_second() - earlier.as_second()) as i128 * 1_000_000_000
        + (later.subsec_nanosecond() - earlier.subsec_nanosecond()) as i128
}

/// Go's `time.Duration.Hours()` — the integer hours plus the fractional
/// remainder, not `nanoseconds / 3.6e12`.
pub fn duration_hours(nanos: i128) -> f64 {
    const NANOS_PER_HOUR: i128 = 3_600_000_000_000;
    let whole = nanos / NANOS_PER_HOUR;
    let remainder = nanos % NANOS_PER_HOUR;
    whole as f64 + (remainder as f64) / (NANOS_PER_HOUR as f64)
}

/// Go's `time.Duration.Hours() / 24` — the days since a duration, rounded the
/// way the oracle rounds them.
pub fn duration_days(nanos: i128) -> f64 {
    duration_hours(nanos) / 24.0
}
