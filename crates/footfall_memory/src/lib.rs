// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

#![no_std]

//! Memory-oriented helpers for Footfall, built on counters.
//!
//! - [`CacheStats`] reports a cache's resident bytes, hits, misses, and
//!   evictions through the counters named by a [`CacheMetrics`].
//! - `TrackedScratchVec`, with the `alloc` feature, is a `Vec` that reports
//!   its bytes, peak, and growth through the counters named by a
//!   `ScratchMetrics`.

#[cfg(feature = "alloc")]
extern crate alloc;

mod cache;
#[cfg(feature = "alloc")]
mod scratch;

pub use cache::{CacheMetrics, CacheStats};
#[cfg(feature = "alloc")]
pub use scratch::{ScratchMetrics, TrackedScratchVec};
