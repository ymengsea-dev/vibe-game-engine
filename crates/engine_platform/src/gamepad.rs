//! Gamepad input.
//!
//! Unlike keyboard and mouse, gamepads do not arrive as `winit` events —
//! `gilrs` runs its own poll loop against the OS. [`Gamepads`] owns that
//! loop and drains it into an [`crate::InputState`] once per frame,
//! sitting beside `InputState::apply_event` rather than replacing it.
//!
//! ## Names
//!
//! Buttons are named by *position*, not by letter:
//! [`GamepadButton::South`] is the bottom face button — A on an Xbox pad,
//! Cross on a PlayStation one. Letter names swap between vendors and
//! reading `Button::A` in gameplay code tells you nothing about where a
//! thumb goes.
//!
//! ## One state for every pad
//!
//! Every connected pad merges into a single [`crate::InputState`]: any
//! pad pressing South counts as South pressed. That is what a
//! single-player game wants, and it means couch co-op needs a per-pad
//! split that does not exist yet.

use std::collections::HashSet;

/// A gamepad button, named by position on a standard layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GamepadButton {
    /// Bottom face button (Xbox A, PlayStation Cross).
    South,
    /// Right face button (Xbox B, PlayStation Circle).
    East,
    /// Left face button (Xbox X, PlayStation Square).
    West,
    /// Top face button (Xbox Y, PlayStation Triangle).
    North,
    /// Left shoulder bumper.
    LeftBumper,
    /// Right shoulder bumper.
    RightBumper,
    /// The "back"/"view"/"share" button.
    Select,
    /// The "start"/"menu"/"options" button.
    Start,
    /// Pressing the left stick in.
    LeftStick,
    /// Pressing the right stick in.
    RightStick,
    /// D-pad up.
    DPadUp,
    /// D-pad down.
    DPadDown,
    /// D-pad left.
    DPadLeft,
    /// D-pad right.
    DPadRight,
}

/// An analog gamepad axis.
///
/// Sticks report `[-1, 1]`; triggers report `[0, 1]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GamepadAxis {
    /// Left stick horizontal. Positive is right.
    LeftStickX,
    /// Left stick vertical. Positive is up.
    LeftStickY,
    /// Right stick horizontal. Positive is right.
    RightStickX,
    /// Right stick vertical. Positive is up.
    RightStickY,
    /// Left trigger pull, `[0, 1]`.
    LeftTrigger,
    /// Right trigger pull, `[0, 1]`.
    RightTrigger,
}

impl GamepadAxis {
    /// Every axis, for iteration.
    pub const ALL: [GamepadAxis; 6] = [
        GamepadAxis::LeftStickX,
        GamepadAxis::LeftStickY,
        GamepadAxis::RightStickX,
        GamepadAxis::RightStickY,
        GamepadAxis::LeftTrigger,
        GamepadAxis::RightTrigger,
    ];

    /// Index into [`GamepadAxes`]' backing array.
    fn index(self) -> usize {
        match self {
            GamepadAxis::LeftStickX => 0,
            GamepadAxis::LeftStickY => 1,
            GamepadAxis::RightStickX => 2,
            GamepadAxis::RightStickY => 3,
            GamepadAxis::LeftTrigger => 4,
            GamepadAxis::RightTrigger => 5,
        }
    }
}

/// Which stick, for the two-axis [`crate::InputState::stick`] query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stick {
    /// The left thumbstick.
    Left,
    /// The right thumbstick.
    Right,
}

impl Stick {
    /// This stick's horizontal and vertical axes.
    pub fn axes(self) -> (GamepadAxis, GamepadAxis) {
        match self {
            Stick::Left => (GamepadAxis::LeftStickX, GamepadAxis::LeftStickY),
            Stick::Right => (GamepadAxis::RightStickX, GamepadAxis::RightStickY),
        }
    }
}

/// Raw axis values, before any deadzone is applied.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GamepadAxes([f32; 6]);

