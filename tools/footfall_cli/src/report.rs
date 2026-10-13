// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Printed summaries of a snapshot: where time went, how frames did, what
//! the counters say, and what was lost.

use crate::format;
use crate::style::{BAD, DIM, GOOD, HEADING, NAME, WARN};
use anstream::println;
use footfall::analysis::{Basis, Caveat, Comparison, Frame, SpanTable};
use footfall::session::OwnedSnapshot;
use std::collections::BTreeMap;

/// Prints which scopes took the most time under `title`, from a table
/// grouped by name.
pub(crate) fn spans(title: &str, table: &SpanTable, limit: usize) {
    println!("{HEADING}{title}{HEADING:#}");
    let rows = table.rows();
    if rows.is_empty() {
        println!("  {DIM}no completed scopes{DIM:#}");
        basis(table.basis());
        return;
    }
    let names: Vec<_> = rows.iter().map(|row| row.describe()).collect();
    let width = names.iter().map(String::len).max().unwrap_or(0);
    println!(
        "  {DIM}{:width$}  {:>7}  {:>10}  {:>10}  {:>10}  {:>10}{DIM:#}",
        "scope", "count", "total", "self", "median", "max"
    );
    for (name, row) in names.iter().zip(rows).take(limit) {
        let stats = row.stats();
        println!(
            "  {NAME}{name:width$}{NAME:#}  {:>7}  {:>10}  {:>10}  {:>10}  {:>10}",
            format::count(stats.count()),
            format::duration(stats.total()),
            format::duration(stats.self_time()),
            format::duration(stats.time().median()),
            format::duration(stats.time().max()),
        );
    }
    if rows.len() > limit {
        println!("  {DIM}… and {} more{DIM:#}", rows.len() - limit);
    }
    basis(table.basis());
}

/// Prints what a table's figures leave out, when they leave anything out.
pub(crate) fn basis(basis: &Basis) {
    if !basis.recorded().is_yes() {
        println!("  {WARN}{}{WARN:#}", basis.recorded());
        return;
    }
    if basis.open() > 0 || basis.unmatched_ends() > 0 {
        println!(
            "  {DIM}left out: {} still open, {} ended without a begin{DIM:#}",
            basis.open(),
            basis.unmatched_ends()
        );
    }
    if basis.missed_events() > 0 {
        println!(
            "  {WARN}{} events missing in {} gap(s); nested times may be incomplete{WARN:#}",
            format::count(basis.missed_events()),
            basis.gaps().len()
        );
    }
}

/// Prints a comparison: each scope's time per unit on both sides, how much
/// of the change came from more calls and how much from slower calls, and
/// how much of it could be noise.
pub(crate) fn comparison(comparison: &Comparison, baseline: &str, candidate: &str, limit: usize) {
    let rows = comparison.rows();
    if rows.is_empty() {
        println!("  {DIM}no completed scopes on either side{DIM:#}");
        return;
    }
    let names: Vec<_> = rows.iter().take(limit).map(|row| row.describe()).collect();
    let width = names.iter().map(String::len).max().unwrap_or(0).max(5);
    println!(
        "  {DIM}{:width$}  {:>10}  {:>10}  {:>10}  {:>14}  {:>14}  {:>9}{DIM:#}",
        "scope", baseline, candidate, "change", "from calls", "from cost", "±"
    );
    for (name, row) in names.iter().zip(rows) {
        let calls = |measure: &footfall::analysis::Measure| measure.count_per_unit();
        let noise = row.standard_error_micros();
        let change = row.change_micros();
        // A change within two standard errors could be noise.
        let style = match noise {
            Some(noise) if change.abs() <= 2.0 * noise => DIM,
            _ if change > 0.0 => BAD,
            _ => GOOD,
        };
        println!(
            "  {NAME}{name:width$}{NAME:#}  {:>10}  {:>10}  {style}{:>10}{style:#}  {:>14}  {:>14}  {:>9}",
            format::change_micros(row.baseline().micros_per_unit()).trim_start_matches('+'),
            format::change_micros(row.candidate().micros_per_unit()).trim_start_matches('+'),
            format::change_micros(change),
            format!(
                "{} ({:.1}→{:.1})",
                format::change_micros(row.volume_micros()),
                calls(row.baseline()),
                calls(row.candidate())
            ),
            format::change_micros(row.cost_micros()),
            noise.map_or_else(
                || String::from("?"),
                |noise| format::change_micros(noise)
                    .trim_start_matches('+')
                    .to_owned()
            ),
        );
    }
    println!(
        "  {DIM}per unit: {} {baseline}, {} {candidate}; spans {} each unit{DIM:#}",
        comparison.baseline().units(),
        comparison.candidate().units(),
        comparison.selection().describe(),
    );
    for caveat in comparison.caveats() {
        let note = match caveat {
            Caveat::FewUnits => String::from("too few units on a side to estimate noise"),
            Caveat::DifferentInstances => {
                String::from("different runs: scopes matched by name, not id")
            }
            Caveat::DifferentProducers {
                baseline,
                candidate,
            } => {
                format!("different producer versions: {baseline} and {candidate}")
            }
            Caveat::DifferentCapture => String::from("captured with different settings"),
            Caveat::Incomplete => String::from("a side is incomplete; see below"),
            other => format!("{other:?}"),
        };
        println!("  {WARN}caution:{WARN:#} {note}");
    }
    for (label, side) in [
        (baseline, comparison.baseline()),
        (candidate, comparison.candidate()),
    ] {
        if !side.basis().is_complete() {
            println!("  {DIM}{label}:{DIM:#}");
            basis(side.basis());
        }
    }
}

