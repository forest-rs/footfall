// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `tracing` subscriber layer for Footfall.
//!
//! [`FootfallLayer`] forwards the `tracing` ecosystem into the Footfall backend:
//!
//! - spans become Footfall scopes, with numeric and boolean span fields carried
//!   as per-instance args
//! - events become Footfall messages, with the `message` field as text and the
//!   event level plus numeric and boolean fields as args
//!
//! Non-message string fields are ignored because [`Args`] only stores static
//! string values.
//!
//! Each span also gets a Footfall context: its numeric and boolean fields at
//! creation, keyed by field name, on top of the labels of its parent span.
//! The layer registers that context when the span is created and releases it
//! when the span closes, so the span's scope and the events recorded inside
//! it are attributed to it. A span with no such fields shares its parent's
//! context, and spans outside any span with no fields use the root context.
//!
//! A span's context comes only from its own fields and its parent `tracing`
//! span, not from footfall scopes or contexts around it, so a span recorded
//! inside a footfall scope is attributed by its `tracing` ancestry alone.
//! Give spans the fields you group by, or wrap the call in a footfall scope
//! under the context you want.
//!
//! Descriptors are interned per callsite. `tracing` callsite metadata is
//! `&'static`, so descriptor names and source locations come straight from
//! the callsite. Each layer leaks one descriptor per callsite it sees, so
//! memory is bounded by the number of callsites. Footfall descriptor identity
//! is the descriptor's address, so these descriptors never collide with
//! application descriptors or with those of another layer.
//!
//! ```no_run
//! use tracing_subscriber::layer::SubscriberExt as _;
//!
//! let subscriber =
//!     tracing_subscriber::registry().with(footfall_tracing::FootfallLayer::new());
//! tracing::subscriber::set_global_default(subscriber)
//!     .expect("subscriber should install once");
//! ```

use footfall_core::{
    Args, Context, ContextHandle, EventDesc, LabelKey, LabelValue, ScopeDesc, ScopeToken,
    SourceLocation,
};
use std::{cell::RefCell, collections::HashMap, fmt::Write as _, sync::Mutex};
use tracing_core::{
    Event, Level, Metadata, Subscriber,
    field::{Field, Visit},
    span,
};
use tracing_subscriber::{layer::Context as LayerContext, registry::LookupSpan};

/// `tracing_subscriber` layer that forwards spans and events into Footfall.
#[derive(Debug)]
pub struct FootfallLayer {
    scope_descs: Mutex<HashMap<CallsiteId, &'static ScopeDesc>>,
    event_descs: Mutex<HashMap<CallsiteId, &'static EventDesc>>,
    label_keys: Mutex<HashMap<&'static str, &'static LabelKey>>,
}

type CallsiteId = tracing_core::callsite::Identifier;

impl FootfallLayer {
    /// Creates a new layer.
    #[must_use]
    pub fn new() -> Self {
        Self {
            scope_descs: Mutex::new(HashMap::new()),
            event_descs: Mutex::new(HashMap::new()),
            label_keys: Mutex::new(HashMap::new()),
        }
    }

