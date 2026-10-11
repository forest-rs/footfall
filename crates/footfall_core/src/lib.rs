// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

#![no_std]

//! Core instrumentation primitives for Footfall.
//!
//! This crate owns:
//!
//! - integer-backed ids
//! - static descriptors, identified by address
//! - the [`Backend`] trait and the process-wide emission seam
//! - explicit semantic context composition
//! - instrumentation levels
//!
//! It does not yet own runtime buffering or export logic.
//!
//! # Descriptors
//!
//! A descriptor's identity is its address. Declare descriptors as `static`
//! items, never `const`: every use of a `const` would create a fresh value at
//! a fresh address, which would silently split one descriptor into many.
//! Each descriptor type carries a private interior-mutable marker, which the
//! compiler refuses to promote or share through a `const`, so both ways of
//! reaching a descriptor through a `const` are compile errors:
//!
//! ```compile_fail,E0716
//! use footfall_core::{Context, ScopeDesc, scope};
//!
//! const RECORD: ScopeDesc = ScopeDesc::new("record");
//! let _guard = scope(&RECORD, &Context::root());
//! ```
//!
//! ```compile_fail,E0492
//! use footfall_core::ScopeDesc;
//!
//! pub const RECORD: &ScopeDesc = &ScopeDesc::new("record");
//! ```
//!
//! ```
//! use footfall_core::{Context, ScopeDesc, scope};
//!
//! static RECORD: ScopeDesc = ScopeDesc::new("record");
//! let _guard = scope(&RECORD, &Context::root());
//! ```
//!
//! Descriptors created at runtime, for example by a bridge from another
//! instrumentation framework, can be leaked with `Box::leak` to obtain a
//! `&'static` descriptor with its own identity.

#[cfg(feature = "alloc")]
extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

mod args;
mod context;
mod descriptor;
mod emit;
mod id;
mod label;
mod level;
mod unit;

pub use args::{ARGS_INLINE_CAPACITY, ArgEntry, Args, ArgsCapacityError};
pub use context::{CONTEXT_INLINE_CAPACITY, Context, ContextCapacityError, ContextEntry};
pub use descriptor::{CounterDesc, EventDesc, ResourceKindDesc, ScopeDesc, SourceLocation};
pub use emit::{
    Backend, FlowPhase, MarkerKind, MarkerPhase, NoopBackend, ScopeGuard, ScopeToken, counter_add,
    flow_begin, flow_end, flow_step, frame_begin, frame_end, frame_marker, gauge_set,
    histogram_record, message, message_with, pass_begin, pass_end, resource_create, resource_drop,
    resource_update, scope, scope_begin, scope_begin_with, scope_end, scope_with,
    transaction_begin, transaction_end,
};
#[cfg(feature = "std")]
pub use emit::{InstallBackendError, install_backend};
pub use id::{
    ContextId, CounterDescId, EventDescId, FlowId, ResourceId, ResourceKindId, ScopeDescId,
};
pub use label::{LabelKey, LabelValue};
pub use level::InstrumentationLevel;
pub use unit::Unit;

/// Builds a [`Context`] from label-key assignments.
///
/// Each key is a path to a `static` [`LabelKey`].
///
/// # Examples
///
/// ```
/// use footfall_core::LabelKey;
///
/// static FRAME: LabelKey = LabelKey::new("frame");
/// static PASS: LabelKey = LabelKey::new("pass");
///
/// let context = footfall_core::context!(FRAME = 7_u32, PASS = 1_u32);
/// assert_eq!(context.len(), 2);
/// ```
#[macro_export]
macro_rules! context {
    () => {
        $crate::Context::root()
    };
    ($($key:path = $value:expr),+ $(,)?) => {{
        let context = $crate::Context::root();
        $(
            let context = context.with(&$key, $value);
        )+
        context
    }};
}

