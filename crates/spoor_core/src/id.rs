// Copyright 2026 the Spoor Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
        #[repr(transparent)]
        pub struct $name(u64);

        impl $name {
            /// Creates an id from its raw integer representation.
            #[must_use]
            pub const fn new(raw: u64) -> Self {
                Self(raw)
            }

            /// Returns the raw integer representation of this id.
            #[must_use]
            pub const fn get(self) -> u64 {
                self.0
            }
        }
    };
}

define_id!(
    /// Canonical runtime handle for a semantic context.
    ContextId
);

define_id!(
    /// Stable id for one scope descriptor.
    ScopeDescId
);

define_id!(
    /// Stable id for one counter descriptor.
    CounterDescId
);

define_id!(
    /// Stable id for one logical resource instance.
    ResourceId
);

define_id!(
    /// Stable id for one logical resource kind.
    ResourceKindId
);

#[cfg(test)]
mod tests {
    use super::{ContextId, CounterDescId, ResourceId, ResourceKindId, ScopeDescId};

    #[test]
    fn ids_round_trip() {
        assert_eq!(ContextId::new(1).get(), 1);
        assert_eq!(ScopeDescId::new(2).get(), 2);
        assert_eq!(CounterDescId::new(3).get(), 3);
        assert_eq!(ResourceId::new(4).get(), 4);
        assert_eq!(ResourceKindId::new(5).get(), 5);
    }
}
