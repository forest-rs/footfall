// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Runs the fake renderer for a fixed number of frames and records it to a
//! capture file in-process, with no session server: the way a program
//! records itself.
//!
//! ```sh
//! cargo run -p footfall_examples --bin record_demo -- demo.ffcap --frames 240
//! footfall analyze pacing demo.ffcap
//! ```

use clap::Parser;
use footfall::runtime::Runtime;
use footfall::session::capture::CaptureRecorder;
use footfall_examples::demo::Renderer;
use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

/// Runs the fake renderer and records it to a capture file.
#[derive(Debug, Parser)]
struct Args {
    /// The capture file to write.
    file: PathBuf,
    /// Frames to render.
    #[arg(long, default_value_t = 240)]
    frames: u64,
    /// Frames per second to aim for.
    #[arg(long, default_value_t = 60)]
    fps: u32,
    /// Frames between writes to the file.
    #[arg(long, default_value_t = 60)]
    write_every: u64,
    /// Raster worker threads.
    #[arg(long, default_value_t = 4)]
    workers: usize,
}

fn main() -> std::io::Result<()> {
    let args = Args::parse();
    let runtime = Runtime::builder()
        .process_name("record_demo")
        .install_global()
        .expect("no other backend is installed");
    let mut recorder = CaptureRecorder::new(runtime, BufWriter::new(File::create(&args.file)?))?;
    let mut renderer = Renderer::new(args.workers);
    let stop = AtomicBool::new(false);
    let mut rendered = 0;
    while rendered < args.frames {
        let frames = args.write_every.max(1).min(args.frames - rendered);
        renderer.run(args.fps, Some(frames), &stop);
        rendered += frames;
        // Each write adds what was recorded since the last one.
        recorder.write()?;
    }
    let finished = recorder.finish()?;
    println!(
        "Wrote {rendered} frames to {}: {} events in {} snapshots, {} lost.",
        args.file.display(),
        finished.events(),
        finished.snapshots(),
        finished.missed(),
    );
    Ok(())
}
