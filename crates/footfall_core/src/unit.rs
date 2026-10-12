// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

/// Unit associated with a counter or gauge descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Unit {
    /// Raw bytes.
    Bytes,
    /// Dimensionless count.
    Count,
    /// Nanoseconds.
    Nanos,
    /// Microseconds.
    Micros,
    /// Ratio in parts per million: `1_000_000` is `1.0`, so `250_000` is a
    /// quarter.
    Ratio,
    /// Generic item count.
    Items,
}

impl Unit {
    /// Returns the stable lowercase name used by exporters and consumers.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bytes => "bytes",
            Self::Count => "count",
            Self::Nanos => "nanos",
            Self::Micros => "micros",
            Self::Ratio => "ratio",
            Self::Items => "items",
        }
    }
}