/// Builds an [`Args`] list from `key => value` assignments.
///
/// # Examples
///
/// ```
/// let args = footfall_core::args!("bytes" => 64_u32, "items" => 3_u32);
/// assert_eq!(args.len(), 2);
/// ```
#[macro_export]
macro_rules! args {
    () => {
        $crate::Args::none()
    };
    ($($key:literal => $value:expr),+ $(,)?) => {{
        let args = $crate::Args::none();
        $(
            let args = args.with($key, $value);
        )+
        args
    }};
}

/// Builds a [`ScopeDesc`].
///
/// The two-argument form attaches static source-location metadata with the
/// provided function name. Assign the result to a `static` item; a
/// descriptor's identity is its address.
///
/// # Examples
///
/// ```
/// static RECORD: footfall_core::ScopeDesc = footfall_core::scope_desc!("record_frame");
/// assert_eq!(RECORD.name(), "record_frame");
/// ```
#[macro_export]
macro_rules! scope_desc {
    ($name:expr $(,)?) => {
        $crate::ScopeDesc::new($name)
    };
    ($name:expr, $function:expr $(,)?) => {
        $crate::ScopeDesc::with_location(
            $name,
            $crate::SourceLocation::new($function, core::file!(), core::line!()),
        )
    };
}

/// Builds a [`CounterDesc`].
///
/// Use the `count` and `bytes` forms for common counter units. Assign the
/// result to a `static` item; a descriptor's identity is its address.
///
/// # Examples
///
/// ```
/// use footfall_core::{CounterDesc, Unit, counter_desc};
///
/// static DRAW_OPS: CounterDesc = counter_desc!(count, "draw_ops");
/// static UPLOAD_BYTES: CounterDesc = counter_desc!(bytes, "upload_bytes");
/// static FRAME_TIME: CounterDesc = counter_desc!("frame_time", Unit::Micros);
/// assert_eq!(FRAME_TIME.unit(), Unit::Micros);
/// ```
#[macro_export]
macro_rules! counter_desc {
    (count, $name:expr $(,)?) => {
        $crate::CounterDesc::count($name)
    };
    (bytes, $name:expr $(,)?) => {
        $crate::CounterDesc::bytes($name)
    };
    ($name:expr, $unit:expr $(,)?) => {
        $crate::CounterDesc::new($name, $unit)
    };
}

/// Builds an [`EventDesc`].
///
/// Assign the result to a `static` item; a descriptor's identity is its
/// address.
#[macro_export]
macro_rules! event_desc {
    ($name:expr $(,)?) => {
        $crate::EventDesc::new($name)
    };
}

/// Builds a [`ResourceKindDesc`].
///
/// Assign the result to a `static` item; a descriptor's identity is its
/// address.
#[macro_export]
macro_rules! resource_kind_desc {
    ($name:expr $(,)?) => {
        $crate::ResourceKindDesc::new($name)
    };
}

#[cfg(test)]
mod tests {
    use crate::{CounterDesc, LabelKey, ResourceKindDesc, ScopeDesc, Unit};

    static FRAME: LabelKey = LabelKey::new("frame");
    static PASS: LabelKey = LabelKey::new("pass");

    static SCOPE: ScopeDesc = crate::scope_desc!("record_frame", "footfall_core::tests");
    static COUNTER: CounterDesc = crate::counter_desc!(bytes, "upload_bytes");
    static RESOURCE: ResourceKindDesc = crate::resource_kind_desc!("atlas");

    #[test]
    fn macros_build_common_call_site_values() {
        let context = crate::context!(FRAME = 7_u32, PASS = 1_u32);

        assert_eq!(SCOPE.name(), "record_frame");
        assert!(SCOPE.location().is_some());
        assert_eq!(COUNTER.unit(), Unit::Bytes);
        assert_eq!(RESOURCE.name(), "atlas");
        assert_eq!(
            context.get(&FRAME).and_then(|value| value.as_unsigned()),
            Some(7)
        );
    }
}
