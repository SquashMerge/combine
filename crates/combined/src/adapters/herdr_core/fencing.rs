//! What an attachment does with each request: whether to apply it, queue it for the reopen,
//! drop it or send it as superseded. Pure; it mirrors `testing/world.rs` (`gate`, `request`,
//! `apply_current_or_superseded`) and the rules in `ports/backend.rs`.

use std::collections::VecDeque;

use combine_protocol::{BackendGeneration, Display};

use crate::ports::{AttachEvent, BackendError, BackendResult};

/// Requests held while a pair reopens.
pub(super) const REOPEN_QUEUE_REQUESTS: usize = 1_024;
/// Bytes of typed input held while a pair reopens.
pub(super) const REOPEN_QUEUE_INPUT: usize = 65_536;
/// Most frames a full reopen queue can replay: one per request, the extra frames of a split
/// write, and the FACTS and RESIZE a first Focus catches up.
pub(super) const REOPEN_QUEUE_FRAMES: usize =
    REOPEN_QUEUE_REQUESTS + REOPEN_QUEUE_INPUT / super::frame::DATA_FRAME_MAX + 2;
const _: () = assert!(REOPEN_QUEUE_FRAMES <= super::connection::WRITER_QUEUE_FRAMES);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    Ready,
    Reopening,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Request {
    Write(Vec<u8>),
    Resize(u16, u16),
    Focus,
    SetDisplay(Display),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Queued {
    request: Request,
    generation: u64,
}

/// A size and display: what was asked for, or what the pair's active viewer was last given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ViewerSettings {
    pub size: (u16, u16),
    pub display: Display,
}

