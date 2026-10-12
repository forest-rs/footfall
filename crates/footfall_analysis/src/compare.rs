// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What changed between two sets of work: more of it, or each piece slower.

use crate::basis::Basis;
use crate::frame::Frame;
use crate::query::{GroupBy, KeyPart};
use crate::time::{Selection, TimeRange};
use crate::trace::Trace;
use core::time::Duration;
use std::collections::BTreeMap;

/// Units of work to compare, such as frames, in one trace.
///
/// Each unit is a time range, and a comparison measures spans per unit, so
/// two populations with different numbers of frames compare fairly.
#[derive(Clone, Debug)]
pub struct Population<'t> {
    trace: &'t Trace,
    units: Vec<TimeRange>,
    /// Each unit's frame number, when the units are numbered frames.
    numbers: Vec<Option<u64>>,
}

impl<'t> Population<'t> {
    /// Takes each of `frames` as a unit, such as the slow frames of a
    /// trace. Spans on timelines reported with a frame's number, such as its
    /// GPU passes, count toward that frame, wherever they ran; see
    /// [`SpanQuery::in_frames`](crate::SpanQuery::in_frames).
    #[must_use]
    pub fn frames<'f>(trace: &'t Trace, frames: impl IntoIterator<Item = &'f Frame>) -> Self {
        let (units, numbers) = frames
            .into_iter()
            .map(|frame| (frame.range(), frame.number()))
            .unzip();
        Self {
            trace,
            units,
            numbers,
        }
    }

    /// Takes each of `ranges` as a unit.
    #[must_use]
    pub fn ranges(trace: &'t Trace, ranges: impl IntoIterator<Item = TimeRange>) -> Self {
        Self {
            trace,
            units: ranges.into_iter().collect(),
            numbers: Vec::new(),
        }
    }

    /// Takes the whole observed trace as one unit, as for comparing two
    /// runs of the same workload.
    #[must_use]
    pub fn whole(trace: &'t Trace) -> Self {
        Self {
            trace,
            units: vec![trace.observed()],
            numbers: Vec::new(),
        }
    }

    /// Returns the trace.
    #[must_use]
    pub const fn trace(&self) -> &'t Trace {
        self.trace
    }

    /// Returns the units.
    #[must_use]
    pub fn units(&self) -> &[TimeRange] {
        &self.units
    }
}

/// Why a [`Comparison`] should be read with care.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Caveat {
    /// A side has fewer than two units, so there is no estimate of how much
    /// the per-unit figures vary, and no difference can be told from noise.
    FewUnits,
    /// The sides come from different runtime instances, so rows were
    /// matched by names, label and argument values, thread names, and
    /// context labels rather than ids; [`GroupBy::Thread`] matches unnamed
    /// threads only by chance.
    DifferentInstances,
    /// The producers' versions differ.
    DifferentProducers {
        /// The baseline producer's version.
        baseline: String,
        /// The candidate producer's version.
        candidate: String,
    },
    /// The producers captured with different settings, such as levels or
    /// capacities, so one may have recorded what the other did not.
    DifferentCapture,
    /// A side's basis is incomplete; see [`Comparison::baseline`] and
    /// [`Comparison::candidate`].
    Incomplete,
}

/// Which of a span's times a [`Comparison`] measures.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Metric {
    /// Self time: each span's time less the spans nested in it.
    ///
    /// Rows then never count the same time twice, so on each thread they add
    /// up to the time spent in spans, and a change shows up in the scope
    /// that did the work rather than in every scope around it.
    #[default]
    SelfTime,
    /// Each span's whole time, including the spans nested in it.
    ///
    /// An enclosing scope, such as a whole frame, then changes by the sum of
    /// everything inside it, and rows overlap.
    Total,
}

/// A comparison to run, from [`Comparison::between`]: set how spans count,
/// which time to measure, and how to group, then [`run`](Self::run) it.
#[derive(Clone, Debug)]
pub struct ComparisonQuery<'q> {
    baseline: &'q Population<'q>,
    candidate: &'q Population<'q>,
    selection: Selection,
    metric: Metric,
    group_by: Vec<GroupBy>,
}

