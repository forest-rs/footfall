// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::ResourceId;
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
/// A key's identity is its name: two keys with the same name are the same
/// key, in process and for consumers such as exporters and remote sessions,
/// which only ever see names. Keys can therefore be `static` or `const`
/// items, and libraries that want keys of their own should prefix the names,
/// for example `"mylib.pass"`.
///
/// ```
/// use footfall_core::{Context, LabelKey};
///
/// static FRAME: LabelKey = LabelKey::new("frame");
/// const ALSO_FRAME: LabelKey = LabelKey::new("frame");
///
/// let context = Context::root().with(&FRAME, 7_u32);
/// assert_eq!(context.get(&ALSO_FRAME).and_then(|value| value.as_unsigned()), Some(7));
/// ```
pub struct LabelKey {
    name: &'static str,
    // FNV-1a hash of `name`, computed at declaration so equality and hashing
    // stay a pointer or integer comparison on the hot path.
    name_hash: u64,
}

/// Empty key used for unused [`Context`](crate::Context) slots.
pub(crate) static EMPTY_LABEL_KEY: LabelKey = LabelKey::new("");

impl LabelKey {
    /// Creates a new label key.
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            name_hash: fnv1a(name.as_bytes()),
        }
    }

    /// Returns the name that identifies this key.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }
}

impl PartialEq for LabelKey {
    /// Keys are equal when their names are equal.
    fn eq(&self, other: &Self) -> bool {
        ptr::eq(self, other) || (self.name_hash == other.name_hash && same_name(self, other))
    }
}

impl Eq for LabelKey {}

/// Compares key names once their hashes match.
///
/// Kept out of line so that key comparison in hot callers such as
/// [`Context::try_with`](crate::Context::try_with) stays small enough to inline.
#[cold]
#[inline(never)]
fn same_name(left: &LabelKey, right: &LabelKey) -> bool {
    left.name == right.name
}

/// 64-bit FNV-1a, usable in `const` contexts.
const fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut index = 0;
    while index < bytes.len() {
        hash ^= bytes[index] as u64;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
        index += 1;
    }
    hash
}

impl Hash for LabelKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.name_hash);
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
    static PASS: LabelKey = LabelKey::new("pass");

    #[test]
    fn label_keys_are_identified_by_name() {
        assert_eq!(FRAME.name(), "frame");
        assert_eq!(FRAME, FRAME);
        assert_eq!(FRAME, OTHER_FRAME, "same name, same key");
        assert_ne!(FRAME, PASS);
    }

    #[test]
    fn equal_keys_hash_equally() {
        use core::hash::{BuildHasher, BuildHasherDefault};

        let hasher = BuildHasherDefault::<FnvTestHasher>::default();
        assert_eq!(hasher.hash_one(&FRAME), hasher.hash_one(&OTHER_FRAME));
        assert_ne!(hasher.hash_one(&FRAME), hasher.hash_one(&PASS));
    }

    /// Minimal deterministic hasher so the test needs no `std`.
    #[derive(Default)]
    struct FnvTestHasher(u64);

    impl core::hash::Hasher for FnvTestHasher {
        fn finish(&self) -> u64 {
            self.0
        }

        fn write(&mut self, bytes: &[u8]) {
            for byte in bytes {
                self.0 = (self.0 ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3);
            }
        }
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
