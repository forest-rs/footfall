// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Public commands inspect the same reproducible edit fixture as API consumers.

use footfall::session::capture::CaptureWriter;
use std::{
    path::PathBuf,
    process::{Command, Output},
};

struct Capture(PathBuf);
impl Drop for Capture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
impl Capture {
    fn new() -> Self {
        let snapshot = footfall_examples::edit_fixture::snapshot(2);
        let mut writer = CaptureWriter::new(Vec::new()).expect("writer");
        writer.write_owned(&snapshot).expect("snapshot");
        let file = Self(
            std::env::temp_dir().join(format!("footfall-inspection-{}.ffcap", std::process::id())),
        );
        std::fs::write(&file.0, writer.finish().expect("finish")).expect("write");
        file
    }
    fn run(&self, command: &str, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_footfall"))
            .args(["analyze", command])
            .arg(&self.0)
            .args(args)
            .output()
            .expect("CLI")
    }
}
fn json(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("UTF-8")
}
#[test]
fn connected_work_and_selective_queries_preserve_identity_and_evidence() {
    let capture = Capture::new();
    let output = json(capture.run(
        "work",
        &[
            "--scope",
            "procedural.edit",
            "--arg",
            "kind=rejected",
            "--detail",
            "--json",
        ],
    ));
    for field in [
        "\"membership\":\"captured_ancestry\"",
        "\"read_complete\":true",
        "\"history_complete\":true",
        "\"name\":\"procedural.failed\"",
        "\"checkpoints\":[",
        "\"resource_interval\":",
    ] {
        assert!(output.contains(field), "missing {field}: {output}");
    }
    assert!(output.contains("\"value_type\":\"unsigned\""));
    assert!(output.contains("\"value_type\":\"string\""));
    let messages = json(capture.run(
        "messages",
        &[
            "--name",
            "procedural.body_outcome",
            "--arg",
            "attempt=u:5",
            "--arg",
            "node=u:2",
            "--limit",
            "1",
            "--json",
        ],
    ));
    assert!(messages.contains("\"text\":\"failed\""));
    assert!(messages.contains("\"total\":1"));
    assert!(messages.contains("\"next\":null"));
    let wrong_type = json(capture.run("messages", &["--arg", "attempt=s:5", "--json"]));
    assert!(wrong_type.contains("\"total\":0"));
    let flows = json(capture.run("flows", &["--id", "5", "--json"]));
    assert!(flows.contains("\"endpoints_present\":true"));
    let interval = json(capture.run(
        "resource-interval",
        &[
            "--start-ns",
            "0",
            "--end-ns",
            "1",
            "--group",
            "creation-root",
            "--json",
        ],
    ));
    assert!(interval.contains("application_reported_bytes"));
    assert!(!interval.contains("elapsed_wall_time_ns"));
    let resources = json(capture.run("resources", &["--json"]));
    assert!(resources.contains("\"lifetime\":"));
    let marker = json(capture.run(
        "work",
        &["--kind", "transaction", "--id", "95", "--detail", "--json"],
    ));
    assert!(marker.contains("\"id\":95"));
    assert!(marker.contains("\"membership\":\"interval_activity\""));
    assert!(marker.contains("\"name\":\"procedural.failed\""));
    let wrong_kind = capture.run("work", &["--kind", "frame", "--id", "95", "--json"]);
    assert!(!wrong_kind.status.success());
    assert!(String::from_utf8_lossy(&wrong_kind.stderr).contains("95"));
    let ambiguous = capture.run("work", &["--scope", "procedural.edit", "--json"]);
    assert!(!ambiguous.status.success());
    assert!(String::from_utf8_lossy(&ambiguous.stderr).contains("matched 7"));
    for (command, args) in [
        ("messages", vec!["--within", "999999", "--json"]),
        ("messages", vec!["--sequence", "999999", "--json"]),
        ("flows", vec!["--id", "999999", "--json"]),
        ("resources", vec!["--lifetime", "999999", "--json"]),
        ("span", vec!["999999", "--json"]),
    ] {
        let output = capture.run(command, &args);
        assert!(!output.status.success(), "invalid identity: {command}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("absent"));
    }
    // A valid retained prefix may have complete event history, while file-read
    // evidence separately reports that the final snapshot was cut off.
    let mut writer = CaptureWriter::new(Vec::new()).expect("writer");
    let snapshot = footfall_examples::edit_fixture::snapshot(2);
    writer.write_owned(&snapshot).expect("first snapshot");
    writer.write_owned(&snapshot).expect("second snapshot");
    let mut bytes = writer.finish().expect("finish");
    bytes.pop();
    std::fs::write(&capture.0, bytes).expect("partial capture");
    let partial = json(capture.run("messages", &["--json"]));
    assert!(partial.contains("\"truncated\":true"));
    assert!(partial.contains("\"read_complete\":false"));
}
