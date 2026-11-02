// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Requests, forecasts, readiness and busy time are independent evidence.

use super::*;
use footfall_analysis::{LateCause, Outcome, Support};
use footfall_core::{FramePointKind::*, TimingBasis};

#[test]
fn completion_without_execution_spans_explains_post_submit_lateness() {
    for (ready, basis, cause, support) in [
        (
            Some(35_000),
            TimingBasis::Observed,
            LateCause::Gpu,
            Support::Observed,
        ),
        (
            Some(35_000),
            TimingBasis::Estimated,
            LateCause::Gpu,
            Support::Inferred,
        ),
        (
            Some(22_000),
            TimingBasis::Observed,
            LateCause::Display,
            Support::Unresolved,
        ),
        (
            None,
            TimingBasis::Unknown,
            LateCause::AfterSubmit,
            Support::Unresolved,
        ),
    ] {
        let mut recording = Recording::new(99);
        recording.clock_sync(0, &HOST, 0);
        recording.numbered_frame(0, 0, 20_000);
        for (kind, at) in [
            (Submitted, 20_000),
            (Deadline, 25_000),
            (Target, 30_000),
            (Presented, 40_000),
        ] {
            recording.frame_point_with(
                40_000,
                0,
                kind,
                &HOST,
                Some(at),
                TimingBasis::Observed,
                Args::none(),
            );
        }
        if let Some(at) = ready {
            recording.frame_point_with(40_000, 0, GpuReady, &HOST, Some(at), basis, Args::none());
        }
        let trace = Trace::from_snapshot(&recording.snapshot(40_000));
        let pacing = trace.pacing(us(500));
        let frame = pacing.frame(0_u64).unwrap();
        assert!(matches!(frame.outcome(), Outcome::Late { .. }));
        assert_eq!(frame.why().unwrap().cause, cause);
        assert_eq!(frame.why().unwrap().support, support);
        assert_eq!(
            frame.gpu_busy(),
            None,
            "completion does not invent execution cost"
        );
        assert_eq!(frame.gpu_ready(), ready.map(us));
    }
}

#[test]
fn a_native_request_is_independent_of_forecast_accuracy() {
    let mut recording = Recording::new(99);
    recording.clock_sync(0, &HOST, 0);
    for (number, requested, target) in [(0, Some(40_000), 30_000), (1, None, 40_000)] {
        recording.numbered_frame(number, 0, 20_000);
        recording.frame_point(40_000, number, Target, &HOST, Some(target));
        recording.frame_point(40_000, number, Presented, &HOST, Some(30_000));
        if let Some(requested) = requested {
            recording.frame_point(40_000, number, PresentRequested, &HOST, Some(requested));
        }
    }
    let trace = Trace::from_snapshot(&recording.snapshot(40_000));
    let pacing = trace.pacing(us(500));
    let native = pacing.frame(0_u64).unwrap();
    assert_eq!(native.outcome(), Outcome::OnTime);
    assert_eq!(native.presented_before_request(), Some(us(10_000)));
    let forecast = pacing.frame(1_u64).unwrap();
    assert!(matches!(forecast.outcome(), Outcome::Early { .. }));
    assert_eq!(forecast.presented_before_request(), None);
}
