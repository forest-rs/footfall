// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What the view knows: the producer, recent frames and scopes, counters per
//! frame, messages, and losses, kept up to date one window at a time.

use crate::{Key, Update};
use footfall::analysis::{Continuity, GroupBy, OutcomeCounts, TimeRange, Trace};
use footfall::session::{CounterKind, Drops, OwnedObservation, OwnedSnapshot};
use footfall::{CounterDescId, MarkerKind, Unit};
use footfall_examples::format;
use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

/// Frames kept for the chart and statistics.
const FRAME_HISTORY: usize = 600;
/// The producer's recent time the scope table covers.
pub(crate) const SCOPE_SPAN: Duration = Duration::from_secs(5);
/// The producer's time the trace keeps; it forgets anything older.
const TRACE_HISTORY: Duration = Duration::from_secs(30);
/// Messages kept.
const MESSAGE_HISTORY: usize = 100;

/// Where the session stands.
#[derive(Clone, Debug)]
pub(crate) enum Status {
    /// Waiting for the first window.
    Connecting,
    /// Windows are arriving with nothing missing.
    Live,
    /// The last window followed a gap: events were lost.
    Gap,
    /// The producer restarted; history starts over.
    Restarted,
    /// The source ended.
    Ended(String),
}

/// The producer, as its snapshots describe it.
#[derive(Clone, Debug)]
pub(crate) struct About {
    pub process: String,
    pub process_id: u32,
    pub session: Option<String>,
    pub level: &'static str,
    pub instance: u64,
    pub event_capacity: usize,
}

/// One scope name's totals over the last [`SCOPE_SPAN`].
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ScopeTotals {
    pub count: u64,
    pub total: u64,
    pub own: u64,
    pub max: u64,
}

/// One counter's movement over recent frames.
#[derive(Clone, Debug)]
pub(crate) struct CounterRow {
    pub name: String,
    pub unit: Unit,
    pub gauge: bool,
    /// Change in the last frame, or the value for a gauge.
    pub last: u64,
    /// Mean change per frame over recent frames.
    pub per_frame: u64,
    /// Value now, summed over contexts.
    pub now: u64,
}

/// One recent message.
#[derive(Clone, Debug)]
pub(crate) struct Message {
    pub name: String,
    pub text: String,
}

/// The view's state.
pub(crate) struct App {
    pub source: String,
    pub budget: u64,
    pub about: Option<About>,
    pub status: Status,
    pub paused: bool,
    pub sort_by_own: bool,
    /// Durations of recent frames, oldest first.
    pub frames: VecDeque<u64>,
    pub frames_seen: u64,
    /// How the frames the trace holds were paced, when a frame scheduler
    /// or display reported presentation times for any of them.
    pub pacing: Option<OutcomeCounts>,
    pub scopes: BTreeMap<String, ScopeTotals>,
    pub counters: Vec<CounterRow>,
    pub messages: VecDeque<Message>,
    pub windows: u64,
    pub last_events: usize,
    pub last_bytes: Option<usize>,
    pub events_per_second: u64,
    pub missed: u64,
    pub gaps: u64,
    pub drops: Drops,
    trace: Trace,
    counter_frames: VecDeque<BTreeMap<CounterDescId, (String, Unit, bool, u64)>>,
    last_window_at: Option<Instant>,
}

impl App {
    pub(crate) fn new(source: String, budget: u64) -> Self {
        Self {
            source,
            budget,
            about: None,
            status: Status::Connecting,
            paused: false,
            sort_by_own: false,
            frames: VecDeque::new(),
            frames_seen: 0,
            pacing: None,
            scopes: BTreeMap::new(),
            counters: Vec::new(),
            messages: VecDeque::new(),
            windows: 0,
            last_events: 0,
            last_bytes: None,
            events_per_second: 0,
            missed: 0,
            gaps: 0,
            drops: Drops::default(),
            trace: Trace::new(),
            counter_frames: VecDeque::new(),
            last_window_at: None,
        }
    }

    /// Handles a key; returns `false` to quit.
    pub(crate) fn key(&mut self, key: Key) -> bool {
        match key {
            Key::Quit => return false,
            Key::Pause => self.paused = !self.paused,
            Key::Sort => self.sort_by_own = !self.sort_by_own,
        }
        true
    }

    pub(crate) fn update(&mut self, update: Update) {
        match update {
            Update::Ended(reason) => self.status = Status::Ended(reason),
            // While paused, windows are still tracked so nothing is
            // misattributed on resume; only the display stops changing.
            Update::Window(window, bytes) => self.window(&window, bytes, !self.paused),
        }
    }

