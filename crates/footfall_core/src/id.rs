// Copyright 2026 the Footfall Authors
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
    /// Id of a registered context in a snapshot.
    ///
    /// It carries the same value as the [`ContextHandle`](crate::ContextHandle)
    /// the context was emitted with; `0` is the empty root context.
    ContextId
);

define_id!(
    /// Opaque id for one scope descriptor, as [`ScopeDesc::id`](crate::ScopeDesc::id)
    /// returns it.
    ///
    /// Inside the process the id is derived from the descriptor's address:
    /// unique among live descriptors within one process run, not stable
    /// across runs. Runtimes must not expose that value; snapshots carry
    /// dense, session-scoped ids instead.
    ScopeDescId
);

define_id!(
    /// Opaque id for one counter descriptor, as [`CounterDesc::id`](crate::CounterDesc::id)
    /// returns it.
    ///
    /// Inside the process the id is derived from the descriptor's address:
    /// unique among live descriptors within one process run, not stable
    /// across runs. Runtimes must not expose that value; snapshots carry
    /// dense, session-scoped ids instead.
    CounterDescId
);

define_id!(
    /// Identifies one resource instance, such as one atlas among several.
    ///
    /// The caller chooses it, for example from an index or a handle it
    /// already has, and passes the same id to
    /// [`resource_create`](crate::resource_create),
    /// [`resource_update`](crate::resource_update), and
    /// [`resource_drop`](crate::resource_drop) for that instance.
    ResourceId
);

define_id!(
    /// Opaque id for one logical resource kind, as
    /// [`ResourceKindDesc::id`](crate::ResourceKindDesc::id) returns it.
    ///
    /// Inside the process the id is derived from the descriptor's address:
    /// unique among live descriptors within one process run, not stable
    /// across runs. Runtimes must not expose that value; snapshots carry
    /// dense, session-scoped ids instead.
    ResourceKindId
);

define_id!(
    /// Links observations that are causally related, such as an invalidation
    /// and the work it caused.
    ///
    /// The caller chooses it and passes the same id to
    /// [`flow_begin`](crate::flow_begin), [`flow_step`](crate::flow_step), and
    /// [`flow_end`](crate::flow_end).
    FlowId
);

define_id!(
    /// Opaque id for one point-event descriptor, as [`EventDesc::id`](crate::EventDesc::id)
    /// returns it.
    ///
    /// Inside the process the id is derived from the descriptor's address:
    /// unique among live descriptors within one process run, not stable
    /// across runs. Runtimes must not expose that value; snapshots carry
    /// dense, session-scoped ids instead.
    EventDescId
);

#[cfg(test)]
mod tests {
    use super::{ContextId, CounterDescId, FlowId, ResourceId, ResourceKindId, ScopeDescId};

    #[test]
    fn ids_round_trip() {
        assert_eq!(ContextId::new(1).get(), 1);
        assert_eq!(ScopeDescId::new(2).get(), 2);
        assert_eq!(CounterDescId::new(3).get(), 3);
        assert_eq!(ResourceId::new(4).get(), 4);
        assert_eq!(ResourceKindId::new(5).get(), 5);
        assert_eq!(FlowId::new(6).get(), 6);
    }
}
