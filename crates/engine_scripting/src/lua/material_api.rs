//! Lua `engine.set_material_value`: sets a shader graph parameter on one
//! object's material.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use mlua::{Lua, MultiValue, Table, Value};

use super::{MAX_COMMANDS, string_arg};
use crate::GameCommand;

type Commands = Rc<RefCell<Vec<GameCommand>>>;

/// The longest parameter name a script may give, matching the material
/// file's limit.
const MAX_NAME_BYTES: usize = 32;

fn error(message: &str) -> mlua::Error {
    mlua::Error::RuntimeError(message.to_owned())
}

fn component(value: Value) -> mlua::Result<f32> {
    let number = match value {
        Value::Number(n) => n,
        Value::Integer(n) => n as f64,
        _ => return Err(error("material values are numbers")),
    };
    if !number.is_finite() || number.abs() > f64::from(f32::MAX) {
        return Err(error("material values must be finite"));
    }
    Ok(number as f32)
}

/// A number (every component), or a colour table `{ r, g, b, a }` (`a`
/// defaults to `1`).
fn value_arg(value: Option<Value>) -> mlua::Result<[f32; 4]> {
    match value {
        Some(Value::Table(table)) => {
            let part = |key: &str, default: Option<f32>| -> mlua::Result<f32> {
                match (table.get::<Value>(key)?, default) {
                    (Value::Nil, Some(default)) => Ok(default),
                    (Value::Nil, None) => Err(error("a colour needs r, g and b")),
                    (value, _) => component(value),
                }
            };
            Ok([
                part("r", None)?,
                part("g", None)?,
                part("b", None)?,
                part("a", Some(1.0))?,
            ])
        }
        Some(value) => Ok([component(value)?; 4]),
        None => Err(error("set_material_value needs a value")),
    }
}

pub(super) fn register(
    lua: &Lua,
    engine: &Table,
    commands: &Commands,
    active: &Rc<Cell<bool>>,
) -> mlua::Result<()> {
    let (queue, on) = (Rc::clone(commands), Rc::clone(active));
    engine.set(
        "set_material_value",
        lua.create_function(move |lua, mut args: MultiValue| {
            if !on.get() {
                return Err(error("engine commands may only run in callbacks"));
            }
            let object_id = string_arg(lua, &mut args, "material target")?;
            if uuid::Uuid::parse_str(&object_id).is_err() {
                return Err(error("material target must be a scene object ID"));
            }
            let name = string_arg(lua, &mut args, "parameter name")?;
            if name.is_empty() || name.len() > MAX_NAME_BYTES {
                return Err(error("parameter names have 1 to 32 bytes"));
            }
            let value = value_arg(args.pop_front())?;
            let mut pending = queue.borrow_mut();
            if pending.len() >= MAX_COMMANDS {
                return Err(error("Lua command limit exceeded"));
            }
            pending.push(GameCommand::SetMaterialValue {
                object_id,
                name,
                value,
            });
            Ok(())
        })?,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{GameCommand, LuaRuntime};

    const LAMP: &str = "3fa85f64-5717-4562-b3fc-2c963f66afa6";

    fn run(body: &str) -> Result<Vec<GameCommand>, crate::ScriptError> {
        let mut runtime = LuaRuntime::new().expect("runtime");
        runtime
            .load_script(
                "assets/scripts/main.lua",
                &format!("local lamp = '{LAMP}' return {{ start = function() {body} end }}"),
            )
            .expect("script loads");
        runtime.start()
    }

    #[test]
    fn numbers_and_colours_queue_a_material_value() {
        let out = run("engine.set_material_value(lamp, 'Glow', 2)
             engine.set_material_value(lamp, 'Tint', { r = 1, g = 0.5, b = 0 })")
        .expect("runs");
        assert_eq!(
            out,
            vec![
                GameCommand::SetMaterialValue {
                    object_id: LAMP.into(),
                    name: "Glow".into(),
                    value: [2.0; 4],
                },
                GameCommand::SetMaterialValue {
                    object_id: LAMP.into(),
                    name: "Tint".into(),
                    value: [1.0, 0.5, 0.0, 1.0],
                },
            ]
        );
    }

    #[test]
    fn bad_material_values_are_refused() {
        for call in [
            "engine.set_material_value('lamp', 'Glow', 1)",
            "engine.set_material_value(lamp, '', 1)",
            "engine.set_material_value(lamp, 'Glow')",
            "engine.set_material_value(lamp, 'Glow', 0/0)",
            "engine.set_material_value(lamp, 'Glow', 'bright')",
            "engine.set_material_value(lamp, 'Tint', { r = 1, g = 1 })",
        ] {
            assert!(run(call).is_err(), "{call} should fail");
        }
    }
}
