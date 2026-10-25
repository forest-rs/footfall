// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Measure decoding, trace construction, and repeated loaded queries independently.
//! Run under a process RSS tool; RSS includes the executable, buffers, and allocator.

use footfall_analysis::{
    EventFilter, FlowQuery, MessageQuery, Page, ResourceGrouping, Trace, WorkId,
};
use footfall_session::capture::CaptureReader;
use std::{error::Error, hint::black_box, time::Instant};

fn measure(name: &str, repetitions: usize, mut action: impl FnMut()) {
    let mut samples = Vec::new();
    for _ in 0..repetitions {
        let start = Instant::now();
        action();
        samples.push(start.elapsed().as_nanos());
    }
    samples.sort_unstable();
    println!(
        "{name}\t{}\t{}\t{}",
        samples[samples.len() / 2],
        samples[0],
        samples[samples.len() - 1]
    );
}
fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .ok_or("usage: inspect_capture FILE [REPETITIONS]")?;
    let repetitions: usize = args.next().as_deref().unwrap_or("30").parse()?;
    if repetitions == 0 {
        return Err("repetitions must be positive".into());
    }
    let bytes = std::fs::read(&path)?;
    let snapshots: Vec<_> = CaptureReader::new(bytes.as_slice())?.collect::<Result<_, _>>()?;
    let events: usize = snapshots
        .iter()
        .map(|snapshot| snapshot.events().len())
        .sum();
    let load = || {
        Trace::from_capture(CaptureReader::new(bytes.as_slice()).expect("reader")).expect("capture")
    };
    let captured = load();
    if !captured.complete() {
        return Err("benchmark requires a fully readable capture".into());
    }
    let trace = captured.trace();
    let spans = trace.spans().iter();
    let edits = trace
        .spans()
        .named("procedural.edit")
        .with_arg("kind", "rejected")
        .iter();
    let root = edits
        .first()
        .or_else(|| spans.first())
        .ok_or("capture needs scopes")?
        .span()
        .id();
    let range = trace.observed();
    println!(
        "capture\t{path}\nbytes\t{}\nwindows\t{}\nevents\t{events}\nspans\t{}\nmessages\t{}\nresources\t{}\ncheckpoints\t{}\ncomplete_history\t{}",
        bytes.len(),
        snapshots.len(),
        spans.len(),
        trace.messages().len(),
        trace.resources().len(),
        trace.counter_checkpoints().len(),
        trace.has_complete_history()
    );
    println!(
        "recording_history_complete\t{}",
        trace
            .query_evidence(footfall_analysis::Signal::Resources, None)
            .is_complete()
    );
    println!("metric\tmedian_ns\tmin_ns\tmax_ns");
    measure("decode", repetitions, || {
        black_box(
            CaptureReader::new(bytes.as_slice())
                .expect("reader")
                .collect::<Result<Vec<_>, _>>()
                .expect("decode"),
        );
    });
    measure("construct", repetitions, || {
        let mut trace = Trace::new();
        for snapshot in &snapshots {
            trace.push(snapshot).expect("push");
        }
        black_box(trace);
    });
    measure("decode_and_construct", repetitions, || {
        black_box(load());
    });
    measure("messages_loaded", repetitions, || {
        black_box(
            trace
                .select_messages(
                    &MessageQuery {
                        filter: EventFilter {
                            within: Some(root),
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                    Page::default(),
                )
                .expect("query"),
        );
    });
    measure("flows_loaded", repetitions, || {
        black_box(
            trace
                .select_flows(
                    &FlowQuery {
                        filter: EventFilter {
                            within: Some(root),
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                    Page::default(),
                )
                .expect("query"),
        );
    });
    measure("work_loaded", repetitions, || {
        black_box(trace.inspect_work(WorkId::Scope(root)).expect("work"));
    });
    measure("resources_loaded", repetitions, || {
        black_box(trace.resource_interval(range, Some(ResourceGrouping::Kind)));
    });
    Ok(())
}
