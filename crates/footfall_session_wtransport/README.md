# footfall_session_wtransport

Serves Footfall sessions over local WebTransport and reads them from
consumers.

- Producers call `LocalSessionServer::start` with the runtimes to offer. It
  manages a self-signed localhost identity, picks a port, registers itself so
  consumers can find it, and answers discovery, snapshot, and subscription
  requests until dropped.
- Consumers use `SessionClient`: `local_servers()` finds every local
  producer, `discover()` lists a server's sessions, `fetch()` reads a
  snapshot from a cursor, and `subscribe()` streams new ones. Every failure
  is a typed `ClientError`.

The byte-level encoding is documented in the
[wire format](https://github.com/forest-rs/footfall/blob/main/docs/session-wire-format.md).

Part of [Footfall](https://github.com/forest-rs/footfall). For instrumenting
code, start with the [`footfall`](https://docs.rs/footfall) crate.

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your
option.
