// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Registered context handles. See the crate docs for the context model.

#[cfg(all(feature = "std", not(footfall_compile_out)))]
use crate::emit::backend;
use crate::{Context, ContextId};
#[cfg(all(feature = "std", not(footfall_compile_out)))]
use core::sync::atomic::{AtomicU64, Ordering};
#[cfg(feature = "std")]
use std::sync::OnceLock;

/// Cheap, copyable reference to a registered [`Context`].
///
/// Every emission function takes a handle instead of a context, so
/// attributing an observation costs one integer however many labels the
/// context carries. Obtain handles from [`register_context`],
/// [`RegisteredContext`], or `LazyContext` (with `std`); [`ContextHandle::ROOT`] stands
/// for the empty root context and needs no registration.
///
/// Handle values are allocated by `footfall_core` from one process-wide
/// counter, so they mean the same thing to every installed backend, and are
/// never reused within a process. They are plain integers with no relation
/// to memory layout, so they are safe to show to consumers.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct ContextHandle(u64);

impl ContextHandle {
    /// Handle of the empty root context.
    pub const ROOT: Self = Self(0);

    /// Creates a handle from its raw value.
    ///
    /// Only backends and tests need this; instrumented code gets handles from
    /// [`register_context`].
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// Returns the raw value of this handle.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Returns `true` for [`ContextHandle::ROOT`].
    #[must_use]
    pub const fn is_root(self) -> bool {
        self.0 == Self::ROOT.0
    }

    /// Returns the id a snapshot uses for the context behind this handle.
    #[must_use]
    pub const fn id(self) -> ContextId {
        ContextId::new(self.0)
    }
}

/// Registers a context and returns the handle to emit it with.
///
/// The installed backend is told about the context once, here, and from then
/// on only sees the handle. Registration is not filtered by the instrumentation
/// level, because the level can rise while the handle is still in use.
///
/// Register once per semantic owner and reuse the handle: two registrations of
/// equal contexts get distinct handles, and backends do not merge them. The
/// empty root context always returns [`ContextHandle::ROOT`] without
/// registering anything.
///
/// Register after installing the backend. A backend installed later never
/// learns about contexts registered before it, and attributes their
/// observations to the root context. Use `LazyContext` (with `std`) for contexts that
/// live in `static` items, which may be first touched before installation.
///
/// Release the handle with [`release_context`] when its owner goes away, or
/// let a [`RegisteredContext`] do it.
#[must_use]
pub fn register_context(context: &Context) -> ContextHandle {
    if context.is_empty() {
        return ContextHandle::ROOT;
    }
    #[cfg(all(feature = "std", not(footfall_compile_out)))]
    {
        let handle = ContextHandle(NEXT_HANDLE.fetch_add(1, Ordering::Relaxed));
        backend().context_registered(handle, context);
        handle
    }
    #[cfg(not(all(feature = "std", not(footfall_compile_out))))]
    {
        ContextHandle::ROOT
    }
}

/// Releases a handle returned by [`register_context`].
///
/// Do not emit with a handle after releasing it. Backends keep resolving it
/// for observations already recorded; `footfall_runtime` attributes events
/// recorded with it afterwards to the root context and counts them, and
/// counts counter rows updated with it afterwards, since aggregated rows
/// cannot be split. Releasing [`ContextHandle::ROOT`] does nothing.
pub fn release_context(handle: ContextHandle) {
    #[cfg(all(feature = "std", not(footfall_compile_out)))]
    if !handle.is_root() {
        backend().context_released(handle);
    }
    #[cfg(not(all(feature = "std", not(footfall_compile_out))))]
    let _ = handle;
}

#[cfg(all(feature = "std", not(footfall_compile_out)))]
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

