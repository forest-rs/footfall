// Copyright 2026 the Spoor Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

#![no_std]

//! Memory-oriented helpers for Spoor.

#[cfg(feature = "alloc")]
extern crate alloc;

mod cache;
#[cfg(feature = "alloc")]
mod scratch;

pub use cache::{CacheMetrics, CacheStats};
#[cfg(feature = "alloc")]
pub use scratch::{ScratchMetrics, TrackedScratchVec};