impl GamepadAxes {
    /// This axis's raw value.
    pub fn get(&self, axis: GamepadAxis) -> f32 {
        self.0[axis.index()]
    }

    /// Sets this axis's raw value, ignoring a non-finite reading rather
    /// than letting a `NaN` propagate into movement maths.
    pub fn set(&mut self, axis: GamepadAxis, value: f32) {
        if value.is_finite() {
            self.0[axis.index()] = value.clamp(-1.0, 1.0);
        }
    }

    /// Zeroes every axis — what a disconnect must do, so a pad unplugged
    /// mid-push does not leave the player walking forever.
    pub fn clear(&mut self) {
        self.0 = [0.0; 6];
    }
}

/// The default radial deadzone: below this fraction of full deflection, a
/// stick reads as centred.
///
/// Sticks rest slightly off-centre and wear worsens it; without a
/// deadzone a game drifts on its own.
pub const DEFAULT_DEADZONE: f32 = 0.15;

/// Applies a radial deadzone to a stick pair and rescales the remainder.
///
/// Radial, not per-axis, for two reasons. A per-axis deadzone carves out
/// a *square* dead region, so a stick pushed diagonally registers before
/// one pushed straight up. And rescaling matters: without it, the moment
/// input passes the threshold it jumps straight to `deadzone` magnitude
/// instead of easing up from zero.
///
/// Returns `(0, 0)` inside the zone, and outside it a vector running
/// smoothly from just above zero to full deflection.
pub fn apply_radial_deadzone(x: f32, y: f32, deadzone: f32) -> (f32, f32) {
    if !x.is_finite() || !y.is_finite() {
        return (0.0, 0.0);
    }
    let deadzone = deadzone.clamp(0.0, 0.99);
    let magnitude = (x * x + y * y).sqrt();
    if magnitude <= deadzone || magnitude <= f32::EPSILON {
        return (0.0, 0.0);
    }
    // Remap [deadzone, 1] onto [0, 1] so there is no step at the edge.
    let scaled = ((magnitude - deadzone) / (1.0 - deadzone)).min(1.0);
    let factor = scaled / magnitude;
    (x * factor, y * factor)
}

/// Applies a deadzone to a single axis, for triggers and lone axes.
pub fn apply_axis_deadzone(value: f32, deadzone: f32) -> f32 {
    if !value.is_finite() {
        return 0.0;
    }
    let deadzone = deadzone.clamp(0.0, 0.99);
    let magnitude = value.abs();
    if magnitude <= deadzone {
        return 0.0;
    }
    let scaled = ((magnitude - deadzone) / (1.0 - deadzone)).min(1.0);
    scaled * value.signum()
}

/// Translates a gilrs button into ours, or `None` for one we do not
/// surface (paddles, vendor buttons).
fn translate_button(button: gilrs::Button) -> Option<GamepadButton> {
    Some(match button {
        gilrs::Button::South => GamepadButton::South,
        gilrs::Button::East => GamepadButton::East,
        gilrs::Button::West => GamepadButton::West,
        gilrs::Button::North => GamepadButton::North,
        gilrs::Button::LeftTrigger => GamepadButton::LeftBumper,
        gilrs::Button::RightTrigger => GamepadButton::RightBumper,
        gilrs::Button::Select => GamepadButton::Select,
        gilrs::Button::Start => GamepadButton::Start,
        gilrs::Button::LeftThumb => GamepadButton::LeftStick,
        gilrs::Button::RightThumb => GamepadButton::RightStick,
        gilrs::Button::DPadUp => GamepadButton::DPadUp,
        gilrs::Button::DPadDown => GamepadButton::DPadDown,
        gilrs::Button::DPadLeft => GamepadButton::DPadLeft,
        gilrs::Button::DPadRight => GamepadButton::DPadRight,
        _ => return None,
    })
}