impl ComparisonQuery<'_> {
    /// Sets how spans count toward each unit; the default is
    /// [`Selection::StartsWithin`]. Comparing frames, use
    /// [`Selection::Inside`] to count work where it ran.
    #[must_use]
    pub const fn selecting(mut self, selection: Selection) -> Self {
        self.selection = selection;
        self
    }

    /// Sets which time to measure; the default is [`Metric::SelfTime`].
    #[must_use]
    pub const fn measuring(mut self, metric: Metric) -> Self {
        self.metric = metric;
        self
    }

    /// Groups rows by `group`, after any grouping already set.
    #[must_use]
    pub fn group_by(mut self, group: GroupBy) -> Self {
        self.group_by.push(group);
        self
    }

    /// Measures both sides and compares them.
    #[must_use]
    pub fn run(&self) -> Comparison {
        Comparison::run(self)
    }
}

/// Span time and counts per unit for each group, on both sides, as
/// [`ComparisonQuery::run`] returns them.
///
/// Each row splits the change in time per unit into a *volume* part, from
/// running more or fewer spans, and a *cost* part, from each span taking
/// longer or shorter. "Three times as many visits, each as fast" calls for
/// a different fix than "the same visits, each three times slower".
#[derive(Clone, Debug, PartialEq)]
pub struct Comparison {
    group_by: Vec<GroupBy>,
    selection: Selection,
    metric: Metric,
    rows: Vec<ComparisonRow>,
    baseline: Side,
    candidate: Side,
    caveats: Vec<Caveat>,
}

/// One side of a [`Comparison`].
#[derive(Clone, Debug, PartialEq)]
pub struct Side {
    units: u64,
    basis: Basis,
}

impl Side {
    /// Returns how many units the side has.
    #[must_use]
    pub const fn units(&self) -> u64 {
        self.units
    }

    /// Returns what the side's spans cover.
    #[must_use]
    pub const fn basis(&self) -> &Basis {
        &self.basis
    }
}

/// One group's spans on one side of a comparison.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Measure {
    units: u64,
    count: u64,
    time: Duration,
    /// Mean and variance of the per-unit time, in microseconds.
    unit_mean: f64,
    unit_variance: Option<f64>,
}

impl Measure {
    fn of(per_unit: &[(u64, Duration)]) -> Self {
        let units = per_unit.len() as u64;
        let count = per_unit.iter().map(|(count, _)| count).sum();
        let time = per_unit.iter().map(|(_, time)| *time).sum();
        let values: Vec<f64> = per_unit.iter().map(|(_, time)| micros_f64(*time)).collect();
        let unit_mean = if values.is_empty() {
            0.0
        } else {
            values.iter().sum::<f64>() / values.len() as f64
        };
        let unit_variance = (values.len() >= 2).then(|| {
            values
                .iter()
                .map(|value| (value - unit_mean).powi(2))
                .sum::<f64>()
                / (values.len() - 1) as f64
        });
        Self {
            units,
            count,
            time,
            unit_mean,
            unit_variance,
        }
    }

    /// Returns how many spans there were in all.
    #[must_use]
    pub const fn count(&self) -> u64 {
        self.count
    }

    /// Returns their summed time, of the comparison's [`Metric`].
    #[must_use]
    pub const fn time(&self) -> Duration {
        self.time
    }

    /// Returns how many spans there were per unit.
    #[must_use]
    pub fn count_per_unit(&self) -> f64 {
        if self.units == 0 {
            0.0
        } else {
            self.count as f64 / self.units as f64
        }
    }

    /// Returns their time per unit, in microseconds.
    #[must_use]
    pub const fn micros_per_unit(&self) -> f64 {
        self.unit_mean
    }

    /// Returns the mean time per span, in microseconds, or `None` without
    /// spans.
    #[must_use]
    pub fn micros_per_span(&self) -> Option<f64> {
        (self.count > 0).then(|| micros_f64(self.time) / self.count as f64)
    }
}

/// One group of a [`Comparison`].
#[derive(Clone, Debug, PartialEq)]
pub struct ComparisonRow {
    key: Vec<KeyPart>,
    baseline: Measure,
    candidate: Measure,
}

