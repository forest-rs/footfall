// Copyright 2026 the Spoor Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

#![no_std]

//! Core instrumentation primitives for Spoor.
//!
//! This crate owns:
//!
//! - stable integer-backed ids
//! - static descriptors
//! - explicit semantic context composition
//! - instrumentation levels
//!
//! It does not yet own runtime buffering or export logic.

#[cfg(feature = "alloc")]
extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

mod context;
mod descriptor;
mod emit;
mod id;
mod label;
mod level;
mod unit;

pub use context::{CONTEXT_INLINE_CAPACITY, Context, ContextCapacityError, ContextEntry};
pub use descriptor::{CounterDesc, ResourceKindDesc, ScopeDesc};
pub use emit::{
    Backend, MarkerKind, MarkerPhase, ScopeToken, counter_add, frame_marker, gauge_set,
    resource_create, resource_drop, resource_update, scope_begin, scope_end,
};
#[cfg(feature = "std")]
pub use emit::{InstallBackendError, install_backend};
pub use id::{ContextId, CounterDescId, ResourceId, ResourceKindId, ScopeDescId};
pub use label::{LabelKey, LabelValue};
pub use level::InstrumentationLevel;
pub use unit::Unit;
