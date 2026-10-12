// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Live Tracy backend for Footfall.
//!
//! [`TracyBackend`] is a [`footfall_core::Backend`] that emits directly into `tracy-client`.
//!
//! This first slice intentionally maps only the parts of Footfall that Tracy represents naturally:
//!
//! - scopes become Tracy zones
//! - counters and gauges become Tracy plots
//! - frame, pass, and transaction markers become Tracy frames
//! - resources become Tracy messages
//!
//! Flow events are currently ignored by this backend because Tracy does not expose a strong native
//! equivalent to Perfetto-style causal links.
//!
//! A [`TracyBackend`] can install itself as the process-wide backend with
//! [`TracyBackend::install_global`], or it can be composed through an adapter such as
//! `footfall_fanout`.

use footfall_core::{
    Args, Backend, Context, ContextHandle, CounterDesc, CounterDescId, EventDesc,
    InstallBackendError, LabelValue, MarkerKind, MarkerPhase, ResourceId, ResourceKindDesc,
    ScopeDesc, ScopeDescId, ScopeToken, SourceLocation, Unit, install_backend,
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

/// Live Tracy backend.
pub struct TracyBackend {
    client: Client,
    instance: u64,
    callstack_depth: u16,
    next_scope_token: AtomicU64,
    counters: Mutex<CounterState>,
    locations: Mutex<HashMap<ScopeDescId, &'static SpanLocation>>,
    /// Rendered text of each registered context, attached to zones and
    /// messages.
    contexts: Mutex<HashMap<ContextHandle, String>>,
}

impl TracyBackend {
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
            instance: NEXT_INSTANCE.fetch_add(1, Ordering::Relaxed),
            callstack_depth,
            next_scope_token: AtomicU64::new(1),
            counters: Mutex::new(CounterState::default()),
            locations: Mutex::new(HashMap::new()),
            contexts: Mutex::new(HashMap::new()),
        }
    }

    /// Installs this backend as the process-wide backend and keeps it alive for the process.
    ///
    /// The calling thread is registered with Tracy as part of installation.
    pub fn install_global(self) -> Result<&'static Self, InstallBackendError> {
        let tracy: &'static Self = Box::leak(Box::new(self));
        tracy.register_current_thread();
        install_backend(tracy)?;
        Ok(tracy)
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

    /// Distinguishes this backend's open spans and frames in thread-local state.
    ///
    /// This is a per-instance id rather than the backend's address, so a scope
    /// begun before the backend is moved (for example into
    /// [`TracyBackend::install_global`]) still ends afterwards.
    const fn key(&self) -> u64 {
        self.instance
    }

    fn next_scope_token(&self) -> ScopeToken {
        let raw = self.next_scope_token.fetch_add(1, Ordering::Relaxed);
        ScopeToken::new(raw)
    }

    /// Returns the rendered text of a registered context, or an empty string
    /// for the root context and handles this backend does not know.
    fn context_text(&self, context: ContextHandle) -> String {
        if context.is_root() {
            return String::new();
        }
        self.contexts
            .lock()
            .expect("tracy context mutex should not be poisoned")
            .get(&context)
            .cloned()
            .unwrap_or_default()
    }

    fn begin_scope(
        &self,
        desc: &'static ScopeDesc,
        context: ContextHandle,
        args: &Args,
    ) -> ScopeToken {
        let token = self.next_scope_token();
        let span = if let Some(location) = desc.location() {
            let location = self.cached_span_location(desc, location);
            self.client.clone().span(location, self.callstack_depth)
        } else {
            // Without a source location there is no meaningful line; Tracy
            // groups zones by name.
            self.client.clone().span_alloc(
                Some(desc.name()),
                "footfall_scope",
                "footfall",
                0,
                self.callstack_depth,
            )
        };

        let context_text = self.context_text(context);
        if !context_text.is_empty() {
            span.emit_text(&context_text);
        }
        let args_text = args_text(args);
        if !args_text.is_empty() {
            span.emit_text(&args_text);
        }

        OPEN_SPANS.with(|spans| {
            spans.borrow_mut().insert((self.key(), token.get()), span);
        });

        token
    }

    fn cached_span_location(
        &self,
        desc: &'static ScopeDesc,
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
        if !token.is_valid() {
            return;
        }
        OPEN_SPANS.with(|spans| {
            spans.borrow_mut().remove(&(self.key(), token.get()));
        });
    }

    fn observe_counter(&self, desc: &'static CounterDesc, value: u64, update: CounterUpdate) {
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
        let plotted = match desc.unit() {
            // Ratios are parts per million; Tracy's percentage plots take
            // percent.
            Unit::Ratio => *current as f64 / 10_000.0,
            _ => *current as f64,
        };
        drop(counters);

        self.client.plot(plot_name, plotted);
    }

    fn handle_marker(&self, kind: MarkerKind, phase: MarkerPhase) {
        match phase {
            MarkerPhase::Begin => {
                let frame = self.client.non_continuous_frame(frame_name(kind));
                OPEN_FRAMES.with(|frames| {
                    frames.borrow_mut().insert((self.key(), kind), frame);
                });
            }
            MarkerPhase::End => {
                OPEN_FRAMES.with(|frames| {
                    frames.borrow_mut().remove(&(self.key(), kind));
                });
            }
        }
    }

    fn emit_resource_message(
        &self,
        verb: &str,
        desc: &'static ResourceKindDesc,
        resource_id: ResourceId,
        context: ContextHandle,
        size: Option<u64>,
    ) {
        let message = resource_message(verb, desc, resource_id, &self.context_text(context), size);
        self.client.message(&message, self.callstack_depth);
    }
}

