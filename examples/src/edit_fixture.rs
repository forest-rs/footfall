// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Reproducible edit-inspection fixture, reduced from execution's procedural demo
//! (`examples/whole-edit-observations`, 41d577c691b64be4424e449060eba2de0a948ccc).
//! This is a synthetic contract fixture, not a recording of native graph execution.
//! Timestamps and sequence numbers are deterministic. Initial buffers survive graph
//! retirement until undo release; amplitude edits reuse topology; a rejected edit
//! reports node 2's failed attempt and node 0's publication notification.

use footfall::runtime::{
    CaptureSettings, CounterKind, EventWindow, Observation, Snapshot, SnapshotMetadata,
    TimedObservation,
};
use footfall::session::OwnedSnapshot;
use footfall::{
    Args, ContextId, CounterDesc, EventDesc, FlowId, FlowPhase, InstrumentationLevel, MarkerKind,
    MarkerPhase, ResourceId, ResourceKindDesc, ScopeDesc, ScopeToken,
};

static EDIT: ScopeDesc = ScopeDesc::new("procedural.edit");
static TAPE: ScopeDesc = ScopeDesc::new("procedural.tape");
static GRID: ScopeDesc = ScopeDesc::new("procedural.grid");
static DEFORM: ScopeDesc = ScopeDesc::new("procedural.deform");
static BODY: EventDesc = EventDesc::new("procedural.body_outcome");
static PUBLISHED: EventDesc = EventDesc::new("procedural.published");
static CUTOFF: EventDesc = EventDesc::new("procedural.cutoff");
static FAILED: EventDesc = EventDesc::new("procedural.failed");
static BEFORE: EventDesc = EventDesc::new("before_edit");
static AFTER: EventDesc = EventDesc::new("after_edit");
static POINTS: ResourceKindDesc = ResourceKindDesc::new("procedural.points");
static TOPOLOGY: ResourceKindDesc = ResourceKindDesc::new("procedural.topology");
static CREATED: CounterDesc = CounterDesc::bytes("created_bytes");
static LIVE: CounterDesc = CounterDesc::bytes("live_bytes");

#[derive(Default)]
struct Fixture {
    events: Vec<TimedObservation>,
    created: u64,
    live: u64,
    sampled: bool,
}
impl Fixture {
    fn event(&mut self, observation: Observation) {
        let seq = self.events.len() as u64;
        self.events
            .push(TimedObservation::new(seq * 10, seq, 1, observation));
    }
    fn scope(&mut self, desc: &'static ScopeDesc, args: Args) -> ScopeToken {
        let token = ScopeToken::new(self.events.len() as u64 + 1);
        self.event(Observation::ScopeBegin {
            desc_id: desc.id(),
            name: desc.name(),
            location: None,
            context_id: ContextId::new(0),
            token,
            args,
        });
        token
    }
    fn end(&mut self, token: ScopeToken) {
        self.event(Observation::ScopeEnd { token });
    }
    fn message(&mut self, desc: &'static EventDesc, text: &str, args: Args) {
        self.event(Observation::Message {
            desc_id: desc.id(),
            name: desc.name(),
            context_id: ContextId::new(0),
            text: text.into(),
            args,
        });
    }
    fn checkpoint(&mut self, desc: &'static EventDesc) {
        if self.sampled {
            for (counter, kind, value) in [
                (&CREATED, CounterKind::Counter, self.created),
                (&LIVE, CounterKind::Gauge, self.live),
            ] {
                self.event(Observation::CounterSample {
                    desc_id: counter.id(),
                    name: counter.name(),
                    unit: counter.unit(),
                    context_id: ContextId::new(0),
                    kind,
                    value,
                });
            }
        }
        self.event(Observation::CounterCheckpoint {
            desc_id: desc.id(),
            name: desc.name(),
            context_id: ContextId::new(0),
            samples: if self.sampled { 2 } else { 0 },
        });
        if self.sampled {
            let at = (self.events.len() as u64 - 1) * 10;
            for event in self.events.iter_mut().rev().take(3) {
                *event =
                    TimedObservation::new(at, event.sequence(), 1, event.observation().clone());
            }
        }
    }
    fn resource(&mut self, kind: &'static ResourceKindDesc, id: u64, size: u64, create: bool) {
        let kind_id = kind.id();
        let kind_name = kind.name();
        let resource_id = ResourceId::new(id);
        let context_id = ContextId::new(0);
        if create {
            self.created += size;
            self.live += size;
            self.sampled = true;
        } else {
            self.live -= size;
        }
        self.event(if create {
            Observation::ResourceCreate {
                kind_id,
                kind_name,
                resource_id,
                context_id,
                size,
            }
        } else {
            Observation::ResourceDrop {
                kind_id,
                kind_name,
                resource_id,
                context_id,
            }
        });
    }
}

