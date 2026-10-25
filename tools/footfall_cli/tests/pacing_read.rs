// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Pacing JSON preserves what reading a complete or partial capture established.

use footfall::runtime::{Observation, Snapshot, SnapshotMetadata, TimedObservation};
use footfall::session::capture::CaptureWriter;
use footfall::{ContextId, FrameNumber, MarkerKind, MarkerPhase};
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug)]
struct CaptureFile(PathBuf);

impl Drop for CaptureFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn snapshot(instance: u64, number: u64, sequence: u64) -> Snapshot {
    let start = number * 10_000;
    let mut builder =
        Snapshot::builder(SnapshotMetadata::new("pacing read test").with_instance_id(instance))
            .captured_at_nanos(start + 5_000);
    for (at, phase, sequence) in [
        (start, MarkerPhase::Begin, sequence),
        (start + 1_000, MarkerPhase::End, sequence + 1),
    ] {
        builder = builder.event(TimedObservation::new(
            at,
            sequence,
            1,
            Observation::FrameMarker {
                kind: MarkerKind::Frame,
                phase,
                number: Some(FrameNumber::from(number)),
                context_id: ContextId::new(0),
            },
        ));
    }
    builder.build()
}

#[test]
fn json_reports_truncated_captures_and_skipped_instances() {
    for (truncated, foreign) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut writer = CaptureWriter::new(Vec::new()).expect("capture header");
        writer
            .write_snapshot(&snapshot(1, 1, 0))
            .expect("first snapshot");
        if foreign {
            writer
                .write_snapshot(&snapshot(2, 1, 0))
                .expect("foreign snapshot");
        }
        if truncated {
            writer
                .write_snapshot(&snapshot(1, 2, 2))
                .expect("last snapshot");
        }
        let mut bytes = writer.finish().expect("finished capture");
        if truncated {
            bytes.pop();
        }
        let capture = CaptureFile(std::env::temp_dir().join(format!(
            "footfall-pacing-read-{}-{truncated}-{foreign}.ffcap",
            std::process::id()
        )));
        std::fs::write(&capture.0, bytes).expect("capture file");
        let output = Command::new(env!("CARGO_BIN_EXE_footfall"))
            .args(["analyze", "pacing"])
            .arg(&capture.0)
            .arg("--json")
            .output()
            .expect("footfall runs");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "valid prefix remains usable: {stderr}"
        );
        let json = String::from_utf8(output.stdout).expect("UTF-8 JSON");
        let expected = format!(
            "\"capture_read\":{{\"complete\":{},\"truncated\":{truncated},\"skipped_snapshots\":{}}}",
            !truncated && !foreign,
            u64::from(foreign)
        );
        assert!(json.contains(&expected), "{expected} missing from {json}");
        assert!(
            json.contains("\"counts\":{\"frames\":1,"),
            "only the first snapshot is analyzed: {json}"
        );
        assert_eq!(
            stderr.is_empty(),
            !truncated && !foreign,
            "read warnings: {stderr}"
        );
    }
}
