// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{ResourceId, descriptor::StaticOnly};
use core::{
    fmt,
    hash::{Hash, Hasher},
    ptr,
};

/// Static key for one semantic label, such as a frame or pass number.
///
/// Footfall ships no label vocabulary of its own; applications and libraries
/// declare the keys they attribute work by:
///
/// ```
/// use footfall_core::{Context, LabelKey};
///
/// static FRAME: LabelKey = LabelKey::new("frame");
/// static PASS: LabelKey = LabelKey::new("pass");
///
/// let context = Context::root().with(&FRAME, 7_u32).with(&PASS, 1_u32);
/// assert_eq!(context.get(&FRAME).and_then(|value| value.as_unsigned()), Some(7));
/// ```
///
/// Like descriptors, a key's identity is its address: declare keys as
/// `static` items (see the [crate docs](crate#descriptors)). A `const` key is
/// rejected where a `&'static` key is required:
///
/// ```compile_fail,E0716
/// use footfall_core::{Context, LabelKey};
///
/// const FRAME: LabelKey = LabelKey::new("frame");
/// let _context = Context::root().with(&FRAME, 7_u32);
/// ```
///
/// Two keys with the same name are different keys in process, but consumers
/// that only see names, such as exporters and remote sessions, cannot tell
/// them apart, so give keys distinct names, for example by prefixing a
/// library's keys with its name.
pub struct LabelKey {
    name: &'static str,
    _static_only: StaticOnly,
}

/// Empty key used for unused [`Context`](crate::Context) slots.
pub(crate) static EMPTY_LABEL_KEY: LabelKey = LabelKey::new("");

impl LabelKey {
    /// Creates a new label key.
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _static_only: StaticOnly,
        }
    }

    /// Returns the name consumers see for this key.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }
}

impl PartialEq for LabelKey {
    /// Keys are equal only when they are the same key.
    fn eq(&self, other: &Self) -> bool {
        ptr::eq(self, other)
    }
}

impl Eq for LabelKey {}

impl Hash for LabelKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        ptr::from_ref(self).hash(state);
    }
}

impl fmt::Debug for LabelKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("LabelKey").field(&self.name).finish()
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

    static FRAME: LabelKey = LabelKey::new("frame");
    static OTHER_FRAME: LabelKey = LabelKey::new("frame");

    #[test]
    fn label_keys_expose_names_and_compare_by_identity() {
        assert_eq!(FRAME.name(), "frame");
        assert_eq!(FRAME, FRAME);
        assert_ne!(FRAME, OTHER_FRAME, "same name, different key");
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
