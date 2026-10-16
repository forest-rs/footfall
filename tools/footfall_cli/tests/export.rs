// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `footfall export` on the demo release capture, in each format.

use std::path::PathBuf;
use std::process::Command;

fn capture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/captures/demo.ffcap")
}

/// Exports the demo capture to a file named `name` in a directory of its
/// own, with `args`, and returns what was written.
fn export(name: &str, args: &[&str]) -> Vec<u8> {
    let directory = std::env::temp_dir().join(format!(
        "footfall-export-test-{}-{name}",
        std::process::id()
    ));
    std::fs::create_dir_all(&directory).expect("a directory");
    let output = directory.join(name);
    let status = Command::new(env!("CARGO_BIN_EXE_footfall"))
        .arg("export")
        .arg(capture())
        .arg("-o")
        .arg(&output)
        .args(args)
        .status()
        .expect("footfall runs");
    assert!(status.success(), "footfall export succeeds");
    let written = std::fs::read(&output).expect("the trace was written");
    std::fs::remove_dir_all(&directory).expect("the directory is removed");
    written
}

#[test]
fn exports_a_perfetto_trace_by_default() {
    let trace = export("demo.pftrace", &[]);
    // A Perfetto trace is a sequence of packets, field 1, each a message.
    assert_eq!(trace.first(), Some(&0x0a), "a trace packet comes first");
    assert!(
        trace.len() > 100_000,
        "every event is in it: {}",
        trace.len()
    );
    assert!(
        trace
            .windows(b"frames on screen".len())
            .any(|window| window == b"frames on screen"),
        "the demo's frames are drawn"
    );
}

#[test]
fn exports_chrome_trace_json_on_request() {
    let json = String::from_utf8(export("demo.json", &["--format", "json"])).expect("UTF-8");
    assert!(json.starts_with("{\"displayTimeUnit\":\"ms\",\"traceEvents\":["));
    assert!(json.contains("\"frames on screen, stream 0\""));
}
