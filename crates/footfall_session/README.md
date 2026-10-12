# footfall_session

The transport-neutral session model for consumers outside the instrumented
process: versioned, chunked snapshot envelopes (contexts, counters,
histograms, events) and a small live-session request and stream vocabulary.

It also owns the envelope codec (`codec`), capture files that save
snapshots for later (`capture`), and `OwnedSnapshot`, the reassembled form
consumers work with live or offline.

It does not participate in hot-path emission. Transports such as
`footfall_session_wtransport` carry these envelopes; the
[consumer contract](https://github.com/forest-rs/footfall/blob/main/docs/session-consumer-contract.md)
describes what a consumer can rely on.

Part of [Footfall](https://github.com/forest-rs/footfall). For instrumenting
code, start with the [`footfall`](https://docs.rs/footfall) crate.

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your
option.
