// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `footfall check`: what a capture gives the analyses, and what it is
//! missing.
//!
//! Each [`Item`] is one thing an analysis uses: frame markers, frame
//! numbers, deadlines, submissions, presentations, stated timing bases,
//! clock syncs, GPU work, counter samples, completeness, and whether the
//! output was visible. For each it says whether the capture has it, which
//! conclusion depends on it, and the call that records it, so integrating a
//! producer is a checklist rather than guesswork.

use crate::format;
use crate::json::Json;
use core::time::Duration;
use footfall::DropReason;
use footfall::analysis::{FramePacing, Pacing, Recorded, Signal, Trace};
use footfall::{FramePointKind, TimingBasis};
use std::path::Path;

/// The guide that says how to record each item.
const GUIDE: &str = "https://github.com/forest-rs/footfall/blob/main/docs/instrumenting-an-app.md";

/// The fewest dropped frames in a row, none of them explained and none
/// shown, that suggest the output was not visible rather than that frames
/// were dropped: a quarter of a second at 120 Hz, half at 60 Hz.
const UNSEEN_RUN: u64 = 30;

/// The most frames at the end of a stream that can be waiting for a point
/// when the capture ends. Presentation feedback arrives a few frames late;
/// more frames than this without a point mean the producer stopped
/// recording it, not that it was still on its way.
const IN_FLIGHT: usize = 16;

/// Whether a capture has one item.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Status {
    /// The capture has it everywhere it applies.
    Present,
    /// The capture has it for some frames or times, not all.
    Partial,
    /// The capture does not have it.
    Missing,
    /// The capture has it, but what it shows looks like an artifact of how
    /// the run was recorded rather than of the program.
    Suspect,
    /// The producer did not record it, so the capture cannot say whether it
    /// happened; the reason is the [`Recorded`] that says why.
    NotRecorded(String),
    /// It does not apply, because something it depends on is missing.
    NotApplicable(&'static str),
}

impl Status {
    /// Returns the status's stable name, as `--json` writes it.
    pub(crate) const fn name(&self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Partial => "partial",
            Self::Missing => "missing",
            Self::Suspect => "suspect",
            Self::NotRecorded(_) => "not_recorded",
            Self::NotApplicable(_) => "not_applicable",
        }
    }

    /// Returns `have` of `of` as present, partial, or missing.
    const fn of(have: u64, of: u64) -> Self {
        if have == 0 {
            Self::Missing
        } else if have < of {
            Self::Partial
        } else {
            Self::Present
        }
    }
}

/// One thing the analyses use, and whether a capture has it.
#[derive(Clone, Debug)]
pub(crate) struct Item {
    /// The item's stable name, as `--json` and `--require` use it.
    pub(crate) id: &'static str,
    /// Whether the capture has it.
    pub(crate) status: Status,
    /// What the capture holds, as a phrase: "214 of 216 numbered frames".
    pub(crate) found: String,
    /// How many frames or times have it, when it is counted.
    pub(crate) have: Option<u64>,
    /// How many could have it, when it is counted.
    pub(crate) of: Option<u64>,
    /// Which conclusions depend on it.
    pub(crate) needed_for: &'static str,
    /// The call or setting that records it.
    pub(crate) fix: &'static str,
    /// The section of the instrumenting guide that explains it.
    pub(crate) section: &'static str,
}

impl Item {
    /// Returns the guide's URL for this item.
    pub(crate) fn guide(&self) -> String {
        format!("{GUIDE}#{}", self.section)
    }

    /// Returns `true` when the capture has the item fully, and it looks
    /// sound: what `--require` asks for.
    pub(crate) fn satisfied(&self) -> bool {
        self.status == Status::Present
    }
}

/// `n` and the noun for it: "1 frame", "2,048 frames".
fn many(n: u64, one: &str, more: &str) -> String {
    format!("{} {}", format::count(n), if n == 1 { one } else { more })
}

