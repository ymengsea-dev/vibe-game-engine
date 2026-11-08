//! Lua `engine.*` keyboard, mouse and gamepad calls.
//!
//! Reads come from the per-frame [`InputSnapshot`] the host installs with
//! `LuaRuntime::set_input`. Unknown names are Lua errors listing valid ones.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use mlua::{Lua, MultiValue, Table, Value};

use super::MAX_COMMANDS;
use crate::GameCommand;
use crate::input::{
    ButtonState, InputSnapshot, ScriptGamepadAxis, ScriptGamepadButton, ScriptKey,
    ScriptMouseButton,
};

type Commands = Rc<RefCell<Vec<GameCommand>>>;
type Input = Rc<RefCell<InputSnapshot>>;

fn error(message: String) -> mlua::Error {
    mlua::Error::RuntimeError(message)
}

/// Registers a `(name) -> bool` call that reads one flag of a named button.
macro_rules! button_query {
    ($lua:expr, $engine:expr, $input:expr, $call:literal, $ty:ty, $field:ident, $flag:ident) => {{
        let input = Rc::clone($input);
        $engine.set(
            $call,
            $lua.create_function(move |_, name: String| {
                let value =
                    <$ty>::from_name(&name).ok_or_else(|| error(<$ty>::unknown_message(&name)))?;
                let state: ButtonState = input
                    .borrow()
                    .$field
                    .get(&value)
                    .copied()
                    .unwrap_or_default();
                Ok(state.$flag)
            })?,
        )?;
    }};
}

fn axis_value(input: &InputSnapshot, axis: ScriptGamepadAxis) -> f32 {
    input.gamepad_axes.get(&axis).copied().unwrap_or(0.0)
}

fn key_down(input: &InputSnapshot, key: ScriptKey) -> f32 {
    if input.keys.get(&key).is_some_and(|state| state.down) {
        1.0
    } else {
        0.0
    }
}

pub(super) fn register(
    lua: &Lua,
    engine: &Table,
    commands: &Commands,
    input: &Input,
    active: &Rc<Cell<bool>>,
) -> mlua::Result<()> {
    button_query!(lua, engine, input, "key_down", ScriptKey, keys, down);
    button_query!(lua, engine, input, "key_pressed", ScriptKey, keys, pressed);
    button_query!(
        lua,
        engine,
        input,
        "key_released",
        ScriptKey,
        keys,
        released
    );
    button_query!(
        lua,
        engine,
        input,
        "mouse_down",
        ScriptMouseButton,
        mouse_buttons,
        down
    );
    button_query!(
        lua,
        engine,
        input,
        "mouse_pressed",
        ScriptMouseButton,
        mouse_buttons,
        pressed
    );
    button_query!(
        lua,
        engine,
        input,
        "mouse_released",
        ScriptMouseButton,
        mouse_buttons,
        released
    );
    button_query!(
        lua,
        engine,
        input,
        "gamepad_down",
        ScriptGamepadButton,
        gamepad_buttons,
        down
    );
    button_query!(
        lua,
        engine,
        input,
        "gamepad_pressed",
        ScriptGamepadButton,
        gamepad_buttons,
        pressed
    );

    let snapshot = Rc::clone(input);
    engine.set(
        "mouse_position",
        lua.create_function(move |_, ()| {
            Ok(match snapshot.borrow().mouse_position {
                Some((x, y)) => MultiValue::from_iter([
                    Value::Number(f64::from(x)),
                    Value::Number(f64::from(y)),
                ]),
                None => MultiValue::from_iter([Value::Nil]),
            })
        })?,
    )?;
    let snapshot = Rc::clone(input);
    engine.set(
        "mouse_delta",
        lua.create_function(move |_, ()| Ok(snapshot.borrow().mouse_delta))?,
    )?;
    let snapshot = Rc::clone(input);
    engine.set(
        "mouse_wheel",
        lua.create_function(move |_, ()| {
            let (x, y) = snapshot.borrow().wheel;
            Ok((y, x))
        })?,
    )?;

    let snapshot = Rc::clone(input);
    engine.set(
        "gamepad_axis",
        lua.create_function(move |_, name: String| {
            let axis = ScriptGamepadAxis::from_name(&name)
                .ok_or_else(|| error(ScriptGamepadAxis::unknown_message(&name)))?;
            Ok(axis_value(&snapshot.borrow(), axis))
        })?,
    )?;

    let snapshot = Rc::clone(input);
    engine.set(
        "input_axis",
        lua.create_function(move |_, name: String| {
            let input = snapshot.borrow();
            let pair = |positive: [ScriptKey; 2], negative: [ScriptKey; 2]| {
                let held =
                    |keys: [ScriptKey; 2]| key_down(&input, keys[0]).max(key_down(&input, keys[1]));
                held(positive) - held(negative)
            };
            let value = match name.as_str() {
                "horizontal" => {
                    pair(
                        [ScriptKey::D, ScriptKey::ArrowRight],
                        [ScriptKey::A, ScriptKey::ArrowLeft],
                    ) + axis_value(&input, ScriptGamepadAxis::LeftX)
                }
                // The platform reports LeftStickY positive-up (see
                // `engine_platform::GamepadAxis`), so it adds to "forward".
                "vertical" => {
                    pair(
                        [ScriptKey::W, ScriptKey::ArrowUp],
                        [ScriptKey::S, ScriptKey::ArrowDown],
                    ) + axis_value(&input, ScriptGamepadAxis::LeftY)
                }
                other => {
                    return Err(error(format!(
                        "unknown input axis '{other}'; expected one of: horizontal, vertical"
                    )));
                }
            };
            Ok(f64::from(value.clamp(-1.0, 1.0)))
        })?,
    )?;

    let (snapshot, pending, active) = (Rc::clone(input), Rc::clone(commands), Rc::clone(active));
    engine.set(
        "set_mouse_locked",
        lua.create_function(move |_, locked: bool| {
            if !active.get() {
                return Err(error("engine commands may only run in callbacks".into()));
            }
            let mut queue = pending.borrow_mut();
            if queue.len() >= MAX_COMMANDS {
                return Err(error("Lua command limit exceeded".into()));
            }
            queue.push(GameCommand::SetMouseLocked { locked });
            snapshot.borrow_mut().mouse_locked = locked;
            Ok(())
        })?,
    )?;
    let snapshot = Rc::clone(input);
    engine.set(
        "is_mouse_locked",
        lua.create_function(move |_, ()| Ok(snapshot.borrow().mouse_locked))?,
    )?;
    Ok(())
}
