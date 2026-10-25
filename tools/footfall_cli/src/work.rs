// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Selection of explicit marked or scoped work, shared by capture commands.

use footfall::analysis::{
    Frame, GroupBy, OwnedLabelValue, Population, Selection, SpanQuery, Trace,
};
use footfall::{ContextId, MarkerKind};
use std::time::Duration;

#[derive(Clone, Copy, Debug, Default, clap::ValueEnum)]
pub(crate) enum Kind {
    #[default]
    Frame,
    Transaction,
    Pass,
}

impl Kind {
    pub(crate) const fn marker(self) -> MarkerKind {
        match self {
            Self::Frame => MarkerKind::Frame,
            Self::Transaction => MarkerKind::Transaction,
            Self::Pass => MarkerKind::Pass,
        }
    }

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Frame => "frame",
            Self::Transaction => "transaction",
            Self::Pass => "pass",
        }
    }
}

/// A typed equality predicate. Strings can be forced with `s:`, signed
/// integers with `i:`, unsigned integers with `u:`, and booleans with `b:`.
#[derive(Clone, Debug)]
pub(crate) struct Term {
    pub(crate) key: String,
    pub(crate) value: OwnedLabelValue,
}

impl std::str::FromStr for Term {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let (key, value) = text.split_once('=').ok_or("expected KEY=VALUE")?;
        if key.is_empty() {
            return Err(String::from("the key must not be empty"));
        }
        let value = if let Some(value) = value.strip_prefix("s:") {
            OwnedLabelValue::from(value)
        } else if let Some(value) = value.strip_prefix("u:") {
            OwnedLabelValue::Unsigned(
                value
                    .parse()
                    .map_err(|_| "expected an unsigned integer after u:")?,
            )
        } else if let Some(value) = value.strip_prefix("i:") {
            OwnedLabelValue::Signed(
                value
                    .parse()
                    .map_err(|_| "expected a signed integer after i:")?,
            )
        } else if let Some(value) = value.strip_prefix("b:") {
            OwnedLabelValue::Bool(
                value
                    .parse()
                    .map_err(|_| "expected true or false after b:")?,
            )
        } else if let Ok(value) = value.parse::<bool>() {
            OwnedLabelValue::Bool(value)
        } else if let Ok(value) = value.parse::<u64>() {
            OwnedLabelValue::Unsigned(value)
        } else if let Ok(value) = value.parse::<i64>() {
            OwnedLabelValue::Signed(value)
        } else {
            OwnedLabelValue::from(value)
        };
        Ok(Self {
            key: key.to_owned(),
            value,
        })
    }
}

#[derive(Clone, Debug, Default, clap::Args)]
pub(crate) struct Args {
    /// Marked work to analyze. Scope selection is a separate alternative.
    #[arg(long, value_enum, default_value = "frame", conflicts_with = "scope")]
    pub(crate) kind: Kind,
    /// Use each completed scope named NAME as one work unit, instead of markers.
    #[arg(long, value_name = "NAME")]
    pub(crate) scope: Option<String>,
    /// Select units in this runtime-local context id.
    #[arg(long, value_name = "ID")]
    pub(crate) context: Option<u64>,
    /// Select units by context label KEY=VALUE; repeat to require every label.
    #[arg(long, value_name = "KEY=VALUE")]
    pub(crate) label: Vec<Term>,
    /// Select scopes by argument KEY=VALUE; requires --scope. Values are typed;
    /// use s:TEXT to force a string that looks numeric or boolean.
    #[arg(long, value_name = "KEY=VALUE", requires = "scope")]
    pub(crate) arg: Vec<Term>,
}

impl Args {
    pub(crate) fn name(&self) -> &str {
        if self.scope.is_some() {
            "scope"
        } else {
            self.kind.name()
        }
    }

    pub(crate) fn is_default(&self) -> bool {
        matches!(self.kind, Kind::Frame)
            && self.scope.is_none()
            && self.context.is_none()
            && self.label.is_empty()
            && self.arg.is_empty()
    }