/// Prints how long frames took against a budget, such as 16.7 ms for 60 Hz.
pub(crate) fn frames(frames: &[Frame], budget_micros: u64) {
    println!("{HEADING}Frames{HEADING:#}");
    let Some(stats) = FrameStats::of(frames, budget_micros) else {
        println!("  {DIM}no complete frames{DIM:#}");
        return;
    };
    let slow_style = if stats.slow == 0 { GOOD } else { BAD };
    println!(
        "  {} frames · mean {} · p95 {} · max {} · {slow_style}{} over {}{slow_style:#}",
        format::count(stats.count),
        format::micros(stats.mean),
        format::micros(stats.p95),
        format::micros(stats.max),
        stats.slow,
        format::micros(budget_micros),
    );
}

/// Prints each counter's current value, largest first within each unit.
pub(crate) fn counters(snapshot: &OwnedSnapshot, limit: usize) {
    println!("{HEADING}Counters{HEADING:#}");
    let mut totals = BTreeMap::<&str, (footfall::Unit, u64)>::new();
    for counter in snapshot.counters() {
        let entry = totals.entry(counter.name()).or_insert((counter.unit(), 0));
        entry.1 = entry.1.saturating_add(counter.value());
    }
    if totals.is_empty() {
        println!("  {DIM}no counters{DIM:#}");
        return;
    }
    let width = totals.keys().map(|name| name.len()).max().unwrap_or(0);
    for (name, (unit, value)) in totals.iter().take(limit) {
        println!(
            "  {NAME}{name:width$}{NAME:#}  {:>12}",
            format::value(*unit, *value)
        );
    }
}

/// Prints what the snapshot could not hold, or that nothing was lost.
pub(crate) fn losses(snapshot: &OwnedSnapshot) {
    let drops = snapshot.drops();
    let missed = snapshot.window().missed();
    if drops.is_empty() && missed == 0 {
        println!("{GOOD}Nothing was lost.{GOOD:#}");
        return;
    }
    println!(
        "{WARN}Lost:{WARN:#} {} events evicted before this read, {} evicted in total, \
         {} counter updates and {} contexts refused",
        format::count(missed),
        format::count(drops.evicted_events()),
        format::count(drops.counter_updates()),
        format::count(drops.refused_contexts()),
    );
}

/// Frame-time statistics over a set of frames.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FrameStats {
    /// Frames counted.
    pub(crate) count: u64,
    /// Mean duration, in microseconds.
    pub(crate) mean: u64,
    /// 95th-percentile duration, in microseconds.
    pub(crate) p95: u64,
    /// Longest duration, in microseconds.
    pub(crate) max: u64,
    /// Frames longer than the budget.
    pub(crate) slow: u64,
}

impl FrameStats {
    /// Computes statistics over `frames`, or `None` when there are none.
    #[must_use]
    pub(crate) fn of(frames: &[Frame], budget_micros: u64) -> Option<Self> {
        Self::of_durations(
            frames
                .iter()
                .map(|frame| format::as_micros(frame.duration())),
            budget_micros,
        )
    }

    /// Computes statistics over frame durations in microseconds.
    #[must_use]
    pub(crate) fn of_durations(
        durations: impl IntoIterator<Item = u64>,
        budget_micros: u64,
    ) -> Option<Self> {
        let mut durations: Vec<u64> = durations.into_iter().collect();
        if durations.is_empty() {
            return None;
        }
        durations.sort_unstable();
        let count = durations.len() as u64;
        let total: u64 = durations.iter().sum();
        let p95_index = (durations.len() * 95).div_ceil(100).saturating_sub(1);
        Some(Self {
            count,
            mean: total / count,
            p95: durations[p95_index],
            max: durations[durations.len() - 1],
            slow: durations
                .iter()
                .filter(|&&duration| duration > budget_micros)
                .count() as u64,
        })
    }
}