/// "`have` of `of` `noun`", with the noun agreeing with `of`.
fn ratio(have: u64, of: u64, one: &str, more: &str) -> String {
    format!("{} of {}", format::count(have), many(of, one, more))
}

/// The ids of every item, in the order `check` reports them.
pub(crate) const IDS: [&str; 11] = [
    "frames",
    "numbered",
    "deadlines",
    "submissions",
    "presentations",
    "bases",
    "clocks",
    "gpu",
    "counter_samples",
    "losses",
    "visibility",
];

/// How a capture was read: whether it was cut short, and how many windows
/// from another run of the producer were left out.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Read {
    /// The capture ends part way through a window.
    pub(crate) truncated: bool,
    /// Windows skipped because they came from another runtime instance.
    pub(crate) skipped: u64,
}

/// Checks what `trace` gives the analyses.
pub(crate) fn check(trace: &Trace, read: Read) -> Vec<Item> {
    let pacing = trace.pacing(Duration::ZERO);
    let frames = trace.frames();
    let numbered = frames
        .iter()
        .filter(|frame| frame.number().is_some())
        .count() as u64;
    let points = pacing.frames();
    let paced = points.len() as u64;
    let marks = trace.recorded(Signal::Marks);

    let mut items = Vec::with_capacity(IDS.len());
    items.push(Item {
        id: "frames",
        status: match &marks {
            Recorded::Yes | Recorded::Unknown if frames.is_empty() => Status::Missing,
            Recorded::Yes | Recorded::Unknown => Status::Present,
            other => Status::NotRecorded(other.to_string()),
        },
        found: many(frames.len() as u64, "frame marked", "frames marked"),
        have: None,
        of: None,
        needed_for: "frame times, slow frames, and per-frame counter changes",
        fix: "mark each frame with frame_begin and frame_end",
        section: "minimal-setup",
    });
    let no_frames = if frames.is_empty() {
        Some("needs frame markers")
    } else {
        None
    };
    items.push(Item {
        id: "numbered",
        status: no_frames.map_or(
            Status::of(numbered, frames.len() as u64),
            Status::NotApplicable,
        ),
        found: format!(
            "{} numbered",
            ratio(numbered, frames.len() as u64, "frame", "frames")
        ),
        have: Some(numbered),
        of: Some(frames.len() as u64),
        needed_for: "frame pacing, and finding a frame by its number",
        fix: "mark frames with frame_begin_numbered and frame_end_numbered",
        section: "per-frame-values",
    });
    let no_numbers = no_frames.or(if paced == 0 {
        Some("needs numbered frames")
    } else {
        None
    });
    let any: fn(&FramePacing) -> bool = |_| true;
    items.push(point_item(
        "deadlines",
        no_numbers,
        coverage(&pacing, any, |frame| frame.deadline().is_some()),
        "have deadlines",
        "judging each frame against its own deadline, and the missed-deadline cause",
        "record FramePointKind::Deadline with frame_point, as frame schedulers such as frameclock do",
    ));
    items.push(point_item(
        "submissions",
        no_numbers,
        coverage(&pacing, could_submit, |frame| frame.submitted().is_some()),
        "have submission times",
        "submitted-late frames, and telling a late submit from a late display",
        "record FramePointKind::Submitted when the frame is handed to the compositor or swapchain",
    ));
    let presentations = coverage(&pacing, any, |frame| {
        frame.presented().is_some() || frame.dropped().is_some()
    });
    items.push(point_item(
        "presentations",
        no_numbers,
        presentations,
        "were reported shown or dropped",
        "whether each frame was shown on time, late, or not at all",
        "record FramePointKind::Presented from the display's feedback, or FramePoint::dropped with why it was not shown",
    ));
    items.push(bases(points, no_numbers));
    items.push(clocks(trace, &pacing));
    items.push(gpu(trace, points, no_numbers));
    items.push(counter_samples(trace, no_frames));
    items.push(losses(trace, read));
    items.push(visibility(&pacing, presentations.have, no_numbers));
    items
}

