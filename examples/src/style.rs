// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Terminal styles for the examples' output.
//!
//! Print through `anstream::println!`, which drops the styles when the output
//! is not a terminal or `NO_COLOR` is set.

use anstyle::{AnsiColor, Style};

/// Section headings.
pub const HEADING: Style = Style::new().bold();
/// Names: scopes, counters, sessions.
pub const NAME: Style = AnsiColor::Cyan.on_default();
/// Secondary detail.
pub const DIM: Style = Style::new().dimmed();
/// Something healthy.
pub const GOOD: Style = AnsiColor::Green.on_default();
/// Something to look at.
pub const WARN: Style = AnsiColor::Yellow.on_default();
/// Something wrong, such as a frame over budget or lost data.
pub const BAD: Style = AnsiColor::Red.on_default().bold();
/// Commands to run and links to open.
pub const HINT: Style = AnsiColor::Magenta.on_default();
