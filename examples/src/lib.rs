// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Code shared by the Footfall examples: the demo workload, number
//! formatting, and terminal output. Each example is a binary in `src/bin`;
//! see the examples README for what each one shows.

pub mod demo;
pub mod edit_fixture;
pub mod format;
pub mod report;
pub mod style;

// The guides' code blocks compile and run as doctests of this crate, which
// has every Footfall feature, so they cannot drift from the API.
#[cfg(doctest)]
#[doc = include_str!("../../README.md")]
struct ReadmeDoctests;
#[cfg(doctest)]
#[doc = include_str!("../../docs/instrumenting-an-app.md")]
struct InstrumentingAnAppDoctests;
#[cfg(doctest)]
#[doc = include_str!("../../docs/session-consumer-contract.md")]
struct SessionConsumerContractDoctests;
#[cfg(doctest)]
#[doc = include_str!("../../docs/analysing-a-trace.md")]
struct AnalysingATraceDoctests;
#[cfg(doctest)]
#[doc = include_str!("../../docs/migrating-to-0.2.md")]
struct MigratingTo02Doctests;

#[cfg(doctest)]
#[doc = include_str!("../../docs/investigating-an-edit.md")]
struct InvestigatingAnEditDoctests;