/// How many numbered frames have one frame point, of those that could.
#[derive(Clone, Copy, Debug)]
struct Coverage {
    /// Frames with the point.
    have: u64,
    /// Frames that could have it.
    of: u64,
    /// Frames left out because they could never have it, such as a frame
    /// dropped before it was submitted.
    cannot: u64,
    /// Frames left out because the capture ended before their point would
    /// have arrived.
    in_flight: u64,
}

/// Counts the numbered frames for which `has` holds, of those for which
/// `could` does. In each stream, up to [`IN_FLIGHT`] frames after the last
/// one that has the point are left out as still in flight, since their
/// points would have arrived later; unless none has it, when none was
/// coming.
fn coverage(
    pacing: &Pacing,
    could: fn(&FramePacing) -> bool,
    has: fn(&FramePacing) -> bool,
) -> Coverage {
    let mut counted = Coverage {
        have: 0,
        of: 0,
        cannot: 0,
        in_flight: 0,
    };
    for stream in pacing
        .frames()
        .chunk_by(|a, b| a.number().stream() == b.number().stream())
    {
        let settled = stream
            .iter()
            .rposition(has)
            .map_or(stream.len(), |at| at + 1)
            .max(stream.len().saturating_sub(IN_FLIGHT));
        counted.in_flight += (stream.len() - settled) as u64;
        for frame in &stream[..settled] {
            if !could(frame) {
                counted.cannot += 1;
                continue;
            }
            counted.of += 1;
            if has(frame) {
                counted.have += 1;
            }
        }
    }
    counted
}

/// Returns whether `frame` could have been submitted: it was not dropped
/// before submitting.
fn could_submit(frame: &FramePacing) -> bool {
    !matches!(
        frame.dropped(),
        Some(
            DropReason::Superseded
                | DropReason::Discarded
                | DropReason::MissedDeadline
                | DropReason::SurfaceUnavailable
        )
    )
}

/// An item counted over numbered frames: how many have a frame point.
fn point_item(
    id: &'static str,
    unavailable: Option<&'static str>,
    counted: Coverage,
    what: &str,
    needed_for: &'static str,
    fix: &'static str,
) -> Item {
    let Coverage {
        have,
        of,
        cannot,
        in_flight,
    } = counted;
    let mut left_out = Vec::new();
    if cannot > 0 {
        left_out.push(format!(
            "{} dropped before submitting",
            format::count(cannot)
        ));
    }
    if in_flight > 0 {
        left_out.push(format!(
            "{in_flight} still in flight when the capture ended"
        ));
    }
    let left_out = if left_out.is_empty() {
        String::new()
    } else {
        format!(", leaving out {}", left_out.join(" and "))
    };
    Item {
        id,
        status: unavailable.map_or(Status::of(have, of), Status::NotApplicable),
        found: format!(
            "{} {what}{left_out}",
            ratio(have, of, "numbered frame", "numbered frames")
        ),
        have: Some(have),
        of: Some(of),
        needed_for,
        fix,
        section: "frame-pacing",
    }
}

/// How many submission and presentation times state how they were known:
/// what "how sure" rests on.
fn bases(points: &[FramePacing], unavailable: Option<&'static str>) -> Item {
    let (mut stated, mut times) = (0_u64, 0_u64);
    for frame in points {
        for kind in [FramePointKind::Submitted, FramePointKind::Presented] {
            if frame.time_of(kind).is_some() {
                times += 1;
                if frame.basis(kind) != TimingBasis::Unknown {
                    stated += 1;
                }
            }
        }
    }
    let unavailable = unavailable.or(if times == 0 {
        Some("needs submission or presentation times")
    } else {
        None
    });
    Item {
        id: "bases",
        status: unavailable.map_or(Status::of(stated, times), Status::NotApplicable),
        found: format!(
            "{} say how they were known",
            ratio(
                stated,
                times,
                "submission or presentation time",
                "submission and presentation times"
            )
        ),
        have: Some(stated),
        of: Some(times),
        needed_for: "how sure each late cause is: observed, inferred, or unresolved",
        fix: "state each time's basis with FramePoint::with_basis, TimingBasis::Observed or Estimated",
        section: "frame-pacing",
    }
}

