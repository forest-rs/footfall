// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Global gate history is independent of runtime levels, including arbitrary installation.

use footfall_analysis::{MessageQuery, Page, Signal, Trace};
use footfall_core::{
    Args, ContextHandle, EventDesc, InstrumentationLevel, ResourceId, ResourceKindDesc,
};
use footfall_runtime::Runtime;
use footfall_session::{
    OwnedSnapshot,
    capture::{CaptureReader, CaptureWriter},
};

static FAILURE: EventDesc = EventDesc::new("failure");
static BUFFER: ResourceKindDesc = ResourceKindDesc::new("buffer");

#[test]
fn front_door_disablements_survive_between_reads_and_capture_round_trip() {
    use InstrumentationLevel::{Off, Scoped, Trace as Full};
    let runtime = Box::leak(Box::new(Runtime::default()));
    footfall_core::install_backend(runtime).expect("one backend in this test binary");
    let first = OwnedSnapshot::from(runtime.snapshot());
    assert_eq!(first.metadata().capture().level_floor(), Some(Full));
    footfall_core::set_max_level(Scoped);
    footfall_core::resource_create(&BUFFER, ResourceId::new(1), ContextHandle::ROOT, 100);
    footfall_core::set_max_level(Full);
    assert_eq!(
        runtime.level(),
        Full,
        "arbitrary installation leaves runtime level unchanged"
    );
    let snapshot = OwnedSnapshot::from(runtime.snapshot());
    assert_eq!(snapshot.metadata().capture().level_floor(), Some(Scoped));
    let trace = Trace::from_snapshot(&snapshot);
    assert!(trace.query_evidence(Signal::Messages, None).is_complete());
    assert!(
        !trace
            .resource_interval(trace.observed(), None)
            .activity
            .ending
            .exact
    );
    footfall_core::set_max_level(Off);
    footfall_core::message_with(&FAILURE, ContextHandle::ROOT, "failed", &Args::none());
    footfall_core::set_max_level(Full);
    let last = OwnedSnapshot::from(runtime.snapshot());
    let mut writer = CaptureWriter::new(Vec::new()).expect("writer");
    writer.write_owned(&first).expect("first");
    writer.write_owned(&last).expect("last");
    let bytes = writer.finish().expect("finish");
    let captured =
        Trace::from_capture(CaptureReader::new(bytes.as_slice()).expect("reader")).expect("trace");
    assert!(captured.complete(), "file read is complete");
    let trace = captured.trace();
    assert!(trace.recorded(Signal::Messages).is_yes());
    let messages = trace
        .select_messages(&MessageQuery::default(), Page::default())
        .expect("query");
    assert_eq!(messages.total, 0);
    assert!(!messages.evidence.is_complete());
    assert!(
        !trace.spans().table().basis().is_complete(),
        "ordinary span reports share historical recording evidence"
    );
}