    /// Returns the label key for a field name, leaking one key per distinct
    /// name, so memory is bounded by the field names the callsites declare.
    fn label_key(&self, name: &'static str) -> &'static LabelKey {
        let mut keys = self
            .label_keys
            .lock()
            .expect("label key mutex should not be poisoned");
        keys.entry(name)
            .or_insert_with(|| Box::leak(Box::new(LabelKey::new(name))))
    }

    /// Builds a span's context from its parent's and its own labels.
    fn span_context(&self, parent: Context, labels: &[(&'static str, LabelValue)]) -> Context {
        labels.iter().fold(parent, |context, (name, value)| {
            // Contexts are bounded; labels past the capacity are dropped.
            context
                .try_with(self.label_key(name), *value)
                .unwrap_or(context)
        })
    }

    fn scope_desc(&self, metadata: &'static Metadata<'static>) -> &'static ScopeDesc {
        let mut descs = self
            .scope_descs
            .lock()
            .expect("scope descriptor mutex should not be poisoned");
        descs
            .entry(metadata.callsite())
            .or_insert_with(|| Box::leak(Box::new(build_scope_desc(metadata))))
    }

    fn event_desc(&self, metadata: &'static Metadata<'static>) -> &'static EventDesc {
        let mut descs = self
            .event_descs
            .lock()
            .expect("event descriptor mutex should not be poisoned");
        descs
            .entry(metadata.callsite())
            .or_insert_with(|| Box::leak(Box::new(EventDesc::new(metadata.target()))))
    }
}

impl Default for FootfallLayer {
    fn default() -> Self {
        Self::new()
    }
}

fn build_scope_desc(metadata: &'static Metadata<'static>) -> ScopeDesc {
    match (metadata.file(), metadata.line()) {
        (Some(file), Some(line)) => ScopeDesc::with_location(
            metadata.name(),
            SourceLocation::new(metadata.target(), file, line),
        ),
        _ => ScopeDesc::new(metadata.name()),
    }
}

/// Span state stored in the tracing registry extensions.
struct SpanState {
    args: Args,
    context: Context,
    handle: ContextHandle,
    /// Whether this span registered `handle` and must release it.
    owns_handle: bool,
}

std::thread_local! {
    static SCOPE_STACK: RefCell<Vec<(u64, ScopeToken)>> = const { RefCell::new(Vec::new()) };
}

impl<S> tracing_subscriber::Layer<S> for FootfallLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &span::Attributes<'_>, id: &span::Id, ctx: LayerContext<'_, S>) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        let mut visitor = ArgsVisitor::default();
        attrs.record(&mut visitor);

        let (parent_context, parent_handle) = span
            .parent()
            .and_then(|parent| {
                parent
                    .extensions()
                    .get::<SpanState>()
                    .map(|state| (state.context, state.handle))
            })
            .unwrap_or((Context::root(), ContextHandle::ROOT));
        let (context, handle, owns_handle) = if visitor.labels.is_empty() {
            (parent_context, parent_handle, false)
        } else {
            let context = self.span_context(parent_context, &visitor.labels);
            let handle = footfall_core::register_context(&context);
            (context, handle, !handle.is_root())
        };
        span.extensions_mut().insert(SpanState {
            args: visitor.args,
            context,
            handle,
            owns_handle,
        });
    }

    fn on_close(&self, id: span::Id, ctx: LayerContext<'_, S>) {
        let Some(span) = ctx.span(&id) else {
            return;
        };
        if let Some(state) = span.extensions().get::<SpanState>()
            && state.owns_handle
        {
            footfall_core::release_context(state.handle);
        }
    }

    fn on_record(&self, id: &span::Id, values: &span::Record<'_>, ctx: LayerContext<'_, S>) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        let mut extensions = span.extensions_mut();
        if let Some(state) = extensions.get_mut::<SpanState>() {
            let mut visitor = ArgsVisitor {
                args: state.args,
                labels: Vec::new(),
            };
            values.record(&mut visitor);
            state.args = visitor.args;
        }
    }

    fn on_enter(&self, id: &span::Id, ctx: LayerContext<'_, S>) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        let desc = self.scope_desc(span.metadata());
        let (args, handle) = span
            .extensions()
            .get::<SpanState>()
            .map_or((Args::none(), ContextHandle::ROOT), |state| {
                (state.args, state.handle)
            });
        let token = footfall_core::scope_begin_with(desc, handle, &args);
        SCOPE_STACK.with(|stack| stack.borrow_mut().push((id.into_u64(), token)));
    }

    fn on_exit(&self, id: &span::Id, _ctx: LayerContext<'_, S>) {
        let token = SCOPE_STACK.with(|stack| {
            let mut stack = stack.borrow_mut();
            let position = stack
                .iter()
                .rposition(|(span_id, _)| *span_id == id.into_u64())?;
            Some(stack.remove(position).1)
        });
        if let Some(token) = token {
            footfall_core::scope_end(token);
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: LayerContext<'_, S>) {
        let handle = ctx
            .event_span(event)
            .and_then(|span| {
                span.extensions()
                    .get::<SpanState>()
                    .map(|state| state.handle)
            })
            .unwrap_or(ContextHandle::ROOT);
        let desc = self.event_desc(event.metadata());
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let args = visitor
            .args
            .try_with("level", level_str(*event.metadata().level()))
            .unwrap_or(visitor.args);
        footfall_core::message_with(desc, handle, &visitor.text, &args);
    }
}

const fn level_str(level: Level) -> &'static str {
    match level {
        Level::TRACE => "trace",
        Level::DEBUG => "debug",
        Level::INFO => "info",
        Level::WARN => "warn",
        Level::ERROR => "error",
    }
}

/// Collects span fields as args and, at span creation, as context labels.
#[derive(Default)]
struct ArgsVisitor {
    args: Args,
    labels: Vec<(&'static str, LabelValue)>,
}

impl ArgsVisitor {
    fn push(&mut self, field: &Field, value: LabelValue) {
        if field.name() == "message" {
            return;
        }
        // Args are bounded; overflowing fields are dropped silently.
        self.args = self.args.try_with(field.name(), value).unwrap_or(self.args);
        self.labels.push((field.name(), value));
    }
}

impl Visit for ArgsVisitor {
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.push(field, LabelValue::Unsigned(value));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.push(field, LabelValue::Signed(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.push(field, LabelValue::Bool(value));
    }

    fn record_debug(&mut self, _field: &Field, _value: &dyn core::fmt::Debug) {}
}

#[derive(Default)]
struct MessageVisitor {
    text: String,
    args: Args,
}

impl Visit for MessageVisitor {
    fn record_u64(&mut self, field: &Field, value: u64) {
        if field.name() != "message" {
            self.args = self
                .args
                .try_with(field.name(), LabelValue::Unsigned(value))
                .unwrap_or(self.args);
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        if field.name() != "message" {
            self.args = self
                .args
                .try_with(field.name(), LabelValue::Signed(value))
                .unwrap_or(self.args);
        }
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        if field.name() != "message" {
            self.args = self
                .args
                .try_with(field.name(), LabelValue::Bool(value))
                .unwrap_or(self.args);
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.text.push_str(value);
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn core::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.text, "{value:?}");
        }
    }
}
