// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Typed selection and relationship traversal, independent of files and transports.

use crate::{
    CounterCheckpoint, Flow, FrameId, Gap, Message, OpenSpan, OwnedLabelValue, Recorded, Resource,
    Signal, Span, SpanId, TimeRange, Trace,
};
use footfall_core::{ContextId, FlowId};
use std::fmt;

/// Shared predicates for point observations. All supplied predicates must match.
#[derive(Clone, Debug, Default)]
pub struct EventFilter {
    /// Half-open runtime time interval; omission selects all retained observations.
    pub range: Option<TimeRange>,
    /// Captured scope ancestry, including the scope itself. Open scopes are supported.
    pub within: Option<SpanId>,
    /// Exact context identity.
    pub context: Option<ContextId>,
    /// Exact recording thread.
    pub thread: Option<u64>,
    /// Typed equality predicates on context labels.
    pub labels: Vec<(String, OwnedLabelValue)>,
}

/// Selection of messages by channel, typed arguments, and recorded relationships.
#[derive(Clone, Debug, Default)]
pub struct MessageQuery {
    /// Shared time, ancestry, context, thread, and label predicates.
    pub filter: EventFilter,
    /// Exact event descriptor name.
    pub name: Option<String>,
    /// Optional exact message event sequence.
    pub sequence: Option<u64>,
    /// Typed equality predicates on message arguments.
    pub args: Vec<(String, OwnedLabelValue)>,
}

/// Select flows when at least one step matches; return every retained step.
#[derive(Clone, Debug, Default)]
pub struct FlowQuery {
    /// Predicates applied to each step.
    pub filter: EventFilter,
    /// Optional exact flow identity.
    pub id: Option<FlowId>,
}

/// Bounded results with an exclusive identity cursor. Reuse the same query to continue.
/// Cursors are event sequences for messages and flow IDs for flows. They are scoped
/// to one runtime instance. Live changes can alter counts between calls.
#[derive(Clone, Copy, Debug)]
pub struct Page {
    /// Return identities strictly greater than this cursor.
    pub after: Option<u64>,
    /// Maximum number of items, at least one.
    pub limit: usize,
}
impl Default for Page {
    fn default() -> Self {
        Self {
            after: None,
            limit: 100,
        }
    }
}

/// Evidence for a point-observation query; file-read status belongs to the caller.
#[derive(Clone, Debug)]
pub struct QueryEvidence {
    /// Whether the requested signal is enabled in the latest snapshot.
    pub recorded: Recorded,
    /// Whether producer-attested history kept this signal enabled throughout.
    pub recording_history: Recorded,
    /// Selected range, or the retained observed range when no range was supplied.
    pub range: TimeRange,
    /// Retained gaps touching the selected range, including zero-duration gaps.
    pub gaps: Vec<Gap>,
    /// Whether the full requested range is retained and observed.
    pub covered: bool,
    /// True when the selected work has no retained end; results describe a prefix.
    pub open_work: bool,
    /// Whether all history is retained. An earlier loss can obscure ancestry or lifetimes.
    pub history_complete: bool,
    /// Observations lacking their requested context, cumulatively reported by the producer.
    pub unattributed: u64,
    /// Refused context registrations that can hide matching labels.
    pub refused_contexts: u64,
    /// Counter updates dropped or rejected after context release; zero for other signals.
    pub counter_updates_lost: u64,
    /// Unplaced timeline spans, for span inspection; zero for other signals.
    pub unplaced_spans: usize,
}
impl QueryEvidence {
    /// True only with recorded, observed, uninterrupted history. This deliberately
    /// remains conservative after pruning and for queries following earlier gaps.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.recorded.is_yes()
            && self.recording_history.is_yes()
            && self.covered
            && !self.open_work
            && self.history_complete
            && self.gaps.is_empty()
            && self.unattributed == 0
            && self.refused_contexts == 0
            && self.counter_updates_lost == 0
            && self.unplaced_spans == 0
    }
}

/// A deterministic page. Total counts all matches, including before the cursor.
#[derive(Clone, Debug)]
pub struct QueryPage<T> {
    /// Selected items in identity order.
    pub items: Vec<T>,
    /// Number of matching retained items before pagination.
    pub total: usize,
    /// Last returned identity when more matches remain; pass as `Page::after`.
    pub next: Option<u64>,
    /// Instrumentation and retained history evidence, independent of pagination.
    pub evidence: QueryEvidence,
}

