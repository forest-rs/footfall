// Copyright 2026 the Spoor Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

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