/// Builds a complete synthetic capture with `amplitude_edits + 5` edits.
/// No counter baseline is fabricated before the first resource observation.
/// Each amplitude edit creates 64 bytes; the quality edit creates 240 bytes;
/// 96 initial bytes survive retirement and are released by the final edit.
#[must_use]
pub fn snapshot(amplitude_edits: u64) -> OwnedSnapshot {
    let mut fixture = Fixture::default();
    let mut current = 3;
    let mut graph = [(1, 64, false), (2, 32, true)];
    for edit in 1..=amplitude_edits + 5 {
        let kind = if edit == 1 {
            "initial"
        } else if edit <= amplitude_edits + 1 {
            "amplitude"
        } else if edit == amplitude_edits + 2 {
            "quality"
        } else if edit == amplitude_edits + 3 {
            "rejected"
        } else if edit == amplitude_edits + 4 {
            "retirement"
        } else {
            "undo-release"
        };
        fixture.event(Observation::FrameMarker {
            kind: MarkerKind::Transaction,
            phase: MarkerPhase::Begin,
            number: None,
            context_id: ContextId::new(0),
        });
        let root = fixture.scope(&EDIT, Args::none().with("edit", edit).with("kind", kind));
        fixture.checkpoint(&BEFORE);
        fixture.event(Observation::Flow {
            flow_id: FlowId::new(edit),
            phase: FlowPhase::Begin,
            context_id: ContextId::new(0),
        });
        if matches!(kind, "initial" | "amplitude" | "quality" | "rejected") {
            let token = fixture.scope(
                &TAPE,
                Args::none().with("node", 0_u64).with("attempt", edit),
            );
            fixture.message(
                &BODY,
                "complete",
                Args::none().with("node", 0_u64).with("attempt", edit),
            );
            fixture.end(token);
            if matches!(kind, "initial" | "quality") {
                let token = fixture.scope(
                    &GRID,
                    Args::none().with("node", 1_u64).with("attempt", edit),
                );
                if kind == "quality" {
                    fixture.resource(&POINTS, graph[0].0, graph[0].1, false);
                    // The initial topology is retained by undo.
                    graph = [(100, 96, false), (101, 48, true)];
                }
                for (id, size, topology) in graph {
                    fixture.resource(if topology { &TOPOLOGY } else { &POINTS }, id, size, true);
                }
                fixture.end(token);
            } else {
                fixture.message(&CUTOFF, "grid", Args::none().with("node", 1_u64));
            }
            let token = fixture.scope(
                &DEFORM,
                Args::none().with("node", 2_u64).with("attempt", edit),
            );
            fixture.event(Observation::Flow {
                flow_id: FlowId::new(edit),
                phase: FlowPhase::Step,
                context_id: ContextId::new(0),
            });
            if kind == "rejected" {
                fixture.message(
                    &BODY,
                    "failed",
                    Args::none().with("node", 2_u64).with("attempt", edit),
                );
            } else {
                if current != 3 {
                    fixture.resource(&POINTS, current, 64, false);
                }
                current = if kind == "initial" { 3 } else { edit + 1000 };
                fixture.resource(
                    &POINTS,
                    current,
                    if kind == "quality" { 96 } else { 64 },
                    true,
                );
                fixture.message(
                    &BODY,
                    "complete",
                    Args::none().with("node", 2_u64).with("attempt", edit),
                );
            }
            fixture.end(token);
            fixture.message(
                &PUBLISHED,
                "parameters",
                Args::none().with("node", 0_u64).with("edit", edit),
            );
            if kind == "rejected" {
                fixture.message(&FAILED, "query failed", Args::none().with("edit", edit));
            }
        } else if kind == "retirement" {
            for (id, size, topology) in graph {
                fixture.resource(if topology { &TOPOLOGY } else { &POINTS }, id, size, false);
            }
            fixture.resource(&POINTS, current, 96, false);
        } else {
            fixture.resource(&TOPOLOGY, 2, 32, false);
            fixture.resource(&POINTS, 3, 64, false);
        }
        fixture.checkpoint(&AFTER);
        fixture.event(Observation::Flow {
            flow_id: FlowId::new(edit),
            phase: FlowPhase::End,
            context_id: ContextId::new(0),
        });
        fixture.end(root);
        fixture.event(Observation::FrameMarker {
            kind: MarkerKind::Transaction,
            phase: MarkerPhase::End,
            number: None,
            context_id: ContextId::new(0),
        });
    }
    let next = fixture.events.len() as u64;
    let metadata = SnapshotMetadata::new("edit inspection contract fixture")
        .with_instance_id(72)
        .with_capture(
            CaptureSettings::new(InstrumentationLevel::Trace, fixture.events.len(), 32, 32)
                .with_level_floor(InstrumentationLevel::Trace)
                .with_counter_samples(true),
        );
    let mut snapshot = Snapshot::builder(metadata)
        .captured_at_nanos(next * 10)
        .window(EventWindow::new(0, next, 0));
    for event in fixture.events {
        snapshot = snapshot.event(event);
    }
    OwnedSnapshot::from(snapshot.build())
}

