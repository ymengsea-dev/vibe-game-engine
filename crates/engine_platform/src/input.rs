//! Engine-facing keyboard/mouse input state.
//!
//! [`InputState`] accumulates [`PlatformEvent`]s into queryable state
//! (held keys/buttons, cursor position, scroll delta). It is a plain data
//! structure decoupled from the event loop: any caller feeds it events via
//! [`InputState::apply_event`] and calls [`InputState::end_frame`] once per
//! tick to advance edge-detection ("just pressed") state.

use std::collections::HashSet;

use crate::event::{KeyCode, MouseButton, PlatformEvent};
use crate::gamepad::{
    DEFAULT_DEADZONE, GamepadAxes, GamepadAxis, GamepadButton, GamepadUpdate, Stick,
    apply_axis_deadzone, apply_radial_deadzone,
};

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

    held_gamepad: HashSet<GamepadButton>,
    pressed_gamepad: HashSet<GamepadButton>,
    released_gamepad: HashSet<GamepadButton>,
    gamepad_axes: GamepadAxes,
    connected_gamepads: usize,
    deadzone: f32,
}

impl InputState {
    /// An empty input state (nothing held, no cursor position yet), with
    /// the default stick deadzone.
    pub fn new() -> Self {
        Self {
            deadzone: DEFAULT_DEADZONE,
            ..Self::default()
        }
    }

    /// Merges one frame's gamepad poll into this state.
    ///
    /// Call once per frame alongside [`InputState::apply_event`], before
    /// querying. Every connected pad merges into one state — see the
    /// `gamepad` module docs.
    pub fn apply_gamepad(&mut self, update: &GamepadUpdate) {
        for &button in &update.pressed {
            self.pressed_gamepad.insert(button);
            self.held_gamepad.insert(button);
        }
        for &button in &update.released {
            self.held_gamepad.remove(&button);
            self.released_gamepad.insert(button);
        }
        self.gamepad_axes = update.axes;
        self.connected_gamepads = update.connected;

        // Nothing plugged in means nothing held. Without this, unplugging
        // a pad mid-press leaves the button stuck down forever.
        if update.connected == 0 {
            for button in self.held_gamepad.drain() {
                self.released_gamepad.insert(button);
            }
        }
    }

    /// How many gamepads are currently connected.
    pub fn connected_gamepads(&self) -> usize {
        self.connected_gamepads
    }

    /// The stick deadzone, as a fraction of full deflection.
    pub fn deadzone(&self) -> f32 {
        self.deadzone
    }

    /// Sets the stick deadzone. Clamped to a sane range — a deadzone of
    /// `1.0` would make the stick unusable.
    pub fn set_deadzone(&mut self, deadzone: f32) {
        self.deadzone = if deadzone.is_finite() {
            deadzone.clamp(0.0, 0.9)
        } else {
            DEFAULT_DEADZONE
        };
    }

    /// Whether `button` is currently down on any connected pad.
    pub fn is_button_held(&self, button: GamepadButton) -> bool {
        self.held_gamepad.contains(&button)
    }

    /// Whether `button` went down this frame. True for exactly one frame
    /// per press.
    pub fn is_button_pressed(&self, button: GamepadButton) -> bool {
        self.pressed_gamepad.contains(&button)
    }

    /// Whether `button` came up this frame.
    pub fn is_button_released(&self, button: GamepadButton) -> bool {
        self.released_gamepad.contains(&button)
    }

    /// One axis's value with the deadzone applied.
    ///
    /// For a thumbstick prefer [`InputState::stick`], which deadzones the
    /// pair together — see `apply_radial_deadzone` for why that matters.
    pub fn axis(&self, axis: GamepadAxis) -> f32 {
        apply_axis_deadzone(self.gamepad_axes.get(axis), self.deadzone)
    }

