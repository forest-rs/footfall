// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Early-only captures and native requests must remain inspectable.

use footfall::runtime::Runtime;
use footfall::session::capture::CaptureWriter;
use footfall::{
    Backend as _, ClockDesc, ClockReading, FramePoint, FramePointKind, TickPeriod, TimingBasis,
};
use std::{fs::File, process::Command};

static HOST: ClockDesc = ClockDesc::new("host");

#[test]
fn early_forecasts_are_reported_even_without_any_late_frames() {
    let runtime = Runtime::default();
    runtime.clock_sync(ClockReading::new(&HOST, 0, TickPeriod::NANOSECOND));
    for (number, kind, ms) in [
        (1, FramePointKind::Target, 40),
        (1, FramePointKind::Presented, 30),
        (2, FramePointKind::Target, 30),
        (2, FramePointKind::Presented, 30),
        (2, FramePointKind::PresentRequested, 40),
        (2, FramePointKind::GpuReady, 25),
    ] {
        runtime.frame_point(
            &FramePoint::at(number, kind, &HOST, TickPeriod::NANOSECOND, ms * 1_000_000)
                .with_basis(TimingBasis::Observed),
        );
    }
    let path = std::env::temp_dir().join(format!(
        "footfall-pacing-evidence-{}.ffcap",
        std::process::id()
    ));
    let mut writer = CaptureWriter::new(File::create(&path).unwrap()).unwrap();
    writer.write_snapshot(&runtime.snapshot()).unwrap();
    writer.finish().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_footfall"))
        .args(["analyze", "pacing"])
        .arg(&path)
        .output()
        .unwrap();
    std::fs::remove_file(path).unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    for expected in [
        "Early forecasts",
        "What the early frames did differently",
        "1 of 1 frames shown before",
        "submitted",
        "GPU ready",
        "native request",
        "target",
        "shown",
    ] {
        assert!(text.contains(expected), "missing {expected}: {text}");
    }
    assert!(!text.contains("Worst misses"), "{text}");
}
