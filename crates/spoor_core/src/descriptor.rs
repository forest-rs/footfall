// Copyright 2026 the Spoor Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::{CounterDescId, ResourceKindId, ScopeDescId, Unit};

/// Static metadata describing one scope kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScopeDesc {
    id: ScopeDescId,
    name: &'static str,
}

impl ScopeDesc {
    /// Creates a new scope descriptor.
    #[must_use]
    pub const fn new(id: ScopeDescId, name: &'static str) -> Self {
        Self { id, name }
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
    use super::{CounterDesc, ResourceKindDesc, ScopeDesc};
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
    fn resource_kind_descriptor_exposes_id() {
        let desc = ResourceKindDesc::new(ResourceKindId::new(11), "atlas");
        assert_eq!(desc.id(), ResourceKindId::new(11));
    }
}
