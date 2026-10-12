// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A span's context is registered when the span opens and released when it
//! closes.

use footfall_core::{Backend, Context, ContextHandle, install_backend};
use footfall_tracing::FootfallLayer;
use std::sync::Mutex;
use tracing::Dispatch;
use tracing_subscriber::layer::SubscriberExt as _;

/// Records context registrations and releases, in order.
#[derive(Debug, Default)]
struct Registrations {
    log: Mutex<Vec<(&'static str, ContextHandle)>>,
}

impl Backend for Registrations {
    fn context_registered(&self, handle: ContextHandle, _context: &Context) {
        self.log
            .lock()
            .expect("log should not be poisoned")
            .push(("registered", handle));
    }

    fn context_released(&self, handle: ContextHandle) {
        self.log
            .lock()
            .expect("log should not be poisoned")
            .push(("released", handle));
    }
}

#[test]
fn closing_a_span_releases_its_context() {
    static BACKEND: Registrations = Registrations {
        log: Mutex::new(Vec::new()),
    };
    install_backend(&BACKEND).expect("backend should install once for this test binary");

    let dispatch = Dispatch::new(tracing_subscriber::registry().with(FootfallLayer::default()));
    tracing::dispatcher::with_default(&dispatch, || {
        let _entered = tracing::info_span!("record_pass", items = 3_u64).entered();
    });

    let log = BACKEND.log.lock().expect("log should not be poisoned");
    let [("registered", registered), ("released", released)] = log.as_slice() else {
        panic!("expected one registration and its release, got {log:?}");
    };
    assert_eq!(registered, released);
}