/// A full retained flow and the steps that selected it.
#[derive(Clone, Debug)]
pub struct FlowMatch<'t> {
    /// Full retained flow, including endpoints outside the selection.
    pub flow: &'t Flow,
    /// Event identities of matching steps, in recorded order.
    pub matching_steps: Vec<u64>,
    /// Evidence over the entire retained flow interval, not just matching steps.
    /// Endpoint presence is separately available through `Flow::is_complete`.
    pub evidence: QueryEvidence,
}

/// Stable work identity within one runtime instance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkId {
    /// A scope begin, completed or open.
    Scope(SpanId),
    /// A completed frame, pass, or transaction begin marker.
    Marker(FrameId),
}

/// How observations became members of a work inspection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Membership {
    /// Captured parent links; simultaneous work on other threads is excluded.
    CapturedAncestry,
    /// Activity in a marker interval; no descendant or causal relationship is implied.
    IntervalActivity,
}

/// Connected observations for one selected unit. No application-specific meaning
/// is assigned to messages, resource roots, or publication notifications.
#[derive(Clone, Debug)]
pub struct Inspection<'t> {
    /// Selected work identity.
    pub id: WorkId,
    /// Membership rule for scopes and point observations.
    pub membership: Membership,
    /// Completed work interval, or observed prefix for an open scope.
    pub range: TimeRange,
    /// True when the selected scope has no retained end.
    pub open: bool,
    /// Selected completed scopes, including a completed scope root, in begin order.
    pub scopes: Vec<&'t Span>,
    /// Selected open scopes with captured arguments and ancestry.
    pub open_scopes: Vec<OpenSpan>,
    /// Selected messages in sequence order.
    pub messages: Vec<&'t Message>,
    /// Full flows touching selected work, including nonmatching endpoints.
    pub flows: Vec<FlowMatch<'t>>,
    /// Named checkpoints selected by the same membership rule.
    pub checkpoints: Vec<&'t CounterCheckpoint>,
    /// Resource lifetimes with at least one selected observation. Creation
    /// provenance and later activity must be distinguished by the consumer.
    pub resources: Vec<&'t Resource>,
    /// Evidence for each included signal, including instrumentation availability.
    pub evidence: Vec<(Signal, QueryEvidence)>,
}

/// Invalid query parameters or a work identity absent from retained history.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InspectionError {
    /// No retained completed or open scope has this identity.
    MissingScope(SpanId),
    /// No retained completed marker has this identity.
    MissingMarker(FrameId),
    /// A page limit must be positive.
    EmptyPage,
}
impl fmt::Display for InspectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingScope(id) => write!(f, "scope {id} is absent from retained history"),
            Self::MissingMarker(id) => {
                write!(f, "completed marker {id} is absent from retained history")
            }
            Self::EmptyPage => f.write_str("page limit must be at least one"),
        }
    }
}
impl std::error::Error for InspectionError {}

impl EventFilter {
    fn evidence(&self, trace: &Trace, signal: Signal) -> QueryEvidence {
        let mut evidence = trace.query_evidence(signal, self.range);
        evidence.open_work = self.within.is_some_and(|id| trace.span(id).is_none());
        evidence
    }
    fn validate(&self, trace: &Trace) -> Result<(), InspectionError> {
        if let Some(id) = self.within
            && trace.scope_parent(id).is_none()
        {
            return Err(InspectionError::MissingScope(id));
        }
        Ok(())
    }
    fn matches(
        &self,
        trace: &Trace,
        at: std::time::Duration,
        context: ContextId,
        thread: u64,
        span: Option<SpanId>,
    ) -> bool {
        self.range.is_none_or(|range| range.contains(at))
            && self.context.is_none_or(|id| id == context)
            && self.thread.is_none_or(|id| id == thread)
            && self
                .labels
                .iter()
                .all(|(key, value)| trace.label(context, key) == Some(value))
            && self
                .within
                .is_none_or(|root| trace.scope_contains(root, span))
    }
}