/// Times and spans recorded on other clocks that the trace could not place,
/// because their clock was never synced to the runtime's.
fn clocks(trace: &Trace, pacing: &Pacing) -> Item {
    let points = pacing.unplaced_points();
    let spans = trace.unplaced_spans() as u64;
    let unplaced = points + spans;
    Item {
        id: "clocks",
        status: if unplaced == 0 {
            Status::Present
        } else {
            Status::Partial
        },
        found: if unplaced == 0 {
            String::from("every time on another clock was placed")
        } else {
            format!(
                "{} and {} on clocks never synced",
                many(points, "frame point", "frame points"),
                many(spans, "timeline span", "timeline spans")
            )
        },
        have: None,
        of: None,
        needed_for: "placing frame points and GPU work on the runtime's clock",
        fix: "sync each other clock with clock_sync or clock_pair before, and while, using it",
        section: "other-clocks",
    }
}

/// GPU work on timelines, and how many frames that reached the GPU have
/// some: the frames shown, or when presentations are not recorded, the
/// frames submitted, or else every settled numbered frame.
fn gpu(trace: &Trace, points: &[FramePacing], unavailable: Option<&'static str>) -> Item {
    let timelines = trace
        .timelines()
        .filter(|track| track.timeline_id().is_some())
        .count();
    let shown: fn(&FramePacing) -> bool = |frame| frame.presented().is_some();
    let submitted: fn(&FramePacing) -> bool = |frame| frame.submitted().is_some();
    let (reached, which): (fn(&FramePacing) -> bool, &str) = if points.iter().any(shown) {
        (shown, "shown")
    } else if points.iter().any(submitted) {
        (submitted, "submitted")
    } else {
        (|_| true, "numbered")
    };
    let of = points.iter().filter(|frame| reached(frame)).count() as u64;
    let have = points
        .iter()
        .filter(|frame| reached(frame) && frame.gpu().is_some())
        .count() as u64;
    let status = match unavailable {
        Some(why) => Status::NotApplicable(why),
        None if timelines == 0 => Status::Missing,
        None => Status::of(have, of),
    };
    Item {
        id: "gpu",
        status,
        found: format!(
            "{} · {} have GPU work",
            many(timelines as u64, "GPU timeline", "GPU timelines"),
            ratio(
                have,
                of,
                &format!("{which} frame"),
                &format!("{which} frames")
            )
        ),
        have: Some(have),
        of: Some(of),
        needed_for: "the GPU cause of lateness, and GPU time per frame",
        fix: "record GPU work with timeline_span on a GPU timeline, with its clock synced",
        section: "gpu-and-other-timelines",
    }
}

/// Counter samples at frame ends, which give per-frame changes.
fn counter_samples(trace: &Trace, unavailable: Option<&'static str>) -> Item {
    let series = trace.counter_series().count() as u64;
    let status = match (trace.recorded(Signal::CounterSamples), unavailable) {
        (Recorded::Yes | Recorded::Unknown, Some(why)) => Status::NotApplicable(why),
        (Recorded::Yes | Recorded::Unknown, None) if trace.counters().is_empty() => {
            Status::NotApplicable("no counters recorded")
        }
        (Recorded::Yes | Recorded::Unknown, None) if series == 0 => Status::Missing,
        (Recorded::Yes | Recorded::Unknown, None) => Status::Present,
        (other, _) => Status::NotRecorded(other.to_string()),
    };
    Item {
        id: "counter_samples",
        status,
        found: format!(
            "{} sampled at frame ends",
            ratio(series, trace.counters().len() as u64, "counter", "counters")
        ),
        have: Some(series),
        of: Some(trace.counters().len() as u64),
        needed_for: "per-frame counter changes",
        fix: "turn on RuntimeBuilder::counter_samples, at a level that captures markers",
        section: "per-frame-values",
    }
}

