//! The frame loop of a game shown in Studio's Scene View.
//!
//! Plays the part a window's event loop plays for a windowed game:
//! delivers Studio's input and resizes as [`PlatformEvent`]s, asks for a
//! redraw at a steady pace, and sends each finished frame back. Pause stops
//! the redraws (and mutes audio); Step runs exactly one frame of
//! [`super::STEP_SECONDS`]. Written against two small seams, [`Embedded`]
//! and [`Link`], so the loop is tested with a fake game and a scripted
//! connection, without a GPU or a socket.

use std::time::{Duration, Instant};

use engine_platform::{PlatformEvent, PlatformHandler};
use engine_renderer::CapturedFrame;

use super::protocol::{Frame, MAX_FRAME_SIDE, ToGame, ToStudio};
use super::{Incoming, Link, STEP_SECONDS};
use crate::app::{Game, GameRunner};

/// What the frame loop needs from the running game.
pub(crate) trait Embedded {
    /// Delivers one platform event, as a window would.
    fn deliver(&mut self, event: PlatformEvent);
    /// Whether the game asked to quit (or failed to start).
    fn should_exit(&self) -> bool;
    /// The newest finished frame, if any, without blocking.
    fn take_frame(&mut self) -> Option<CapturedFrame>;
    /// Enters or leaves the paused state.
    fn set_paused(&mut self, paused: bool);
    /// Makes the next redraw advance exactly `seconds`.
    fn prepare_step(&mut self, seconds: f32);
    /// A change in whether the game wants the cursor locked, once per change.
    fn cursor_lock(&self) -> Option<bool>;
    /// Last chance to save, as when a window closes.
    fn finish(&mut self);
}

impl<G: Game> Embedded for GameRunner<G> {
    fn deliver(&mut self, event: PlatformEvent) {
        self.on_event(event);
    }

    fn should_exit(&self) -> bool {
        PlatformHandler::should_exit(self)
    }

    fn take_frame(&mut self) -> Option<CapturedFrame> {
        GameRunner::take_frame(self)
    }

    fn set_paused(&mut self, paused: bool) {
        GameRunner::set_paused(self, paused);
    }

    fn prepare_step(&mut self, seconds: f32) {
        GameRunner::prepare_step(self, seconds);
    }

    fn cursor_lock(&self) -> Option<bool> {
        PlatformHandler::cursor_lock(self)
    }

    fn finish(&mut self) {
        self.on_exit();
    }
}

/// Whether Studio asked for a size the game can render.
pub(crate) fn valid_size(width: u32, height: u32) -> bool {
    (1..=MAX_FRAME_SIDE).contains(&width) && (1..=MAX_FRAME_SIDE).contains(&height)
}

/// Runs the game until Studio stops it, the game exits, or the connection
/// closes; every way out runs [`Embedded::finish`] once.
///
/// # Errors
///
/// An I/O error sending to Studio (after `finish` has run).
pub(crate) fn drive(
    game: &mut impl Embedded,
    link: &mut impl Link,
    interval: Duration,
) -> std::io::Result<()> {
    let mut paused = false;
    let mut sequence = 0u64;
    let mut next_frame = Instant::now();
    let result = loop {
        let mut step = false;
        let mut stop = false;
        loop {
            match link.poll() {
                Incoming::Message(ToGame::Resize { width, height }) => {
                    if valid_size(width, height) {
                        game.deliver(PlatformEvent::Resized { width, height });
                    } else {
                        tracing::warn!(width, height, "ignored an invalid preview size");
                    }
                }
                Incoming::Message(ToGame::Input(input)) => {
                    // A frozen game does not see the player's input.
                    if !paused {
                        game.deliver(input.into_platform());
                    }
                }
                Incoming::Message(ToGame::Pause) => {
                    if !paused {
                        paused = true;
                        game.set_paused(true);
                        if let Err(err) = link.send(&ToStudio::Paused) {
                            game.finish();
                            return Err(err);
                        }
                    }
                }
                Incoming::Message(ToGame::Resume) => {
                    if paused {
                        paused = false;
                        game.set_paused(false);
                        if let Err(err) = link.send(&ToStudio::Resumed) {
                            game.finish();
                            return Err(err);
                        }
                    }
                }
                Incoming::Message(ToGame::Step) => step |= paused,
                Incoming::Message(ToGame::Stop) => {
                    game.deliver(PlatformEvent::CloseRequested);
                    stop = true;
                    break;
                }
                Incoming::Closed => {
                    stop = true;
                    break;
                }
                Incoming::Empty => break,
            }
        }
        if stop || game.should_exit() {
            break Ok(());
        }
        if !paused || step {
            if step {
                game.prepare_step(STEP_SECONDS);
            }
            game.deliver(PlatformEvent::RedrawRequested);
        }
        if let Some(locked) = game.cursor_lock()
            && let Err(err) = link.send(&ToStudio::CursorLock { locked })
        {
            break Err(err);
        }
        if let Some(frame) = game.take_frame() {
            sequence += 1;
            let message = ToStudio::Frame(Frame {
                sequence,
                width: frame.width,
                height: frame.height,
                rgba: frame.rgba,
            });
            if let Err(err) = link.send(&message) {
                break Err(err);
            }
        }
        // Keep a steady pace; after a slow frame, start again from now
        // rather than rushing to catch up.
        next_frame += interval;
        let now = Instant::now();
        match next_frame.checked_duration_since(now) {
            Some(wait) => std::thread::sleep(wait),
            None => next_frame = now,
        }
    };
    game.finish();
    result
}