fn page<T>(
    mut matches: Vec<T>,
    spec: Page,
    identity: impl Fn(&T) -> u64,
    evidence: QueryEvidence,
) -> Result<QueryPage<T>, InspectionError> {
    if spec.limit == 0 {
        return Err(InspectionError::EmptyPage);
    }
    matches.sort_by_key(&identity);
    let total = matches.len();
    matches.retain(|item| spec.after.is_none_or(|after| identity(item) > after));
    let more = matches.len() > spec.limit;
    matches.truncate(spec.limit);
    let next = more.then(|| identity(matches.last().expect("positive limit")));
    Ok(QueryPage {
        items: matches,
        total,
        next,
        evidence,
    })
}

impl Trace {
    /// Tests captured ancestry, including `root` itself. Missing links stop
    /// traversal; simultaneous intervals never establish containment.
    #[must_use]
    pub fn scope_contains(&self, root: SpanId, mut candidate: Option<SpanId>) -> bool {
        while let Some(id) = candidate {
            if id == root {
                return true;
            }
            candidate = self.scope_parent(id).flatten();
        }
        false
    }

    /// Describes retained recording evidence without coupling queries to file reads.
    #[must_use]
    pub fn query_evidence(&self, signal: Signal, range: Option<TimeRange>) -> QueryEvidence {
        let observed = self.observed();
        let range = range.unwrap_or(observed);
        QueryEvidence {
            recorded: self.recorded(signal),
            recording_history: self.recording_history(signal),
            open_work: false,
            range,
            gaps: self
                .gaps()
                .iter()
                .copied()
                .filter(|gap| {
                    gap.range().start() <= range.end() && gap.range().end() >= range.start()
                })
                .collect(),
            covered: range.start() >= observed.start() && range.end() <= observed.end(),
            history_complete: self.has_complete_history(),
            unattributed: self.drops().unattributed(),
            refused_contexts: self.drops().refused_contexts(),
            unplaced_spans: if signal == Signal::Spans {
                self.unplaced_spans()
            } else {
                0
            },
            counter_updates_lost: if matches!(signal, Signal::Counters | Signal::CounterSamples) {
                self.drops()
                    .counter_updates()
                    .saturating_add(self.drops().updated_after_release())
            } else {
                0
            },
        }
    }

    /// Resolves a retained message event identity within this runtime instance.
    #[must_use]
    pub fn message(&self, sequence: u64) -> Option<&Message> {
        self.messages()
            .iter()
            .find(|message| message.sequence() == sequence)
    }

    /// Selects messages using typed predicates and captured nesting, ordered by
    /// event sequence. An invalid scope is an error rather than an empty answer.
    pub fn select_messages(
        &self,
        query: &MessageQuery,
        spec: Page,
    ) -> Result<QueryPage<&Message>, InspectionError> {
        query.filter.validate(self)?;
        let matches =
            self.messages()
                .iter()
                .filter(|message| {
                    query.sequence.is_none_or(|seq| message.sequence() == seq)
                        && query
                            .name
                            .as_deref()
                            .is_none_or(|name| message.name() == name)
                        && query.args.iter().all(|(key, value)| {
                            crate::query::arg(message.args(), key) == Some(value)
                        })
                        && query.filter.matches(
                            self,
                            message.at(),
                            message.context_id(),
                            message.thread_id(),
                            message.span(),
                        )
                })
                .collect();
        page(
            matches,
            spec,
            |message| message.sequence(),
            query.filter.evidence(self, Signal::Messages),
        )
    }

