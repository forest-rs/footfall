// Copyright 2026 the Spoor Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Live Tracy backend for Spoor.
//!
//! `spoor_tracy` installs a process-wide [`spoor_core::Backend`] that emits directly into
//! `tracy-client`.
//!
//! This first slice intentionally maps only the parts of Spoor that Tracy represents naturally:
//!
//! - scopes become Tracy zones
//! - counters and gauges become Tracy plots
//! - frame, pass, and transaction markers become Tracy frames
//! - resources become Tracy messages
//!
//! Flow events are currently ignored by this backend because Tracy does not expose a strong native
//! equivalent to Perfetto-style causal links.
//!
//! This backend is exclusive with other Spoor backends today. Installing it replaces the bounded
//! in-process runtime path rather than multiplexing to it.

use spoor_core::{
    Backend, Context, CounterDesc, CounterDescId, FlowId, FlowPhase, InstallBackendError,
    LabelValue, MarkerKind, MarkerPhase, ResourceId, ResourceKindDesc, ScopeDesc, ScopeDescId,
    ScopeToken, SourceLocation, Unit, install_backend,
};
use std::{
    cell::RefCell,
    collections::HashMap,
    fmt,
    fmt::Write as _,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};
use tracy_client::{
    Client, Frame, FrameName, PlotConfiguration, PlotFormat, PlotName, Span, SpanLocation,
};

/// Installs a Tracy backend as the process-wide Spoor backend.
pub fn install_global(tracy: &'static Tracy) -> Result<(), InstallError> {
    tracy.register_current_thread();
    TRACY
        .set(tracy)
        .map_err(|_| InstallError::TracyAlreadyInstalled)?;
    install_backend(BACKEND).map_err(InstallError::BackendAlreadyInstalled)
}

/// Error returned when Tracy backend installation fails.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstallError {
    /// A Tracy backend has already been installed.
    TracyAlreadyInstalled,
    /// Another Spoor backend has already been installed.
    BackendAlreadyInstalled(InstallBackendError),
}

/// Live Tracy backend state.
pub struct Tracy {
    client: Client,
    callstack_depth: u16,
    next_scope_token: AtomicU64,
    counters: Mutex<CounterState>,
    locations: Mutex<HashMap<ScopeDescId, &'static SpanLocation>>,
}

impl Tracy {
    /// Creates a Tracy backend with no callstack capture on zones or messages.
    #[must_use]
    pub fn new() -> Self {
        Self::with_callstack_depth(0)
    }

    /// Creates a Tracy backend with the provided callstack depth.
    #[must_use]
    pub fn with_callstack_depth(callstack_depth: u16) -> Self {
        Self {
            client: Client::start(),
            callstack_depth,
            next_scope_token: AtomicU64::new(1),
            counters: Mutex::new(CounterState::default()),
            locations: Mutex::new(HashMap::new()),
        }
    }

    /// Returns the callstack depth configured for this backend.
    #[must_use]
    pub const fn callstack_depth(&self) -> u16 {
        self.callstack_depth
    }

    /// Sets the current thread name in Tracy.
    pub fn set_current_thread_name(&self, name: &str) {
        self.client.set_thread_name(name);
    }

    /// Copies the current Rust thread name into Tracy when one is available.
    pub fn sync_current_thread_name(&self) {
        if let Some(name) = std::thread::current().name() {
            self.client.set_thread_name(name);
        }
    }