#[cfg(test)]
mod tests {
    use super::super::protocol::InputEvent;
    use super::*;
    use std::collections::VecDeque;

    /// Records what the loop asked of the game; a redraw "renders" a
    /// 1×1 frame that is ready on the next take.
    #[derive(Default)]
    struct FakeGame {
        events: Vec<PlatformEvent>,
        pending: Option<CapturedFrame>,
        paused: Vec<bool>,
        steps: Vec<f32>,
        finished: u32,
        exit_after_redraws: Option<usize>,
        lock_changes: std::cell::RefCell<VecDeque<bool>>,
    }

    impl FakeGame {
        fn redraws(&self) -> usize {
            self.events
                .iter()
                .filter(|event| **event == PlatformEvent::RedrawRequested)
                .count()
        }
    }

    impl Embedded for FakeGame {
        fn deliver(&mut self, event: PlatformEvent) {
            if event == PlatformEvent::RedrawRequested {
                self.pending = Some(CapturedFrame {
                    width: 1,
                    height: 1,
                    rgba: vec![9, 9, 9, 255],
                });
            }
            self.events.push(event);
        }
        fn should_exit(&self) -> bool {
            self.exit_after_redraws
                .is_some_and(|limit| self.redraws() >= limit)
        }
        fn take_frame(&mut self) -> Option<CapturedFrame> {
            self.pending.take()
        }
        fn set_paused(&mut self, paused: bool) {
            self.paused.push(paused);
        }
        fn prepare_step(&mut self, seconds: f32) {
            self.steps.push(seconds);
        }
        fn finish(&mut self) {
            self.finished += 1;
        }
        fn cursor_lock(&self) -> Option<bool> {
            self.lock_changes.borrow_mut().pop_front()
        }
    }

    /// Delivers one scripted batch per loop turn (`None` = nothing that
    /// turn), then closes; records what was sent.
    struct ScriptedLink {
        turns: VecDeque<Vec<ToGame>>,
        current: VecDeque<ToGame>,
        turn_started: bool,
        sent: Vec<ToStudio>,
    }

    impl ScriptedLink {
        fn new(turns: Vec<Vec<ToGame>>) -> Self {
            Self {
                turns: turns.into(),
                current: VecDeque::new(),
                turn_started: false,
                sent: Vec::new(),
            }
        }
        fn frames(&self) -> usize {
            self.sent
                .iter()
                .filter(|message| matches!(message, ToStudio::Frame(_)))
                .count()
        }
    }

    impl Link for ScriptedLink {
        fn poll(&mut self) -> Incoming {
            if !self.turn_started {
                let Some(turn) = self.turns.pop_front() else {
                    return Incoming::Closed;
                };
                self.current = turn.into();
                self.turn_started = true;
            }
            match self.current.pop_front() {
                Some(message) => Incoming::Message(message),
                None => {
                    self.turn_started = false;
                    Incoming::Empty
                }
            }
        }
        fn send(&mut self, message: &ToStudio) -> std::io::Result<()> {
            self.sent.push(message.clone());
            Ok(())
        }
    }

