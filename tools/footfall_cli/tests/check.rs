// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `footfall check` on the release captures in `examples/captures`: what
//! each gives the analyses, and how `--require` and `--json` report it.

use std::path::PathBuf;
use std::process::{Command, Output};

/// Every check, in the order `footfall check` reports them.
const CHECKS: [&str; 13] = [
    "frames",
    "numbered",
    "scheduled",
    "opportunities",
    "deadlines",
    "submissions",
    "presentations",
    "bases",
    "clocks",
    "gpu",
    "counter_samples",
    "losses",
    "visibility",
];

fn capture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/captures")
        .join(name)
}

fn check(name: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_footfall"))
        .arg("check")
        .arg(capture(name))
        .args(args)
        .output()
        .expect("footfall runs")
}

/// `--require` for each of `checks`.
fn requiring<'a>(checks: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    checks
        .into_iter()
        .flat_map(|check| ["--require", check])
        .collect()
}

/// The checks for what protocol 1.1 added, which the 1.0 release captures
/// predate.
const SINCE_1_1: [&str; 4] = ["scheduled", "plans", "opportunities", "buffers"];

/// Every check a 1.0 capture can satisfy.
fn checks_before_1_1() -> impl Iterator<Item = &'static str> {
    CHECKS.into_iter().filter(|id| !SINCE_1_1.contains(id))
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn the_demo_capture_has_everything_the_analyses_used_when_it_was_recorded() {
    let output = check("demo.ffcap", &requiring(checks_before_1_1()));
    assert!(output.status.success(), "{}", stderr(&output));
    let text = String::from_utf8(output.stdout).expect("UTF-8");
    assert!(text.contains("11 of 15 present"), "{text}");
    // It was recorded before frames said when they were to begin.
    let output = check("demo.ffcap", &requiring(SINCE_1_1));
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output)
            .contains("required checks not met: scheduled (missing), plans (missing), opportunities (missing), buffers (missing)"),
        "{}",
        stderr(&output)
    );
    // Its superseded frames and its last frame are explained, not counted
    // against it.
    assert!(
        text.contains("238 of 238 numbered frames have submission times, leaving out 2 dropped before submitting"),
        "{text}"
    );
    assert!(
        text.contains("leaving out 1 still in flight when the capture ended"),
        "{text}"
    );
}

#[test]
fn the_lightweald_capture_has_everything_but_counters() {
    let all_but_counters = checks_before_1_1().filter(|&id| id != "counter_samples");
    let output = check("lightweald-metal.ffcap", &requiring(all_but_counters));
    assert!(output.status.success(), "{}", stderr(&output));

    // It records no counters, so requiring their samples fails, and says
    // why.
    let output = check("lightweald-metal.ffcap", &requiring(["counter_samples"]));
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("required checks not met: counter_samples (not applicable)"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn the_json_names_every_check_with_its_status_and_fix() {
    let output = check("lightweald-metal.ffcap", &["--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let json = String::from_utf8(output.stdout).expect("UTF-8");
    let mut from = 0;
    for id in CHECKS {
        let at = json[from..]
            .find(&format!(r#"{{"id":"{id}","status":""#))
            .unwrap_or_else(|| panic!("{id} is reported, in order: {json}"));
        from += at + 1;
    }
    assert!(json.contains(r#"{"id":"gpu","status":"present","reason":null,"found":"1 GPU timeline · 326 of 326 shown frames have GPU work","have":326,"of":326,"#));
    assert!(json.contains(
        r#"{"id":"counter_samples","status":"not_applicable","reason":"no counters recorded","#
    ));
    assert!(json.contains(r#""guide":"https://github.com/forest-rs/footfall/blob/main/docs/instrumenting-an-app.md#gpu-and-other-timelines""#));
}

#[test]
fn an_unknown_check_is_refused_with_the_ones_there_are() {
    let output = check("demo.ffcap", &["--require", "pacing"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("possible values: frames, numbered"));
}
