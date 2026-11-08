//! Plain-data input snapshot that hosts hand to the Lua runtime each frame.
//!
//! This file has no Lua dependency so it compiles without the `lua` feature.

use std::collections::HashMap;

/// Defines a fieldless enum whose Lua-visible names are given explicitly.
macro_rules! named_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident [$what:literal] {
            $($(#[$vmeta:meta])* $variant:ident => $text:literal),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        $vis enum $name {
            $($(#[$vmeta])* $variant),+
        }

        impl $name {
            /// Every value, in the order listed in error messages.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// The exact name Lua scripts use.
            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $text),+
                }
            }

            /// Looks a value up by its Lua name.
            pub fn from_name(name: &str) -> Option<Self> {
                match name {
                    $($text => Some(Self::$variant),)+
                    _ => None,
                }
            }

            /// Lua error text for an unknown name, listing the valid ones.
            pub fn unknown_message(name: &str) -> String {
                let choices = Self::ALL
                    .iter()
                    .map(|value| value.name())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(concat!("unknown ", $what, " '{}'; expected one of: {}"), name, choices)
            }
        }
    };
}

named_enum! {
    /// Keyboard keys exposed to Lua by their exact names.
    pub enum ScriptKey ["key"] {
        /// A key.
        A => "A",
        /// D key.
        D => "D",
        /// S key.
        S => "S",
        /// W key.
        W => "W",
        /// Space bar.
        Space => "Space",
        /// Escape key.
        Escape => "Escape",
        /// Enter key.
        Enter => "Enter",
        /// Left arrow.
        ArrowLeft => "ArrowLeft",
        /// Right arrow.
        ArrowRight => "ArrowRight",
        /// Up arrow.
        ArrowUp => "ArrowUp",
        /// Down arrow.
        ArrowDown => "ArrowDown",
        /// Left Shift.
        ShiftLeft => "ShiftLeft",
        /// Right Shift.
        ShiftRight => "ShiftRight",
        /// B key.
        B => "B",
        /// C key.
        C => "C",
        /// E key.
        E => "E",
        /// F key.
        F => "F",
        /// G key.
        G => "G",
        /// H key.
        H => "H",
        /// I key.
        I => "I",
        /// J key.
        J => "J",
        /// K key.
        K => "K",
        /// L key.
        L => "L",
        /// M key.
        M => "M",
        /// N key.
        N => "N",
        /// O key.
        O => "O",
        /// P key.
        P => "P",
        /// Q key.
        Q => "Q",
        /// R key.
        R => "R",
        /// T key.
        T => "T",
        /// U key.
        U => "U",
        /// V key.
        V => "V",
        /// X key.
        X => "X",
        /// Y key.
        Y => "Y",
        /// Z key.
        Z => "Z",
        /// Top-row 0.
        Digit0 => "Digit0",
        /// Top-row 1.
        Digit1 => "Digit1",
        /// Top-row 2.
        Digit2 => "Digit2",
        /// Top-row 3.
        Digit3 => "Digit3",
        /// Top-row 4.
        Digit4 => "Digit4",
        /// Top-row 5.
        Digit5 => "Digit5",
        /// Top-row 6.
        Digit6 => "Digit6",
        /// Top-row 7.
        Digit7 => "Digit7",
        /// Top-row 8.
        Digit8 => "Digit8",
        /// Top-row 9.
        Digit9 => "Digit9",
        /// Tab key.
        Tab => "Tab",
        /// Backspace key.
        Backspace => "Backspace",
        /// Left Control.
        ControlLeft => "ControlLeft",
        /// Right Control.
        ControlRight => "ControlRight",
        /// Left Alt/Option.
        AltLeft => "AltLeft",
        /// Right Alt/Option.
        AltRight => "AltRight",
    }
}

named_enum! {
    /// Mouse buttons exposed to Lua.
    pub enum ScriptMouseButton ["mouse button"] {
        /// Primary button.
        Left => "left",
        /// Secondary button.
        Right => "right",
        /// Middle button / wheel click.
        Middle => "middle",
    }
}

named_enum! {
    /// Gamepad buttons exposed to Lua.
    pub enum ScriptGamepadButton ["gamepad button"] {
        /// Bottom face button.
        South => "south",
        /// Right face button.
        East => "east",
        /// Left face button.
        West => "west",
        /// Top face button.
        North => "north",
        /// Start / Menu.
        Start => "start",
        /// Select / Back.
        Select => "select",
        /// Left bumper.
        LeftShoulder => "left_shoulder",
        /// Right bumper.
        RightShoulder => "right_shoulder",
        /// Left stick click.
        LeftStick => "left_stick",
        /// Right stick click.
        RightStick => "right_stick",
        /// D-pad up.
        DPadUp => "dpad_up",
        /// D-pad down.
        DPadDown => "dpad_down",
        /// D-pad left.
        DPadLeft => "dpad_left",
        /// D-pad right.
        DPadRight => "dpad_right",
    }
}

named_enum! {
    /// Gamepad axes exposed to Lua.
    pub enum ScriptGamepadAxis ["gamepad axis"] {
        /// Left stick horizontal, positive right.
        LeftX => "left_x",
        /// Left stick vertical, positive up.
        LeftY => "left_y",
        /// Right stick horizontal, positive right.
        RightX => "right_x",
        /// Right stick vertical, positive up.
        RightY => "right_y",
        /// Left trigger, 0 to 1.
        LeftTrigger => "left_trigger",
        /// Right trigger, 0 to 1.
        RightTrigger => "right_trigger",
    }
}

/// Held / just-pressed / just-released flags for one button or key.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ButtonState {
    /// Currently held.
    pub down: bool,
    /// Went down this frame.
    pub pressed: bool,
    /// Went up this frame.
    pub released: bool,
}

/// Everything a Lua script can read about input during one callback.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InputSnapshot {
    /// Keyboard state; absent keys are up.
    pub keys: HashMap<ScriptKey, ButtonState>,
    /// Mouse button state; absent buttons are up.
    pub mouse_buttons: HashMap<ScriptMouseButton, ButtonState>,
    /// Cursor position in game-frame pixels, if known.
    pub mouse_position: Option<(f32, f32)>,
    /// Raw mouse motion this frame.
    pub mouse_delta: (f32, f32),
    /// Wheel scroll this frame as `(x, y)`.
    pub wheel: (f32, f32),
    /// Gamepad button state; absent buttons are up.
    pub gamepad_buttons: HashMap<ScriptGamepadButton, ButtonState>,
    /// Gamepad axes, already dead-zoned by the platform.
    pub gamepad_axes: HashMap<ScriptGamepadAxis, f32>,
    /// Whether the game currently wants the mouse locked.
    pub mouse_locked: bool,
}
