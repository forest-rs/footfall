// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use alloc::vec::Vec;
use core::mem::size_of;
use footfall_core::{ContextHandle, CounterDesc, counter_add, gauge_set};

/// Standard counter descriptors for scratch-buffer observation.
#[derive(Clone, Copy, Debug)]
pub struct ScratchMetrics {
    /// Current reserved bytes.
    current_bytes: &'static CounterDesc,
    /// Peak reserved bytes.
    peak_bytes: &'static CounterDesc,
    /// Capacity growth count.
    grow_count: &'static CounterDesc,
}

impl ScratchMetrics {
    /// Creates a new `ScratchMetrics`.
    #[must_use]
    pub const fn new(
        current_bytes: &'static CounterDesc,
        peak_bytes: &'static CounterDesc,
        grow_count: &'static CounterDesc,
    ) -> Self {
        Self {
            current_bytes,
            peak_bytes,
            grow_count,
        }
    }

    /// Returns the current reserved bytes.
    #[must_use]
    pub const fn current_bytes(&self) -> &'static CounterDesc {
        self.current_bytes
    }

    /// Returns the peak reserved bytes.
    #[must_use]
    pub const fn peak_bytes(&self) -> &'static CounterDesc {
        self.peak_bytes
    }

    /// Returns the capacity growth count.
    #[must_use]
    pub const fn grow_count(&self) -> &'static CounterDesc {
        self.grow_count
    }
}

/// `Vec` wrapper that reports scratch-buffer growth and capacity.
#[derive(Debug)]
pub struct TrackedScratchVec<T> {
    inner: Vec<T>,
    metrics: ScratchMetrics,
    context: ContextHandle,
    peak_bytes: u64,
}

impl<T> TrackedScratchVec<T> {
    /// Creates an empty tracked scratch vector attributed to `context`.
    #[must_use]
    pub fn new(metrics: ScratchMetrics, context: ContextHandle) -> Self {
        let mut scratch = Self {
            inner: Vec::new(),
            metrics,
            context,
            peak_bytes: 0,
        };
        scratch.report_capacity();
        scratch
    }

    /// Pushes one item and records capacity growth when it occurs.
    pub fn push(&mut self, value: T) {
        let previous_capacity = self.inner.capacity();
        self.inner.push(value);
        self.observe_growth(previous_capacity);
    }

    /// Reserves additional capacity.
    pub fn reserve(&mut self, additional: usize) {
        let previous_capacity = self.inner.capacity();
        self.inner.reserve(additional);
        self.observe_growth(previous_capacity);
    }

    /// Clears the vector contents without discarding reserved capacity.
    pub fn clear(&mut self) {
        self.inner.clear();
        self.report_capacity();
    }

    /// Returns the current logical length.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Returns `true` when the scratch vector is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Returns the current element capacity.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.inner.capacity()
    }

    /// Returns the current reserved bytes.
    #[must_use]
    pub fn reserved_bytes(&self) -> u64 {
        bytes_for_capacity::<T>(self.inner.capacity())
    }

    fn observe_growth(&mut self, previous_capacity: usize) {
        if self.inner.capacity() != previous_capacity {
            counter_add(self.metrics.grow_count, self.context, 1);
        }
        self.report_capacity();
    }

    fn report_capacity(&mut self) {
        let current_bytes = self.reserved_bytes();
        gauge_set(self.metrics.current_bytes, self.context, current_bytes);
        if current_bytes > self.peak_bytes {
            self.peak_bytes = current_bytes;
            gauge_set(self.metrics.peak_bytes, self.context, self.peak_bytes);
        }
    }
}

fn bytes_for_capacity<T>(capacity: usize) -> u64 {
    let bytes = capacity.saturating_mul(size_of::<T>());
    u64::try_from(bytes).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::{ScratchMetrics, TrackedScratchVec};
    use footfall_core::{ContextHandle, CounterDesc};

    static CURRENT: CounterDesc = CounterDesc::bytes("scratch_current");
    static PEAK: CounterDesc = CounterDesc::bytes("scratch_peak");
    static GROW: CounterDesc = CounterDesc::count("scratch_grow");

    #[test]
    fn tracked_scratch_vec_reports_capacity_locally() {
        let metrics = ScratchMetrics {
            current_bytes: &CURRENT,
            peak_bytes: &PEAK,
            grow_count: &GROW,
        };
        let mut scratch = TrackedScratchVec::<u32>::new(metrics, ContextHandle::ROOT);
        assert_eq!(scratch.len(), 0);
        scratch.push(1);
        scratch.reserve(16);
        assert!(!scratch.is_empty());
        assert!(scratch.reserved_bytes() > 0);
    }
}
