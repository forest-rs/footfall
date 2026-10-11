// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::ResourceId;

/// Shared label keys used to attribute work and counters.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
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

impl LabelKey {
    /// Returns the stable string name for this label key.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Frame => "frame",
            Self::Transaction => "transaction",
            Self::Pass => "pass",
            Self::Subsystem => "subsystem",
            Self::Node => "node",
            Self::Widget => "widget",
            Self::Layer => "layer",
            Self::Resource => "resource",
            Self::Cache => "cache",
            Self::Reason => "reason",
        }
    }
}

/// Compact label value attached to a [`LabelKey`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
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

impl LabelValue {
    /// Returns the unsigned payload when present.
    #[must_use]
    pub const fn as_unsigned(self) -> Option<u64> {
        match self {
            Self::Unsigned(value) => Some(value),
            _ => None,
        }
    }

    /// Returns the signed payload when present.
    #[must_use]
    pub const fn as_signed(self) -> Option<i64> {
        match self {
            Self::Signed(value) => Some(value),
            _ => None,
        }
    }

    /// Returns the boolean payload when present.
    #[must_use]
    pub const fn as_bool(self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(value),
            _ => None,
        }
    }

    /// Returns the static string payload when present.
    #[must_use]
    pub const fn as_static_str(self) -> Option<&'static str> {
        match self {
            Self::StaticStr(value) => Some(value),
            _ => None,
        }
    }
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
    use super::{LabelKey, LabelValue};
    use crate::ResourceId;

    #[test]
    fn label_key_names_are_stable() {
        assert_eq!(LabelKey::Frame.as_str(), "frame");
        assert_eq!(LabelKey::Reason.as_str(), "reason");
    }

    #[test]
    fn resource_id_converts_to_unsigned_value() {
        assert_eq!(
            LabelValue::from(ResourceId::new(12)),
            LabelValue::Unsigned(12)
        );
    }

    #[test]
    fn label_value_accessors_match_variant() {
        assert_eq!(LabelValue::Unsigned(7).as_unsigned(), Some(7));
        assert_eq!(LabelValue::Signed(-3).as_signed(), Some(-3));
        assert_eq!(LabelValue::Bool(true).as_bool(), Some(true));
        assert_eq!(
            LabelValue::StaticStr("atlas").as_static_str(),
            Some("atlas")
        );
    }
}
