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

use footfall_core::{Args, EventDesc, LabelValue, ScopeDesc, ScopeToken, SourceLocation};
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
}

type CallsiteId = tracing_core::callsite::Identifier;

impl FootfallLayer {
    /// Creates a new layer.
    #[must_use]
    pub fn new() -> Self {
        Self {
            scope_descs: Mutex::new(HashMap::new()),
            event_descs: Mutex::new(HashMap::new()),
        }
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
struct SpanArgs {
    args: Args,
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
        span.extensions_mut()
            .insert(SpanArgs { args: visitor.args });
    }

    fn on_record(&self, id: &span::Id, values: &span::Record<'_>, ctx: LayerContext<'_, S>) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        let mut extensions = span.extensions_mut();
        if let Some(state) = extensions.get_mut::<SpanArgs>() {
            let mut visitor = ArgsVisitor { args: state.args };
            values.record(&mut visitor);
            state.args = visitor.args;
        }
    }

    fn on_enter(&self, id: &span::Id, ctx: LayerContext<'_, S>) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        let desc = self.scope_desc(span.metadata());
        let args = span
            .extensions()
            .get::<SpanArgs>()
            .map_or_else(Args::none, |state| state.args);
        let token = footfall_core::scope_begin_with(desc, &footfall_core::Context::root(), &args);
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

    fn on_event(&self, event: &Event<'_>, _ctx: LayerContext<'_, S>) {
        let desc = self.event_desc(event.metadata());
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let args = visitor
            .args
            .try_with("level", level_str(*event.metadata().level()))
            .unwrap_or(visitor.args);
        footfall_core::message_with(desc, &footfall_core::Context::root(), &visitor.text, &args);
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

#[derive(Default)]
struct ArgsVisitor {
    args: Args,
}

impl ArgsVisitor {
    fn push(&mut self, field: &Field, value: LabelValue) {
        if field.name() == "message" {
            return;
        }
        // Args are bounded; overflowing fields are dropped silently.
        self.args = self.args.try_with(field.name(), value).unwrap_or(self.args);
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
