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
static COPIED: footfall::CounterDesc = footfall::CounterDesc::bytes("copied");
static STORAGE: footfall::EventDesc = footfall::EventDesc::new("storage");
static BUFFER: footfall::ResourceKindDesc = footfall::ResourceKindDesc::new("buffer");

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
    for invalid in ["NaN", "inf", "1e300"] {
        let output = Command::new(env!("CARGO_BIN_EXE_footfall"))
            .args(["analyze", "slow", "unused.ffcap", "--budget-ms", invalid])
            .output()
            .expect("invalid request runs");
        assert_eq!(
            output.status.code(),
            Some(2),
            "invalid budget is rejected by the CLI"
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains("panicked"));
    }
    runtime.counter_checkpoint(&STORAGE, ContextHandle::ROOT);
    let mut buffer = 0;
    for kind in ["amplitude", "quality", "amplitude"] {
        runtime.frame_marker(
            MarkerKind::Transaction,
            MarkerPhase::Begin,
            ContextHandle::ROOT,
        );
        let edit =
            runtime.scope_begin(&EDIT, ContextHandle::ROOT, &Args::none().with("kind", kind));
        let body = runtime.scope_begin(&BODY, ContextHandle::ROOT, &Args::none());
        buffer += 1;
        runtime.resource_create(
            &BUFFER,
            footfall::ResourceId::new(buffer),
            ContextHandle::ROOT,
            64,
        );
        runtime.counter_add(&COPIED, ContextHandle::ROOT, 3);
        runtime.scope_end(body);
        runtime.scope_end(edit);
        runtime.frame_marker(
            MarkerKind::Transaction,
            MarkerPhase::End,
            ContextHandle::ROOT,
        );
    }
    runtime.counter_checkpoint(&STORAGE, ContextHandle::ROOT);
    let trace = footfall::analysis::Trace::from_snapshot(&footfall::session::OwnedSnapshot::from(
        runtime.snapshot(),
    ));
    let points = trace.counter_checkpoints();
    let from = points[0].sequence().to_string();
    let to = points[1].sequence().to_string();
    let edits = trace.spans().named("edit").iter();
    let origin = edits[0].span().id().sequence().to_string();
    let alive = edits[0].span().end().as_nanos().to_string();
    let capture =
        Capture(std::env::temp_dir().join(format!("footfall-work-{}.ffcap", std::process::id())));
    let mut writer = CaptureWriter::new(Vec::new()).expect("writer");
    writer
        .write_snapshot(&runtime.snapshot())
        .expect("snapshot");
    std::fs::write(&capture.0, writer.finish().expect("bytes")).expect("capture");
    let json = text(capture.command(&["analyze", "resources", "--json"]));
    assert!(
        json.contains(r#""count":3,"without_creation_scope":0"#),
        "{json}"
    );
    let json = text(capture.command(&[
        "analyze",
        "resources",
        "--created-in",
        &origin,
        "--alive-at-ns",
        &alive,
        "--json",
    ]));
    assert!(
        json.contains(r#""count":1,"without_creation_scope":0"#),
        "{json}"
    );
    assert!(json.contains(r#""resource_id":1"#), "{json}");
    assert!(
        json.contains(r#""key":"kind","value":"amplitude""#),
        "{json}"
    );
    let json = text(capture.command(&[
        "analyze",
        "checkpoints",
        "--json",
        "--from",
        &from,
        "--to",
        &to,
    ]));
    assert!(json.contains(r#""change":9"#), "{json}");
    assert!(json.contains(r#""complete":true"#), "{json}");
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
    assert!(
        json.contains(r#""scope_arguments":[{"key":"kind","value":"amplitude"}]"#),
        "{json}"
    );
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
    let output = capture.command(&["check", "--kind", "transaction", "--require", "frames"]);
    assert!(
        !output.status.success(),
        "frame checks do not apply to a transaction selection"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("checks are work, spans, counter_samples, losses")
    );

    let mut args = vec!["analyze", "slow", "--json", "--budget-ms", "0"];
    args.extend(selected);
    let json = text(capture.command(&args));
    assert!(json.contains(r#""candidate_units":2"#), "{json}");
    assert!(
        json.contains(r#""caveats":[{"kind":"few_units"}"#),
        "{json}"
    );
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