/// A registered context that is released when dropped.
///
/// This is the usual way to give a semantic owner, such as a pass, a layer,
/// or a cache, its context: create it alongside the owner and emit with
/// [`handle`](Self::handle).
///
/// ```
/// use footfall_core::{CounterDesc, LabelKey, RegisteredContext, context, counter_add};
///
/// static CACHE: LabelKey = LabelKey::new("cache");
/// static EVICTIONS: CounterDesc = CounterDesc::count("evictions");
///
/// let glyphs = RegisteredContext::new(&context!(CACHE = "glyphs"));
/// counter_add(&EVICTIONS, glyphs.handle(), 1);
/// ```
#[derive(Debug)]
#[must_use = "dropping a RegisteredContext immediately releases its handle"]
pub struct RegisteredContext {
    handle: ContextHandle,
    context: Context,
}

impl RegisteredContext {
    /// Registers `context` with the installed backend.
    pub fn new(context: &Context) -> Self {
        Self {
            handle: register_context(context),
            context: *context,
        }
    }

    /// Returns the registered context, for building a child context that
    /// extends its labels.
    #[must_use]
    pub const fn context(&self) -> &Context {
        &self.context
    }

    /// Returns the handle to emit with.
    #[must_use]
    pub const fn handle(&self) -> ContextHandle {
        self.handle
    }

    /// Keeps the context registered for the rest of the process and returns
    /// its handle.
    #[must_use]
    pub fn into_handle(self) -> ContextHandle {
        let handle = self.handle;
        core::mem::forget(self);
        handle
    }
}

impl Drop for RegisteredContext {
    fn drop(&mut self) {
        release_context(self.handle);
    }
}

/// A context in a `static` item, registered the first time it is used after
/// a backend is installed.
///
/// Until a backend is installed, [`handle`](Self::handle) returns
/// [`ContextHandle::ROOT`] without registering, so touching the static early
/// never loses its attribution for good. The context stays registered for
/// the rest of the process.
///
/// ```
/// use footfall_core::{CounterDesc, LabelKey, LazyContext, context, counter_add};
///
/// static SUBSYSTEM: LabelKey = LabelKey::new("subsystem");
/// static UPLOADS: CounterDesc = CounterDesc::bytes("upload_bytes");
/// static RENDERER: LazyContext = LazyContext::new(|| context!(SUBSYSTEM = "renderer"));
///
/// counter_add(&UPLOADS, RENDERER.handle(), 4096);
/// ```
#[cfg(feature = "std")]
#[derive(Debug)]
pub struct LazyContext {
    build: fn() -> Context,
    handle: OnceLock<ContextHandle>,
}

#[cfg(feature = "std")]
impl LazyContext {
    /// Creates a lazily registered context built by `build`.
    #[must_use]
    pub const fn new(build: fn() -> Context) -> Self {
        Self {
            build,
            handle: OnceLock::new(),
        }
    }

    /// Returns the handle, registering the context on the first call made
    /// after a backend is installed.
    #[must_use]
    pub fn handle(&self) -> ContextHandle {
        if let Some(handle) = self.handle.get() {
            return *handle;
        }
        if !crate::emit::backend_installed() {
            return ContextHandle::ROOT;
        }
        *self
            .handle
            .get_or_init(|| register_context(&(self.build)()))
    }
}

#[cfg(test)]
mod tests {
    use super::{ContextHandle, register_context, release_context};
    use crate::{Context, LabelKey};

    static PASS: LabelKey = LabelKey::new("pass");

    #[test]
    fn root_context_registers_as_root() {
        assert_eq!(register_context(&Context::root()), ContextHandle::ROOT);
        release_context(ContextHandle::ROOT);
        assert!(ContextHandle::default().is_root());
        assert_eq!(ContextHandle::new(9).id().get(), 9);
    }

    #[cfg(all(feature = "std", not(footfall_compile_out)))]
    #[test]
    fn registrations_get_distinct_handles() {
        let context = Context::root().with(&PASS, 1_u32);
        let first = super::RegisteredContext::new(&context);
        let second = super::RegisteredContext::new(&context);
        assert!(!first.handle().is_root());
        assert_ne!(
            first.handle(),
            second.handle(),
            "equal contexts are not merged"
        );
    }

    #[cfg(not(all(feature = "std", not(footfall_compile_out))))]
    #[test]
    fn registration_without_a_backend_seam_is_root() {
        let context = Context::root().with(&PASS, 1_u32);
        assert_eq!(register_context(&context), ContextHandle::ROOT);
    }
}
