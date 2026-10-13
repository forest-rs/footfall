// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Human-readable numbers: durations, byte sizes, and counter values.

use footfall::Unit;

/// Formats a duration in microseconds: `850 µs`, `12.4 ms`, `3.20 s`.
#[must_use]
pub(crate) fn micros(micros: u64) -> String {
    match micros {
        0..1_000 => format!("{micros} µs"),
        1_000..10_000 => format!("{:.2} ms", as_f64(micros) / 1e3),
        10_000..1_000_000 => format!("{:.1} ms", as_f64(micros) / 1e3),
        _ => format!("{:.2} s", as_f64(micros) / 1e6),
    }
}

/// Formats a duration: `850 µs`, `12.4 ms`, `3.20 s`.
#[must_use]
pub(crate) fn duration(duration: core::time::Duration) -> String {
    micros(as_micros(duration))
}

/// Returns a duration in whole microseconds, saturating.
#[must_use]
pub(crate) fn as_micros(duration: core::time::Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

/// Formats a signed change in microseconds: `+850 µs`, `-12.4 ms`.
#[must_use]
pub(crate) fn change_micros(micros: f64) -> String {
    let sign = if micros < 0.0 { '-' } else { '+' };
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a duration in microseconds, rounded for display"
    )]
    let magnitude = micros.abs().round() as u64;
    format!("{sign}{}", self::micros(magnitude))
}

/// Formats a byte count: `512 B`, `4.0 KiB`, `1.25 MiB`.
#[must_use]
pub(crate) fn bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;
    match bytes {
        0..KIB => format!("{bytes} B"),
        KIB..MIB => format!("{:.1} KiB", as_f64(bytes) / as_f64(KIB)),
        MIB..GIB => format!("{:.2} MiB", as_f64(bytes) / as_f64(MIB)),
        _ => format!("{:.2} GiB", as_f64(bytes) / as_f64(GIB)),
    }
}

/// Formats a count with thousands separators: `1,234,567`.
#[must_use]
pub(crate) fn count(count: u64) -> String {
    let digits = count.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// Formats a counter value in its unit.
#[must_use]
pub(crate) fn value(unit: Unit, value: u64) -> String {
    match unit {
        Unit::Bytes => bytes(value),
        Unit::Micros => micros(value),
        Unit::Nanos => micros(value / 1_000),
        Unit::Ratio => format!("{:.1}%", as_f64(value) / 10_000.0),
        _ => count(value),
    }
}

#[expect(
    clippy::cast_precision_loss,
    reason = "display only; values beyond 2^53 lose digits nobody reads"
)]
fn as_f64(value: u64) -> f64 {
    value as f64
}

#[cfg(test)]
mod tests {
    use super::{bytes, count, micros};

    #[test]
    fn numbers_read_naturally() {
        assert_eq!(micros(850), "850 µs");
        assert_eq!(micros(12_400), "12.4 ms");
        assert_eq!(micros(3_200_000), "3.20 s");
        assert_eq!(bytes(4096), "4.0 KiB");
        assert_eq!(count(1_234_567), "1,234,567");
        assert_eq!(count(12), "12");
    }
}
