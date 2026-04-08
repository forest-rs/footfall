// Copyright 2026 the Spoor Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

/// Unit associated with a counter or gauge descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Unit {
    /// Raw bytes.
    Bytes,
    /// Dimensionless count.
    Count,
    /// Nanoseconds.
    Nanos,
    /// Microseconds.
    Micros,
    /// Ratio represented as a scalar fraction.
    Ratio,
    /// Generic item count.
    Items,
}
