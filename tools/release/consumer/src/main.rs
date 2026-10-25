// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! An external consumer of the packaged instrumentation and application APIs.

#[cfg(not(feature = "full"))]
fn main() {
    let plan = footfall::FramePlan::new(1, footfall::TickPeriod::NANOSECOND).with_depth(2);
    assert_eq!(plan.depth(), Some(2), "the bare facade exposes frame plans");
    footfall::frame_plan(&plan);
    println!("bare facade passed");
}

#[cfg(feature = "full")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use footfall::analysis::{LateCause, Outcome, Trace};
    use footfall::runtime::Runtime;
    use footfall::session::capture::{CaptureReader, CaptureWriter};
    use footfall::{
        Backend as _, ClockDesc, ClockReading, ContextHandle, FramePlan, FramePoint,
        FramePointKind, ScopeDesc, TickPeriod, TimingBasis,
    };
    use std::time::Duration;

    static HOST: ClockDesc = ClockDesc::new("release consumer host");
    static WORK: ScopeDesc = ScopeDesc::new("release consumer work");
    let runtime = Runtime::builder()
        .process_name("release consumer")
        .install_global()?;
    runtime.clock_sync(ClockReading::new(&HOST, 0, TickPeriod::NANOSECOND));
    footfall::frame_begin_numbered(1, ContextHandle::ROOT);
    let work = footfall::scope(&WORK, ContextHandle::ROOT);
    footfall::frame_plan(
        &FramePlan::new(1, TickPeriod::NANOSECOND)
            .with_interval(10_000_000)
            .with_depth(2),
    );
    for (kind, at) in [
        (FramePointKind::BufferRequested, 11_000_000),
        (FramePointKind::BufferAcquired, 16_000_000),
        (FramePointKind::Deadline, 23_000_000),
        (FramePointKind::Submitted, 25_000_000),
        (FramePointKind::Target, 30_000_000),
        (FramePointKind::Presented, 40_000_000),
    ] {
        footfall::frame_point(
            &FramePoint::at(1, kind, &HOST, TickPeriod::NANOSECOND, at)
                .with_basis(TimingBasis::Observed),
        );
    }
    work.end();
    footfall::frame_end_numbered(1, ContextHandle::ROOT);
    let snapshot = runtime.snapshot();
    assert_eq!(
        snapshot.metadata().producer_version(),
        "0.2.0",
        "matching runtime"
    );
    let mut writer = CaptureWriter::new(Vec::new())?;
    writer.write_snapshot(&snapshot)?;
    let bytes = writer.finish()?;
    let captured = Trace::from_capture(CaptureReader::new(bytes.as_slice())?)?;
    assert!(captured.complete(), "the round-trip is complete");
    let trace = captured.into_trace();
    let pacing = trace.pacing(Duration::ZERO);
    let frame = pacing.frame(1).expect("recorded frame");
    assert_eq!(
        frame.plan().and_then(|plan| plan.depth()),
        Some(2),
        "plan survived"
    );
    assert_eq!(
        frame.buffer_wait(),
        Some(Duration::from_millis(5)),
        "wait survived"
    );
    assert!(
        matches!(
            frame.outcome(),
            Outcome::Late {
                cause: LateCause::WaitedForBuffer,
                ..
            }
        ),
        "queue attribution survived"
    );
    let owned = snapshot.into();
    let perfetto = footfall::export::to_perfetto_trace(&owned);
    assert_eq!(
        perfetto.first(),
        Some(&0x0a),
        "Perfetto starts with a packet"
    );
    assert!(
        footfall::export::to_chrome_trace_json(&owned).contains("traceEvents"),
        "JSON export"
    );
    std::fs::write(std::env::args().nth(1).expect("capture output path"), bytes)?;
    println!("recording, plans, buffer waits, capture round-trip, analysis, and exports passed");
    Ok(())
}