    /// Registers the current thread with Tracy once, using the Rust thread name when available.
    pub fn register_current_thread(&self) {
        THREAD_REGISTERED.with(|registered| {
            if registered.get() {
                return;
            }

            let thread = std::thread::current();
            let name = thread
                .name()
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| format!("thread::{:?}", thread.id()));
            self.client.set_thread_name(&name);
            registered.set(true);
        });
    }

    /// Returns `true` when a Tracy profiler is currently connected.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        Client::is_connected()
    }

    /// Returns `true` when the Tracy client runtime is active.
    #[must_use]
    pub fn is_running(&self) -> bool {
        Client::is_running()
    }

    fn next_scope_token(&self) -> ScopeToken {
        let raw = self.next_scope_token.fetch_add(1, Ordering::Relaxed);
        ScopeToken::new(raw)
    }

    fn begin_scope(&self, desc: &ScopeDesc, context: Context) -> ScopeToken {
        let token = self.next_scope_token();
        let span = if let Some(location) = desc.location() {
            let location = self.cached_span_location(desc, location);
            self.client.clone().span(location, self.callstack_depth)
        } else {
            let line = u32::try_from(desc.id().get()).unwrap_or(u32::MAX);
            self.client.clone().span_alloc(
                Some(desc.name()),
                "spoor_scope",
                "spoor",
                line,
                self.callstack_depth,
            )
        };

        let context_text = context_text(context);
        if !context_text.is_empty() {
            span.emit_text(&context_text);
        }

        OPEN_SPANS.with(|spans| {
            spans.borrow_mut().insert(token.get(), span);
        });

        token
    }

    fn cached_span_location(
        &self,
        desc: &ScopeDesc,
        location: SourceLocation,
    ) -> &'static SpanLocation {
        let mut locations = self
            .locations
            .lock()
            .expect("tracy location mutex should not be poisoned");
        locations
            .entry(desc.id())
            .or_insert_with(|| leak_span_location(desc.name(), location))
    }

    fn end_scope(&self, token: ScopeToken) {
        OPEN_SPANS.with(|spans| {
            spans.borrow_mut().remove(&token.get());
        });
    }

    fn observe_counter(&self, desc: &CounterDesc, value: u64, update: CounterUpdate) {
        let mut counters = self
            .counters
            .lock()
            .expect("tracy counter mutex should not be poisoned");
        let plot_name = counters.plot_name(desc);
        if counters.configured.insert(desc.id()) {
            self.client
                .plot_config(plot_name, plot_configuration(desc.unit()));
        }

        let current = counters.values.entry(desc.id()).or_insert(0);
        *current = match update {
            CounterUpdate::Add => current.saturating_add(value),
            CounterUpdate::Set => value,
        };
        let plotted = *current as f64;
        drop(counters);

        self.client.plot(plot_name, plotted);
    }

    fn handle_marker(&self, kind: MarkerKind, phase: MarkerPhase) {
        match phase {
            MarkerPhase::Begin => {
                let frame = self.client.non_continuous_frame(frame_name(kind));
                OPEN_FRAMES.with(|frames| {
                    frames.borrow_mut().insert(kind, frame);
                });
            }
            MarkerPhase::End => {
                OPEN_FRAMES.with(|frames| {
                    frames.borrow_mut().remove(&kind);
                });
            }
        }
    }

    fn emit_resource_message(
        &self,
        verb: &str,
        desc: &ResourceKindDesc,
        resource_id: ResourceId,
        context: Context,
        size: Option<u64>,
    ) {
        let message = resource_message(verb, desc, resource_id, context, size);
        self.client.message(&message, self.callstack_depth);
    }
}

impl Default for Tracy {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Tracy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Tracy")
            .field("callstack_depth", &self.callstack_depth)
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
struct CounterState {
    plots: HashMap<CounterDescId, PlotName>,
    configured: std::collections::HashSet<CounterDescId>,
    values: HashMap<CounterDescId, u64>,
}

impl CounterState {
    fn plot_name(&mut self, desc: &CounterDesc) -> PlotName {
        *self
            .plots
            .entry(desc.id())
            .or_insert_with(|| PlotName::new_leak(desc.name().to_owned()))
    }
}

#[derive(Clone, Copy, Debug)]
enum CounterUpdate {
    Add,
    Set,
}

thread_local! {
    static THREAD_REGISTERED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static OPEN_SPANS: RefCell<HashMap<u64, Span>> = RefCell::new(HashMap::new());
    static OPEN_FRAMES: RefCell<HashMap<MarkerKind, Frame>> = RefCell::new(HashMap::new());
}

static TRACY: OnceLock<&'static Tracy> = OnceLock::new();

const BACKEND: Backend = Backend {
    counter_add: backend_counter_add,
    gauge_set: backend_gauge_set,
    scope_begin: backend_scope_begin,
    scope_end: backend_scope_end,
    resource_create: backend_resource_create,
    resource_update: backend_resource_update,
    resource_drop: backend_resource_drop,
    frame_marker: backend_frame_marker,
    flow: backend_flow,
};

fn backend_counter_add(desc: &CounterDesc, _context: Context, value: u64) {
    with_tracy(|tracy| tracy.observe_counter(desc, value, CounterUpdate::Add));
}

fn backend_gauge_set(desc: &CounterDesc, _context: Context, value: u64) {
    with_tracy(|tracy| tracy.observe_counter(desc, value, CounterUpdate::Set));
}

fn backend_scope_begin(desc: &ScopeDesc, context: Context) -> ScopeToken {
    with_tracy(|tracy| tracy.begin_scope(desc, context)).unwrap_or_default()
}

fn backend_scope_end(token: ScopeToken) {
    let _ = with_tracy(|tracy| tracy.end_scope(token));
}

fn backend_resource_create(
    desc: &ResourceKindDesc,
    resource_id: ResourceId,
    context: Context,
    size: u64,
) {
    let _ = with_tracy(|tracy| {
        tracy.emit_resource_message("create", desc, resource_id, context, Some(size));
    });
}

fn backend_resource_update(
    desc: &ResourceKindDesc,
    resource_id: ResourceId,
    context: Context,
    size: u64,
) {
    let _ = with_tracy(|tracy| {
        tracy.emit_resource_message("update", desc, resource_id, context, Some(size));
    });
}

fn backend_resource_drop(desc: &ResourceKindDesc, resource_id: ResourceId, context: Context) {
    let _ =
        with_tracy(|tracy| tracy.emit_resource_message("drop", desc, resource_id, context, None));
}

fn backend_frame_marker(kind: MarkerKind, phase: MarkerPhase, _context: Context) {
    let _ = with_tracy(|tracy| tracy.handle_marker(kind, phase));
}

fn backend_flow(_flow_id: FlowId, _phase: FlowPhase, _context: Context) {}

fn with_tracy<T>(f: impl FnOnce(&Tracy) -> T) -> Option<T> {
    TRACY.get().map(|tracy| {
        tracy.register_current_thread();
        f(tracy)
    })
}

fn plot_configuration(unit: Unit) -> PlotConfiguration {
    let format = match unit {
        Unit::Bytes => PlotFormat::Memory,
        Unit::Ratio => PlotFormat::Percentage,
        Unit::Count | Unit::Nanos | Unit::Micros | Unit::Items => PlotFormat::Number,
    };
    PlotConfiguration::default().format(format)
}

fn frame_name(kind: MarkerKind) -> FrameName {
    match kind {
        MarkerKind::Frame => *FRAME_NAME.get_or_init(|| FrameName::new_leak(String::from("frame"))),
        MarkerKind::Transaction => {
            *TRANSACTION_NAME.get_or_init(|| FrameName::new_leak(String::from("transaction")))
        }
        MarkerKind::Pass => *PASS_NAME.get_or_init(|| FrameName::new_leak(String::from("pass"))),
    }
}

static FRAME_NAME: OnceLock<FrameName> = OnceLock::new();
static TRANSACTION_NAME: OnceLock<FrameName> = OnceLock::new();
static PASS_NAME: OnceLock<FrameName> = OnceLock::new();
fn resource_message(
    verb: &str,
    desc: &ResourceKindDesc,
    resource_id: ResourceId,
    context: Context,
    size: Option<u64>,
) -> String {
    let mut message = String::new();
    let _ = write!(
        message,
        "resource {verb} {}#{}",
        desc.name(),
        resource_id.get()
    );
    if let Some(size) = size {
        let _ = write!(message, " size={size}");
    }
    let context = context_text(context);
    if !context.is_empty() {
        let _ = write!(message, " [{context}]");
    }
    message
}

fn leak_span_location(name: &str, location: SourceLocation) -> &'static SpanLocation {
    let type_name = Box::leak(format!("{}::S", location.function()).into_boxed_str());
    let name = Box::leak(format!("{name}\0").into_boxed_str());
    let file = Box::leak(format!("{}\0", location.file()).into_boxed_str());
    let location = tracy_client::internal::make_span_location(
        type_name,
        name.as_ptr(),
        file.as_ptr(),
        location.line(),
    );
    Box::leak(Box::new(location))
}