    fn matches_context(&self, trace: &Trace, id: ContextId) -> bool {
        self.context
            .is_none_or(|wanted| ContextId::new(wanted) == id)
            && self
                .label
                .iter()
                .all(|term| trace.label(id, &term.key) == Some(&term.value))
    }

    pub(crate) fn select<'t>(&self, trace: &'t Trace) -> crate::Result<Units<'t>> {
        if !self.arg.is_empty() && self.scope.is_none() {
            return Err("--arg requires --scope; markers do not carry arguments".into());
        }
        Ok(match &self.scope {
            Some(name) => {
                let mut query = trace.spans().named(name);
                for term in &self.arg {
                    query = query.with_arg(&term.key, term.value.clone());
                }
                let units = query
                    .iter()
                    .into_iter()
                    .filter_map(|selected| {
                        let span = selected.span();
                        self.matches_context(trace, span.context_id())
                            .then_some(Unit {
                                id: span.id().sequence(),
                                range: span.range(),
                                context: span.context_id(),
                                thread: span.thread_id(),
                                frame: None,
                            })
                    })
                    .collect();
                Units { units }
            }
            None => Units {
                units: trace
                    .marks(self.kind.marker())
                    .iter()
                    .filter(|frame| self.matches_context(trace, frame.context_id()))
                    .map(|frame| Unit {
                        id: frame.id().sequence(),
                        range: frame.range(),
                        context: frame.context_id(),
                        thread: frame.thread_id(),
                        frame: Some(frame),
                    })
                    .collect(),
            },
        })
    }

    /// Non-frame work has no display budget. Require an explicit budget
    /// for slow-work comparisons rather than silently assuming 60 Hz.
    pub(crate) fn budget(
        &self,
        trace: &Trace,
        ms: Option<f64>,
    ) -> crate::Result<crate::budget::Budget> {
        if ms.is_none() && (self.scope.is_some() || !matches!(self.kind, Kind::Frame)) {
            return Err(
                "--budget-ms is required for non-frame work; it has no display deadline".into(),
            );
        }
        if ms.is_some_and(|ms| !ms.is_finite() || ms < 0.0) {
            return Err("--budget-ms must be finite and nonnegative".into());
        }
        Ok(crate::budget::Budget::for_trace(trace, ms))
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Unit<'t> {
    pub(crate) id: u64,
    pub(crate) range: footfall::analysis::TimeRange,
    pub(crate) context: ContextId,
    pub(crate) thread: u64,
    pub(crate) frame: Option<&'t Frame>,
}

impl Unit<'_> {
    pub(crate) fn duration(self) -> Duration {
        self.range.end().saturating_sub(self.range.start())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Units<'t> {
    pub(crate) units: Vec<Unit<'t>>,
}

