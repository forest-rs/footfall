# footfall_cli

The `footfall` command: watch a running Footfall session live, record it to a
capture file, and ask the capture questions later.

```sh
cargo install footfall_cli

footfall top                                 # watch the running session
footfall record demo.ffcap                   # save it until Ctrl-C
footfall analyze summary demo.ffcap          # frames, scopes, counters, losses
footfall analyze slow demo.ffcap             # what the slow frames did differently
footfall analyze pacing demo.ffcap           # which frames were shown late, and why
footfall analyze compare before.ffcap after.ffcap
footfall analyze span demo.ffcap 13909       # one span and the work around it
footfall export demo.ffcap                   # one Chrome trace, for Perfetto
footfall top --file demo.ffcap               # replay a capture in the live view
```

Programs serve their sessions with
[`footfall_session_wtransport`](https://docs.rs/footfall_session_wtransport),
and `footfall` finds them through a per-user state directory without
configuration. Set `FOOTFALL_STATE_DIR` to the same directory for both to
keep experiments apart from anything else running. Add `--help` to any
command to see its options.

Everything the command shows comes from Footfall's public API: the session
client, capture files, and [`footfall_analysis`](https://docs.rs/footfall_analysis).

Part of [Footfall](https://github.com/forest-rs/footfall). For instrumenting
code, start with the [`footfall`](https://docs.rs/footfall) crate.

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your
option.