impl ComparisonRow {
    /// Returns the row's key, from the candidate side when it has the group.
    #[must_use]
    pub fn key(&self) -> &[KeyPart] {
        &self.key
    }

    /// Returns the key as text, parts joined by ` / `.
    #[must_use]
    pub fn describe(&self) -> String {
        self.key
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" / ")
    }

    /// Returns the group's spans on the baseline side.
    #[must_use]
    pub const fn baseline(&self) -> &Measure {
        &self.baseline
    }

    /// Returns the group's spans on the candidate side.
    #[must_use]
    pub const fn candidate(&self) -> &Measure {
        &self.candidate
    }

    /// Returns how much more time per unit the candidate spent, in
    /// microseconds; negative when it spent less.
    #[must_use]
    pub fn change_micros(&self) -> f64 {
        self.candidate.unit_mean - self.baseline.unit_mean
    }

    /// Returns the part of [`change_micros`](Self::change_micros) from
    /// running more or fewer spans per unit, at the baseline's time per
    /// span.
    ///
    /// The volume and cost parts add up to the whole change. Work the
    /// baseline did not run at all is all volume.
    #[must_use]
    pub fn volume_micros(&self) -> f64 {
        match self.baseline.micros_per_span() {
            Some(per_span) => {
                (self.candidate.count_per_unit() - self.baseline.count_per_unit()) * per_span
            }
            None => self.change_micros(),
        }
    }

    /// Returns the part of [`change_micros`](Self::change_micros) from each
    /// span taking longer or shorter, at the candidate's spans per unit.
    #[must_use]
    pub fn cost_micros(&self) -> f64 {
        self.change_micros() - self.volume_micros()
    }

    /// Returns the standard error of [`change_micros`](Self::change_micros),
    /// from how much the time per unit varies on each side, or `None` when
    /// a side has fewer than two units.
    ///
    /// A change within about two standard errors could be noise; this
    /// assumes units are independent, which consecutive frames may not be.
    #[must_use]
    pub fn standard_error_micros(&self) -> Option<f64> {
        let baseline = self.baseline.unit_variance? / self.baseline.units as f64;
        let candidate = self.candidate.unit_variance? / self.candidate.units as f64;
        Some((baseline + candidate).sqrt())
    }
}

