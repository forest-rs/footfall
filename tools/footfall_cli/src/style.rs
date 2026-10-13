// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Terminal styles for the examples' output.
//!
//! Print through `anstream::println!`, which drops the styles when the output
//! is not a terminal or `NO_COLOR` is set.

use anstyle::{AnsiColor, Style};

/// Section headings.
pub(crate) const HEADING: Style = Style::new().bold();
/// Names: scopes, counters, sessions.
pub(crate) const NAME: Style = AnsiColor::Cyan.on_default();
/// Secondary detail.
pub(crate) const DIM: Style = Style::new().dimmed();
/// Something healthy.
pub(crate) const GOOD: Style = AnsiColor::Green.on_default();
/// Something to look at.
pub(crate) const WARN: Style = AnsiColor::Yellow.on_default();
/// Something wrong, such as a frame over budget or lost data.
pub(crate) const BAD: Style = AnsiColor::Red.on_default().bold();
/// Commands to run and links to open.
pub(crate) const HINT: Style = AnsiColor::Magenta.on_default();
