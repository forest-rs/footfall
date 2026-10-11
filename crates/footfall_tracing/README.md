# footfall_tracing

A `tracing` subscriber layer that forwards spans and events into Footfall.
Spans become scopes with their numeric and boolean fields as arguments and as
a registered context inherited by child spans; events become messages.
Existing `tracing` instrumentation shows up in every Footfall backend with no
code changes.

Part of [Footfall](https://github.com/forest-rs/footfall). For instrumenting
code, start with the [`footfall`](https://docs.rs/footfall) crate.

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your
option.