impl<'t> Units<'t> {
    pub(crate) fn population(&self, trace: &'t Trace) -> Population<'t> {
        if self.units.iter().all(|unit| unit.frame.is_some()) {
            Population::frames(trace, self.units.iter().filter_map(|unit| unit.frame))
        } else {
            Population::ranges(trace, self.units.iter().map(|unit| unit.range))
        }
    }

    pub(crate) fn query(&self, trace: &'t Trace) -> SpanQuery<'t> {
        let frames: Vec<_> = self
            .units
            .iter()
            .filter_map(|unit| unit.frame.map(Frame::id))
            .collect();
        let mut query = trace
            .spans()
            .in_frames(&frames)
            .selecting(Selection::Inside);
        for unit in self.units.iter().filter(|unit| unit.frame.is_none()) {
            query = query.within(unit.range);
        }
        query.group_by(GroupBy::Name)
    }

    pub(crate) fn split(&self, budget: &crate::budget::Budget) -> (Self, Self, usize) {
        let (mut slow, mut normal) = (Vec::new(), Vec::new());
        let mut unknown = 0;
        for unit in &self.units {
            let judged = match (unit.frame, budget) {
                (_, crate::budget::Budget::Fixed { limit, .. }) => Some(unit.duration() > *limit),
                (Some(frame), _) => budget.is_slow(frame),
                _ => None,
            };
            match judged {
                Some(true) => slow.push(*unit),
                Some(false) => normal.push(*unit),
                None => unknown += 1,
            }
        }
        (Self { units: normal }, Self { units: slow }, unknown)
    }
}

pub(crate) fn summary(
    path: &std::path::Path,
    args: &Args,
    budget_ms: Option<f64>,
    json: bool,
) -> crate::Result<()> {
    let (snapshot, read) = crate::read_joined_saying(path)?;
    let trace = Trace::from_snapshot(&snapshot);
    let units = args.select(&trace)?;
    let table = units.query(&trace).table();
    let slow = budget_ms
        .map(|ms| args.budget(&trace, Some(ms)))
        .transpose()?
        .map(|budget| units.split(&budget).1.units.len() as u64);
    if json {
        anstream::println!(
            "{}",
            crate::work_json::document([
                ("capture", crate::work_json::capture(path, &trace, read)),
                ("selection", crate::work_json::selection(args, &units)),
                ("slow_units", crate::json::Json::from(slow)),
                ("scopes", crate::work_json::table(&table)),
            ])
        );
    } else {
        anstream::println!(
            "{} {} units · elapsed wall time",
            units.units.len(),
            args.name()
        );
        if let Some(slow) = slow {
            anstream::println!("  {slow} over the stated budget");
        }
        let stats =
            footfall::analysis::DurationStats::of(units.units.iter().map(|unit| unit.duration()));
        anstream::println!(
            "  median {} · max {}",
            crate::format::duration(stats.median()),
            crate::format::duration(stats.max())
        );
        crate::report::spans("Activity during selected intervals", &table, 12);
        crate::report::losses(&trace, read);
    }
    Ok(())
}

pub(crate) fn compare(
    before: &std::path::Path,
    after: &std::path::Path,
    args: &Args,
    json: bool,
    top: usize,
) -> crate::Result<()> {
    let (baseline, baseline_read) = crate::read_trace_saying(before)?;
    let (candidate, candidate_read) = crate::read_trace_saying(after)?;
    let a = args.select(&baseline)?;
    let b = args.select(&candidate)?;
    if a.units.is_empty() || b.units.is_empty() {
        return Err("comparison needs at least one selected work unit on each side".into());
    }
    let comparison = footfall::analysis::Comparison::between(
        &a.population(&baseline),
        &b.population(&candidate),
    )
    .selecting(Selection::Inside)
    .group_by(GroupBy::Name)
    .run();
    if json {
        anstream::println!(
            "{}",
            crate::work_json::document([
                (
                    "baseline",
                    crate::work_json::capture(before, &baseline, baseline_read)
                ),
                (
                    "candidate",
                    crate::work_json::capture(after, &candidate, candidate_read)
                ),
                ("baseline_selection", crate::work_json::selection(args, &a)),
                ("candidate_selection", crate::work_json::selection(args, &b)),
                ("comparison", crate::work_json::comparison(&comparison)),
            ])
        );
    } else {
        anstream::println!(
            "What changed per {} · {} before → {} after",
            args.name(),
            a.units.len(),
            b.units.len()
        );
        crate::report::comparison(&comparison, "before", "after", top);
        crate::report::losses(&baseline, baseline_read);
        crate::report::losses(&candidate, candidate_read);
    }
    Ok(())
}

pub(crate) fn slow(
    path: &std::path::Path,
    args: &Args,
    budget_ms: Option<f64>,
    top: usize,
    by_arg: Option<&str>,
    json: bool,
) -> crate::Result<()> {
    let (trace, read) = crate::read_trace_saying(path)?;
    let budget = args.budget(&trace, budget_ms)?;
    let units = args.select(&trace)?;
    let (normal, slow, unknown) = units.split(&budget);
    let normal_population = normal.population(&trace);
    let slow_population = slow.population(&trace);
    let mut query = footfall::analysis::Comparison::between(&normal_population, &slow_population)
        .selecting(Selection::Inside)
        .group_by(GroupBy::Name);
    if let Some(key) = by_arg {
        query = query.group_by(GroupBy::Arg(key.to_owned()));
    }
    let comparison = query.run();
    if json {
        anstream::println!(
            "{}",
            crate::work_json::document([
                ("capture", crate::work_json::capture(path, &trace, read)),
                ("selection", crate::work_json::selection(args, &units)),
                ("budget", crate::json::Json::from(budget.describe())),
                ("unjudged", crate::json::Json::from(unknown as u64)),
                ("normal", crate::work_json::selection(args, &normal)),
                ("slow", crate::work_json::selection(args, &slow)),
                ("comparison", crate::work_json::comparison(&comparison)),
            ])
        );
    } else {
        anstream::println!(
            "{} of {} {} units {} · {} unjudged",
            slow.units.len(),
            units.units.len(),
            args.name(),
            budget.describe(),
            unknown
        );
        crate::report::comparison(&comparison, "normal", "slow", top);
        crate::report::losses(&trace, read);
    }
    Ok(())
}

pub(crate) fn checks(
    path: &std::path::Path,
    args: &Args,
    json: bool,
    required: &[String],
) -> crate::Result<()> {
    use crate::check::{Item, Status};
    let (trace, read) = crate::read_trace_saying(path)?;
    let units = args.select(&trace)?;
    let table = units.query(&trace).table();
    let signal = if args.scope.is_some() {
        footfall::analysis::Signal::Spans
    } else {
        footfall::analysis::Signal::Marks
    };
    let recorded = trace.recorded(signal);
    let status = if !recorded.is_yes() {
        Status::NotRecorded(recorded.to_string())
    } else if units.units.is_empty() {
        Status::Missing
    } else {
        Status::Present
    };
    let mut items = vec![
        Item {
            id: "work",
            status,
            found: format!("{} completed {} units", units.units.len(), args.name()),
            have: Some(units.units.len() as u64),
            of: None,
            needed_for: "a population of completed units to summarize and compare",
            fix: "record matching markers or scopes and check the context, label, and argument filters",
            section: "minimal-setup",
        },
        Item {
            id: "spans",
            status: if !table.basis().recorded().is_yes() {
                Status::NotRecorded(table.basis().recorded().to_string())
            } else if table.basis().spans() == 0 {
                Status::Missing
            } else if !table.basis().is_complete() {
                Status::Partial
            } else {
                Status::Present
            },
            found: format!(
                "{} scoped observations in the selected intervals",
                table.basis().spans()
            ),
            have: Some(table.basis().spans()),
            of: None,
            needed_for: "where elapsed work occurred within each unit",
            fix: "record scopes around the work and retain their begin and end events",
            section: "minimal-setup",
        },
    ];
    items.push(crate::check::losses(&trace, read));
    let samples = trace.counter_series().count() as u64;
    let recorded = trace.recorded(footfall::analysis::Signal::CounterSamples);
    items.insert(2, Item {
        id: "counter_samples",
        status: if !recorded.is_yes() { Status::NotRecorded(recorded.to_string()) }
            else if samples == 0 && trace.counter_checkpoints().is_empty() { Status::Missing } else { Status::Present },
        found: format!("{samples} counter series · {} named checkpoints", trace.counter_checkpoints().len()),
        have: Some(samples), of: None,
        needed_for: "counter and gauge values at explicit work boundaries; interval activity is not exclusive attribution",
        fix: "record counter_checkpoint before and after work; enable RuntimeBuilder::counter_samples",
        section: "counter-checkpoints",
    });
    let unmet = crate::check::unmet(&items, required)?;
    if json {
        let crate::json::Json::Object(mut fields) = crate::check::document(path, &trace, &items)
        else {
            unreachable!("check documents are objects")
        };
        fields.push((
            "selection".to_owned(),
            crate::work_json::selection(args, &units),
        ));
        fields.push((
            "capture_read".to_owned(),
            crate::work_json::capture(path, &trace, read),
        ));
        anstream::println!("{}", crate::json::Json::Object(fields));
    } else {
        crate::print_check(path, &trace, &items);
    }
    if !unmet.is_empty() {
        return Err(format!(
            "required checks not met: {}",
            unmet
                .iter()
                .map(|item| item.id)
                .collect::<Vec<_>>()
                .join(", ")
        )
        .into());
    }
    Ok(())
}