/// Something to send to the input half of the pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Out {
    Data(Vec<u8>),
    Automatic(Vec<u8>),
    Resize(u16, u16),
    Facts(Display),
    Focus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Effect {
    Send(Out),
    Emit(AttachEvent),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gate {
    Skip,
    Queue,
    Apply,
}

/// One attachment's request state.
#[derive(Debug)]
pub(super) struct Fence {
    pub phase: Phase,
    pub watch_only: bool,
    /// The input half confirmed AUTOMATIC_INPUT.
    pub automatic: bool,
    /// The backend generation of the current pair.
    pub generation: u64,
    pub requested: ViewerSettings,
    pub sent: ViewerSettings,
    queue: VecDeque<Queued>,
    queued_input: usize,
    queued_frames: usize,
}

impl Fence {
    pub(super) fn new(watch_only: bool, requested: ViewerSettings) -> Self {
        Fence {
            phase: Phase::Ready,
            watch_only,
            automatic: false,
            generation: 1,
            sent: requested.clone(),
            requested,
            queue: VecDeque::new(),
            queued_input: 0,
            queued_frames: 0,
        }
    }

    fn gate(&self) -> BackendResult<Gate> {
        match self.phase {
            Phase::Closed => Err(BackendError::Closed),
            _ if self.watch_only => Ok(Gate::Skip),
            Phase::Reopening => Ok(Gate::Queue),
            Phase::Ready => Ok(Gate::Apply),
        }
    }

    /// Frames one request would need on the input half, at most.
    pub(super) fn frames_needed(request: &Request) -> usize {
        match request {
            Request::Write(bytes) => bytes.len().div_ceil(super::frame::DATA_FRAME_MAX).max(1),
            Request::Resize(..) | Request::SetDisplay(_) => 1,
            Request::Focus => 3,
        }
    }

    /// Frames the queued requests replay, at most. A new pair reserves this before the replay.
    pub(super) fn queued_frames_bound(&self) -> usize {
        self.queued_frames + 2
    }

    /// Handles a request that carries an ownership generation. `highest` is the highest one any
    /// request for this terminal has carried.
    pub(super) fn request(
        &mut self,
        highest: &mut u64,
        generation: u64,
        request: Request,
    ) -> BackendResult<Vec<Effect>> {
        let gate = self.gate()?;
        if gate != Gate::Skip {
            *highest = (*highest).max(generation);
        }
        match gate {
            Gate::Skip => Ok(Vec::new()),
            Gate::Queue => self.enqueue(generation, request).map(|()| Vec::new()),
            Gate::Apply => Ok(self.apply(*highest, generation, request)),
        }
    }

    fn enqueue(&mut self, generation: u64, request: Request) -> BackendResult<()> {
        let input = match &request {
            Request::Write(bytes) => bytes.len(),
            _ => 0,
        };
        if self.queue.len() >= REOPEN_QUEUE_REQUESTS
            || self.queued_input + input > REOPEN_QUEUE_INPUT
        {
            return Err(BackendError::Failed("reopen queue full".to_string()));
        }
        self.queued_input += input;
        self.queued_frames += match &request {
            Request::Write(_) => Fence::frames_needed(&request),
            _ => 1,
        };
        self.queue.push_back(Queued {
            request,
            generation,
        });
        Ok(())
    }

    /// Applies a request, or, when a newer ownership generation has reached the terminal,
    /// the superseded form: it keeps the latest requested size and display but neither
    /// activates the attachment nor changes what the terminal holds. Typed bytes were accepted
    /// while the attachment owned the terminal, so they still go out, once.
    fn apply(&mut self, highest: u64, generation: u64, request: Request) -> Vec<Effect> {
        let superseded = generation < highest;
        match request {
            Request::Write(bytes) => {
                let out = if superseded && self.automatic {
                    Out::Automatic(bytes)
                } else {
                    Out::Data(bytes)
                };
                vec![Effect::Send(out)]
            }
            Request::Resize(cols, rows) => {
                self.requested.size = (cols, rows);
                if superseded {
                    return Vec::new();
                }
                self.sent.size = (cols, rows);
                vec![Effect::Send(Out::Resize(cols, rows))]
            }
            Request::SetDisplay(display) => {
                self.requested.display = display.clone();
                if superseded {
                    return Vec::new();
                }
                self.sent.display = display.clone();
                vec![
                    Effect::Send(Out::Facts(display.clone())),
                    Effect::Emit(AttachEvent::Facts(display)),
                ]
            }
            Request::Focus => {
                if superseded {
                    return Vec::new();
                }
                let mut effects = Vec::new();
                if self.sent.display != self.requested.display {
                    self.sent.display = self.requested.display.clone();
                    effects.push(Effect::Send(Out::Facts(self.requested.display.clone())));
                }
                if self.sent.size != self.requested.size {
                    self.sent.size = self.requested.size;
                    let (cols, rows) = self.requested.size;
                    effects.push(Effect::Send(Out::Resize(cols, rows)));
                }
                effects.push(Effect::Send(Out::Focus));
                effects
            }
        }
    }

    /// An automatic reply, fenced by the generation of the `Started` that opened its epoch.
    pub(super) fn write_automatic(
        &mut self,
        generation: BackendGeneration,
        bytes: &[u8],
    ) -> BackendResult<Vec<Effect>> {
        match self.gate()? {
            Gate::Skip | Gate::Queue => Ok(Vec::new()),
            Gate::Apply => {
                if generation.0 == self.generation && self.automatic {
                    Ok(vec![Effect::Send(Out::Automatic(bytes.to_vec()))])
                } else {
                    Ok(Vec::new())
                }
            }
        }
    }

    /// Whether credit goes to the core: `Ok(false)` drops it while the pair reopens.
    pub(super) fn credit_gate(&self) -> BackendResult<bool> {
        match self.phase {
            Phase::Closed => Err(BackendError::Closed),
            Phase::Reopening => Ok(false),
            Phase::Ready => Ok(true),
        }
    }

    pub(super) fn begin_reopen(&mut self) {
        if self.phase == Phase::Ready {
            self.phase = Phase::Reopening;
        }
    }

    /// The new pair is open: it starts at the next generation with what was last requested,
    /// and the queue replays in order.
    pub(super) fn finish_reopen(&mut self, highest: u64) -> Vec<Effect> {
        self.generation += 1;
        self.sent = self.requested.clone();
        self.phase = Phase::Ready;
        let queued: Vec<Queued> = self.queue.drain(..).collect();
        self.queued_input = 0;
        self.queued_frames = 0;
        queued
            .into_iter()
            .flat_map(|queued| self.apply(highest, queued.generation, queued.request))
            .collect()
    }

    pub(super) fn close(&mut self) {
        self.phase = Phase::Closed;
        self.queue.clear();
        self.queued_input = 0;
        self.queued_frames = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(tag: &str) -> Display {
        Display {
            foreground: format!("#{tag}{tag}{tag}"),
            background: "#000000".to_string(),
            cursor_color: "#ffffff".to_string(),
            cell_width: 9,
            cell_height: 18,
        }
    }

    fn fence(watch_only: bool, automatic: bool) -> Fence {
        let mut fence = Fence::new(
            watch_only,
            ViewerSettings {
                size: (80, 24),
                display: display("aa"),
            },
        );
        fence.automatic = automatic;
        fence
    }

    fn send(out: Out) -> Effect {
        Effect::Send(out)
    }

    #[test]
    fn watch_only_requests_are_ok_no_ops() {
        let mut fence = fence(true, false);
        let mut highest = 0;
        for request in [
            Request::Write(b"x".to_vec()),
            Request::Resize(100, 30),
            Request::Focus,
            Request::SetDisplay(display("bb")),
        ] {
            assert_eq!(fence.request(&mut highest, 5, request), Ok(Vec::new()));
        }
        assert_eq!(highest, 0, "a watch-only attachment never raises the fence");
        assert_eq!(
            fence.write_automatic(BackendGeneration(1), b"r"),
            Ok(Vec::new())
        );
    }

    #[test]
    fn requests_while_reopening_are_queued_in_order() {
        let mut fence = fence(false, true);
        let mut highest = 0;
        fence.begin_reopen();
        fence
            .request(&mut highest, 0, Request::Resize(100, 30))
            .expect("queued");
        fence
            .request(&mut highest, 0, Request::Write(b"ab".to_vec()))
            .expect("queued");
        fence
            .request(&mut highest, 0, Request::Focus)
            .expect("queued");
        let effects = fence.finish_reopen(highest);
        assert_eq!(
            effects,
            vec![
                send(Out::Resize(100, 30)),
                send(Out::Data(b"ab".to_vec())),
                send(Out::Focus)
            ]
        );
        assert_eq!(fence.phase, Phase::Ready);
        assert_eq!(fence.generation, 2);
    }

    #[test]
    fn reopen_queue_refuses_the_1025th_request() {
        let mut fence = fence(false, true);
        let mut highest = 0;
        fence.begin_reopen();
        for _ in 0..REOPEN_QUEUE_REQUESTS {
            fence
                .request(&mut highest, 0, Request::Focus)
                .expect("room");
        }
        assert_eq!(
            fence.request(&mut highest, 0, Request::Focus),
            Err(BackendError::Failed("reopen queue full".to_string()))
        );
    }

    #[test]
    fn reopen_queue_refuses_input_past_64_kib() {
        let mut fence = fence(false, true);
        let mut highest = 0;
        fence.begin_reopen();
        fence
            .request(&mut highest, 0, Request::Write(vec![0; REOPEN_QUEUE_INPUT]))
            .expect("exactly the limit");
        assert_eq!(
            fence.request(&mut highest, 0, Request::Write(vec![0])),
            Err(BackendError::Failed("reopen queue full".to_string()))
        );
    }

    #[test]
    fn queued_requests_raise_the_highest_generation() {
        let mut fence = fence(false, true);
        let mut highest = 0;
        fence.begin_reopen();
        fence
            .request(&mut highest, 3, Request::Focus)
            .expect("queued");
        assert_eq!(highest, 3);
        fence
            .request(&mut highest, 1, Request::Focus)
            .expect("queued");
        assert_eq!(highest, 3);
        let effects = fence.finish_reopen(highest);
        assert_eq!(
            effects,
            vec![send(Out::Focus)],
            "the older focus is superseded"
        );
    }

    #[test]
    fn superseded_write_goes_out_as_automatic_input_once() {
        let mut fence = fence(false, true);
        let mut highest = 4;
        let effects = fence.request(&mut highest, 2, Request::Write(b"k".to_vec()));
        assert_eq!(effects, Ok(vec![send(Out::Automatic(b"k".to_vec()))]));
    }

    #[test]
    fn superseded_write_without_automatic_input_goes_out_as_data() {
        let mut fence = fence(false, false);
        let mut highest = 4;
        let effects = fence.request(&mut highest, 2, Request::Write(b"k".to_vec()));
        assert_eq!(effects, Ok(vec![send(Out::Data(b"k".to_vec()))]));
    }

    #[test]
    fn superseded_resize_and_display_update_the_latest_request_only() {
        let mut fence = fence(false, true);
        let mut highest = 4;
        assert_eq!(
            fence.request(&mut highest, 2, Request::Resize(100, 30)),
            Ok(Vec::new())
        );
        assert_eq!(
            fence.request(&mut highest, 2, Request::SetDisplay(display("bb"))),
            Ok(Vec::new())
        );
        assert_eq!(fence.requested.size, (100, 30));
        assert_eq!(fence.requested.display, display("bb"));
        assert_eq!(fence.sent.size, (80, 24));
        assert_eq!(fence.sent.display, display("aa"));
    }

    #[test]
    fn superseded_focus_is_a_no_op() {
        let mut fence = fence(false, true);
        let mut highest = 4;
        assert_eq!(
            fence.request(&mut highest, 2, Request::Focus),
            Ok(Vec::new())
        );
    }

    #[test]
    fn focus_reapplies_the_latest_display_and_size_before_focusing() {
        let mut fence = fence(false, true);
        let mut highest = 0;
        fence
            .request(&mut highest, 1, Request::Resize(100, 30))
            .expect("applies");
        fence.sent.size = (80, 24);
        fence.sent.display = display("aa");
        fence.requested.display = display("cc");
        let effects = fence
            .request(&mut highest, 1, Request::Focus)
            .expect("applies");
        assert_eq!(
            effects,
            vec![
                send(Out::Facts(display("cc"))),
                send(Out::Resize(100, 30)),
                send(Out::Focus)
            ]
        );
    }

    #[test]
    fn set_display_sends_facts_and_emits_facts() {
        let mut fence = fence(false, true);
        let mut highest = 0;
        let effects = fence.request(&mut highest, 1, Request::SetDisplay(display("bb")));
        assert_eq!(
            effects,
            Ok(vec![
                send(Out::Facts(display("bb"))),
                Effect::Emit(AttachEvent::Facts(display("bb")))
            ])
        );
    }

    #[test]
    fn write_automatic_needs_the_pair_generation_and_confirmation() {
        let mut confirmed = fence(false, true);
        assert_eq!(
            confirmed.write_automatic(BackendGeneration(1), b"r"),
            Ok(vec![send(Out::Automatic(b"r".to_vec()))])
        );
        assert_eq!(
            confirmed.write_automatic(BackendGeneration(2), b"r"),
            Ok(Vec::new())
        );
        let mut unconfirmed = fence(false, false);
        assert_eq!(
            unconfirmed.write_automatic(BackendGeneration(1), b"r"),
            Ok(Vec::new())
        );
        confirmed.begin_reopen();
        assert_eq!(
            confirmed.write_automatic(BackendGeneration(1), b"r"),
            Ok(Vec::new())
        );
    }

    #[test]
    fn write_automatic_on_a_closed_attachment_is_closed() {
        let mut fence = fence(false, true);
        fence.close();
        assert_eq!(
            fence.write_automatic(BackendGeneration(1), b"r"),
            Err(BackendError::Closed)
        );
    }

    #[test]
    fn frames_needed_bounds_the_frames_a_request_sends() {
        fn frames(effects: &[Effect]) -> usize {
            effects
                .iter()
                .map(|effect| match effect {
                    Effect::Send(Out::Data(bytes) | Out::Automatic(bytes)) => bytes
                        .len()
                        .div_ceil(super::super::frame::DATA_FRAME_MAX)
                        .max(1),
                    Effect::Send(_) => 1,
                    Effect::Emit(_) => 0,
                })
                .sum()
        }
        let mut fence = fence(false, true);
        let mut highest = 0;
        let mut seen = Vec::new();
        for (generation, request) in [
            (5, Request::Write(vec![0; 40_000])),
            (5, Request::Write(Vec::new())),
            (1, Request::Resize(100, 30)),
            (1, Request::SetDisplay(display("cc"))),
            (5, Request::Focus),
        ] {
            let needed = Fence::frames_needed(&request);
            let effects = fence
                .request(&mut highest, generation, request)
                .expect("applied");
            assert!(frames(&effects) <= needed, "{effects:?}");
            seen.push(frames(&effects));
        }
        assert_eq!(
            seen.last(),
            Some(&3),
            "the Focus after a superseded resize and display catches up with 3 frames"
        );
    }

    #[test]
    fn closed_attachment_refuses_requests() {
        let mut fence = fence(false, true);
        let mut highest = 0;
        fence.close();
        assert_eq!(
            fence.request(&mut highest, 1, Request::Focus),
            Err(BackendError::Closed)
        );
        assert_eq!(fence.credit_gate(), Err(BackendError::Closed));
        let mut watching = self::fence(true, false);
        watching.close();
        assert_eq!(
            watching.request(&mut highest, 1, Request::Focus),
            Err(BackendError::Closed)
        );
    }
}
