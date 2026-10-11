// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{CounterDescId, EventDescId, ResourceKindId, ScopeDescId, Unit};

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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScopeDesc {
    id: ScopeDescId,
    name: &'static str,
    location: Option<SourceLocation>,
}

impl ScopeDesc {
    /// Creates a new scope descriptor.
    #[must_use]
    pub const fn new(id: ScopeDescId, name: &'static str) -> Self {
        Self {
            id,
            name,
            location: None,
        }
    }

    /// Creates a new scope descriptor with static source location metadata.
    #[must_use]
    pub const fn with_location(
        id: ScopeDescId,
        name: &'static str,
        location: SourceLocation,
    ) -> Self {
        Self {
            id,
            name,
            location: Some(location),
        }
    }

    /// Returns the scope descriptor id.
    #[must_use]
    pub const fn id(self) -> ScopeDescId {
        self.id
    }

    /// Returns the stable descriptor name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.name
    }

    /// Returns the static source location when one is available.
    #[must_use]
    pub const fn location(self) -> Option<SourceLocation> {
        self.location
    }
}

/// Static metadata describing one point-event kind, such as a log channel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EventDesc {
    id: EventDescId,
    name: &'static str,
}

impl EventDesc {
    /// Creates a new point-event descriptor.
    #[must_use]
    pub const fn new(id: EventDescId, name: &'static str) -> Self {
        Self { id, name }
    }

    /// Returns the stable descriptor id.
    #[must_use]
    pub const fn id(self) -> EventDescId {
        self.id
    }

    /// Returns the stable descriptor name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.name
    }
}

/// Static metadata describing one counter or gauge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CounterDesc {
    id: CounterDescId,
    name: &'static str,
    unit: Unit,
}

impl CounterDesc {
    /// Creates a new counter descriptor.
    #[must_use]
    pub const fn new(id: CounterDescId, name: &'static str, unit: Unit) -> Self {
        Self { id, name, unit }
    }

    /// Creates a counter descriptor measured in bytes.
    #[must_use]
    pub const fn bytes(id: CounterDescId, name: &'static str) -> Self {
        Self::new(id, name, Unit::Bytes)
    }

    /// Creates a counter descriptor measured as a raw count.
    #[must_use]
    pub const fn count(id: CounterDescId, name: &'static str) -> Self {
        Self::new(id, name, Unit::Count)
    }

    /// Returns the counter descriptor id.
    #[must_use]
    pub const fn id(self) -> CounterDescId {
        self.id
    }

    /// Returns the stable descriptor name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.name
    }

    /// Returns the unit associated with the descriptor.
    #[must_use]
    pub const fn unit(self) -> Unit {
        self.unit
    }
}

/// Static metadata describing one logical resource kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResourceKindDesc {
    id: ResourceKindId,
    name: &'static str,
}

impl ResourceKindDesc {
    /// Creates a new resource kind descriptor.
    #[must_use]
    pub const fn new(id: ResourceKindId, name: &'static str) -> Self {
        Self { id, name }
    }

    /// Returns the resource kind id.
    #[must_use]
    pub const fn id(self) -> ResourceKindId {
        self.id
    }

    /// Returns the stable resource kind name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.name
    }
}

#[cfg(test)]
mod tests {
    use super::{CounterDesc, ResourceKindDesc, ScopeDesc, SourceLocation};
    use crate::{CounterDescId, ResourceKindId, ScopeDescId, Unit};

    #[test]
    fn counter_helper_sets_bytes_unit() {
        let desc = CounterDesc::bytes(CounterDescId::new(7), "upload_bytes");
        assert_eq!(desc.unit(), Unit::Bytes);
    }

    #[test]
    fn scope_descriptor_exposes_name() {
        let desc = ScopeDesc::new(ScopeDescId::new(2), "record_pass");
        assert_eq!(desc.name(), "record_pass");
    }

    #[test]
    fn scope_descriptor_can_carry_source_location() {
        let location = SourceLocation::new("record_pass", "record.rs", 42);
        let desc = ScopeDesc::with_location(ScopeDescId::new(2), "record_pass", location);
        assert_eq!(desc.location(), Some(location));
    }

    #[test]
    fn resource_kind_descriptor_exposes_id() {
        let desc = ResourceKindDesc::new(ResourceKindId::new(11), "atlas");
        assert_eq!(desc.id(), ResourceKindId::new(11));
    }
}
