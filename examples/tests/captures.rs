// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Captures recorded by real programs, read back by today's Footfall.
//!
//! These guard the capture-format promise: a capture a released Footfall
//! wrote stays readable by later releases, and still means the same thing.
//! Each test reads one capture in `captures/` and checks both that it
//! decodes and what it says: frames, pacing, scopes, and counters. A
//! failure here means a change would misread captures people already have.
//! Fix the change, or, when the analysis deliberately changed what it
//! concludes, update the figures and say so in the changelog. Never
//! re-record a capture to make a test pass; see `captures/README.md`.

use core::time::Duration;
use std::fs::File;
use std::path::PathBuf;

use footfall::analysis::{GroupBy, Pacing, Trace};
use footfall::export::to_chrome_trace_json;
use footfall::session::capture::CaptureReader;
use footfall::session::{OwnedSnapshot, ProtocolVersion};

fn path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("captures")
        .join(name)
}

fn reader(name: &str) -> CaptureReader<File> {
    CaptureReader::new(File::open(path(name)).expect("the capture is checked in"))
        .expect("the header reads")
}

/// Reads `name` whole, checking it is complete, was written at protocol
/// 1.0, and lost nothing; returns it as one snapshot and as a trace.
fn read(name: &str) -> (OwnedSnapshot, Trace) {
    assert_eq!(
        reader(name).protocol_version(),
        ProtocolVersion::new(1, 0),
        "{name} was written at protocol 1.0"
    );
    let joined = reader(name).join().expect("the capture joins");
    assert!(joined.complete(), "{name} reads whole");
    let captured = Trace::from_capture(reader(name)).expect("the capture reads as a trace");
    assert!(captured.complete(), "{name} reads whole as a trace");
    let snapshot = joined.into_snapshot();
    assert_eq!(snapshot.window().missed(), 0, "{name} lost nothing");
    assert!(
        !to_chrome_trace_json(&snapshot).is_empty(),
        "{name} exports"
    );
    (snapshot, captured.into_trace())
}

/// The most-run scope on CPU threads, by total time, with how many times it
/// ran.
fn top_scope(trace: &Trace) -> (String, u64) {
    let table = trace.spans().on_threads().group_by(GroupBy::Name).table();
    let row = table.rows().first().expect("scopes ran");
    (row.describe(), row.stats().count())
}

/// How many frames had each outcome: on time, early, late, dropped,
/// submitted late, skipped, unknown.
fn outcomes(pacing: &Pacing) -> [u64; 7] {
    let counts = pacing.counts();
    [
        counts.on_time,
        counts.early,
        counts.late,
        counts.dropped,
        counts.submitted_late,
        counts.skipped,
        counts.unknown,
    ]
}

/// `micros` to the nearest microsecond, for durations measured on a real
/// clock.
fn about(duration: Duration) -> u128 {
    (duration.as_nanos() + 500) / 1000
}

#[test]
fn the_demo_capture_reads_as_it_was_recorded() {
    let (snapshot, trace) = read("demo.ffcap");
    assert_eq!(snapshot.metadata().process_name(), "record_demo");
    assert_eq!(snapshot.metadata().producer_version(), "0.1.0");
    assert_eq!(snapshot.events().len(), 17_498);
    assert_eq!(
        reader("demo.ffcap").count(),
        5,
        "a write every 60 frames, and the last as it finished"
    );

    assert_eq!(trace.frames().len(), 240);
    assert_eq!(top_scope(&trace), (String::from("raster_tile"), 3_840));
    let draw_calls: u64 = snapshot
        .counters()
        .iter()
        .filter(|counter| counter.name() == "draw_calls")
        .map(|counter| counter.value())
        .sum();
    assert_eq!(draw_calls, 16_553);

    let pacing = trace.pacing(Duration::from_millis(1));
    assert_eq!(outcomes(&pacing), [237, 0, 0, 0, 1, 2, 0]);
    let deadlines = pacing.deadlines();
    assert_eq!((deadlines.met, deadlines.missed), (239, 1));
    assert_eq!(about(pacing.intervals().median()), 16_667);
    assert_eq!(pacing.stutters(), 3);
    assert_eq!(pacing.unplaced_points(), 0);
}

#[test]
fn the_lightweald_metal_capture_reads_as_it_was_recorded() {
    let (snapshot, trace) = read("lightweald-metal.ffcap");
    assert_eq!(snapshot.metadata().process_name(), "lighting_showcase");
    assert_eq!(snapshot.events().len(), 5_267);
    assert_eq!(reader("lightweald-metal.ffcap").count(), 2);

    assert_eq!(trace.frames().len(), 331);
    let gpu_frames: u64 = trace
        .timelines()
        .filter_map(|timeline| timeline.timeline_id())
        .map(|id| {
            let table = trace
                .spans()
                .on_timeline(id)
                .group_by(GroupBy::Name)
                .table();
            table
                .rows()
                .iter()
                .map(|row| row.stats().count())
                .sum::<u64>()
        })
        .sum();
    assert_eq!(gpu_frames, 326, "one GPU span a submitted frame");
    let submission_work: u64 = snapshot
        .histograms()
        .iter()
        .filter(|histogram| histogram.name() == "frameclock_submission_work")
        .map(|histogram| histogram.count())
        .sum();
    assert_eq!(submission_work, 326);

    let pacing = trace.pacing(Duration::from_millis(1));
    assert_eq!(outcomes(&pacing), [287, 10, 29, 1, 0, 0, 4]);
    let counts = pacing.counts();
    let causes = counts.late_causes;
    assert_eq!(
        (
            causes.missed_deadline,
            causes.backlog,
            causes.gpu,
            causes.display,
            causes.after_submit,
            causes.unknown
        ),
        (6, 18, 1, 4, 0, 0)
    );
    let support = counts.late_support;
    assert_eq!(
        (
            support.observed,
            support.observed_unstated,
            support.inferred,
            support.unresolved
        ),
        (7, 0, 18, 4)
    );
    let deadlines = pacing.deadlines();
    assert_eq!((deadlines.met, deadlines.missed), (317, 10));
    let basis = pacing.presentation_basis();
    assert_eq!(basis.estimated + basis.unknown, 0, "the display said");
    assert_eq!(about(pacing.intervals().median()), 8_333);
    assert_eq!(pacing.stutters(), 6);
    assert_eq!(pacing.unplaced_points(), 0);
}