impl Default for TracyBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for TracyBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TracyBackend")
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
    fn plot_name(&mut self, desc: &'static CounterDesc) -> PlotName {
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

static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static THREAD_REGISTERED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static OPEN_SPANS: RefCell<HashMap<(u64, u64), Span>> = RefCell::new(HashMap::new());
    static OPEN_FRAMES: RefCell<HashMap<(u64, MarkerKind), Frame>> = RefCell::new(HashMap::new());
}

impl Backend for TracyBackend {
    fn context_registered(&self, handle: ContextHandle, context: &Context) {
        self.contexts
            .lock()
            .expect("tracy context mutex should not be poisoned")
            .insert(handle, context_text(context));
    }

    fn context_released(&self, handle: ContextHandle) {
        self.contexts
            .lock()
            .expect("tracy context mutex should not be poisoned")
            .remove(&handle);
    }

    fn counter_add(&self, desc: &'static CounterDesc, _context: ContextHandle, value: u64) {
        self.register_current_thread();
        self.observe_counter(desc, value, CounterUpdate::Add);
    }

    fn gauge_set(&self, desc: &'static CounterDesc, _context: ContextHandle, value: u64) {
        self.register_current_thread();
        self.observe_counter(desc, value, CounterUpdate::Set);
    }

    fn histogram_record(&self, desc: &'static CounterDesc, _context: ContextHandle, value: u64) {
        // Tracy has no histogram primitive; plot each sample so the distribution
        // is at least visible as a time series.
        self.register_current_thread();
        self.observe_counter(desc, value, CounterUpdate::Set);
    }

    fn scope_begin(
        &self,
        desc: &'static ScopeDesc,
        context: ContextHandle,
        args: &Args,
    ) -> ScopeToken {
        self.register_current_thread();
        self.begin_scope(desc, context, args)
    }

    fn scope_end(&self, token: ScopeToken) {
        self.end_scope(token);
    }

    fn resource_create(
        &self,
        desc: &'static ResourceKindDesc,
        resource_id: ResourceId,
        context: ContextHandle,
        size: u64,
    ) {
        self.register_current_thread();
        self.emit_resource_message("create", desc, resource_id, context, Some(size));
    }

    fn resource_update(
        &self,
        desc: &'static ResourceKindDesc,
        resource_id: ResourceId,
        context: ContextHandle,
        size: u64,
    ) {
        self.register_current_thread();
        self.emit_resource_message("update", desc, resource_id, context, Some(size));
    }

    fn resource_drop(
        &self,
        desc: &'static ResourceKindDesc,
        resource_id: ResourceId,
        context: ContextHandle,
    ) {
        self.register_current_thread();
        self.emit_resource_message("drop", desc, resource_id, context, None);
    }

    fn frame_marker(&self, kind: MarkerKind, phase: MarkerPhase, _context: ContextHandle) {
        self.register_current_thread();
        self.handle_marker(kind, phase);
    }

    // Flows are ignored: Tracy has no native causal-link primitive.

    fn message(&self, desc: &'static EventDesc, context: ContextHandle, text: &str, args: &Args) {
        self.register_current_thread();
        let message = message_text(desc, &self.context_text(context), text, args);
        self.client.message(&message, self.callstack_depth);
    }
}

fn plot_configuration(unit: Unit) -> PlotConfiguration {
    let format = match unit {
        Unit::Bytes => PlotFormat::Memory,
        Unit::Ratio => PlotFormat::Percentage,
        // Count, time, item, and any future units plot as plain numbers.
        _ => PlotFormat::Number,
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
        _ => *MARKER_NAME.get_or_init(|| FrameName::new_leak(String::from("marker"))),
    }
}

