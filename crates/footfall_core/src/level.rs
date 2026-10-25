// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use core::sync::atomic::{AtomicU8, Ordering};

/// How much the installed backend collects.
///
/// Each level adds to the one before: `Off`, then `Counters`, then `Scoped`,
/// then `Trace`. Emissions above the level return immediately. The installed
/// backend sets the level with [`set_max_level`]; [`max_level`] reads it, so a
/// call site can skip preparing arguments that would be discarded. The
/// runtime's level is set through `RuntimeBuilder::level` and
/// `Runtime::set_level` in `footfall_runtime`, and reported in each snapshot.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub enum InstrumentationLevel {
    /// Instrumentation is disabled.
    #[default]
    Off,
    /// Counters, gauges, and histograms are collected.
    Counters,
    /// Adds scopes, frame, transaction, and pass markers, flows, and
    /// messages.
    Scoped,
    /// Adds resource lifecycle observations, so everything is collected.
    Trace,
}

impl InstrumentationLevel {
    /// Returns the level's stable lowercase name: `"off"`, `"counters"`,
    /// `"scoped"`, or `"trace"`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Counters => "counters",
            Self::Scoped => "scoped",
            Self::Trace => "trace",
        }
    }

    /// Returns `true` when counters are enabled at this level.
    #[must_use]
    pub const fn captures_counters(self) -> bool {
        !matches!(self, Self::Off)
    }

    /// Returns `true` when scopes are enabled at this level.
    #[must_use]
    pub const fn captures_scopes(self) -> bool {
        matches!(self, Self::Scoped | Self::Trace)
    }

    /// Returns `true` when resource observations are enabled at this level.
    #[must_use]
    pub const fn captures_resources(self) -> bool {
        matches!(self, Self::Trace)
    }

    /// Returns `true` when frame, transaction, and pass markers are enabled.
    #[must_use]
    pub const fn captures_markers(self) -> bool {
        matches!(self, Self::Scoped | Self::Trace)
    }

    /// Returns `true` when causal flow observations are enabled.
    #[must_use]
    pub const fn captures_flows(self) -> bool {
        matches!(self, Self::Scoped | Self::Trace)
    }

    /// Returns `true` when message events are enabled.
    #[must_use]
    pub const fn captures_messages(self) -> bool {
        matches!(self, Self::Scoped | Self::Trace)
    }

    const fn to_u8(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::Counters => 1,
            Self::Scoped => 2,
            Self::Trace => 3,
        }
    }

    const fn from_u8(raw: u8) -> Self {
        match raw {
            0 => Self::Off,
            1 => Self::Counters,
            2 => Self::Scoped,
            _ => Self::Trace,
        }
    }
}

/// Process-wide ceiling checked by every emission before dispatch.
// Current level and minimum within the installed backend's epoch share one
// atomic, so a concurrent setter cannot publish a level without its floor.
const LEVEL_MASK: u8 = 0b11;
const FLOOR_SHIFT: u32 = 2;
const TRACK_HISTORY: u8 = 1 << 4;
static MAX_LEVEL: AtomicU8 = AtomicU8::new(InstrumentationLevel::Off.to_u8());

/// Sets the process-wide maximum instrumentation level.
///
/// Every emission function checks this level first, with one relaxed load and
/// a branch, and returns before reaching the backend when the level does not
/// capture that kind of observation. It starts at
/// [`InstrumentationLevel::Off`], so emissions before a backend is installed
/// cost only that check; `install_backend` raises it to
/// [`InstrumentationLevel::Trace`] so the installed backend sees everything
/// until the level is lowered.
///
/// The level only gates emissions that start or stand alone: scope ends and
/// `End` markers always reach the backend, so lowering the level while a
/// scope or frame is open never leaves a backend with an unclosed one.
/// Backends may still apply their own, finer filtering.
pub fn set_max_level(level: InstrumentationLevel) {
    let level = level.to_u8();
    let mut state = MAX_LEVEL.load(Ordering::Relaxed);
    loop {
        let history = if state & TRACK_HISTORY != 0 {
            TRACK_HISTORY | (((state >> FLOOR_SHIFT) & LEVEL_MASK).min(level) << FLOOR_SHIFT)
        } else {
            0
        };
        match MAX_LEVEL.compare_exchange_weak(
            state,
            level | history,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(current) => state = current,
        }
    }
}

/// Returns the process-wide maximum instrumentation level.
///
/// See [`set_max_level`].
#[inline]
#[must_use]
pub fn max_level() -> InstrumentationLevel {
    InstrumentationLevel::from_u8(MAX_LEVEL.load(Ordering::Relaxed) & LEVEL_MASK)
}

/// Returns the lowest process-wide ceiling since successful backend installation.
/// Raising the ceiling never restores recording history suppressed by a lower one.
///
/// Before installation completes, returns `Trace` as the identity for combining
/// with a backend's own floor: no installed front-door epoch applies yet. This
/// does not certify pre-installation emissions, which can reach the no-op backend.
/// Emissions concurrent with unfinished installation have no coverage guarantee.
#[must_use]
pub fn max_level_floor() -> InstrumentationLevel {
    let state = MAX_LEVEL.load(Ordering::Relaxed);
    if state & TRACK_HISTORY == 0 {
        InstrumentationLevel::Trace
    } else {
        InstrumentationLevel::from_u8((state >> FLOOR_SHIFT) & LEVEL_MASK)
    }
}

/// Starts the sole installed-backend epoch, atomically raising the front door.
#[cfg(feature = "std")]
pub(crate) fn start_max_level_history() {
    let trace = InstrumentationLevel::Trace.to_u8();
    MAX_LEVEL.store(
        trace | (trace << FLOOR_SHIFT) | TRACK_HISTORY,
        Ordering::Relaxed,
    );
}

#[cfg(test)]
mod tests {
    use super::InstrumentationLevel;

    #[test]
    fn trace_captures_resources() {
        assert!(InstrumentationLevel::Trace.captures_resources());
        assert!(!InstrumentationLevel::Scoped.captures_resources());
    }

    #[test]
    fn scoped_captures_markers_and_flows() {
        assert!(InstrumentationLevel::Scoped.captures_markers());
        assert!(InstrumentationLevel::Scoped.captures_flows());
        assert!(!InstrumentationLevel::Counters.captures_markers());
        assert!(!InstrumentationLevel::Counters.captures_flows());
    }
}
