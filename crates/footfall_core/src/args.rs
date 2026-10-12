// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::LabelValue;
use core::fmt;

/// Maximum number of inline entries stored in an [`Args`] value.
pub const ARGS_INLINE_CAPACITY: usize = 4;

/// Per-instance argument values attached to one observation.
///
/// Where [`Context`](crate::Context) attributes an observation to a semantic
/// owner shared across many observations, `Args` carries small facts about one
/// specific observation instance, such as how many items one particular scope
/// processed. Keys are static strings so the hot path stays free of dynamic
/// string assembly.
///
/// Build one with [`args!`](crate::args!) or [`Args::none`] and
/// [`Args::with`], and pass it to [`scope_with`](crate::scope_with),
/// [`scope_begin_with`](crate::scope_begin_with), or
/// [`message_with`](crate::message_with).
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct Args {
    entries: [ArgEntry; ARGS_INLINE_CAPACITY],
    len: u8,
}

impl Args {
    /// Returns the empty argument list.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            entries: [ArgEntry::EMPTY; ARGS_INLINE_CAPACITY],
            len: 0,
        }
    }

    /// Returns the number of entries carried by this argument list.
    #[must_use]
    pub const fn len(self) -> usize {
        self.len as usize
    }

    /// Returns `true` when this argument list has no entries.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    /// Inserts or replaces one argument.
    ///
    /// # Panics
    ///
    /// Panics when the inline capacity is exhausted. Hot paths that cannot
    /// tolerate panics should use [`Args::try_with`] instead.
    #[must_use]
    #[track_caller]
    pub fn with<V>(self, key: &'static str, value: V) -> Self
    where
        V: Into<LabelValue>,
    {
        match self.try_with(key, value) {
            Ok(args) => args,
            Err(_) => panic!("args inline capacity exceeded"),
        }
    }

    /// Tries to insert or replace one argument.
    pub fn try_with<V>(mut self, key: &'static str, value: V) -> Result<Self, ArgsCapacityError>
    where
        V: Into<LabelValue>,
    {
        let value = value.into();
        for entry in &mut self.entries[..usize::from(self.len)] {
            if entry.key == key {
                *entry = ArgEntry::new(key, value);
                return Ok(self);
            }
        }

        let len = usize::from(self.len);
        if len == ARGS_INLINE_CAPACITY {
            return Err(ArgsCapacityError);
        }

        self.entries[len] = ArgEntry::new(key, value);
        self.len += 1;
        Ok(self)
    }

    /// Returns the entries carried by this argument list in append order.
    #[must_use]
    pub fn as_slice(&self) -> &[ArgEntry] {
        let len = usize::from(self.len);
        // The prefix up to `len` is always initialized.
        &self.entries[..len]
    }

    /// Returns an iterator over the entries carried by this argument list.
    pub fn iter(&self) -> core::slice::Iter<'_, ArgEntry> {
        self.as_slice().iter()
    }

    /// Returns the value associated with the given key.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<LabelValue> {
        self.as_slice()
            .iter()
            .find(|entry| entry.key == key)
            .map(|entry| entry.value)
    }
}

impl fmt::Debug for Args {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("Args")
            .field(&self.as_slice())
            .finish()
    }
}

impl Default for Args {
    fn default() -> Self {
        Self::none()
    }
}

/// One argument stored in an [`Args`] value, as [`Args::iter`] yields it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArgEntry {
    key: &'static str,
    value: LabelValue,
}

impl ArgEntry {
    const EMPTY: Self = Self {
        key: "",
        value: LabelValue::Unsigned(0),
    };

    /// Creates a new argument entry.
    #[must_use]
    pub const fn new(key: &'static str, value: LabelValue) -> Self {
        Self { key, value }
    }

    /// Returns the argument key.
    #[must_use]
    pub const fn key(self) -> &'static str {
        self.key
    }

    /// Returns the argument value.
    #[must_use]
    pub const fn value(self) -> LabelValue {
        self.value
    }
}

/// Returned by [`Args::try_with`] when a new key would exceed
/// [`ARGS_INLINE_CAPACITY`] entries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct ArgsCapacityError;

impl fmt::Display for ArgsCapacityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("args inline capacity exceeded")
    }
}

impl core::error::Error for ArgsCapacityError {}

#[cfg(test)]
mod tests {
    use super::{ARGS_INLINE_CAPACITY, Args, ArgsCapacityError};

    #[test]
    fn none_is_empty() {
        let args = Args::none();
        assert!(args.is_empty());
        assert_eq!(args.as_slice(), &[]);
    }

    #[test]
    fn entries_preserve_order_and_replace_duplicates() {
        let args = Args::none()
            .with("bytes", 64_u32)
            .with("items", 3_u32)
            .with("bytes", 128_u32);

        assert_eq!(args.len(), 2);
        assert_eq!(args.as_slice()[0].key(), "bytes");
        assert_eq!(
            args.get("bytes").and_then(|value| value.as_unsigned()),
            Some(128)
        );
        assert_eq!(
            args.get("items").and_then(|value| value.as_unsigned()),
            Some(3)
        );
        assert_eq!(args.get("missing"), None);
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn debug_shows_only_entries_in_use() {
        assert_eq!(alloc::format!("{:?}", Args::none()), "Args([])");
        assert_eq!(
            alloc::format!("{:?}", Args::none().with("items", 3_u32)),
            "Args([ArgEntry { key: \"items\", value: Unsigned(3) }])"
        );
    }

    #[test]
    fn try_with_reports_capacity() {
        let mut args = Args::none();
        let keys = ["a", "b", "c", "d"];
        for (index, key) in keys.into_iter().enumerate().take(ARGS_INLINE_CAPACITY) {
            args = args
                .try_with(key, u32::try_from(index).expect("index should fit"))
                .expect("entry should fit");
        }
        assert_eq!(args.try_with("e", 9_u32), Err(ArgsCapacityError));
    }
}
