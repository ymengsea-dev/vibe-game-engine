//! Engine-facing keyboard/mouse input state.
//!
//! [`InputState`] accumulates [`PlatformEvent`]s into queryable state
//! (held keys/buttons, cursor position, scroll delta). It is a plain data
//! structure decoupled from the event loop: any caller feeds it events via
//! [`InputState::apply_event`] and calls [`InputState::end_frame`] once per
//! tick to advance edge-detection ("just pressed") state.

use std::collections::HashSet;

use crate::event::{KeyCode, MouseButton, PlatformEvent};

/// Accumulated keyboard/mouse input state for one frame.
#[derive(Debug, Clone, Default)]
pub struct InputState {
    held_keys: HashSet<KeyCode>,
    pressed_keys: HashSet<KeyCode>,
    released_keys: HashSet<KeyCode>,
    held_buttons: HashSet<MouseButton>,
    pressed_buttons: HashSet<MouseButton>,
    released_buttons: HashSet<MouseButton>,
    cursor_position: Option<(f64, f64)>,
    scroll_delta: (f32, f32),
}

impl InputState {
    /// An empty input state (nothing held, no cursor position yet).
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one platform event into the accumulated state.
    ///
    /// Call this for every [`PlatformEvent`] received (typically from
    /// [`crate::PlatformHandler::on_event`]) before querying state for the
    /// current frame.
    pub fn apply_event(&mut self, event: &PlatformEvent) {
        match *event {
            PlatformEvent::KeyboardInput {
                key,
                pressed,
                repeat,
            } => {
                if pressed {
                    if !repeat {
                        self.pressed_keys.insert(key);
                    }
                    self.held_keys.insert(key);
                } else {
                    self.held_keys.remove(&key);
                    self.released_keys.insert(key);
                }
            }
            PlatformEvent::MouseButtonInput { button, pressed } => {
                if pressed {
                    self.pressed_buttons.insert(button);
                    self.held_buttons.insert(button);
                } else {
                    self.held_buttons.remove(&button);
                    self.released_buttons.insert(button);
                }
            }
            PlatformEvent::CursorMoved { x, y } => {
                self.cursor_position = Some((x, y));
            }
            PlatformEvent::MouseWheel { delta_x, delta_y } => {
                self.scroll_delta.0 += delta_x;
                self.scroll_delta.1 += delta_y;
            }
            PlatformEvent::Resized { .. }
            | PlatformEvent::RedrawRequested
            | PlatformEvent::CloseRequested => {}
        }
    }

    /// Clears per-frame edge state (`just pressed` / `just released` /
    /// scroll delta). Call once per tick, after reading input for that
    /// tick and before the next batch of events arrives.
    ///
    /// Held keys/buttons and cursor position are *not* cleared — they
    /// persist until the corresponding release/move event.
    pub fn end_frame(&mut self) {
        self.pressed_keys.clear();
        self.released_keys.clear();
        self.pressed_buttons.clear();
        self.released_buttons.clear();
        self.scroll_delta = (0.0, 0.0);
    }

    /// `true` while `key` is held down.
    pub fn is_key_held(&self, key: KeyCode) -> bool {
        self.held_keys.contains(&key)
    }

    /// `true` on the tick `key` transitioned from released to pressed
    /// (excludes OS auto-repeat).
    pub fn is_key_pressed(&self, key: KeyCode) -> bool {
        self.pressed_keys.contains(&key)
    }

    /// `true` on the tick `key` transitioned from pressed to released.
    pub fn is_key_released(&self, key: KeyCode) -> bool {
        self.released_keys.contains(&key)
    }

    /// `true` while `button` is held down.
    pub fn is_mouse_button_held(&self, button: MouseButton) -> bool {
        self.held_buttons.contains(&button)
    }

    /// `true` on the tick `button` transitioned from released to pressed.
    pub fn is_mouse_button_pressed(&self, button: MouseButton) -> bool {
        self.pressed_buttons.contains(&button)
    }

    /// `true` on the tick `button` transitioned from pressed to released.
    pub fn is_mouse_button_released(&self, button: MouseButton) -> bool {
        self.released_buttons.contains(&button)
    }

    /// Last known cursor position in physical pixels, or `None` if the
    /// cursor hasn't moved over the window yet this session.
    pub fn cursor_position(&self) -> Option<(f64, f64)> {
        self.cursor_position
    }

