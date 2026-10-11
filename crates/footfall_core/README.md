# footfall_core

Core instrumentation primitives for Footfall: static descriptors identified by
address, user-defined label keys, contexts registered once and emitted by
handle, instrumentation levels, the `Backend` trait, and the process-wide
emission functions.

`footfall_core` is `no_std`; the `std` feature adds `install_backend` and
`LazyContext`. Most code depends on the `footfall` facade, which re-exports
this crate; the crate documentation here is the full reference, including the
rules for descriptors and how to compile instrumentation out.

Part of [Footfall](https://github.com/forest-rs/footfall). For instrumenting
code, start with the [`footfall`](https://docs.rs/footfall) crate.

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your
option.