fn context_text(context: Context) -> String {
    let mut text = String::new();
    for (index, entry) in context.iter().enumerate() {
        if index > 0 {
            text.push_str(" / ");
        }
        text.push_str(entry.key().as_str());
        text.push('=');
        push_label_value(&mut text, entry.value());
    }
    text
}

fn push_label_value(text: &mut String, value: LabelValue) {
    match value {
        LabelValue::Unsigned(value) => {
            let _ = write!(text, "{value}");
        }
        LabelValue::Signed(value) => {
            let _ = write!(text, "{value}");
        }
        LabelValue::Bool(value) => {
            let _ = write!(text, "{value}");
        }
        LabelValue::StaticStr(value) => text.push_str(value),
    }
}

#[cfg(test)]
mod tests {
    use super::{context_text, plot_configuration, resource_message};
    use spoor_core::{
        Context, CounterDesc, CounterDescId, LabelKey, ResourceId, ResourceKindDesc,
        ResourceKindId, Unit,
    };

    static BYTES: CounterDesc = CounterDesc::bytes(CounterDescId::new(1), "bytes");
    static ATLAS: ResourceKindDesc = ResourceKindDesc::new(ResourceKindId::new(2), "atlas");

    #[test]
    fn context_text_formats_compact_labels() {
        let context = Context::root()
            .with(LabelKey::Frame, 7_u32)
            .with(LabelKey::Pass, 1_u32)
            .with(LabelKey::Reason, "invalidate");

        assert_eq!(
            context_text(context),
            "frame=7 / pass=1 / reason=invalidate"
        );
    }

    #[test]
    fn bytes_use_memory_plot_format() {
        let debug = format!("{:?}", plot_configuration(BYTES.unit()));
        assert!(debug.contains("Memory"));
        let number = format!("{:?}", plot_configuration(Unit::Count));
        assert!(number.contains("Number"));
    }

    #[test]
    fn resource_messages_include_context_and_size() {
        let context = Context::root().with(LabelKey::Cache, "glyph_atlas");
        let message = resource_message("create", &ATLAS, ResourceId::new(9), context, Some(4096));
        assert!(message.contains("resource create atlas#9"));
        assert!(message.contains("size=4096"));
        assert!(message.contains("cache=glyph_atlas"));
    }
}
