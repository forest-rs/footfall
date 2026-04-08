// Copyright 2026 the Spoor Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{LabelKey, LabelValue};
use core::fmt;

/// Maximum number of inline labels stored in a [`Context`].
pub const CONTEXT_INLINE_CAPACITY: usize = 8;

/// An explicit semantic attribution value passed through instrumented code.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
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

    /// Appends one label to the context.
    ///
    /// # Panics
    ///
    /// Panics when the inline capacity is exhausted. Hot paths that cannot
    /// tolerate panics should use [`Context::try_with`] instead.
    #[must_use]
    pub fn with<V>(self, key: LabelKey, value: V) -> Self
    where
        V: Into<LabelValue>,
    {
        match self.try_with(key, value) {
            Ok(context) => context,
            Err(_) => panic!("context inline capacity exceeded"),
        }
    }

    /// Tries to append one label to the context.
    pub fn try_with<V>(mut self, key: LabelKey, value: V) -> Result<Self, ContextCapacityError>
    where
        V: Into<LabelValue>,
    {
        let len = usize::from(self.len);
        if len == CONTEXT_INLINE_CAPACITY {
            return Err(ContextCapacityError);
        }

        self.entries[len] = ContextEntry::new(key, value.into());
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
}

impl Default for Context {
    fn default() -> Self {
        Self::root()
    }
}

/// One semantic label stored in a [`Context`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ContextEntry {
    key: LabelKey,
    value: LabelValue,
}

impl ContextEntry {
    const EMPTY: Self = Self {
        key: LabelKey::Frame,
        value: LabelValue::Unsigned(0),
    };

    /// Creates a new context entry.
    #[must_use]
    pub const fn new(key: LabelKey, value: LabelValue) -> Self {
        Self { key, value }
    }

    /// Returns the label key.
    #[must_use]
    pub const fn key(self) -> LabelKey {
        self.key
    }

    /// Returns the label value.
    #[must_use]
    pub const fn value(self) -> LabelValue {
        self.value
    }
}

/// Returned when a [`Context`] exceeds its inline capacity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContextCapacityError;

impl fmt::Display for ContextCapacityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("context inline capacity exceeded")
    }
}

#[cfg(test)]
mod tests {
    use super::{CONTEXT_INLINE_CAPACITY, Context, ContextCapacityError};
    use crate::LabelKey;

    #[test]
    fn root_context_is_empty() {
        let context = Context::root();
        assert!(context.is_empty(), "root context should be empty");
        assert_eq!(context.as_slice(), &[]);
    }

    #[test]
    fn appended_labels_preserve_order() {
        let context = Context::root()
            .with(LabelKey::Frame, 7_u32)
            .with(LabelKey::Pass, 3_u64);

        let entries = context.as_slice();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].key(), LabelKey::Frame);
        assert_eq!(entries[1].key(), LabelKey::Pass);
    }

    #[test]
    fn try_with_reports_capacity() {
        let mut context = Context::root();
        for index in 0..CONTEXT_INLINE_CAPACITY {
            context = context
                .try_with(
                    LabelKey::Node,
                    u32::try_from(index).expect("context index should fit into u32"),
                )
                .expect("context entry should fit");
        }

        let result = context.try_with(LabelKey::Node, 99_u32);
        assert_eq!(result, Err(ContextCapacityError));
    }
}
