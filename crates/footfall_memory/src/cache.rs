// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use footfall_core::{ContextHandle, CounterDesc, counter_add, gauge_set};

/// The counters a [`CacheStats`] reports through: resident bytes, hits,
/// misses, and evictions. Pass it to [`CacheStats::new`].
#[derive(Clone, Copy, Debug)]
pub struct CacheMetrics {
    /// Resident byte gauge.
    current_bytes: &'static CounterDesc,
    /// Hit counter.
    hit_count: &'static CounterDesc,
    /// Miss counter.
    miss_count: &'static CounterDesc,
    /// Eviction counter.
    eviction_count: &'static CounterDesc,
}

impl CacheMetrics {
    /// Creates a new `CacheMetrics`.
    #[must_use]
    pub const fn new(
        current_bytes: &'static CounterDesc,
        hit_count: &'static CounterDesc,
        miss_count: &'static CounterDesc,
        eviction_count: &'static CounterDesc,
    ) -> Self {
        Self {
            current_bytes,
            hit_count,
            miss_count,
            eviction_count,
        }
    }

    /// Returns the resident byte gauge.
    #[must_use]
    pub const fn current_bytes(&self) -> &'static CounterDesc {
        self.current_bytes
    }

    /// Returns the hit counter.
    #[must_use]
    pub const fn hit_count(&self) -> &'static CounterDesc {
        self.hit_count
    }

    /// Returns the miss counter.
    #[must_use]
    pub const fn miss_count(&self) -> &'static CounterDesc {
        self.miss_count
    }

    /// Returns the eviction counter.
    #[must_use]
    pub const fn eviction_count(&self) -> &'static CounterDesc {
        self.eviction_count
    }
}

/// Reports one cache's activity through the counters of a [`CacheMetrics`],
/// under one context. Create it with [`CacheStats::new`] next to the cache.
#[derive(Clone, Copy, Debug)]
pub struct CacheStats {
    metrics: CacheMetrics,
    context: ContextHandle,
    current_bytes: u64,
}

impl CacheStats {
    /// Creates a new cache statistics helper attributed to `context`.
    #[must_use]
    pub const fn new(metrics: CacheMetrics, context: ContextHandle) -> Self {
        Self {
            metrics,
            context,
            current_bytes: 0,
        }
    }

    /// Returns the observed resident bytes.
    #[must_use]
    pub const fn current_bytes(self) -> u64 {
        self.current_bytes
    }

    /// Records a cache hit.
    pub fn record_hit(&self) {
        counter_add(self.metrics.hit_count, self.context, 1);
    }

    /// Records a cache miss.
    pub fn record_miss(&self) {
        counter_add(self.metrics.miss_count, self.context, 1);
    }

    /// Records a cache eviction and updates resident bytes.
    pub fn record_eviction(&mut self, resident_bytes: u64) {
        counter_add(self.metrics.eviction_count, self.context, 1);
        self.observe_resident_bytes(resident_bytes);
    }

    /// Updates the observed resident bytes.
    pub fn observe_resident_bytes(&mut self, resident_bytes: u64) {
        self.current_bytes = resident_bytes;
        gauge_set(self.metrics.current_bytes, self.context, resident_bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::{CacheMetrics, CacheStats};
    use footfall_core::{ContextHandle, CounterDesc};

    static CURRENT: CounterDesc = CounterDesc::bytes("cache_current");
    static HITS: CounterDesc = CounterDesc::count("cache_hits");
    static MISSES: CounterDesc = CounterDesc::count("cache_misses");
    static EVICTIONS: CounterDesc = CounterDesc::count("cache_evictions");

    #[test]
    fn cache_stats_updates_local_resident_bytes() {
        let metrics = CacheMetrics {
            current_bytes: &CURRENT,
            hit_count: &HITS,
            miss_count: &MISSES,
            eviction_count: &EVICTIONS,
        };
        let mut cache = CacheStats::new(metrics, ContextHandle::ROOT);

        assert_eq!(cache.current_bytes(), 0);
        cache.record_hit();
        cache.record_miss();
        cache.observe_resident_bytes(4_096);
        assert_eq!(cache.current_bytes(), 4_096);
        cache.record_eviction(2_048);
        assert_eq!(cache.current_bytes(), 2_048);
    }
}
