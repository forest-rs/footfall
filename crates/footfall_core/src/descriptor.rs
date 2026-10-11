// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Static descriptors. See the crate docs for the identity rules.

use crate::{CounterDescId, EventDescId, ResourceKindId, ScopeDescId, Unit};
use core::{fmt, ptr};

/// Private marker that keeps descriptors and label keys out of `const` promotion.
///
/// A value whose type has a `Drop` impl cannot be promoted to a `'static`
/// temporary, so `&CONST_DESCRIPTOR` fails to compile where `&'static` is
/// required. The marker is zero-sized, and `static` items are never dropped,
/// so the impl never runs for a declared descriptor.
pub(crate) struct StaticOnly;

impl Drop for StaticOnly {
    fn drop(&mut self) {}
}

/// Returns the opaque identity of a `'static` descriptor.
fn address_id<T>(desc: &'static T) -> u64 {
    // `usize` is at most 64 bits on every supported target.
    ptr::from_ref(desc).addr() as u64
}

/// Static source location associated with a descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceLocation {
    function: &'static str,
    file: &'static str,
    line: u32,
}

impl SourceLocation {
    /// Creates a new static source location.
    #[must_use]
    pub const fn new(function: &'static str, file: &'static str, line: u32) -> Self {
        Self {
            function,
            file,
            line,
        }
    }

    /// Returns the stable function name.
    #[must_use]
    pub const fn function(self) -> &'static str {
        self.function
    }

    /// Returns the stable file path.
    #[must_use]
    pub const fn file(self) -> &'static str {
        self.file
    }

    /// Returns the 1-based source line.
    #[must_use]
    pub const fn line(self) -> u32 {
        self.line
    }
}

/// Static metadata describing one scope kind.
///
/// Identity is the descriptor's address; see the [crate docs](crate#descriptors) for why
/// descriptors must be `static` items.
pub struct ScopeDesc {
    name: &'static str,
    location: Option<SourceLocation>,
    _static_only: StaticOnly,
}

impl ScopeDesc {
    /// Creates a new scope descriptor.
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            location: None,
            _static_only: StaticOnly,
        }
    }

    /// Creates a new scope descriptor with static source location metadata.
    #[must_use]
    pub const fn with_location(name: &'static str, location: SourceLocation) -> Self {
        Self {
            name,
            location: Some(location),
            _static_only: StaticOnly,
        }
    }

    /// Returns the opaque id derived from this descriptor's address.
    ///
    /// Ids are unique among live descriptors within one process run and are
    /// not stable across runs.
    #[must_use]
    pub fn id(&'static self) -> ScopeDescId {
        ScopeDescId::new(address_id(self))
    }

    /// Returns the stable descriptor name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Returns the static source location when one is available.
    #[must_use]
    pub const fn location(&self) -> Option<SourceLocation> {
        self.location
    }
}

/// Static metadata describing one point-event kind, such as a log channel.
///
/// Identity is the descriptor's address; see the [crate docs](crate#descriptors) for why
/// descriptors must be `static` items.
pub struct EventDesc {
    name: &'static str,
    _static_only: StaticOnly,
}

impl EventDesc {
    /// Creates a new point-event descriptor.
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _static_only: StaticOnly,
        }
    }

    /// Returns the opaque id derived from this descriptor's address.
    ///
    /// Ids are unique among live descriptors within one process run and are
    /// not stable across runs.
    #[must_use]
    pub fn id(&'static self) -> EventDescId {
        EventDescId::new(address_id(self))
    }

    /// Returns the stable descriptor name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }
}

/// Static metadata describing one counter, gauge, or histogram.
///
/// Identity is the descriptor's address; see the [crate docs](crate#descriptors) for why
/// descriptors must be `static` items.
pub struct CounterDesc {
    name: &'static str,
    unit: Unit,
    _static_only: StaticOnly,
}

impl CounterDesc {
    /// Creates a new counter descriptor.
    #[must_use]
    pub const fn new(name: &'static str, unit: Unit) -> Self {
        Self {
            name,
            unit,
            _static_only: StaticOnly,
        }
    }

    /// Creates a counter descriptor measured in bytes.
    #[must_use]
    pub const fn bytes(name: &'static str) -> Self {
        Self::new(name, Unit::Bytes)
    }