    /// Selects full flows by matching steps; endpoint presence never proves that
    /// intermediate steps survived loss. Each flow carries its own evidence.
    pub fn select_flows(
        &self,
        query: &FlowQuery,
        spec: Page,
    ) -> Result<QueryPage<FlowMatch<'_>>, InspectionError> {
        query.filter.validate(self)?;
        let matches = self
            .flows()
            .filter(|flow| query.id.is_none_or(|id| flow.id() == id))
            .filter_map(|flow| {
                let matching_steps: Vec<_> = flow
                    .steps()
                    .iter()
                    .filter(|step| {
                        query.filter.matches(
                            self,
                            step.at(),
                            step.context_id(),
                            step.thread_id(),
                            step.span(),
                        )
                    })
                    .map(|step| step.sequence())
                    .collect();
                if matching_steps.is_empty() {
                    return None;
                }
                let range = TimeRange::new(flow.steps().first()?.at(), flow.steps().last()?.at());
                Some(FlowMatch {
                    flow,
                    matching_steps,
                    evidence: self.query_evidence(Signal::Flows, Some(range)),
                })
            })
            .collect();
        page(
            matches,
            spec,
            |found| found.flow.id().get(),
            query.filter.evidence(self, Signal::Flows),
        )
    }

    /// Inspects a completed marker or completed/open scope. Open inspections
    /// describe only retained observations so far and are never complete. Scope
    /// membership uses captured ancestry; marker membership uses time alone.
    pub fn inspect_work(&self, id: WorkId) -> Result<Inspection<'_>, InspectionError> {
        let open = self.open_spans();
        let (range, is_open, membership, filter) = match id {
            WorkId::Scope(root) => {
                let completed = self.span(root);
                let begun = open.iter().find(|span| span.id() == root);
                let (begin, end) = if let Some(span) = completed {
                    (span.begin(), span.end())
                } else if let Some(span) = begun {
                    (span.begin(), self.observed().end())
                } else {
                    return Err(InspectionError::MissingScope(root));
                };
                (
                    TimeRange::new(begin, end),
                    completed.is_none(),
                    Membership::CapturedAncestry,
                    EventFilter {
                        within: Some(root),
                        ..Default::default()
                    },
                )
            }
            WorkId::Marker(root) => {
                let frame = self
                    .marks(footfall_core::MarkerKind::Frame)
                    .iter()
                    .chain(self.marks(footfall_core::MarkerKind::Pass))
                    .chain(self.marks(footfall_core::MarkerKind::Transaction))
                    .find(|frame| frame.id() == root)
                    .ok_or(InspectionError::MissingMarker(root))?;
                (
                    frame.range(),
                    false,
                    Membership::IntervalActivity,
                    EventFilter {
                        range: Some(frame.range()),
                        ..Default::default()
                    },
                )
            }
        };
        let belongs = |at, context, thread, span| filter.matches(self, at, context, thread, span);
        let scope_belongs = |begin, end, candidate| match id {
            WorkId::Scope(root) => self.scope_contains(root, Some(candidate)),
            WorkId::Marker(_) => range.overlaps(begin, end),
        };
        let mut scopes: Vec<_> = self
            .spans()
            .iter()
            .into_iter()
            .map(|selected| selected.span())
            .filter(|span| scope_belongs(span.begin(), span.end(), span.id()))
            .collect();
        scopes.sort_by_key(|span| (span.begin(), span.id()));
        let open_scopes = open
            .into_iter()
            .filter(|span| scope_belongs(span.begin(), self.observed().end(), span.id()))
            .collect();
        let all = Page {
            limit: usize::MAX,
            ..Default::default()
        };
        let messages = self
            .select_messages(
                &MessageQuery {
                    filter: filter.clone(),
                    ..Default::default()
                },
                all,
            )?
            .items;
        let flows = self
            .select_flows(
                &FlowQuery {
                    filter: filter.clone(),
                    ..Default::default()
                },
                all,
            )?
            .items;
        let checkpoints = self
            .counter_checkpoints()
            .iter()
            .filter(|point| {
                belongs(
                    point.at(),
                    point.context_id(),
                    point.thread_id(),
                    point.span(),
                )
            })
            .collect();
        let resources = self
            .resources()
            .iter()
            .filter(|resource| {
                resource.observations().iter().any(|event| {
                    belongs(
                        event.at(),
                        event.context_id(),
                        event.thread_id(),
                        event.span(),
                    )
                })
            })
            .collect();
        let evidence = [
            Signal::Spans,
            Signal::Messages,
            Signal::Flows,
            Signal::Resources,
            Signal::CounterSamples,
        ]
        .into_iter()
        .map(|signal| {
            let mut evidence = self.query_evidence(signal, Some(range));
            evidence.open_work = is_open;
            (signal, evidence)
        })
        .collect();
        Ok(Inspection {
            id,
            membership,
            range,
            open: is_open,
            scopes,
            open_scopes,
            messages,
            flows,
            checkpoints,
            resources,
            evidence,
        })
    }
}
