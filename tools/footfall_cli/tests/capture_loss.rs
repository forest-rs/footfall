// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Retaining streamed history is independent of runtime eviction.

use footfall::runtime::{
    Drops, EventWindow, Observation, Snapshot, SnapshotMetadata, TimedObservation,
};
use footfall::session::capture::CaptureWriter;
use footfall::{ContextId, MarkerKind, MarkerPhase};
use std::path::PathBuf;
use std::process::Command;

struct Capture(PathBuf);

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn snapshot(since: u64, first: u64, evicted: u64) -> Snapshot {
    let mut builder = Snapshot::builder(SnapshotMetadata::new("streamed").with_instance_id(42))
        .window(EventWindow::new(since, first + 2, first - since))
        .captured_at_nanos((first + 2) * 1_000)
        .drops(Drops::new().with_evicted_events(evicted));
    for (sequence, phase) in [(first, MarkerPhase::Begin), (first + 1, MarkerPhase::End)] {
        builder = builder.event(TimedObservation::new(
            sequence * 1_000,
            sequence,
            1,
            Observation::FrameMarker {
                kind: MarkerKind::Transaction,
                phase,
                number: None,
                context_id: ContextId::new(0),
            },
        ));
    }
    builder.build()
}

#[test]
fn streamed_eviction_is_not_capture_loss_but_missing_events_and_truncation_are() {
    for (missing, truncated) in [(false, false), (true, false), (false, true)] {
        let mut writer = CaptureWriter::new(Vec::new()).expect("header");
        writer.write_snapshot(&snapshot(0, 0, 0)).expect("first");
        let first = if missing { 4 } else { 2 };
        writer
            .write_snapshot(&snapshot(2, first, first))
            .expect("second");
        let mut bytes = writer.finish().expect("capture");
        if truncated {
            bytes.pop();
        }
        let capture = Capture(std::env::temp_dir().join(format!(
            "footfall-streamed-loss-{}-{missing}-{truncated}.ffcap",
            std::process::id()
        )));
        std::fs::write(&capture.0, bytes).expect("file");
        let output = Command::new(env!("CARGO_BIN_EXE_footfall"))
            .arg("check")
            .arg(&capture.0)
            .args(["--json", "--require", "losses"])
            .output()
            .expect("check");
        let json = String::from_utf8(output.stdout).expect("JSON");
        let status = if missing || truncated {
            "partial"
        } else {
            "present"
        };
        assert!(
            json.contains(&format!(r#""id":"losses","status":"{status}""#)),
            "{json}"
        );
        assert_eq!(output.status.success(), !missing && !truncated, "{json}");
        if !truncated {
            let output = Command::new(env!("CARGO_BIN_EXE_footfall"))
                .args(["analyze", "summary"])
                .arg(&capture.0)
                .output()
                .expect("summary");
            assert!(output.status.success(), "summary runs");
            let text = String::from_utf8(output.stdout).expect("text");
            assert!(text.contains("Elapsed wall time"), "{text}");
            assert!(!text.contains("on the CPU"), "{text}");
            if !missing {
                assert!(!text.contains("Lost:"), "{text}");
                assert!(text.contains("2 events evicted by the runtime"), "{text}");
            } else {
                assert!(text.contains("2 events missing"), "{text}");
            }
        }
    }
}

#[test]
fn a_missing_creation_does_not_prove_liveness_before_the_first_observation() {
    let metadata = SnapshotMetadata::new("streamed")
        .with_instance_id(42)
        .with_capture(footfall::runtime::CaptureSettings::new(
            footfall::InstrumentationLevel::Trace,
            16,
            2,
            16,
        ));
    let later = Snapshot::builder(metadata)
        .window(EventWindow::new(2, 5, 2))
        .captured_at_nanos(5_000)
        .drops(Drops::new().with_evicted_events(4))
        .event(TimedObservation::new(
            4_000,
            4,
            1,
            Observation::ResourceUpdate {
                kind_id: footfall::ResourceKindId::new(1),
                kind_name: "buffer",
                resource_id: footfall::ResourceId::new(1),
                context_id: ContextId::new(0),
                size: 64,
            },
        ))
        .build();
    let mut writer = CaptureWriter::new(Vec::new()).expect("capture");
    writer
        .write_snapshot(&snapshot(0, 0, 0))
        .expect("first window");
    writer.write_snapshot(&later).expect("later window");
    let capture = Capture(std::env::temp_dir().join(format!(
        "footfall-unknown-resource-{}.ffcap",
        std::process::id()
    )));
    std::fs::write(&capture.0, writer.finish().expect("bytes")).expect("file");
    let output = Command::new(env!("CARGO_BIN_EXE_footfall"))
        .args(["analyze", "resources"])
        .arg(&capture.0)
        .args(["--json", "--alive-at-ns", "3000"])
        .output()
        .expect("CLI");
    assert!(
        output.status.success(),
        "a partial capture remains inspectable"
    );
    let json = String::from_utf8(output.stdout).expect("JSON");
    assert!(
        json.contains(r#""count":0,"without_creation_scope":1,"unknown_at_query":1"#),
        "{json}"
    );
    assert!(json.contains(r#""missed_events":2"#), "{json}");
}