static FRAME_NAME: OnceLock<FrameName> = OnceLock::new();
static TRANSACTION_NAME: OnceLock<FrameName> = OnceLock::new();
static PASS_NAME: OnceLock<FrameName> = OnceLock::new();
static MARKER_NAME: OnceLock<FrameName> = OnceLock::new();
fn resource_message(
    verb: &str,
    desc: &'static ResourceKindDesc,
    resource_id: ResourceId,
    context: &str,
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
    if !context.is_empty() {
        let _ = write!(message, " [{context}]");
    }
    message
}

fn message_text(desc: &'static EventDesc, context_text: &str, text: &str, args: &Args) -> String {
    let mut message = String::new();
    let _ = write!(message, "{}: {text}", desc.name());
    if !context_text.is_empty() {
        let _ = write!(message, " [{context_text}]");
    }
    let args_text = args_text(args);
    if !args_text.is_empty() {
        let _ = write!(message, " {{{args_text}}}");
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

fn context_text(context: &Context) -> String {
    let mut text = String::new();
    for (index, entry) in context.iter().enumerate() {
        if index > 0 {
            text.push_str(" / ");
        }
        text.push_str(entry.key().name());
        text.push('=');
        push_label_value(&mut text, entry.value());
    }
    text
}

fn args_text(args: &Args) -> String {
    let mut text = String::new();
    for (index, entry) in args.iter().enumerate() {
        if index > 0 {
            text.push_str(" / ");
        }
        text.push_str(entry.key());
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
        _ => text.push('?'),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        OPEN_SPANS, TracyBackend, context_text, message_text, plot_configuration, resource_message,
    };
    use footfall_core::{
        Args, Backend, Context, ContextHandle, CounterDesc, EventDesc, LabelKey, ResourceId,
        ResourceKindDesc, ScopeDesc, Unit,
    };

    static FRAME: LabelKey = LabelKey::new("frame");
    static PASS: LabelKey = LabelKey::new("pass");
    static CACHE: LabelKey = LabelKey::new("cache");
    static REASON: LabelKey = LabelKey::new("reason");

    static BYTES: CounterDesc = CounterDesc::bytes("bytes");
    static ATLAS: ResourceKindDesc = ResourceKindDesc::new("atlas");
    static APP_LOG: EventDesc = EventDesc::new("app_log");
    static RECORD: ScopeDesc = ScopeDesc::new("record");

    #[test]
    fn scope_begun_before_a_move_ends_after_it() {
        let backend = TracyBackend::new();
        let token = backend.scope_begin(&RECORD, ContextHandle::ROOT, &Args::none());
        assert_eq!(OPEN_SPANS.with(|spans| spans.borrow().len()), 1);

        let moved = Box::new(backend);
        moved.scope_end(token);
        assert_eq!(
            OPEN_SPANS.with(|spans| spans.borrow().len()),
            0,
            "the span opened before the move should close after it"
        );
    }

    #[test]
    fn registered_contexts_render_until_released() {
        let backend = TracyBackend::new();
        let handle = ContextHandle::new(41);
        backend.context_registered(handle, &Context::root().with(&FRAME, 7_u32));
        assert_eq!(backend.context_text(handle), "frame=7");
        assert_eq!(backend.context_text(ContextHandle::ROOT), "");

        backend.context_released(handle);
        assert_eq!(
            backend.context_text(handle),
            "",
            "released handles render as root"
        );
    }

    #[test]
    fn context_text_formats_compact_labels() {
        let context = Context::root()
            .with(&FRAME, 7_u32)
            .with(&PASS, 1_u32)
            .with(&REASON, "invalidate");

        assert_eq!(
            context_text(&context),
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
        let context = Context::root().with(&CACHE, "glyph_atlas");
        let message = resource_message(
            "create",
            &ATLAS,
            ResourceId::new(9),
            &context_text(&context),
            Some(4096),
        );
        assert!(message.contains("resource create atlas#9"));
        assert!(message.contains("size=4096"));
        assert!(message.contains("cache=glyph_atlas"));
    }

    #[test]
    fn messages_include_context_and_args() {
        let context = Context::root().with(&FRAME, 7_u32);
        let args = Args::none().with("bytes", 4096_u32).with("level", "info");
        let message = message_text(&APP_LOG, &context_text(&context), "uploaded atlas", &args);

        assert!(message.contains("app_log: uploaded atlas"));
        assert!(message.contains("frame=7"));
        assert!(message.contains("bytes=4096"));
        assert!(message.contains("level=info"));
    }
}
