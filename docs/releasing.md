# Releasing Footfall

The public crates share one version. The current preparation targets
**0.2.0**, with Rust 1.88 and protocol 1.1. Packaging and the consumer
rehearsal below do not publish crates or create a release tag.

## Prepare and verify

Update `[workspace.package].version`, all internal dependency requirements,
the lockfile, and current dependency snippets together. Keep versions in
historical capture fixtures and protocol golden tests unchanged. Review
the [migration guide](migrating-to-0.2.md) against the current public APIs.
Refresh `Cargo.lock` with `cargo update` and inspect direct requirements
with `cargo upgrade --dry-run --incompatible allow --recursive false`.
Review releases outside the current semver ranges before upgrading, and
recheck the MSRV and feature matrix after dependency changes.
The changelog's Unreleased section is the 0.2.0 candidate; move it to a
dated `0.2.0` heading when publishing.

Land the release-preparation changes, then publish from that clean commit
on `main`. Run the workspace checks before landing:

```sh
cargo fmt --all --check
taplo fmt --check
typos
bash .github/copyright.sh
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --all-features --no-deps --locked
```

Also run the feature, no_std, compile-out, MSRV, WebAssembly, and nightly
documentation configurations in `.github/workflows/ci.yml` before landing.

## Package rehearsal

Use a Cargo version that supports workspace packaging. Inspect the files
and build the packaged sources, with every optional feature enabled:

```sh
cargo package --workspace --exclude footfall_benchmarks --exclude footfall_examples --list
cargo package --workspace --exclude footfall_benchmarks --exclude footfall_examples --all-features
```

Every public crate must include its README and both license files.
Workspace packaging stages the unpublished sibling packages for dependency
resolution. A successful local rehearsal verifies the package set; it does
not establish registry availability or grant publication permission.

Run the independent consumer with Python 3.11+ and the
`x86_64-unknown-none` Rust target installed:

```sh
python3 tools/release/rehearse.py
```

The script inspects each archive's version, README, licenses, and source
content, then extracts the archives into a temporary directory outside the
workspace. It patches Footfall names to those extracted packages while
0.2.0 is unpublished and checks the dependency graph for old versions or
workspace source paths. It builds and runs bare and fully featured
consumers, exercises recording, plans, buffer points, capture round-trip,
analysis, both export formats, and the packaged CLI's structured output.
It also checks the packaged facade without std. The temporary sources are
removed on exit; the report and command log remain in
`target/release-rehearsal.json` and `target/release-consumer.log`.

After publication, repeat the consumer build without patches against
crates.io, including `cargo install footfall_cli --version 0.2.0 --locked`.

Do not substitute `cargo package --no-verify` for verification. It is useful
only for inspecting package contents when sibling versions are unavailable.

## 0.2.0 preparation results

The preparation pass on 2026-10-25 verified all twelve public packages with
all features enabled using Cargo 1.99. The external consumer exercised bare
and fully featured use, the packaged no_std facade, the capture and export
round-trip, and the packaged CLI. Workspace formatting, spelling, copyright,
Clippy with warnings denied, tests, doctests, documentation, and Rust 1.88
checks passed. The migration guide's Rust example is also a doctest.

The first package verification failed inside the restricted environment
while unpacking a staged sibling, with Cargo's internal `no hash listed`
error. The identical command passed with writable Cargo caches; do not
treat that failed run as verification. Rehearsal reports describe the
specific archive checksums tested. Registry publication and installation
without local patches remain part of the explicit publishing step.

## Publish in dependency order

Confirm the intended version is not already published, and confirm the
release commit and changelog date before publishing. Publish these crates
in order, waiting for each dependency to become available in the index:

1. `footfall_core`
2. `footfall_runtime`
3. `footfall_session`
4. `footfall_analysis`
5. `footfall_export`
6. `footfall_fanout`
7. `footfall_memory`
8. `footfall_tracing`
9. `footfall_tracy`
10. `footfall_session_wtransport`
11. `footfall`
12. `footfall_cli`

Use `cargo publish -p NAME --locked` for each. In particular, export now
depends on analysis, and the facade's optional dependencies must all be
published before the facade. The examples and benchmark crates have
`publish = false`.

The existing repository tag convention is `v0.1.0`; use `v0.2.0` for the
verified release commit after publication succeeds. Tag creation, tag
push, and publishing require an explicit release instruction.
