// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Readings of clocks other than the backend's own.

use crate::ClockDesc;

/// How long one tick of a clock lasts, as `nanos` nanoseconds per `ticks`
/// ticks: the form platform timebases take.
///
/// Mach absolute time on Apple silicon is 125 nanoseconds per 3 ticks
/// (24 MHz); a 10 MHz performance counter is 100 per 1; a clock in
/// nanoseconds is [`TickPeriod::NANOSECOND`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TickPeriod {
    nanos: u32,
    ticks: u32,
}

impl TickPeriod {
    /// One nanosecond per tick.
    pub const NANOSECOND: Self = Self { nanos: 1, ticks: 1 };

    /// One microsecond per tick, as `performance.now()` scaled to integers.
    pub const MICROSECOND: Self = Self {
        nanos: 1_000,
        ticks: 1,
    };

    /// Creates the period of `nanos` nanoseconds per `ticks` ticks, such as a
    /// Mach timebase's numerator and denominator. `ticks` of `0` is taken as
    /// `1`.
    #[must_use]
    pub const fn new(nanos: u32, ticks: u32) -> Self {
        Self {
            nanos,
            ticks: if ticks == 0 { 1 } else { ticks },
        }
    }

    /// Creates the period of a clock that ticks `hertz` times a second, such
    /// as a performance counter's frequency.
    #[must_use]
    pub const fn from_hertz(hertz: u32) -> Self {
        Self::new(1_000_000_000, hertz)
    }

    /// Creates the period of a clock whose tick lasts `nanos` nanoseconds,
    /// such as a GPU timestamp period, to a millionth of a nanosecond.
    #[must_use]
    pub fn from_nanos(nanos: f32) -> Self {
        let scaled = f64::from(nanos) * 1_000_000.0;
        let nanos_per_million = if scaled.is_nan() || scaled <= 0.0 {
            0
        } else if scaled >= f64::from(u32::MAX) {
            u32::MAX
        } else {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "the value is in range and rounded"
            )]
            let rounded = (scaled + 0.5) as u32;
            rounded
        };
        Self::new(nanos_per_million, 1_000_000)
    }

    /// Returns how many nanoseconds [`ticks`](Self::ticks) ticks last.
    #[must_use]
    pub const fn nanos(self) -> u32 {
        self.nanos
    }

    /// Returns how many ticks last [`nanos`](Self::nanos) nanoseconds; never
    /// `0`.
    #[must_use]
    pub const fn ticks(self) -> u32 {
        self.ticks
    }

    /// Returns how many nanoseconds `ticks` ticks last, saturating.
    #[must_use]
    pub fn ticks_to_nanos(self, ticks: u64) -> u64 {
        let nanos = u128::from(ticks) * u128::from(self.nanos) / u128::from(self.ticks);
        u64::try_from(nanos).unwrap_or(u64::MAX)
    }
}

/// One reading of a clock: the clock, its tick count, and how long a tick
/// lasts.
///
/// Pass it to [`clock_sync`](crate::clock_sync) or
/// [`clock_pair`](crate::clock_pair).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClockReading {
    clock: &'static ClockDesc,
    ticks: u64,
    period: TickPeriod,
}

impl ClockReading {
    /// Creates a reading of `clock`.
    #[must_use]
    pub const fn new(clock: &'static ClockDesc, ticks: u64, period: TickPeriod) -> Self {
        Self {
            clock,
            ticks,
            period,
        }
    }

    /// Returns the clock.
    #[must_use]
    pub const fn clock(&self) -> &'static ClockDesc {
        self.clock
    }

    /// Returns the tick count.
    #[must_use]
    pub const fn ticks(&self) -> u64 {
        self.ticks
    }

    /// Returns how long a tick lasts.
    #[must_use]
    pub const fn period(&self) -> TickPeriod {
        self.period
    }
}

#[cfg(test)]
mod tests {
    use super::TickPeriod;

    #[test]
    fn periods_convert_ticks_to_nanoseconds() {
        assert_eq!(
            TickPeriod::new(125, 3).ticks_to_nanos(24_000_000),
            1_000_000_000
        );
        assert_eq!(TickPeriod::from_hertz(10_000_000).ticks_to_nanos(10), 1_000);
        assert_eq!(
            TickPeriod::from_nanos(41.666_668).ticks_to_nanos(24_000_000),
            1_000_000_032
        );
        assert_eq!(TickPeriod::new(1, 0).ticks(), 1);
        assert_eq!(TickPeriod::NANOSECOND.ticks_to_nanos(u64::MAX), u64::MAX);
    }
}
