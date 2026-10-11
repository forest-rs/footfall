# footfall_fanout

A Footfall `Backend` that forwards every emission to two child backends, for
example a bounded runtime capture and a live Tracy session. Fan-outs nest, so
more than two sinks compose as a tree.

Part of [Footfall](https://github.com/forest-rs/footfall). For instrumenting
code, start with the [`footfall`](https://docs.rs/footfall) crate.

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your
option.