/// Translates a gilrs axis into ours.
fn translate_axis(axis: gilrs::Axis) -> Option<GamepadAxis> {
    Some(match axis {
        gilrs::Axis::LeftStickX => GamepadAxis::LeftStickX,
        gilrs::Axis::LeftStickY => GamepadAxis::LeftStickY,
        gilrs::Axis::RightStickX => GamepadAxis::RightStickX,
        gilrs::Axis::RightStickY => GamepadAxis::RightStickY,
        _ => return None,
    })
}

/// What one poll of the gamepad backend produced.
///
/// A plain value rather than a direct mutation of [`crate::InputState`],
/// so the merge step is testable without any hardware.
#[derive(Debug, Clone, Default)]
pub struct GamepadUpdate {
    /// Buttons that went down this poll.
    pub pressed: HashSet<GamepadButton>,
    /// Buttons that came up this poll.
    pub released: HashSet<GamepadButton>,
    /// Current axis values, raw.
    pub axes: GamepadAxes,
    /// How many pads are connected after this poll.
    pub connected: usize,
    /// Whether a pad connected or disconnected during it.
    pub topology_changed: bool,
}

/// Owns the gamepad backend and turns its events into [`GamepadUpdate`]s.
///
/// Construction never fails hard: a machine with no gamepad support (or a
/// backend that refuses to start) logs once and every poll afterwards
/// reports nothing connected, so a game runs keyboard-only rather than
/// not at all.
pub struct Gamepads {
    backend: Option<gilrs::Gilrs>,
    axes: GamepadAxes,
}

impl Gamepads {
    /// Starts the gamepad backend, or falls back to a no-op.
    pub fn new() -> Self {
        match gilrs::Gilrs::new() {
            Ok(backend) => {
                let connected = backend.gamepads().count();
                if connected > 0 {
                    tracing::info!(connected, "gamepad backend ready");
                }
                Self {
                    backend: Some(backend),
                    axes: GamepadAxes::default(),
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, "no gamepad support; running without");
                Self {
                    backend: None,
                    axes: GamepadAxes::default(),
                }
            }
        }
    }

    /// Whether a backend is actually running.
    pub fn is_available(&self) -> bool {
        self.backend.is_some()
    }

    /// Drains pending events and reports the frame's gamepad changes.
    ///
    /// Call once per frame. Cheap and silent when nothing is connected.
    pub fn poll(&mut self) -> GamepadUpdate {
        let Some(backend) = self.backend.as_mut() else {
            return GamepadUpdate::default();
        };

        let mut update = GamepadUpdate::default();
        while let Some(event) = backend.next_event() {
            match event.event {
                gilrs::EventType::ButtonPressed(button, _) => {
                    if let Some(button) = translate_button(button) {
                        update.pressed.insert(button);
                    }
                }
                gilrs::EventType::ButtonReleased(button, _) => {
                    if let Some(button) = translate_button(button) {
                        update.released.insert(button);
                    }
                }
                gilrs::EventType::AxisChanged(axis, value, _) => {
                    if let Some(axis) = translate_axis(axis) {
                        self.axes.set(axis, value);
                    }
                }
                gilrs::EventType::ButtonChanged(button, value, _) => {
                    // Triggers arrive as analog button changes.
                    match button {
                        gilrs::Button::LeftTrigger2 => {
                            self.axes.set(GamepadAxis::LeftTrigger, value)
                        }
                        gilrs::Button::RightTrigger2 => {
                            self.axes.set(GamepadAxis::RightTrigger, value)
                        }
                        _ => {}
                    }
                }
                gilrs::EventType::Connected => {
                    update.topology_changed = true;
                    tracing::info!("gamepad connected");
                }
                gilrs::EventType::Disconnected => {
                    update.topology_changed = true;
                    // Whatever was held is no longer held. Without this a
                    // pad unplugged mid-push leaves the player walking.
                    self.axes.clear();
                    tracing::info!("gamepad disconnected");
                }
                _ => {}
            }
        }

        update.connected = backend.gamepads().count();
        if update.connected == 0 {
            self.axes.clear();
        }
        update.axes = self.axes;
        update
    }
}

