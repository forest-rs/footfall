// Copyright 2026 the Spoor Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::ResourceId;

/// Shared label keys used to attribute work and counters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LabelKey {
    /// A frame boundary or frame-local owner.
    Frame,
    /// A transaction boundary or transaction-local owner.
    Transaction,
    /// A pass boundary or pass-local owner.
    Pass,
    /// A broad subsystem identifier.
    Subsystem,
    /// A display or scene node identifier.
    Node,
    /// A widget identifier.
    Widget,
    /// A layer identifier.
    Layer,
    /// A logical resource identifier.
    Resource,
    /// A cache identifier.
    Cache,
    /// A causal or invalidation reason.
    Reason,
}

/// Compact label value attached to a [`LabelKey`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LabelValue {
    /// Unsigned numeric payload.
    Unsigned(u64),
    /// Signed numeric payload.
    Signed(i64),
    /// Boolean payload.
    Bool(bool),
    /// Static string payload for non-hot-path labels.
    StaticStr(&'static str),
}

impl From<bool> for LabelValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<u8> for LabelValue {
    fn from(value: u8) -> Self {
        Self::Unsigned(u64::from(value))
    }
}

impl From<u16> for LabelValue {
    fn from(value: u16) -> Self {
        Self::Unsigned(u64::from(value))
    }
}

impl From<u32> for LabelValue {
    fn from(value: u32) -> Self {
        Self::Unsigned(u64::from(value))
    }
}

impl From<u64> for LabelValue {
    fn from(value: u64) -> Self {
        Self::Unsigned(value)
    }
}

impl From<i8> for LabelValue {
    fn from(value: i8) -> Self {
        Self::Signed(i64::from(value))
    }
}

impl From<i16> for LabelValue {
    fn from(value: i16) -> Self {
        Self::Signed(i64::from(value))
    }
}

impl From<i32> for LabelValue {
    fn from(value: i32) -> Self {
        Self::Signed(i64::from(value))
    }
}

impl From<i64> for LabelValue {
    fn from(value: i64) -> Self {
        Self::Signed(value)
    }
}

impl From<&'static str> for LabelValue {
    fn from(value: &'static str) -> Self {
        Self::StaticStr(value)
    }
}

impl From<ResourceId> for LabelValue {
    fn from(value: ResourceId) -> Self {
        Self::Unsigned(value.get())
    }
}

#[cfg(test)]
mod tests {
    use super::LabelValue;
    use crate::ResourceId;

    #[test]
    fn resource_id_converts_to_unsigned_value() {
        assert_eq!(
            LabelValue::from(ResourceId::new(12)),
            LabelValue::Unsigned(12)
        );
    }
}