    /// A thumbstick as an `(x, y)` pair, radially deadzoned.
    ///
    /// `(0.0, 0.0)` when the stick is at rest or nothing is connected.
    pub fn stick(&self, stick: Stick) -> (f32, f32) {
        let (x_axis, y_axis) = stick.axes();
        apply_radial_deadzone(
            self.gamepad_axes.get(x_axis),
            self.gamepad_axes.get(y_axis),
            self.deadzone,
        )
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
            // Nothing here tracks window lifecycle or file drops.
            PlatformEvent::Resized { .. }
            | PlatformEvent::RedrawRequested
            | PlatformEvent::CloseRequested
            | PlatformEvent::FileDropped { .. } => {}
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
        self.pressed_gamepad.clear();
        self.released_gamepad.clear();
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

    // --- T-09: gamepad state ----------------------------------------

    fn update_with(pressed: &[GamepadButton], released: &[GamepadButton]) -> GamepadUpdate {
        GamepadUpdate {
            pressed: pressed.iter().copied().collect(),
            released: released.iter().copied().collect(),
            axes: GamepadAxes::default(),
            connected: 1,
            topology_changed: false,
        }
    }

    #[test]
    fn button_pressed_is_edge_triggered_for_one_frame() {
        let mut input = InputState::new();
        input.apply_gamepad(&update_with(&[GamepadButton::South], &[]));

        assert!(input.is_button_pressed(GamepadButton::South));
        assert!(input.is_button_held(GamepadButton::South));

        input.end_frame();
        assert!(
            !input.is_button_pressed(GamepadButton::South),
            "pressed must be true for exactly one frame",
        );
        assert!(
            input.is_button_held(GamepadButton::South),
            "but it is still held until released",
        );
    }

    #[test]
    fn releasing_clears_held_and_reports_the_edge() {
        let mut input = InputState::new();
        input.apply_gamepad(&update_with(&[GamepadButton::South], &[]));
        input.end_frame();
        input.apply_gamepad(&update_with(&[], &[GamepadButton::South]));

        assert!(input.is_button_released(GamepadButton::South));
        assert!(!input.is_button_held(GamepadButton::South));
    }

    #[test]
    fn disconnect_releases_everything_that_was_held() {
        let mut input = InputState::new();
        input.apply_gamepad(&update_with(&[GamepadButton::South], &[]));
        input.end_frame();

        // The pad goes away mid-press.
        input.apply_gamepad(&GamepadUpdate {
            connected: 0,
            ..GamepadUpdate::default()
        });

        assert!(
            !input.is_button_held(GamepadButton::South),
            "an unplugged pad must not leave a button stuck down",
        );
        assert!(input.is_button_released(GamepadButton::South));
        assert_eq!(input.connected_gamepads(), 0);
    }

    #[test]
    fn disconnect_zeroes_axes() {
        let mut axes = GamepadAxes::default();
        axes.set(GamepadAxis::LeftStickX, 0.9);
        let mut input = InputState::new();
        input.apply_gamepad(&GamepadUpdate {
            axes,
            connected: 1,
            ..GamepadUpdate::default()
        });
        assert!(input.axis(GamepadAxis::LeftStickX) > 0.0);

        input.apply_gamepad(&GamepadUpdate {
            connected: 0,
            ..GamepadUpdate::default()
        });
        assert_eq!(
            input.stick(Stick::Left),
            (0.0, 0.0),
            "a disconnected pad must not keep pushing the player",
        );
    }

    #[test]
    fn stick_drift_below_the_deadzone_reads_exactly_zero() {
        let mut axes = GamepadAxes::default();
        axes.set(GamepadAxis::LeftStickX, 0.04);
        axes.set(GamepadAxis::LeftStickY, -0.06);
        let mut input = InputState::new();
        input.apply_gamepad(&GamepadUpdate {
            axes,
            connected: 1,
            ..GamepadUpdate::default()
        });
        assert_eq!(input.stick(Stick::Left), (0.0, 0.0));
    }

    #[test]
    fn deadzone_is_clamped_to_something_usable() {
        let mut input = InputState::new();
        input.set_deadzone(5.0);
        assert!(
            input.deadzone() <= 0.9,
            "a deadzone of 1 would kill the stick"
        );
        input.set_deadzone(-1.0);
        assert_eq!(input.deadzone(), 0.0);
        input.set_deadzone(f32::NAN);
        assert_eq!(input.deadzone(), DEFAULT_DEADZONE);
    }

    #[test]
    fn no_gamepad_connected_is_silent_and_empty() {
        let input = InputState::new();
        assert_eq!(input.connected_gamepads(), 0);
        assert!(!input.is_button_held(GamepadButton::South));
        assert_eq!(input.stick(Stick::Left), (0.0, 0.0));
        assert_eq!(input.axis(GamepadAxis::RightTrigger), 0.0);
    }
}