/// Whether anything was lost: missing events, refused contexts, dropped
/// counter updates, or a capture cut short.
fn losses(trace: &Trace, read: Read) -> Item {
    let missing: u64 = trace
        .gaps()
        .iter()
        .map(footfall::analysis::Gap::missed)
        .sum();
    let drops = trace.drops();
    let mut lost = Vec::new();
    let mut note = |count: u64, one: &str, more: &str| {
        if count > 0 {
            lost.push(many(count, one, more));
        }
    };
    note(missing, "event missing", "events missing");
    note(drops.evicted_events(), "event evicted", "events evicted");
    note(
        drops.refused_contexts(),
        "context refused",
        "contexts refused",
    );
    note(
        drops.counter_updates(),
        "counter update dropped",
        "counter updates dropped",
    );
    note(
        drops.unattributed(),
        "event without a context",
        "events without a context",
    );
    note(
        drops.updated_after_release(),
        "update after release",
        "updates after release",
    );
    note(
        read.skipped,
        "window from another run skipped",
        "windows from another run skipped",
    );
    if read.truncated {
        lost.push(String::from("the capture ends part way through a window"));
    }
    Item {
        id: "losses",
        status: if lost.is_empty() {
            Status::Present
        } else {
            Status::Partial
        },
        found: if lost.is_empty() {
            String::from("nothing lost")
        } else {
            lost.join(" · ")
        },
        have: None,
        of: None,
        needed_for: "complete answers: whatever was lost leaves holes in every analysis",
        fix: "raise RuntimeBuilder::event_capacity, or read more often with footfall record --interval-ms",
        section: "sizing-capacity",
    }
}

/// Whether the frames looked like they could be seen: a long run of frames
/// dropped with no reason and none shown suggests the display was asleep or
/// the window hidden, which a pacing judgment would count as drops.
fn visibility(pacing: &Pacing, reported: u64, unavailable: Option<&'static str>) -> Item {
    let mut longest = 0_u64;
    let mut hidden = 0_u64;
    for stream in pacing
        .frames()
        .chunk_by(|a, b| a.number().stream() == b.number().stream())
    {
        let mut run = 0_u64;
        for frame in stream {
            match frame.dropped() {
                Some(DropReason::OutputUnavailable) => hidden += 1,
                Some(DropReason::NotPresented | DropReason::Unknown) => {
                    run += 1;
                    longest = longest.max(run);
                    continue;
                }
                _ => {}
            }
            run = 0;
        }
    }
    let unavailable = unavailable.or(if reported == 0 {
        Some("needs presentations")
    } else {
        None
    });
    let status = match unavailable {
        Some(why) => Status::NotApplicable(why),
        None if longest >= UNSEEN_RUN => Status::Suspect,
        None => Status::Present,
    };
    let found = match (longest >= UNSEEN_RUN, hidden) {
        (true, _) => format!(
            "{} frames in a row dropped without a reason and none shown: the display may have been asleep or the window hidden",
            format::count(longest)
        ),
        (false, 0) => String::from("no long runs of unexplained drops"),
        (false, hidden) => {
            format!(
                "{} dropped because the output was unavailable",
                many(hidden, "frame reported", "frames reported")
            )
        }
    };
    Item {
        id: "visibility",
        status,
        found,
        have: None,
        of: None,
        needed_for: "telling frames the display dropped from output nobody could see",
        fix: "report frames dropped while the display is asleep or the window hidden as DropReason::OutputUnavailable",
        section: "frame-pacing",
    }
}

