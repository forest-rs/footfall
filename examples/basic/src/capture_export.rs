// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Converts a capture file into one Chrome trace.
//!
//! `capture_export <capture-file> > trace.json` joins the snapshots of the
//! capture's first runtime instance, such as the windows a
//! `session_consumer --record` subscription saved, and writes them as Chrome
//! Trace Event JSON for Perfetto.

use footfall_session::{OwnedSnapshot, capture::CaptureReader};
use std::{env, fs::File, io::BufReader, process};

fn main() {
    let Some(path) = env::args().nth(1) else {
        eprintln!("usage: capture_export <capture-file> > trace.json");
        process::exit(2);
    };
    let file =
        File::open(&path).unwrap_or_else(|error| fail(&format!("cannot open {path}: {error}")));
    let reader = CaptureReader::new(BufReader::new(file))
        .unwrap_or_else(|error| fail(&format!("{path}: {error}")));

    let mut joined: Option<OwnedSnapshot> = None;
    let mut skipped = 0_usize;
    for snapshot in reader {
        let snapshot = match snapshot {
            Ok(snapshot) => snapshot,
            // A recording stopped with Ctrl-C can end partway through a
            // snapshot; keep the complete ones.
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                eprintln!("{path} ends partway through a snapshot; using the complete ones");
                break;
            }
            Err(error) => fail(&format!("{path}: {error}")),
        };
        match &mut joined {
            None => joined = Some(snapshot),
            Some(joined) => {
                if joined.append(snapshot).is_err() {
                    skipped += 1;
                }
            }
        }
    }
    let Some(joined) = joined else {
        fail(&format!("{path} holds no snapshots"));
    };
    if skipped > 0 {
        eprintln!("skipped {skipped} snapshots from other runtime instances");
    }
    let window = joined.window();
    eprintln!(
        "events {}..{}: {} kept, {} missed",
        window.since(),
        window.next(),
        joined.events().len(),
        window.missed()
    );
    println!("{}", footfall_export::to_chrome_trace_json(&joined));
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    process::exit(1);
}
