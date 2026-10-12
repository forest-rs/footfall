# footfall_session_wtransport

Serves Footfall sessions over local WebTransport and fetches them from
consumers. The producer side manages a self-signed localhost identity and
answers discovery and snapshot requests; the consumer side, `SessionClient`,
discovers sessions and fetches snapshots, reassembled and checked, with a
timeout and a typed error for each way a request can fail. The byte-level encoding is
documented in the
[wire format](https://github.com/forest-rs/footfall/blob/main/docs/session-wire-format.md).

Part of [Footfall](https://github.com/forest-rs/footfall). For instrumenting
code, start with the [`footfall`](https://docs.rs/footfall) crate.

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your
option.