impl Comparison {
    /// Starts comparing the spans of `baseline` and `candidate`, such as slow
    /// frames against the rest, or one run's frames against another's.
    ///
    /// Rows are matched by key; across runtime instances, by what keys mean
    /// rather than ids, as [`Caveat::DifferentInstances`] explains.
    #[must_use]
    pub fn between<'q>(
        baseline: &'q Population<'q>,
        candidate: &'q Population<'q>,
    ) -> ComparisonQuery<'q> {
        ComparisonQuery {
            baseline,
            candidate,
            selection: Selection::StartsWithin,
            metric: Metric::SelfTime,
            group_by: Vec::new(),
        }
    }

    fn run(query: &ComparisonQuery<'_>) -> Self {
        let (baseline, candidate) = (query.baseline, query.candidate);
        let (selection, metric, group_by) = (query.selection, query.metric, &query.group_by);
        let same_instance = baseline.trace.instance_id() == candidate.trace.instance_id();
        let match_key = |key: &[KeyPart]| -> Vec<String> {
            key.iter()
                .map(|part| {
                    if same_instance {
                        format!("{part:?}")
                    } else {
                        part.semantic()
                    }
                })
                .collect()
        };

        let mut groups = BTreeMap::<
            Vec<String>,
            (Vec<KeyPart>, Vec<(u64, Duration)>, Vec<(u64, Duration)>),
        >::new();
        let mut tally = |population: &Population<'_>, candidate_side: bool| {
            let units = population.units.len();
            for (unit_index, unit) in population.units.iter().enumerate() {
                let number = population.numbers.get(unit_index).copied().flatten();
                let mut query = population
                    .trace
                    .spans()
                    .in_frame_range(*unit, number)
                    .selecting(selection);
                for group in group_by {
                    query = query.group_by(group.clone());
                }
                for row in query.table().rows() {
                    let entry = groups
                        .entry(match_key(row.key()))
                        .or_insert_with(|| (row.key().to_vec(), Vec::new(), Vec::new()));
                    if candidate_side {
                        entry.0 = row.key().to_vec();
                    }
                    let side = if candidate_side {
                        &mut entry.2
                    } else {
                        &mut entry.1
                    };
                    side.resize(units, (0, Duration::ZERO));
                    let time = match metric {
                        Metric::SelfTime => row.stats().self_time(),
                        Metric::Total => row.stats().total(),
                    };
                    side[unit_index] = (row.stats().count(), time);
                }
            }
        };
        tally(baseline, false);
        tally(candidate, true);

        let mut rows: Vec<_> = groups
            .into_values()
            .map(|(key, mut base, mut cand)| {
                base.resize(baseline.units.len(), (0, Duration::ZERO));
                cand.resize(candidate.units.len(), (0, Duration::ZERO));
                ComparisonRow {
                    key,
                    baseline: Measure::of(&base),
                    candidate: Measure::of(&cand),
                }
            })
            .collect();
        rows.sort_by(|a, b| {
            b.change_micros()
                .abs()
                .total_cmp(&a.change_micros().abs())
                .then_with(|| a.key.cmp(&b.key))
        });

        let side = |population: &Population<'_>| Side {
            units: population.units.len() as u64,
            basis: population
                .units
                .iter()
                .enumerate()
                .fold(
                    population.trace.spans().selecting(selection),
                    |query, (index, unit)| {
                        query
                            .in_frame_range(*unit, population.numbers.get(index).copied().flatten())
                    },
                )
                .basis(),
        };
        let baseline_side = side(baseline);
        let candidate_side = side(candidate);

        let mut caveats = Vec::new();
        if baseline_side.units < 2 || candidate_side.units < 2 {
            caveats.push(Caveat::FewUnits);
        }
        if !same_instance {
            caveats.push(Caveat::DifferentInstances);
        }
        if let (Some(a), Some(b)) = (baseline.trace.metadata(), candidate.trace.metadata()) {
            if a.producer_version() != b.producer_version() {
                caveats.push(Caveat::DifferentProducers {
                    baseline: a.producer_version().to_owned(),
                    candidate: b.producer_version().to_owned(),
                });
            }
            if a.capture() != b.capture() {
                caveats.push(Caveat::DifferentCapture);
            }
        }
        if !baseline_side.basis.is_complete() || !candidate_side.basis.is_complete() {
            caveats.push(Caveat::Incomplete);
        }

        Self {
            group_by: group_by.clone(),
            selection,
            metric,
            rows,
            baseline: baseline_side,
            candidate: candidate_side,
            caveats,
        }
    }

    /// Returns how rows are grouped.
    #[must_use]
    pub fn group_by(&self) -> &[GroupBy] {
        &self.group_by
    }

    /// Returns how spans counted toward units.
    #[must_use]
    pub const fn selection(&self) -> Selection {
        self.selection
    }

    /// Returns which time was measured.
    #[must_use]
    pub const fn metric(&self) -> Metric {
        self.metric
    }

    /// Returns one row per group on either side, most changed first.
    #[must_use]
    pub fn rows(&self) -> &[ComparisonRow] {
        &self.rows
    }

    /// Returns the row whose key reads `key`, as
    /// [`ComparisonRow::describe`] writes it.
    #[must_use]
    pub fn row(&self, key: &str) -> Option<&ComparisonRow> {
        self.rows.iter().find(|row| row.describe() == key)
    }

    /// Returns the baseline side.
    #[must_use]
    pub const fn baseline(&self) -> &Side {
        &self.baseline
    }

    /// Returns the candidate side.
    #[must_use]
    pub const fn candidate(&self) -> &Side {
        &self.candidate
    }

    /// Returns why the comparison should be read with care, if at all.
    #[must_use]
    pub fn caveats(&self) -> &[Caveat] {
        &self.caveats
    }
}

/// Converts to microseconds without the rounding a detour through seconds
/// adds.
fn micros_f64(duration: Duration) -> f64 {
    duration.as_nanos() as f64 / 1000.0
}
