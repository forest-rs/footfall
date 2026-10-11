// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use core::sync::atomic::{AtomicU8, Ordering};

/// Runtime instrumentation level.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub enum InstrumentationLevel {
    /// Instrumentation is disabled.
    #[default]
    Off,
    /// Counters and gauges are collected.
    Counters,
    /// Counters and scopes are collected.
    Scoped,
    /// Full trace capture is enabled.
    Trace,
}

impl InstrumentationLevel {
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
    MAX_LEVEL.store(level.to_u8(), Ordering::Relaxed);
}

/// Returns the process-wide maximum instrumentation level.
///
/// See [`set_max_level`].
#[inline]
#[must_use]
pub fn max_level() -> InstrumentationLevel {
    InstrumentationLevel::from_u8(MAX_LEVEL.load(Ordering::Relaxed))
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
