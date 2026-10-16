# footfall_tracy

A live [Tracy](https://github.com/wolfpld/tracy) backend for Footfall. Scopes
become zones, counters and gauges become plots, frame, pass, and transaction
markers become Tracy frames, and messages and resource lifecycles become Tracy
messages. Timelines, such as GPU queues, become Tracy GPU contexts, each a row
of its own, and how late each frame was shown becomes a plot.

Frame pacing gets rows of its own too. Each frame stream's frame
opportunities, such as display-link callbacks, become a Tracy frame set,
whose frame graph shows each skipped callback as a long frame, with a plot of
the wait between them. Each frame's CPU work is a zone on a `frames built`
row and its time on screen a zone on a `frames on screen` row, named after
the frame, and plots show each frame's latency from its work to the screen
and its slack before its deadline.

## Tracy Version

This crate speaks the protocol of **Tracy 0.14**, so connect with the Tracy
0.14 profiler; Tracy refuses clients of other versions. It uses `tracy-client`
0.19 and `tracy-client-sys` 0.30, the versions `tracing-tracy` 0.12 uses, so
a program can use both.

Part of [Footfall](https://github.com/forest-rs/footfall). For instrumenting
code, start with the [`footfall`](https://docs.rs/footfall) crate.

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your
option.