impl Default for Gamepads {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_stick_deflection_reads_as_centred() {
        // Resting drift must not move the player.
        let (x, y) = apply_radial_deadzone(0.05, 0.03, DEFAULT_DEADZONE);
        assert_eq!((x, y), (0.0, 0.0));
    }

    #[test]
    fn full_deflection_survives_the_deadzone() {
        let (x, y) = apply_radial_deadzone(1.0, 0.0, DEFAULT_DEADZONE);
        assert!(
            (x - 1.0).abs() < 1e-5,
            "full push should stay full, got {x}"
        );
        assert_eq!(y, 0.0);
    }

    #[test]
    fn deadzone_is_radial_not_square() {
        // A diagonal push whose components are each under the threshold
        // but whose magnitude is over it must register. A per-axis
        // deadzone would wrongly zero this.
        let deadzone = 0.15;
        let component = 0.12;
        let magnitude = (component * component * 2.0f32).sqrt();
        assert!(component < deadzone && magnitude > deadzone);

        let (x, y) = apply_radial_deadzone(component, component, deadzone);
        assert!(
            x > 0.0 && y > 0.0,
            "diagonal should register, got ({x}, {y})"
        );
    }

    #[test]
    fn output_eases_up_from_zero_at_the_threshold() {
        // Just outside the deadzone the result must be near zero, not a
        // jump to the deadzone's own magnitude.
        let (x, _) = apply_radial_deadzone(0.16, 0.0, 0.15);
        assert!(x > 0.0 && x < 0.05, "expected a small value, got {x}");
    }

    #[test]
    fn deadzone_handles_non_finite_input() {
        assert_eq!(apply_radial_deadzone(f32::NAN, 0.5, 0.15), (0.0, 0.0));
        assert_eq!(apply_radial_deadzone(0.5, f32::INFINITY, 0.15), (0.0, 0.0));
        assert_eq!(apply_axis_deadzone(f32::NAN, 0.15), 0.0);
    }

    #[test]
    fn a_zero_deadzone_passes_everything_through() {
        let (x, y) = apply_radial_deadzone(0.01, 0.0, 0.0);
        assert!(x > 0.0, "with no deadzone even tiny input counts");
        assert_eq!(y, 0.0);
    }

    #[test]
    fn axis_deadzone_keeps_the_sign() {
        assert!(apply_axis_deadzone(-0.8, 0.15) < 0.0);
        assert_eq!(apply_axis_deadzone(-0.05, 0.15), 0.0);
    }

    #[test]
    fn axes_reject_non_finite_values() {
        let mut axes = GamepadAxes::default();
        axes.set(GamepadAxis::LeftStickX, 0.5);
        axes.set(GamepadAxis::LeftStickX, f32::NAN);
        assert_eq!(
            axes.get(GamepadAxis::LeftStickX),
            0.5,
            "a NaN reading must not overwrite a good one",
        );
    }

    #[test]
    fn axes_clamp_out_of_range_values() {
        let mut axes = GamepadAxes::default();
        axes.set(GamepadAxis::LeftStickY, 5.0);
        assert_eq!(axes.get(GamepadAxis::LeftStickY), 1.0);
    }

    #[test]
    fn clearing_zeroes_every_axis() {
        let mut axes = GamepadAxes::default();
        for axis in GamepadAxis::ALL {
            axes.set(axis, 0.9);
        }
        axes.clear();
        for axis in GamepadAxis::ALL {
            assert_eq!(axes.get(axis), 0.0, "{axis:?} survived a disconnect");
        }
    }

    #[test]
    fn stick_maps_to_its_two_axes() {
        assert_eq!(
            Stick::Left.axes(),
            (GamepadAxis::LeftStickX, GamepadAxis::LeftStickY)
        );
        assert_eq!(
            Stick::Right.axes(),
            (GamepadAxis::RightStickX, GamepadAxis::RightStickY)
        );
    }
}