/// The document `check --json` prints for `items`, read from `path`.
pub(crate) fn document(path: &Path, trace: &Trace, items: &[Item]) -> Json {
    let capture = trace.metadata().map_or(Json::Null, |metadata| {
        Json::object([
            ("path", Json::from(path.display().to_string())),
            ("process", Json::from(metadata.process_name())),
            ("process_id", Json::from(metadata.process_id())),
            (
                "instance",
                Json::from(format!("{:016x}", metadata.instance_id())),
            ),
            ("producer_version", Json::from(metadata.producer_version())),
        ])
    });
    let items = items.iter().map(|item| {
        let reason = match &item.status {
            Status::NotRecorded(why) => Json::from(why.as_str()),
            Status::NotApplicable(why) => Json::from(*why),
            _ => Json::Null,
        };
        Json::object([
            ("id", Json::from(item.id)),
            ("status", Json::from(item.status.name())),
            ("reason", reason),
            ("found", Json::from(item.found.as_str())),
            ("have", Json::from(item.have)),
            ("of", Json::from(item.of)),
            ("needed_for", Json::from(item.needed_for)),
            ("fix", Json::from(item.fix)),
            ("guide", Json::from(item.guide())),
        ])
    });
    Json::object([
        ("footfall_version", Json::from(env!("CARGO_PKG_VERSION"))),
        ("capture", capture),
        ("items", Json::Array(items.collect())),
    ])
}

/// Returns the required items `items` does not satisfy, or an error naming
/// an id that is not an item.
pub(crate) fn unmet<'a>(items: &'a [Item], required: &[String]) -> Result<Vec<&'a Item>, String> {
    required
        .iter()
        .map(|id| {
            items
                .iter()
                .find(|item| item.id == id)
                .ok_or_else(|| format!("no check named {id}; checks are {}", IDS.join(", ")))
        })
        .filter(|item| !matches!(item, Ok(item) if item.satisfied()))
        .collect()
}

#[cfg(test)]
mod tests {
    use footfall::analysis::Trace;
    use footfall::runtime::Runtime;
    use footfall::session::OwnedSnapshot;
    use footfall::{
        Backend as _, ClockDesc, ClockReading, ContextHandle, CounterDesc, DropReason, FrameNumber,
        FramePoint, FramePointKind, MarkerKind, MarkerPhase, TickPeriod, TimingBasis,
    };

    use super::{Item, Read, Status, check, unmet};

    static HOST: ClockDesc = ClockDesc::new("host");
    static NEVER_SYNCED: ClockDesc = ClockDesc::new("never synced");
    static UPLOADS: CounterDesc = CounterDesc::bytes("uploads");

    /// A runtime whose host clock is synced, not installed: tests run in
    /// parallel.
    fn runtime() -> Runtime {
        let runtime = Runtime::default();
        runtime.clock_sync(ClockReading::new(&HOST, 0, TickPeriod::NANOSECOND));
        runtime
    }

    /// Marks `count` numbered frames on `runtime`, recording each frame's
    /// points with `points` between its begin and end.
    fn frames(runtime: &Runtime, count: u64, points: impl Fn(&Runtime, u64)) {
        for number in 0..count {
            let marker = |phase| {
                runtime.numbered_marker(
                    MarkerKind::Frame,
                    phase,
                    FrameNumber::from(number),
                    ContextHandle::ROOT,
                );
            };
            marker(MarkerPhase::Begin);
            points(runtime, number);
            marker(MarkerPhase::End);
        }
    }

    /// A point for frame `number` on the host clock, `number` milliseconds
    /// in, with `basis`.
    fn point(number: u64, kind: FramePointKind, basis: TimingBasis) -> FramePoint {
        FramePoint::at(
            number,
            kind,
            &HOST,
            TickPeriod::NANOSECOND,
            number * 1_000_000,
        )
        .with_basis(basis)
    }

    /// A frame scheduler with display feedback: a deadline, an observed
    /// submission, and an observed presentation.
    fn scheduled(runtime: &Runtime, number: u64) {
        runtime.frame_point(&point(
            number,
            FramePointKind::Deadline,
            TimingBasis::Observed,
        ));
        runtime.frame_point(&point(
            number,
            FramePointKind::Submitted,
            TimingBasis::Observed,
        ));
        runtime.frame_point(&point(
            number,
            FramePointKind::Presented,
            TimingBasis::Observed,
        ));
    }