#[cfg(test)]
mod tests {
    use super::snapshot;
    use footfall::analysis::{MessageQuery, Page, ResourceGrouping, Trace, WorkId};

    #[test]
    fn one_edit_can_be_investigated_entirely_through_public_results() {
        let trace = Trace::from_snapshot(&snapshot(2));
        let rejected = trace
            .spans()
            .named("procedural.edit")
            .with_arg("kind", "rejected")
            .iter();
        let work = trace
            .inspect_work(WorkId::Scope(rejected[0].span().id()))
            .expect("selected edit");
        let failed = work
            .messages
            .iter()
            .find(|message| {
                message.name() == "procedural.body_outcome" && message.text() == "failed"
            })
            .expect("failed body");
        let args = |message: &footfall::analysis::Message, key| {
            message
                .args()
                .iter()
                .find(|arg| arg.key() == key)
                .map(|arg| arg.value().clone())
        };
        assert_eq!(args(failed, "node"), Some(2_u64.into()));
        assert_eq!(args(failed, "attempt"), Some(5_u64.into()));
        let completion = work
            .messages
            .iter()
            .find(|message| message.name() == "procedural.failed")
            .expect("query completion");
        let notifications: Vec<_> = work
            .messages
            .iter()
            .filter(|message| {
                message.name() == "procedural.published"
                    && message.sequence() < completion.sequence()
            })
            .collect();
        assert_eq!(notifications.len(), 1);
        assert_eq!(args(notifications[0], "node"), Some(0_u64.into()));
        assert!(
            work.scopes
                .iter()
                .all(|scope| scope.name() != "procedural.grid")
        );
        assert!(
            work.messages
                .iter()
                .any(|message| message.name() == "procedural.cutoff"
                    && args(message, "node") == Some(1_u64.into()))
        );
        let retirement = trace
            .spans()
            .named("procedural.edit")
            .with_arg("kind", "retirement")
            .iter();
        let interval =
            trace.resource_interval(retirement[0].span().range(), Some(ResourceGrouping::Kind));
        assert_eq!(interval.activity.ending.observed, 96);
        assert!(interval.activity.ending.exact);
        assert_eq!(interval.activity.survivors.len(), 2);
        for id in &interval.activity.survivors {
            let origin = trace
                .resource_lifetime(*id)
                .expect("lifetime")
                .creation()
                .expect("origin")
                .span()
                .expect("scope");
            let initial = trace
                .spans()
                .named("procedural.edit")
                .with_arg("kind", "initial")
                .iter()[0]
                .span()
                .id();
            assert!(trace.scope_contains(initial, Some(origin)));
        }
        for (kind, created) in [("amplitude", 64), ("quality", 240)] {
            for selected in trace
                .spans()
                .named("procedural.edit")
                .with_arg("kind", kind)
                .iter()
            {
                let report =
                    trace.resource_interval(selected.span().range(), Some(ResourceGrouping::Kind));
                assert_eq!(report.activity.created.observed, created);
                assert!(report.activity.created.exact);
                let topology = report
                    .groups
                    .iter()
                    .find(|group| {
                        group.key
                            == footfall::analysis::ResourceGroupKey::Kind(
                                "procedural.topology".into(),
                            )
                    })
                    .expect("kind");
                assert_eq!(
                    topology.activity.created.observed,
                    if kind == "quality" { 48 } else { 0 }
                );
            }
        }
        assert!(
            trace
                .counter_checkpoints()
                .iter()
                .all(|point| point.complete())
        );
        let first = &trace.counter_checkpoints()[0];
        let second = &trace.counter_checkpoints()[1];
        let changes = trace
            .counter_changes(first.sequence(), second.sequence())
            .expect("boundaries");
        assert!(
            changes
                .counters()
                .iter()
                .filter(|counter| counter.counter().kind() == footfall::runtime::CounterKind::Gauge)
                .all(|counter| counter.before().is_none()),
            "an empty first batch is not a zero gauge baseline"
        );
        assert_eq!(
            trace
                .select_messages(
                    &MessageQuery::default(),
                    Page {
                        limit: 1,
                        ..Default::default()
                    }
                )
                .expect("bounded survey")
                .items
                .len(),
            1
        );
    }
}
