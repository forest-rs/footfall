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
    Backend, FlowPhase, MarkerKind, MarkerPhase, ScopeGuard, ScopeToken, counter_add, flow_begin,
    flow_end, flow_step, frame_begin, frame_end, frame_marker, gauge_set, histogram_record,
    message, message_with, pass_begin, pass_end, resource_create, resource_drop, resource_update,
    scope, scope_begin, scope_begin_with, scope_end, scope_with, transaction_begin,
    transaction_end,
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
/// # Examples
///
/// ```
/// let context = spoor_core::context!(Frame = 7_u32, Pass = 1_u32);
/// assert_eq!(context.len(), 2);
/// ```
#[macro_export]
macro_rules! context {
    () => {
        $crate::Context::root()
    };
    ($($key:ident = $value:expr),+ $(,)?) => {{
        let context = $crate::Context::root();
        $(
            let context = context.with($crate::LabelKey::$key, $value);
        )+
        context
    }};
}

/// Builds an [`Args`] list from `key => value` assignments.
///
/// # Examples
///
/// ```
/// let args = spoor_core::args!("bytes" => 64_u32, "items" => 3_u32);
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
/// The three-argument form attaches static source-location metadata with the
/// provided function name.
///
/// # Examples
///
/// ```
/// static RECORD: spoor_core::ScopeDesc = spoor_core::scope_desc!(1, "record_frame");
/// assert_eq!(RECORD.name(), "record_frame");
/// ```
#[macro_export]
macro_rules! scope_desc {
    ($id:expr, $name:expr $(,)?) => {
        $crate::ScopeDesc::new($crate::ScopeDescId::new($id), $name)
    };
    ($id:expr, $name:expr, $function:expr $(,)?) => {
        $crate::ScopeDesc::with_location(
            $crate::ScopeDescId::new($id),
            $name,
            $crate::SourceLocation::new($function, core::file!(), core::line!()),
        )
    };
}

/// Builds a [`CounterDesc`].
///
/// Use the `count` and `bytes` forms for common counter units.
#[macro_export]
macro_rules! counter_desc {
    (count, $id:expr, $name:expr $(,)?) => {
        $crate::CounterDesc::count($crate::CounterDescId::new($id), $name)
    };
    (bytes, $id:expr, $name:expr $(,)?) => {
        $crate::CounterDesc::bytes($crate::CounterDescId::new($id), $name)
    };
    ($id:expr, $name:expr, $unit:expr $(,)?) => {
        $crate::CounterDesc::new($crate::CounterDescId::new($id), $name, $unit)
    };
}

/// Builds an [`EventDesc`].
#[macro_export]
macro_rules! event_desc {
    ($id:expr, $name:expr $(,)?) => {
        $crate::EventDesc::new($crate::EventDescId::new($id), $name)
    };
}

/// Builds a [`ResourceKindDesc`].
#[macro_export]
macro_rules! resource_kind_desc {
    ($id:expr, $name:expr $(,)?) => {
        $crate::ResourceKindDesc::new($crate::ResourceKindId::new($id), $name)
    };
}

#[cfg(test)]
mod tests {
    use crate::{CounterDesc, LabelKey, ResourceKindDesc, ScopeDesc, Unit};

    static SCOPE: ScopeDesc = crate::scope_desc!(1, "record_frame", "spoor_core::tests");
    static COUNTER: CounterDesc = crate::counter_desc!(bytes, 2, "upload_bytes");
    static RESOURCE: ResourceKindDesc = crate::resource_kind_desc!(3, "atlas");

    #[test]
    fn macros_build_common_call_site_values() {
        let context = crate::context!(Frame = 7_u32, Pass = 1_u32);

        assert_eq!(SCOPE.name(), "record_frame");
        assert!(SCOPE.location().is_some());
        assert_eq!(COUNTER.unit(), Unit::Bytes);
        assert_eq!(RESOURCE.name(), "atlas");
        assert_eq!(
            context
                .get(LabelKey::Frame)
                .and_then(|value| value.as_unsigned()),
            Some(7)
        );
    }
}