    fn checked(runtime: &Runtime) -> Vec<Item> {
        check(
            &Trace::from_snapshot(&OwnedSnapshot::from(runtime.snapshot())),
            Read::default(),
        )
    }

    fn item<'a>(items: &'a [Item], id: &str) -> &'a Item {
        items
            .iter()
            .find(|item| item.id == id)
            .expect("every check is reported")
    }

    fn status(items: &[Item], id: &str) -> Status {
        item(items, id).status.clone()
    }

    #[test]
    fn a_complete_integration_is_present_everywhere_pacing_looks() {
        let runtime = runtime();
        frames(&runtime, 40, scheduled);
        let items = checked(&runtime);
        for id in [
            "frames",
            "numbered",
            "deadlines",
            "submissions",
            "presentations",
            "bases",
        ] {
            assert_eq!(status(&items, id), Status::Present, "{id}");
        }
        assert_eq!(status(&items, "clocks"), Status::Present);
        assert_eq!(status(&items, "losses"), Status::Present);
        assert_eq!(status(&items, "visibility"), Status::Present);
        // No GPU timeline was recorded, and nothing pretends otherwise.
        assert_eq!(status(&items, "gpu"), Status::Missing);
        assert!(
            unmet(&items, &[String::from("presentations")])
                .expect("a check")
                .is_empty()
        );
        let unmet = unmet(&items, &[String::from("gpu")]).expect("a check");
        assert_eq!(unmet.len(), 1);
        assert_eq!(unmet[0].id, "gpu");
    }

    #[test]
    fn unnumbered_frames_leave_the_pacing_checks_without_frames_to_judge() {
        let runtime = runtime();
        for _ in 0..3 {
            runtime.frame_marker(MarkerKind::Frame, MarkerPhase::Begin, ContextHandle::ROOT);
            runtime.frame_marker(MarkerKind::Frame, MarkerPhase::End, ContextHandle::ROOT);
        }
        let items = checked(&runtime);
        assert_eq!(status(&items, "frames"), Status::Present);
        assert_eq!(status(&items, "numbered"), Status::Missing);
        assert_eq!(item(&items, "numbered").found, "0 of 3 frames numbered");
        for id in [
            "deadlines",
            "submissions",
            "presentations",
            "bases",
            "gpu",
            "visibility",
        ] {
            assert_eq!(
                status(&items, id),
                Status::NotApplicable("needs numbered frames"),
                "{id}"
            );
        }
    }

    #[test]
    fn a_scheduler_without_display_feedback_is_missing_presentations() {
        let runtime = runtime();
        frames(&runtime, 40, |runtime, number| {
            runtime.frame_point(&point(
                number,
                FramePointKind::Deadline,
                TimingBasis::Observed,
            ));
            runtime.frame_point(&point(
                number,
                FramePointKind::Submitted,
                TimingBasis::Observed,
            ));
        });
        let items = checked(&runtime);
        assert_eq!(status(&items, "deadlines"), Status::Present);
        assert_eq!(status(&items, "submissions"), Status::Present);
        // None reported, so none were on their way: all 40 count.
        assert_eq!(status(&items, "presentations"), Status::Missing);
        assert_eq!(item(&items, "presentations").of, Some(40));
        assert_eq!(
            status(&items, "visibility"),
            Status::NotApplicable("needs presentations")
        );
    }

    #[test]
    fn the_last_frames_waiting_for_feedback_are_left_out_but_feedback_that_stops_is_not() {
        let waiting = runtime();
        frames(&waiting, 40, |runtime, number| {
            if number < 37 {
                scheduled(runtime, number);
            }
        });
        let items = checked(&waiting);
        assert_eq!(status(&items, "presentations"), Status::Present);
        assert_eq!(
            item(&items, "presentations").found,
            "37 of 37 numbered frames were reported shown or dropped, leaving out 3 still in flight when the capture ended"
        );

        // Feedback that stopped after 10 frames: more frames without it
        // than could be on their way.
        let stopped = runtime();
        frames(&stopped, 40, |runtime, number| {
            if number < 10 {
                scheduled(runtime, number);
            }
        });
        let items = checked(&stopped);
        assert_eq!(status(&items, "presentations"), Status::Partial);
        assert_eq!(item(&items, "presentations").have, Some(10));
        assert_eq!(item(&items, "presentations").of, Some(24));
    }

    #[test]
    fn frames_dropped_before_submitting_do_not_count_against_submissions() {
        let runtime = runtime();
        frames(&runtime, 20, |runtime, number| {
            if number == 5 {
                runtime.frame_point(&FramePoint::dropped(number, DropReason::Superseded));
            } else {
                scheduled(runtime, number);
            }
        });
        let items = checked(&runtime);
        assert_eq!(status(&items, "submissions"), Status::Present);
        assert_eq!(item(&items, "submissions").of, Some(19));
        assert!(
            item(&items, "submissions")
                .found
                .contains("1 dropped before submitting")
        );
    }

    #[test]
    fn times_without_a_stated_basis_leave_how_sure_partial() {
        let runtime = runtime();
        frames(&runtime, 10, |runtime, number| {
            runtime.frame_point(&point(
                number,
                FramePointKind::Submitted,
                TimingBasis::Observed,
            ));
            runtime.frame_point(&point(
                number,
                FramePointKind::Presented,
                TimingBasis::Unknown,
            ));
        });
        let items = checked(&runtime);
        assert_eq!(status(&items, "bases"), Status::Partial);
        assert_eq!(
            (item(&items, "bases").have, item(&items, "bases").of),
            (Some(10), Some(20))
        );
    }

    #[test]
    fn points_on_a_clock_never_synced_are_reported() {
        let runtime = runtime();
        frames(&runtime, 10, |runtime, number| {
            scheduled(runtime, number);
            runtime.frame_point(&FramePoint::at(
                number,
                FramePointKind::Target,
                &NEVER_SYNCED,
                TickPeriod::NANOSECOND,
                number,
            ));
        });
        let items = checked(&runtime);
        assert_eq!(status(&items, "clocks"), Status::Partial);
        assert_eq!(
            item(&items, "clocks").found,
            "10 frame points and 0 timeline spans on clocks never synced"
        );
    }

    #[test]
    fn a_long_run_of_unexplained_drops_suggests_the_output_was_not_visible() {
        let asleep = runtime();
        frames(&asleep, 60, |runtime, number| {
            if number < 10 {
                scheduled(runtime, number);
            } else {
                runtime.frame_point(&FramePoint::dropped(number, DropReason::NotPresented));
            }
        });
        let items = checked(&asleep);
        assert_eq!(status(&items, "visibility"), Status::Suspect);
        assert!(
            item(&items, "visibility")
                .found
                .starts_with("50 frames in a row")
        );

        // The same run, with the producer saying why.
        let reported = runtime();
        frames(&reported, 60, |runtime, number| {
            if number < 10 {
                scheduled(runtime, number);
            } else {
                runtime.frame_point(&FramePoint::dropped(number, DropReason::OutputUnavailable));
            }
        });
        let items = checked(&reported);
        assert_eq!(status(&items, "visibility"), Status::Present);
        assert_eq!(
            item(&items, "visibility").found,
            "50 frames reported dropped because the output was unavailable"
        );
    }

    #[test]
    fn counter_samples_turned_off_are_not_recorded_rather_than_missing() {
        let runtime = Runtime::builder().counter_samples(false).build();
        runtime.clock_sync(ClockReading::new(&HOST, 0, TickPeriod::NANOSECOND));
        frames(&runtime, 3, |runtime, _| {
            runtime.counter_add(&UPLOADS, ContextHandle::ROOT, 64);
        });
        let items = checked(&runtime);
        assert!(
            matches!(status(&items, "counter_samples"), Status::NotRecorded(_)),
            "{:?}",
            status(&items, "counter_samples")
        );
    }
}
