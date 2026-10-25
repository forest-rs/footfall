// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Write the deterministic edit-inspection contract fixture for CLI exploration.

use footfall::session::capture::CaptureWriter;
use std::{error::Error, fs::File};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .ok_or("usage: edit_inspection NEW_CAPTURE.ffcap [AMPLITUDE_EDITS]")?;
    let edits: u64 = args.next().as_deref().unwrap_or("2").parse()?;
    if edits > 100_000 {
        return Err("use at most 100,000 amplitude edits".into());
    }
    let snapshot = footfall_examples::edit_fixture::snapshot(edits);
    let file = File::options().write(true).create_new(true).open(&path)?;
    let mut writer = CaptureWriter::new(file)?;
    writer.write_owned(&snapshot)?;
    writer.finish()?;
    println!(
        "{path}: synthetic contract fixture, {} events, {} edits",
        snapshot.events().len(),
        edits + 5
    );
    Ok(())
}