    fn window(&mut self, window: &OwnedSnapshot, bytes: Option<usize>, show: bool) {
        // A snapshot from another runtime instance is a restarted producer,
        // whose history starts over in a new trace.
        let (pushed, restarted) = match self.trace.push(window) {
            Ok(pushed) => (pushed, false),
            Err(_) => {
                self.trace = Trace::new();
                self.frames.clear();
                self.counter_frames.clear();
                self.messages.clear();
                let pushed = self
                    .trace
                    .push(window)
                    .expect("a new trace accepts a full snapshot");
                (pushed, true)
            }
        };
        let now_in_trace = self.trace.observed().end();
        self.trace
            .forget_before(now_in_trace.saturating_sub(TRACE_HISTORY));
        if !show {
            return;
        }
        let now = Instant::now();
        if let Some(previous) = self.last_window_at {
            let millis = u64::try_from(now.duration_since(previous).as_millis())
                .unwrap_or(u64::MAX)
                .max(1);
            self.events_per_second = window.events().len() as u64 * 1000 / millis;
        }
        self.last_window_at = Some(now);

        self.status = match pushed.continuity() {
            _ if restarted => Status::Restarted,
            Continuity::Gap { .. } => {
                self.gaps += 1;
                Status::Gap
            }
            Continuity::First | Continuity::Continuous => Status::Live,
        };
        let metadata = window.metadata();
        let capture = metadata.capture();
        self.about = Some(About {
            process: metadata.process_name().to_owned(),
            process_id: metadata.process_id(),
            session: metadata.session_name().map(str::to_owned),
            level: capture.level().as_str(),
            instance: metadata.instance_id(),
            event_capacity: capture.event_capacity(),
        });
        self.windows += 1;
        self.last_events = window.events().len();
        self.last_bytes = bytes;
        self.missed += window.window().missed();
        self.drops = window.drops();

        let frames = pushed
            .frames()
            .iter()
            .filter_map(|id| self.trace.frame(*id))
            .filter(|frame| frame.kind() == MarkerKind::Frame);
        for frame in frames {
            self.frames.push_back(format::as_micros(frame.duration()));
            self.frames_seen += 1;
            let mut counters = BTreeMap::new();
            for counter in frame.counters() {
                let gauge = counter.kind() == CounterKind::Gauge;
                let entry = counters.entry(counter.desc_id()).or_insert((
                    counter.name().to_owned(),
                    counter.unit(),
                    gauge,
                    0_u64,
                ));
                let amount = if gauge {
                    counter.value()
                } else {
                    counter.change().unwrap_or(0)
                };
                entry.3 = entry.3.saturating_add(amount);
            }
            self.counter_frames.push_back(counters);
        }
        while self.frames.len() > FRAME_HISTORY {
            self.frames.pop_front();
        }
        while self.counter_frames.len() > FRAME_HISTORY {
            self.counter_frames.pop_front();
        }

        let counts = self.trace.pacing(Duration::from_millis(1)).counts();
        self.pacing = (counts.total() > counts.unknown).then_some(counts);
        let recent = TimeRange::new(now_in_trace.saturating_sub(SCOPE_SPAN), now_in_trace);
        let table = self
            .trace
            .spans()
            .within(recent)
            .group_by(GroupBy::Name)
            .table();
        self.scopes = table
            .rows()
            .iter()
            .map(|row| {
                let stats = row.stats();
                (
                    row.describe(),
                    ScopeTotals {
                        count: stats.count(),
                        total: format::as_micros(stats.total()),
                        own: format::as_micros(stats.self_time()),
                        max: format::as_micros(stats.time().max()),
                    },
                )
            })
            .collect();

        self.counters = self.counter_rows(window);

        for event in window.events() {
            if let OwnedObservation::Message { name, text, .. } = event.observation() {
                self.messages.push_front(Message {
                    name: name.clone(),
                    text: text.clone(),
                });
            }
        }
        self.messages.truncate(MESSAGE_HISTORY);
    }

    fn counter_rows(&self, window: &OwnedSnapshot) -> Vec<CounterRow> {
        let mut now = BTreeMap::<CounterDescId, u64>::new();
        for counter in window.counters() {
            let value = now.entry(counter.desc_id()).or_default();
            *value = value.saturating_add(counter.value());
        }
        let Some(last) = self.counter_frames.back() else {
            return Vec::new();
        };
        let recent = self.counter_frames.len().min(60);
        let mut rows: Vec<_> = last
            .iter()
            .map(|(id, (name, unit, gauge, amount))| {
                let sum: u64 = self
                    .counter_frames
                    .iter()
                    .rev()
                    .take(recent)
                    .filter_map(|frame| frame.get(id).map(|entry| entry.3))
                    .sum();
                CounterRow {
                    name: name.clone(),
                    unit: *unit,
                    gauge: *gauge,
                    last: *amount,
                    per_frame: sum / recent as u64,
                    now: now.get(id).copied().unwrap_or(*amount),
                }
            })
            .collect();
        rows.sort_by(|left, right| left.name.cmp(&right.name));
        rows
    }

    /// Scope rows, largest first by the chosen time.
    pub(crate) fn scope_rows(&self) -> Vec<(&str, ScopeTotals)> {
        let mut rows: Vec<_> = self
            .scopes
            .iter()
            .map(|(name, totals)| (name.as_str(), *totals))
            .collect();
        if self.sort_by_own {
            rows.sort_by_key(|(_, totals)| core::cmp::Reverse(totals.own));
        } else {
            rows.sort_by_key(|(_, totals)| core::cmp::Reverse(totals.total));
        }
        rows
    }
}
