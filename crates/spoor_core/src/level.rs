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
}

#[cfg(test)]
mod tests {
    use super::InstrumentationLevel;

    #[test]
    fn trace_captures_resources() {
        assert!(InstrumentationLevel::Trace.captures_resources());
        assert!(!InstrumentationLevel::Scoped.captures_resources());
    }
}