    /// Accumulated scroll delta since the last [`InputState::end_frame`].
    pub fn scroll_delta(&self) -> (f32, f32) {
        self.scroll_delta
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_press_sets_held_and_pressed() {
        let mut input = InputState::new();
        input.apply_event(&PlatformEvent::KeyboardInput {
            key: KeyCode::KeyW,
            pressed: true,
            repeat: false,
        });
        assert!(input.is_key_held(KeyCode::KeyW));
        assert!(input.is_key_pressed(KeyCode::KeyW));
        assert!(!input.is_key_released(KeyCode::KeyW));
    }

    #[test]
    fn key_repeat_holds_but_does_not_re_trigger_pressed() {
        let mut input = InputState::new();
        input.apply_event(&PlatformEvent::KeyboardInput {
            key: KeyCode::KeyW,
            pressed: true,
            repeat: false,
        });
        input.end_frame();
        input.apply_event(&PlatformEvent::KeyboardInput {
            key: KeyCode::KeyW,
            pressed: true,
            repeat: true,
        });
        assert!(input.is_key_held(KeyCode::KeyW));
        assert!(!input.is_key_pressed(KeyCode::KeyW));
    }

    #[test]
    fn key_release_clears_held_sets_released() {
        let mut input = InputState::new();
        input.apply_event(&PlatformEvent::KeyboardInput {
            key: KeyCode::KeyW,
            pressed: true,
            repeat: false,
        });
        input.end_frame();
        input.apply_event(&PlatformEvent::KeyboardInput {
            key: KeyCode::KeyW,
            pressed: false,
            repeat: false,
        });
        assert!(!input.is_key_held(KeyCode::KeyW));
        assert!(input.is_key_released(KeyCode::KeyW));
    }

    #[test]
    fn end_frame_clears_edge_state_but_not_held_state() {
        let mut input = InputState::new();
        input.apply_event(&PlatformEvent::KeyboardInput {
            key: KeyCode::KeyW,
            pressed: true,
            repeat: false,
        });
        input.end_frame();
        assert!(input.is_key_held(KeyCode::KeyW));
        assert!(!input.is_key_pressed(KeyCode::KeyW));
    }

    #[test]
    fn mouse_button_lifecycle() {
        let mut input = InputState::new();
        input.apply_event(&PlatformEvent::MouseButtonInput {
            button: MouseButton::Left,
            pressed: true,
        });
        assert!(input.is_mouse_button_held(MouseButton::Left));
        assert!(input.is_mouse_button_pressed(MouseButton::Left));
        input.end_frame();
        input.apply_event(&PlatformEvent::MouseButtonInput {
            button: MouseButton::Left,
            pressed: false,
        });
        assert!(!input.is_mouse_button_held(MouseButton::Left));
        assert!(input.is_mouse_button_released(MouseButton::Left));
    }

    #[test]
    fn cursor_position_tracks_latest() {
        let mut input = InputState::new();
        assert_eq!(input.cursor_position(), None);
        input.apply_event(&PlatformEvent::CursorMoved { x: 1.0, y: 2.0 });
        input.apply_event(&PlatformEvent::CursorMoved { x: 3.0, y: 4.0 });
        assert_eq!(input.cursor_position(), Some((3.0, 4.0)));
    }

    #[test]
    fn scroll_delta_accumulates_and_resets_on_end_frame() {
        let mut input = InputState::new();
        input.apply_event(&PlatformEvent::MouseWheel {
            delta_x: 1.0,
            delta_y: 2.0,
        });
        input.apply_event(&PlatformEvent::MouseWheel {
            delta_x: 0.5,
            delta_y: -1.0,
        });
        assert_eq!(input.scroll_delta(), (1.5, 1.0));
        input.end_frame();
        assert_eq!(input.scroll_delta(), (0.0, 0.0));
    }

    #[test]
    fn non_input_events_are_ignored() {
        let mut input = InputState::new();
        input.apply_event(&PlatformEvent::Resized {
            width: 100,
            height: 100,
        });
        input.apply_event(&PlatformEvent::RedrawRequested);
        input.apply_event(&PlatformEvent::CloseRequested);
        assert_eq!(input.cursor_position(), None);
        assert_eq!(input.scroll_delta(), (0.0, 0.0));
    }
}