    /// Creates a counter descriptor measured as a raw count.
    #[must_use]
    pub const fn count(name: &'static str) -> Self {
        Self::new(name, Unit::Count)
    }

    /// Returns the opaque id derived from this descriptor's address.
    ///
    /// Ids are unique among live descriptors within one process run and are
    /// not stable across runs.
    #[must_use]
    pub fn id(&'static self) -> CounterDescId {
        CounterDescId::new(address_id(self))
    }

    /// Returns the stable descriptor name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Returns the unit associated with the descriptor.
    #[must_use]
    pub const fn unit(&self) -> Unit {
        self.unit
    }
}

/// Static metadata describing one logical resource kind.
///
/// Identity is the descriptor's address; see the [crate docs](crate#descriptors) for why
/// descriptors must be `static` items.
pub struct ResourceKindDesc {
    name: &'static str,
    _static_only: StaticOnly,
}

impl ResourceKindDesc {
    /// Creates a new resource kind descriptor.
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _static_only: StaticOnly,
        }
    }

    /// Returns the opaque id derived from this descriptor's address.
    ///
    /// Ids are unique among live descriptors within one process run and are
    /// not stable across runs.
    #[must_use]
    pub fn id(&'static self) -> ResourceKindId {
        ResourceKindId::new(address_id(self))
    }

    /// Returns the stable resource kind name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }
}

/// Implements identity equality and a compact `Debug` for a descriptor type.
macro_rules! descriptor_identity {
    ($name:ident { $($field:ident),* }) => {
        impl PartialEq for $name {
            /// Descriptors are equal only when they are the same descriptor.
            fn eq(&self, other: &Self) -> bool {
                ptr::eq(self, other)
            }
        }

        impl Eq for $name {}

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_struct(stringify!($name))
                    $(.field(stringify!($field), &self.$field))*
                    .finish_non_exhaustive()
            }
        }
    };
}

descriptor_identity!(ScopeDesc { name, location });
descriptor_identity!(EventDesc { name });
descriptor_identity!(CounterDesc { name, unit });
descriptor_identity!(ResourceKindDesc { name });

#[cfg(test)]
mod tests {
    use super::{CounterDesc, EventDesc, ResourceKindDesc, ScopeDesc, SourceLocation};
    use crate::Unit;

    static UPLOAD_BYTES: CounterDesc = CounterDesc::bytes("upload_bytes");
    static RECORD_PASS: ScopeDesc = ScopeDesc::new("record_pass");
    static RECORD_PASS_TWIN: ScopeDesc = ScopeDesc::new("record_pass");
    static LOCATED: ScopeDesc = ScopeDesc::with_location(
        "record_pass",
        SourceLocation::new("record_pass", "record.rs", 42),
    );
    static ATLAS: ResourceKindDesc = ResourceKindDesc::new("atlas");
    static APP_LOG: EventDesc = EventDesc::new("app_log");

    #[test]
    fn counter_helper_sets_bytes_unit() {
        assert_eq!(UPLOAD_BYTES.unit(), Unit::Bytes);
    }

    #[test]
    fn scope_descriptor_exposes_name() {
        assert_eq!(RECORD_PASS.name(), "record_pass");
    }

    #[test]
    fn scope_descriptor_can_carry_source_location() {
        assert_eq!(
            LOCATED.location(),
            Some(SourceLocation::new("record_pass", "record.rs", 42))
        );
    }

    #[test]
    fn identity_follows_the_static_not_its_contents() {
        assert_eq!(RECORD_PASS.id(), RECORD_PASS.id());
        assert_ne!(
            RECORD_PASS.id(),
            RECORD_PASS_TWIN.id(),
            "equal contents in distinct statics are distinct descriptors"
        );
        assert_eq!(RECORD_PASS, RECORD_PASS);
        assert_ne!(RECORD_PASS, RECORD_PASS_TWIN);
        assert_ne!(ATLAS.id().get(), APP_LOG.id().get());
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn leaked_descriptors_get_their_own_identity() {
        use alloc::boxed::Box;

        let first: &'static EventDesc = Box::leak(Box::new(EventDesc::new("bridge")));
        let second: &'static EventDesc = Box::leak(Box::new(EventDesc::new("bridge")));
        assert_ne!(first.id(), second.id());
        assert_ne!(first.id().get(), 0, "addresses are never null");
    }
}
