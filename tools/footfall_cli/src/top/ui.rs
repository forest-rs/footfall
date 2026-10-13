// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Drawing the view.

use super::app::{App, Status};
use crate::format;
use crate::report::FrameStats;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize as _};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Bar, BarChart, Block, BorderType, Cell, Paragraph, Row, Table};

const ACCENT: Color = Color::Cyan;
const SLOW: Color = Color::Red;
const FAST: Color = Color::Green;
const DIM: Style = Style::new().fg(Color::DarkGray);

pub(crate) fn draw(frame: &mut Frame<'_>, app: &App) {
    let [header, chart, middle, bottom, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(12),
        Constraint::Min(8),
        Constraint::Length(8),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    draw_header(frame, header, app);
    draw_chart(frame, chart, app);
    let [scopes, counters] =
        Layout::horizontal([Constraint::Percentage(52), Constraint::Percentage(48)]).areas(middle);
    draw_scopes(frame, scopes, app);
    draw_counters(frame, counters, app);
    let [messages, capture] =
        Layout::horizontal([Constraint::Percentage(52), Constraint::Percentage(48)]).areas(bottom);
    draw_messages(frame, messages, app);
    draw_capture(frame, capture, app);
    draw_footer(frame, footer, app);
}

fn block<'a>(title: impl Into<String>) -> Block<'a> {
    let title: String = title.into();
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(DIM)
        .title(Line::from(format!(" {title} ")).bold())
}

fn draw_header(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let (badge, color) = match (&app.status, app.paused) {
        (_, true) => (String::from("paused"), Color::Yellow),
        (Status::Connecting, _) => (String::from("connecting"), Color::Yellow),
        (Status::Live, _) => (String::from("live"), FAST),
        (Status::Gap, _) => (String::from("events lost"), Color::Yellow),
        (Status::Restarted, _) => (String::from("producer restarted"), Color::Yellow),
        (Status::Ended(reason), _) => (reason.clone(), SLOW),
    };
    let mut title = vec![
        Span::from("footfall top").bold().fg(ACCENT),
        Span::from("  "),
    ];
    if let Some(about) = &app.about {
        title.push(Span::from(about.process.clone()).bold());
        title.push(Span::styled(
            format!(
                "  pid {} · session {} · level {}",
                about.process_id,
                about.session.as_deref().unwrap_or("-"),
                about.level
            ),
            DIM,
        ));
    }
    title.push(Span::from("  "));
    title.push(Span::from(format!("● {badge}")).fg(color).bold());

    let stats = FrameStats::of_durations(app.frames.iter().copied());
    let summary = match stats {
        Some(stats) => {
            let over = app
                .frames
                .iter()
                .filter(|&&micros| micros > app.budget)
                .count();
            let slow = if over == 0 {
                Span::from("none over budget").fg(FAST)
            } else {
                Span::from(format!("{over} over budget")).fg(SLOW).bold()
            };
            Line::from(vec![
                Span::styled(format!("last {} frames  ", stats.count), DIM),
                Span::from(format!("mean {}", format::micros(stats.mean))),
                Span::styled(" · ", DIM),
                Span::from(format!("p95 {}", format::micros(stats.p95))),
                Span::styled(" · ", DIM),
                Span::from(format!("max {}", format::micros(stats.max))),
                Span::styled(" · ", DIM),
                slow,
                Span::styled(format!("  (budget {})", format::micros(app.budget)), DIM),
            ])
        }
        None => Line::styled("waiting for complete frames…", DIM),
    };
    // Shown on time, from what the frame scheduler or display reported.
    let shown = match app.pacing {
        Some(counts) => {
            let missed = if counts.missed() == 0 {
                Span::from("none missed").fg(FAST)
            } else {
                Span::from(format!(
                    "{} late, {} dropped, {} submitted late",
                    counts.late, counts.dropped, counts.submitted_late
                ))
                .fg(SLOW)
                .bold()
            };
            let mut line = vec![
                Span::styled("shown  ", DIM),
                Span::from(format!("{} on time", counts.on_time)),
                Span::styled(" · ", DIM),
                missed,
            ];
            if counts.early > 0 {
                line.push(Span::from(format!(" · {} early", counts.early)).fg(SLOW));
            }
            if counts.skipped > 0 {
                line.push(Span::styled(format!(" · {} skipped", counts.skipped), DIM));
            }
            Line::from(line)
        }
        None => Line::styled(app.source.clone(), DIM),
    };
    frame.render_widget(
        Paragraph::new(vec![Line::from(title), summary, shown]),
        area,
    );
}

fn draw_chart(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let block = block("Frame times · newest on the right · red over budget");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let width = usize::from(inner.width);
    let bars: Vec<Bar<'_>> = app
        .frames
        .iter()
        .skip(app.frames.len().saturating_sub(width))
        .map(|&micros| {
            let color = if micros > app.budget { SLOW } else { FAST };
            Bar::new(micros)
                .style(Style::new().fg(color))
                .text_value(String::new())
        })
        .collect();
    // Scale to at least twice the budget, so the budget sits mid-chart.
    let max = app
        .frames
        .iter()
        .copied()
        .max()
        .unwrap_or(0)
        .max(app.budget * 2);
    frame.render_widget(BarChart::new(bars).bar_width(1).bar_gap(0).max(max), inner);
}

fn draw_scopes(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let sort = if app.sort_by_own { "self" } else { "total" };
    let header = Row::new(["scope", "count", "total", "self", "max"]).style(DIM);
    let rows = app.scope_rows().into_iter().map(|(name, totals)| {
        Row::new([
            Cell::from(name.to_owned()).fg(ACCENT),
            Cell::from(format::count(totals.count)),
            Cell::from(format::micros(totals.total)),
            Cell::from(format::micros(totals.own)),
            Cell::from(format::micros(totals.max)),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Min(14),
            Constraint::Length(6),
            Constraint::Length(8),
            Constraint::Length(8),
            Constraint::Length(8),
        ],
    )
    .header(header)
    .block(block(format!(
        "Scopes · last {} s · by {sort} time",
        super::app::SCOPE_SPAN.as_secs()
    )));
    frame.render_widget(table, area);
}

fn draw_counters(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let header = Row::new(["counter", "last", "per frame", "now"]).style(DIM);
    let rows = app.counters.iter().map(|row| {
        let (last, per_frame) = if row.gauge {
            (format::value(row.unit, row.last), String::from("gauge"))
        } else {
            (
                format::value(row.unit, row.last),
                format::value(row.unit, row.per_frame),
            )
        };
        Row::new([
            Cell::from(row.name.clone()).fg(ACCENT),
            Cell::from(last),
            Cell::from(per_frame),
            Cell::from(format::value(row.unit, row.now)),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Min(14),
            Constraint::Length(9),
            Constraint::Length(9),
            Constraint::Length(9),
        ],
    )
    .header(header)
    .block(block("Counters · sampled at each frame end"));
    frame.render_widget(table, area);
}

fn draw_messages(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let lines: Vec<Line<'_>> = if app.messages.is_empty() {
        vec![Line::styled("no messages yet", DIM)]
    } else {
        app.messages
            .iter()
            .map(|message| {
                Line::from(vec![
                    Span::from(format!("{:<10}", message.name)).fg(ACCENT),
                    Span::from(message.text.clone()),
                ])
            })
            .collect()
    };
    frame.render_widget(
        Paragraph::new(lines).block(block("Messages · newest first")),
        area,
    );
}

fn draw_capture(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let lost = app.missed > 0 || !app.drops.is_empty();
    let health = if lost {
        Span::from("some data was lost").fg(Color::Yellow).bold()
    } else {
        Span::from("nothing lost").fg(FAST)
    };
    let row = |label: &str, value: String| {
        Line::from(vec![
            Span::styled(format!("{label:<9}"), DIM),
            Span::from(value),
        ])
    };
    let mut lines = vec![
        Line::from(health),
        row(
            "events",
            format!(
                "{} last window · {}/s",
                format::count(app.last_events as u64),
                format::count(app.events_per_second)
            ),
        ),
        row(
            "wire",
            app.last_bytes.map_or_else(
                || String::from("— (replaying a file)"),
                |bytes| format!("{} per window", format::bytes(bytes as u64)),
            ),
        ),
        row(
            "instance",
            app.about
                .as_ref()
                .map_or_else(String::new, |about| format!("{:016x}", about.instance)),
        ),
        row(
            "missed",
            format!(
                "{} events in {} gaps",
                format::count(app.missed),
                format::count(app.gaps)
            ),
        ),
        row(
            "evicted",
            format!(
                "{} of {} buffered",
                format::count(app.drops.evicted_events()),
                app.about
                    .as_ref()
                    .map_or_else(String::new, |about| format::count(
                        about.event_capacity as u64
                    ))
            ),
        ),
    ];
    if app.drops.counter_updates() + app.drops.refused_contexts() > 0 {
        lines.push(row(
            "refused",
            format!(
                "{} counter updates, {} contexts",
                format::count(app.drops.counter_updates()),
                format::count(app.drops.refused_contexts())
            ),
        ));
    }
    frame.render_widget(Paragraph::new(lines).block(block("Capture")), area);
}

fn draw_footer(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let key = |key: &'static str| Span::from(key).fg(ACCENT).add_modifier(Modifier::BOLD);
    let line = Line::from(vec![
        key(" q"),
        Span::styled(" quit   ", DIM),
        key("p"),
        Span::styled(
            if app.paused {
                " resume   "
            } else {
                " pause   "
            },
            DIM,
        ),
        key("s"),
        Span::styled(
            if app.sort_by_own {
                " sort by total time"
            } else {
                " sort by self time"
            },
            DIM,
        ),
        Span::styled(
            format!("   · {} frames seen", format::count(app.frames_seen)),
            DIM,
        ),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}