    #[test]
    fn every_turn_redraws_and_sends_the_frame_until_the_connection_closes() {
        let mut game = FakeGame::default();
        let mut link = ScriptedLink::new(vec![vec![], vec![], vec![]]);
        drive(&mut game, &mut link, Duration::ZERO).expect("drive");
        assert_eq!(game.redraws(), 3);
        assert_eq!(link.frames(), 3);
        let sequences: Vec<u64> = link
            .sent
            .iter()
            .filter_map(|message| match message {
                ToStudio::Frame(frame) => Some(frame.sequence),
                _ => None,
            })
            .collect();
        assert_eq!(sequences, [1, 2, 3]);
        assert_eq!(game.finished, 1, "a closed connection still saves");
    }

    #[test]
    fn pause_freezes_the_game_and_step_runs_exactly_one_frame() {
        let mut game = FakeGame::default();
        let key = InputEvent::Key {
            key: engine_platform::KeyCode::KeyW,
            pressed: true,
            repeat: false,
        };
        let mut link = ScriptedLink::new(vec![
            vec![ToGame::Pause],
            vec![ToGame::Input(key)],
            vec![ToGame::Step],
            vec![],
            vec![ToGame::Resume],
        ]);
        drive(&mut game, &mut link, Duration::ZERO).expect("drive");
        assert_eq!(game.redraws(), 2, "one step while paused, one after resume");
        assert_eq!(game.steps, [STEP_SECONDS]);
        assert_eq!(game.paused, [true, false]);
        assert!(
            !game.events.contains(&key.into_platform()),
            "input while paused is dropped"
        );
        assert!(link.sent.contains(&ToStudio::Paused));
        assert!(link.sent.contains(&ToStudio::Resumed));
    }

    #[test]
    fn stop_closes_the_game_like_a_window_and_saves_once() {
        let mut game = FakeGame::default();
        let mut link = ScriptedLink::new(vec![vec![], vec![ToGame::Stop], vec![]]);
        drive(&mut game, &mut link, Duration::ZERO).expect("drive");
        assert_eq!(game.events.last(), Some(&PlatformEvent::CloseRequested));
        assert_eq!(game.redraws(), 1, "nothing renders after Stop");
        assert_eq!(game.finished, 1);
    }

    #[test]
    fn input_and_resizes_reach_the_game_and_bad_sizes_do_not() {
        let mut game = FakeGame::default();
        let mut link = ScriptedLink::new(vec![vec![
            ToGame::Resize {
                width: 800,
                height: 600,
            },
            ToGame::Resize {
                width: 0,
                height: 600,
            },
            ToGame::Input(InputEvent::CursorMoved { x: 4.0, y: 2.0 }),
        ]]);
        drive(&mut game, &mut link, Duration::ZERO).expect("drive");
        assert_eq!(
            &game.events[..2],
            &[
                PlatformEvent::Resized {
                    width: 800,
                    height: 600
                },
                PlatformEvent::CursorMoved { x: 4.0, y: 2.0 },
            ]
        );
    }

    #[test]
    fn the_loop_ends_when_the_game_quits() {
        let mut game = FakeGame {
            exit_after_redraws: Some(2),
            ..FakeGame::default()
        };
        let mut link = ScriptedLink::new(vec![vec![]; 10]);
        drive(&mut game, &mut link, Duration::ZERO).expect("drive");
        assert_eq!(game.redraws(), 2);
        assert_eq!(game.finished, 1);
    }

    #[test]
    fn driver_sends_cursor_lock_when_the_game_asks() {
        let mut game = FakeGame::default();
        game.lock_changes.borrow_mut().push_back(true);
        let mut link = ScriptedLink::new(vec![vec![], vec![], vec![]]);
        drive(&mut game, &mut link, Duration::ZERO).expect("drive");
        let locks: Vec<_> = link
            .sent
            .iter()
            .filter(|message| matches!(message, ToStudio::CursorLock { .. }))
            .collect();
        assert_eq!(locks, [&ToStudio::CursorLock { locked: true }]);
    }
}
