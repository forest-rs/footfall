// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Work selection operates on recorded transactions and argument-bearing scopes.

use footfall::runtime::Runtime;
use footfall::session::capture::CaptureWriter;
use footfall::{Args, Backend as _, ContextHandle, MarkerKind, MarkerPhase, ScopeDesc};
use std::{
    path::PathBuf,
    process::{Command, Output},
};

static EDIT: ScopeDesc = ScopeDesc::new("edit");
static BODY: ScopeDesc = ScopeDesc::new("body");

struct Capture(PathBuf);
impl Drop for Capture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

impl Capture {
    fn command(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_footfall"))
            .args(args)
            .arg(&self.0)
            .output()
            .expect("CLI runs")
    }
}

fn text(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("UTF-8")
}

#[test]
fn transaction_and_scope_units_have_shared_structured_analysis() {
    let runtime = Runtime::default();
    for kind in ["amplitude", "quality", "amplitude"] {
        runtime.frame_marker(
            MarkerKind::Transaction,
            MarkerPhase::Begin,
            ContextHandle::ROOT,
        );
        let edit =
            runtime.scope_begin(&EDIT, ContextHandle::ROOT, &Args::none().with("kind", kind));
        let body = runtime.scope_begin(&BODY, ContextHandle::ROOT, &Args::none());
        runtime.scope_end(body);
        runtime.scope_end(edit);
        runtime.frame_marker(
            MarkerKind::Transaction,
            MarkerPhase::End,
            ContextHandle::ROOT,
        );
    }
    let capture =
        Capture(std::env::temp_dir().join(format!("footfall-work-{}.ffcap", std::process::id())));
    let mut writer = CaptureWriter::new(Vec::new()).expect("writer");
    writer
        .write_snapshot(&runtime.snapshot())
        .expect("snapshot");
    std::fs::write(&capture.0, writer.finish().expect("bytes")).expect("capture");
    let json = text(capture.command(&["analyze", "summary", "--kind", "transaction", "--json"]));
    assert!(
        json.contains(r#""kind":"transaction","scope":null"#),
        "{json}"
    );
    assert!(json.contains(r#""count":3,"units":["#), "{json}");
    assert!(json.contains(r#""scope":"body","count":3"#), "{json}");

    let selected = [
        "--scope",
        "edit",
        "--arg",
        "kind=amplitude",
        "--context",
        "0",
    ];
    let mut args = vec!["analyze", "summary", "--json"];
    args.extend(selected);
    let json = text(capture.command(&args));
    assert!(json.contains(r#""count":2,"units":["#), "{json}");
    assert!(json.contains(r#""scope":"body","count":2"#), "{json}");

    let mut args = vec![
        "check",
        "--json",
        "--require",
        "work",
        "--require",
        "spans",
        "--require",
        "losses",
    ];
    args.extend(selected);
    let json = text(capture.command(&args));
    assert!(json.contains(r#""id":"work","status":"present""#), "{json}");

    let mut args = vec!["analyze", "slow", "--json", "--budget-ms", "0"];
    args.extend(selected);
    let json = text(capture.command(&args));
    assert!(json.contains(r#""candidate_units":2"#), "{json}");
    assert!(json.contains(r#""caveats":["FewUnits"#), "{json}");
    let mut command = Command::new(env!("CARGO_BIN_EXE_footfall"));
    let json = text(
        command
            .args(["analyze", "compare"])
            .arg(&capture.0)
            .arg(&capture.0)
            .args(selected)
            .arg("--json")
            .output()
            .expect("compare"),
    );
    assert!(
        json.contains(r#""baseline_units":2,"candidate_units":2"#),
        "{json}"
    );
    assert!(json.contains(r#""change_ns_per_unit":0"#), "{json}");

    let output = capture.command(&["analyze", "slow", "--kind", "transaction"]);
    assert!(
        !output.status.success(),
        "non-frame work needs an explicit budget"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("--budget-ms is required"));
    let output = capture.command(&[
        "analyze",
        "summary",
        "--kind",
        "transaction",
        "--arg",
        "kind=amplitude",
    ]);
    assert!(!output.status.success(), "markers do not carry arguments");
    let json = text(capture.command(&[
        "analyze",
        "summary",
        "--scope",
        "edit",
        "--arg",
        "kind=absent",
        "--json",
    ]));
    assert!(json.contains(r#""count":0,"units":[]"#), "{json}");
}
