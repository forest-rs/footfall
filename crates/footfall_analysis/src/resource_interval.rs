// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Application-observed storage activity. Sizes do not include allocator overhead.

use crate::{
    QueryEvidence, Resource, ResourceAction, ResourceObservation, Signal, SpanId, TimeRange, Trace,
};
use std::{collections::BTreeMap, time::Duration};

/// Identifies a retained resource lifetime by its first observation sequence.
/// Unlike a logical resource ID, this distinguishes reuse after a drop. It stays
/// stable across pushes and pruning for as long as that lifetime is retained.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ResourceLifetimeId(u64);
impl ResourceLifetimeId {
    /// References a lifetime whose first retained observation had this sequence.
    #[must_use]
    pub const fn new(sequence: u64) -> Self {
        Self(sequence)
    }
    /// Returns the identifying event sequence within this runtime instance.
    #[must_use]
    pub const fn sequence(self) -> u64 {
        self.0
    }
}
impl Resource {
    /// Returns this lifetime's stable identity, distinct from a reusable logical ID.
    #[must_use]
    pub fn lifetime_id(&self) -> ResourceLifetimeId {
        ResourceLifetimeId(
            self.observations()
                .first()
                .expect("resources have observations")
                .sequence(),
        )
    }
}

/// Observed byte sum with an explicit exactness qualification. When incomplete,
/// this is a sum of supported observations, not an estimate of unknown storage.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ObservedBytes {
    /// Sum of supported application-reported bytes, saturating on overflow.
    pub observed: u64,
    /// Whether missing observations, sizes, or overflow can change the total.
    pub exact: bool,
}
impl ObservedBytes {
    fn add(&mut self, value: u64) {
        let (sum, overflow) = self.observed.overflowing_add(value);
        self.observed = if overflow { u64::MAX } else { sum };
        self.exact &= !overflow;
    }
}

/// One interval's global activity or a subgroup by kind and creating work.
/// Resizes are separate from create/drop totals. They are changes in reported
/// storage, without implying physical allocations or allocator traffic.
#[derive(Clone, Debug)]
pub struct ResourceActivity {
    /// Bytes on retained creation events in the half-open interval.
    pub created: ObservedBytes,
    /// Last supported size before each retained drop in the interval.
    pub released: ObservedBytes,
    /// Positive reported size changes on update events in the interval.
    pub grown: ObservedBytes,
    /// Negative reported size changes on update events in the interval.
    pub shrunk: ObservedBytes,
    /// Live state immediately before events at the starting timestamp.
    pub baseline: ObservedBytes,
    /// Live state immediately before events at the ending timestamp.
    pub ending: ObservedBytes,
    /// Maximum supported live bytes, including the baseline and every intermediate
    /// state in `(timestamp, sequence)` order. Missing history makes this incomplete.
    pub peak: ObservedBytes,
    /// Retained lifetimes observed live at the end, including ones created inside
    /// the interval. Missing creations are not extrapolated backward from updates.
    /// With incomplete evidence these are candidates, not proof of uninterrupted retention.
    pub survivors: Vec<ResourceLifetimeId>,
}
impl ResourceActivity {
    fn new(exact: bool) -> Self {
        let zero = ObservedBytes { observed: 0, exact };
        Self {
            created: zero,
            released: zero,
            grown: zero,
            shrunk: zero,
            baseline: zero,
            ending: zero,
            peak: zero,
            survivors: Vec::new(),
        }
    }
}

/// Choice of grouping for interval storage activity. Grouping never implies
/// exclusive attribution to overlapping edits or current resource ownership.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceGrouping {
    /// Resource descriptor name.
    Kind,
    /// Nearest captured creation scope, or the unknown-origin group.
    CreationScope,
    /// Highest retained ancestor of the creating scope, or unknown origin when
    /// ancestry cannot be resolved all the way to a captured root.
    CreationRoot,
}

/// A stable grouping key. Unknown origin includes unscoped and missing creations.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ResourceGroupKey {
    /// Resource descriptor name.
    Kind(String),
    /// Captured creating work.
    Scope(SpanId),
    /// No supported creation scope or a broken ancestor chain.
    UnknownOrigin,
}

/// One group of resource lifetimes and its interval accounting.
#[derive(Clone, Debug)]
pub struct ResourceGroup {
    /// Kind or creating work identity.
    pub key: ResourceGroupKey,
    /// Global interval activity for this subset of lifetimes.
    pub activity: ResourceActivity,
}

/// Resource accounting independent of parsing, file status, or serialization.
#[derive(Clone, Debug)]
pub struct ResourceInterval {
    /// Half-open activity interval `[start, end)`. Equal timestamps are ordered
    /// by sequence internally; all events at a boundary belong to the interval
    /// starting there. Boundary liveness is the state before those events.
    pub range: TimeRange,
    /// Global application-reported activity across all recording threads.
    pub activity: ResourceActivity,
    /// Deterministic group order, if requested. Group peaks are not additive.
    pub groups: Vec<ResourceGroup>,
    /// Signal availability and retained history, separate from capture-file reads.
    pub evidence: QueryEvidence,
}

impl Trace {
    /// Resolves a resource lifetime identity, including logical ID reuse.
    #[must_use]
    pub fn resource_lifetime(&self, id: ResourceLifetimeId) -> Option<&Resource> {
        self.resources()
            .iter()
            .find(|resource| resource.lifetime_id() == id)
    }

