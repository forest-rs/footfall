// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Printed summaries of a snapshot: where time went, how frames did, what
//! the counters say, and what was lost.

use crate::format;
use crate::style::{BAD, DIM, GOOD, HEADING, NAME, WARN};
use anstream::println;
use footfall::analysis::{Frame, Spans};
use footfall::session::OwnedSnapshot;

/// Prints which scopes took the most time, by name.
pub fn spans(spans: &Spans, limit: usize) {
    let mut rows: Vec<_> = spans.by_name().into_iter().collect();
    rows.sort_by_key(|(_, stats)| core::cmp::Reverse(stats.total_micros()));
    println!("{HEADING}Where the time went{HEADING:#}");
    if rows.is_empty() {
        println!("  {DIM}no completed scopes{DIM:#}");
        return;
    }
    let width = rows.iter().map(|(name, _)| name.len()).max().unwrap_or(0);
    println!(
        "  {DIM}{:width$}  {:>7}  {:>10}  {:>10}  {:>10}  {:>10}{DIM:#}",
        "scope", "count", "total", "self", "mean", "max"
    );
    for (name, stats) in rows.iter().take(limit) {
        println!(
            "  {NAME}{name:width$}{NAME:#}  {:>7}  {:>10}  {:>10}  {:>10}  {:>10}",
            format::count(stats.count()),
            format::micros(stats.total_micros()),
            format::micros(stats.self_micros()),
            format::micros(stats.mean_micros()),
            format::micros(stats.max_micros()),
        );
    }
    if rows.len() > limit {
        println!("  {DIM}… and {} more{DIM:#}", rows.len() - limit);
    }
    if !spans.open().is_empty() || spans.unmatched_ends() > 0 {
        println!(
            "  {DIM}{} still open, {} ended before the window began{DIM:#}",
            spans.open().len(),
            spans.unmatched_ends()
        );
    }
}

/// Prints the time each scope took under each value of a context label, such
/// as each render pass: the attribution Footfall exists for.
pub fn spans_by_label(spans: &Spans, snapshot: &OwnedSnapshot, key: &str) {
    println!("{HEADING}By {key}{HEADING:#}");
    let groups = spans.by_label(snapshot, key);
    if groups.is_empty() {
        println!("  {DIM}no scopes ran under a `{key}` label{DIM:#}");
        return;
    }
    let rows: Vec<_> = groups
        .into_iter()
        .map(|(value, scopes)| {
            let total: u64 = scopes.values().map(|stats| stats.total_micros()).sum();
            let parts: Vec<_> = scopes
                .iter()
                .map(|(name, stats)| format!("{name} {}", format::micros(stats.total_micros())))
                .collect();
            (format!("{key}={value}"), total, parts.join(", "))
        })
        .collect();
    let width = rows
        .iter()
        .map(|(label, ..)| label.len())
        .max()
        .unwrap_or(0);
    for (label, total, parts) in rows {
        println!(
            "  {NAME}{label:width$}{NAME:#}  {:>10}  {DIM}{parts}{DIM:#}",
            format::micros(total)
        );
    }
}

/// Prints how long frames took against a budget, such as 16.7 ms for 60 Hz.
pub fn frames(frames: &[Frame], budget_micros: u64) {
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
pub fn counters(snapshot: &OwnedSnapshot, limit: usize) {
    println!("{HEADING}Counters{HEADING:#}");
    let mut totals = std::collections::BTreeMap::<&str, (footfall::Unit, u64)>::new();
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
pub fn losses(snapshot: &OwnedSnapshot) {
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
pub struct FrameStats {
    /// Frames counted.
    pub count: u64,
    /// Mean duration, in microseconds.
    pub mean: u64,
    /// 95th-percentile duration, in microseconds.
    pub p95: u64,
    /// Longest duration, in microseconds.
    pub max: u64,
    /// Frames longer than the budget.
    pub slow: u64,
}

impl FrameStats {
    /// Computes statistics over `frames`, or `None` when there are none.
    #[must_use]
    pub fn of(frames: &[Frame], budget_micros: u64) -> Option<Self> {
        Self::of_durations(frames.iter().map(Frame::duration_micros), budget_micros)
    }

    /// Computes statistics over frame durations in microseconds.
    #[must_use]
    pub fn of_durations(
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
