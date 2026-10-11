// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{LabelKey, LabelValue, label::EMPTY_LABEL_KEY};
use core::fmt;

/// Maximum number of inline labels stored in a [`Context`].
pub const CONTEXT_INLINE_CAPACITY: usize = 8;

/// An explicit semantic attribution value passed through instrumented code.
///
/// A context is a small, copyable set of labels keyed by static [`LabelKey`]s.
/// Labels keep their append order; setting an existing key replaces its value
/// in place.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct Context {
    entries: [ContextEntry; CONTEXT_INLINE_CAPACITY],
    len: u8,
}

impl Context {
    /// Returns the empty root context.
    #[must_use]
    pub const fn root() -> Self {
        Self {
            entries: [ContextEntry::EMPTY; CONTEXT_INLINE_CAPACITY],
            len: 0,
        }
    }

    /// Returns the number of labels carried by this context.
    #[must_use]
    pub const fn len(self) -> usize {
        self.len as usize
    }

    /// Returns `true` when this context has no labels.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    /// Inserts or replaces one label in the context.
    ///
    /// # Panics
    ///
    /// Panics when the inline capacity is exhausted. Hot paths that cannot
    /// tolerate panics should use [`Context::try_with`] instead.
    #[must_use]
    #[track_caller]
    pub fn with<V>(self, key: &'static LabelKey, value: V) -> Self
    where
        V: Into<LabelValue>,
    {
        match self.try_with(key, value) {
            Ok(context) => context,
            Err(_) => panic!("context inline capacity exceeded"),
        }
    }

    /// Tries to insert or replace one label in the context.
    pub fn try_with<V>(
        mut self,
        key: &'static LabelKey,
        value: V,
    ) -> Result<Self, ContextCapacityError>
    where
        V: Into<LabelValue>,
    {
        let entry = ContextEntry::new(key, value.into());
        let len = usize::from(self.len);
        for existing in &mut self.entries[..len] {
            if existing.key() == key {
                *existing = entry;
                return Ok(self);
            }
        }

        if len == CONTEXT_INLINE_CAPACITY {
            return Err(ContextCapacityError);
        }

        self.entries[len] = entry;
        self.len += 1;
        Ok(self)
    }

    /// Returns the labels carried by this context in append order.
    #[must_use]
    pub fn as_slice(&self) -> &[ContextEntry] {
        let len = usize::from(self.len);
        // The prefix up to `len` is always initialized.
        &self.entries[..len]
    }

    /// Returns an iterator over the labels carried by this context.
    pub fn iter(&self) -> core::slice::Iter<'_, ContextEntry> {
        self.as_slice().iter()
    }

    /// Returns `true` when this context contains the given key.
    #[must_use]
    pub fn contains(&self, key: &'static LabelKey) -> bool {
        self.get(key).is_some()
    }

    /// Returns the value associated with the given key.
    #[must_use]
    pub fn get(&self, key: &'static LabelKey) -> Option<LabelValue> {
        self.as_slice()
            .iter()
            .find(|entry| entry.key() == key)
            .map(|entry| entry.value)
    }
}

impl fmt::Debug for Context {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("Context")
            .field(&self.as_slice())
            .finish()
    }
}

impl Default for Context {
    fn default() -> Self {
        Self::root()
    }
}

/// One semantic label stored in a [`Context`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ContextEntry {
    // `None` only in unused slots, which keeps an empty context all-zero so
    // `Context::root` compiles to a plain fill.
    key: Option<&'static LabelKey>,
    value: LabelValue,
}

impl ContextEntry {
    const EMPTY: Self = Self {
        key: None,
        value: LabelValue::Unsigned(0),
    };

    /// Creates a new context entry.
    #[must_use]
    pub const fn new(key: &'static LabelKey, value: LabelValue) -> Self {
        Self {
            key: Some(key),
            value,
        }
    }

    /// Returns the label key.
    #[must_use]
    pub const fn key(self) -> &'static LabelKey {
        match self.key {
            Some(key) => key,
            None => &EMPTY_LABEL_KEY,
        }
    }

    /// Returns the label value.
    #[must_use]
    pub const fn value(self) -> LabelValue {
        self.value
    }
}

/// Returned when a [`Context`] exceeds its inline capacity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct ContextCapacityError;

impl fmt::Display for ContextCapacityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("context inline capacity exceeded")
    }
}

impl core::error::Error for ContextCapacityError {}

#[cfg(test)]
mod tests {
    use super::{CONTEXT_INLINE_CAPACITY, Context, ContextCapacityError};
    use crate::LabelKey;

    static FRAME: LabelKey = LabelKey::new("frame");
    static PASS: LabelKey = LabelKey::new("pass");
    static NODE: LabelKey = LabelKey::new("node");
    static REASON: LabelKey = LabelKey::new("reason");
    static CACHE: LabelKey = LabelKey::new("cache");
    static FILLER: [LabelKey; CONTEXT_INLINE_CAPACITY] = [
        LabelKey::new("k0"),
        LabelKey::new("k1"),
        LabelKey::new("k2"),
        LabelKey::new("k3"),
        LabelKey::new("k4"),
        LabelKey::new("k5"),
        LabelKey::new("k6"),
        LabelKey::new("k7"),
    ];

    #[test]
    fn root_context_is_empty() {
        let context = Context::root();
        assert!(context.is_empty(), "root context should be empty");
        assert_eq!(context.as_slice(), &[]);
    }

    #[test]
    fn appended_labels_preserve_order() {
        let context = Context::root().with(&FRAME, 7_u32).with(&PASS, 3_u64);

        let entries = context.as_slice();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].key(), &FRAME);
        assert_eq!(entries[0].key().name(), "frame");
        assert_eq!(entries[1].key(), &PASS);
    }

    #[test]
    fn duplicate_key_replaces_existing_value() {
        let context = Context::root().with(&FRAME, 7_u32).with(&FRAME, 11_u32);

        assert_eq!(context.len(), 1);
        assert_eq!(
            context.get(&FRAME).and_then(|value| value.as_unsigned()),
            Some(11)
        );
    }

    #[test]
    fn contains_and_get_follow_inserted_labels() {
        let context = Context::root()
            .with(&NODE, 42_u32)
            .with(&REASON, "invalidate");

        assert!(context.contains(&NODE));
        assert_eq!(
            context.get(&REASON).and_then(|value| value.as_static_str()),
            Some("invalidate")
        );
        assert!(!context.contains(&CACHE));
    }

    #[test]
    fn contexts_with_equal_labels_are_equal() {
        let first = Context::root().with(&FRAME, 1_u32).with(&PASS, 2_u32);
        let second = Context::root().with(&FRAME, 1_u32).with(&PASS, 2_u32);
        assert_eq!(first, second);
        assert_ne!(first, Context::root().with(&FRAME, 1_u32));
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn capacity_error_is_an_error() {
        use alloc::string::ToString;

        let error: &dyn core::error::Error = &ContextCapacityError;
        assert_eq!(error.to_string(), "context inline capacity exceeded");
    }

    #[test]
    fn try_with_reports_capacity() {
        let mut context = Context::root();
        for (index, key) in FILLER.iter().enumerate() {
            context = context
                .try_with(
                    key,
                    u32::try_from(index).expect("context index should fit into u32"),
                )
                .expect("context entry should fit");
        }

        let result = context.try_with(&CACHE, 99_u32);
        assert_eq!(result, Err(ContextCapacityError));
    }
}
