// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use footfall_core::{Context, CounterDesc, counter_add, gauge_set};

/// Standard counter descriptors for cache observation.
#[derive(Clone, Copy, Debug)]
pub struct CacheMetrics {
    /// Resident byte gauge.
    pub current_bytes: &'static CounterDesc,
    /// Hit counter.
    pub hit_count: &'static CounterDesc,
    /// Miss counter.
    pub miss_count: &'static CounterDesc,
    /// Eviction counter.
    pub eviction_count: &'static CounterDesc,
}

/// Lightweight cache statistics helper.
#[derive(Clone, Copy, Debug)]
pub struct CacheStats {
    metrics: CacheMetrics,
    context: Context,
    current_bytes: u64,
}

impl CacheStats {
    /// Creates a new cache statistics helper.
    #[must_use]
    pub const fn new(metrics: CacheMetrics, context: Context) -> Self {
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
    use footfall_core::{Context, CounterDesc, CounterDescId};

    static CURRENT: CounterDesc = CounterDesc::bytes(CounterDescId::new(200), "cache_current");
    static HITS: CounterDesc = CounterDesc::count(CounterDescId::new(201), "cache_hits");
    static MISSES: CounterDesc = CounterDesc::count(CounterDescId::new(202), "cache_misses");
    static EVICTIONS: CounterDesc = CounterDesc::count(CounterDescId::new(203), "cache_evictions");

    #[test]
    fn cache_stats_updates_local_resident_bytes() {
        let metrics = CacheMetrics {
            current_bytes: &CURRENT,
            hit_count: &HITS,
            miss_count: &MISSES,
            eviction_count: &EVICTIONS,
        };
        let mut cache = CacheStats::new(metrics, Context::root());

        assert_eq!(cache.current_bytes(), 0);
        cache.record_hit();
        cache.record_miss();
        cache.observe_resident_bytes(4_096);
        assert_eq!(cache.current_bytes(), 4_096);
        cache.record_eviction(2_048);
        assert_eq!(cache.current_bytes(), 2_048);
    }
}