    /// Accounts for observed create/drop/resize activity and boundary states.
    /// A prior gap can invalidate the baseline even when the selected interval
    /// contains no gap. Exactness is deliberately conservative after any loss or
    /// forgetting. No observations are invented before a lifetime's first event.
    #[must_use]
    pub fn resource_interval(
        &self,
        range: TimeRange,
        grouping: Option<ResourceGrouping>,
    ) -> ResourceInterval {
        let evidence = self.query_evidence(Signal::Resources, Some(range));
        let resources: Vec<_> = self.resources().iter().collect();
        let exact = evidence.is_complete()
            && resources
                .iter()
                .all(|resource| resource.creation().is_some());
        let activity = self.account_resources(&resources, range, exact);
        let mut groups = BTreeMap::<ResourceGroupKey, Vec<&Resource>>::new();
        if let Some(grouping) = grouping {
            for resource in &resources {
                let origin = resource.creation().and_then(|event| event.span());
                let key = match grouping {
                    ResourceGrouping::Kind => {
                        ResourceGroupKey::Kind(resource.kind_name().to_owned())
                    }
                    ResourceGrouping::CreationScope => {
                        origin.map_or(ResourceGroupKey::UnknownOrigin, ResourceGroupKey::Scope)
                    }
                    ResourceGrouping::CreationRoot => {
                        let mut root = origin;
                        while let Some(id) = root {
                            match self.scope_parent(id) {
                                Some(Some(parent)) => root = Some(parent),
                                Some(None) => break,
                                None => {
                                    root = None;
                                    break;
                                }
                            }
                        }
                        root.map_or(ResourceGroupKey::UnknownOrigin, ResourceGroupKey::Scope)
                    }
                };
                groups.entry(key).or_default().push(resource);
            }
        }
        let groups = groups
            .into_iter()
            .map(|(key, resources)| ResourceGroup {
                key,
                activity: self.account_resources(&resources, range, exact),
            })
            .collect();
        ResourceInterval {
            range,
            activity,
            groups,
            evidence,
        }
    }

    fn supported_size(&self, previous: &ResourceObservation, at: Duration) -> bool {
        self.recording_history(Signal::Resources).is_yes()
            && previous.size().is_some()
            && (self.has_complete_history() || previous.at() >= self.observed().start())
            && !self
                .gaps()
                .iter()
                .any(|gap| gap.range().start() <= at && gap.range().end() >= previous.at())
    }

    fn account_resources(
        &self,
        resources: &[&Resource],
        range: TimeRange,
        exact: bool,
    ) -> ResourceActivity {
        let mut result = ResourceActivity::new(exact);
        let mut live = BTreeMap::<ResourceLifetimeId, (u64, &ResourceObservation)>::new();
        let mut events = Vec::new();
        for resource in resources {
            let id = resource.lifetime_id();
            let mut previous = None;
            for event in resource.observations() {
                if event.at() >= range.end() {
                    break;
                }
                if event.at() < range.start() {
                    match event.action() {
                        ResourceAction::Create | ResourceAction::Update => {
                            live.insert(id, (event.size().expect("sized action"), event));
                        }
                        ResourceAction::Drop => {
                            live.remove(&id);
                        }
                    }
                } else {
                    events.push((id, event, previous));
                }
                previous = Some(event);
            }
        }
        // Baseline observations separated from the boundary by loss no longer
        // support a live-size claim. Do not keep a stale size as a lower bound.
        for resource in resources {
            if let Some(previous) = resource
                .observations()
                .iter()
                .rev()
                .find(|event| event.at() < range.start())
                && !self.supported_size(previous, range.start())
            {
                live.remove(&resource.lifetime_id());
            }
        }
        for (size, _) in live.values() {
            result.baseline.add(*size);
        }
        result.peak = result.baseline;
        events.sort_by_key(|(_, event, _)| (event.at(), event.sequence()));
        for (id, event, previous) in events {
            live.retain(|_, (_, previous)| self.supported_size(previous, event.at()));
            let old = previous
                .filter(|previous| self.supported_size(previous, event.at()))
                .and_then(|previous| previous.size());
            match event.action() {
                ResourceAction::Create => {
                    let size = event.size().expect("creation size");
                    result.created.add(size);
                    live.insert(id, (size, event));
                }
                ResourceAction::Update => {
                    let size = event.size().expect("update size");
                    if let Some(old) = old {
                        result.grown.add(size.saturating_sub(old));
                        result.shrunk.add(old.saturating_sub(size));
                    } else {
                        result.grown.exact = false;
                        result.shrunk.exact = false;
                    }
                    live.insert(id, (size, event));
                }
                ResourceAction::Drop => {
                    if let Some(old) = old {
                        result.released.add(old);
                    } else {
                        result.released.exact = false;
                    }
                    live.remove(&id);
                }
            }
            let mut current = ObservedBytes {
                observed: 0,
                exact: result.peak.exact,
            };
            for (size, _) in live.values() {
                current.add(*size);
            }
            result.peak.observed = result.peak.observed.max(current.observed);
            result.peak.exact &= current.exact;
        }
        // Exclude claims whose last supporting observation predates a gap.
        for resource in resources {
            if let Some(previous) = resource
                .observations()
                .iter()
                .rev()
                .find(|event| event.at() < range.end())
                && !self.supported_size(previous, range.end())
            {
                live.remove(&resource.lifetime_id());
            }
        }
        for (size, _) in live.values() {
            result.ending.add(*size);
        }
        result.survivors = live.keys().copied().collect();
        result
    }
}
