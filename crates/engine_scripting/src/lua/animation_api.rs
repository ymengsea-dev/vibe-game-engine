//! Lua `engine.*` calls for animated models and sprites: `play_animation`,
//! `set_animation_speed` and `set_animator_value`. `set_animation_playing` (in `lua.rs`) pauses
//! both sprites and models.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use mlua::{Lua, MultiValue, Table, Value};

use super::{MAX_COMMANDS, string_arg};
use crate::GameCommand;

/// Fastest playback a script may ask for, either way.
const MAX_SPEED: f64 = 100.0;
/// Longest crossfade a script may ask for, in seconds.
const MAX_FADE_SECONDS: f64 = 10.0;

type Commands = Rc<RefCell<Vec<GameCommand>>>;

fn error(message: &str) -> mlua::Error {
    mlua::Error::RuntimeError(message.to_owned())
}

fn object_arg(lua: &Lua, args: &mut MultiValue) -> mlua::Result<String> {
    let object_id = string_arg(lua, args, "animation target")?;
    if uuid::Uuid::parse_str(&object_id).is_err() {
        return Err(error("animation target must be a scene object ID"));
    }
    Ok(object_id)
}

fn speed_value(value: f64) -> mlua::Result<f32> {
    if !value.is_finite() || value.abs() > MAX_SPEED {
        return Err(error("speed must be a finite number from -100 to 100"));
    }
    Ok(value as f32)
}

fn number(value: Value, what: &str) -> mlua::Result<f64> {
    match value {
        Value::Number(n) => Ok(n),
        Value::Integer(n) => Ok(n as f64),
        _ => Err(error(&format!("{what} must be a number"))),
    }
}

/// `{ loop = true, speed = 1, fade = 0 }`, every field optional; a missing
/// `loop` keeps the clip's own setting.
fn options(value: Option<Value>) -> mlua::Result<(Option<bool>, f32, f32)> {
    let table = match value {
        None | Some(Value::Nil) => return Ok((None, 1.0, 0.0)),
        Some(Value::Table(table)) => table,
        Some(_) => return Err(error("animation options must be a table")),
    };
    let looping = match table.get::<Value>("loop")? {
        Value::Nil => None,
        Value::Boolean(looping) => Some(looping),
        _ => return Err(error("loop must be true or false")),
    };
    let speed = match table.get::<Value>("speed")? {
        Value::Nil => 1.0,
        value => speed_value(number(value, "speed")?)?,
    };
    let fade = match table.get::<Value>("fade")? {
        Value::Nil => 0.0,
        value => {
            let fade = number(value, "fade")?;
            if !fade.is_finite() || !(0.0..=MAX_FADE_SECONDS).contains(&fade) {
                return Err(error("fade must be between 0 and 10 seconds"));
            }
            fade as f32
        }
    };
    Ok((looping, speed, fade))
}

fn push(queue: &Commands, command: GameCommand) -> mlua::Result<()> {
    let mut pending = queue.borrow_mut();
    if pending.len() >= MAX_COMMANDS {
        return Err(error("Lua command limit exceeded"));
    }
    pending.push(command);
    Ok(())
}

pub(super) fn register(
    lua: &Lua,
    engine: &Table,
    commands: &Commands,
    active: &Rc<Cell<bool>>,
) -> mlua::Result<()> {
    let (queue, on) = (Rc::clone(commands), Rc::clone(active));
    engine.set(
        "play_animation",
        lua.create_function(move |lua, mut args: MultiValue| {
            if !on.get() {
                return Err(error("engine commands may only run in callbacks"));
            }
            let object_id = object_arg(lua, &mut args)?;
            let clip = string_arg(lua, &mut args, "clip name")?;
            if clip.is_empty() || clip.len() > 256 {
                return Err(error("clip name must be 1 to 256 bytes"));
            }
            let (looping, speed, fade) = options(args.pop_front())?;
            push(
                &queue,
                GameCommand::PlayAnimation {
                    object_id,
                    clip,
                    looping,
                    speed,
                    fade,
                },
            )
        })?,
    )?;

    let (queue, on) = (Rc::clone(commands), Rc::clone(active));
    engine.set(
        "set_animator_value",
        lua.create_function(move |lua, mut args: MultiValue| {
            if !on.get() {
                return Err(error("engine commands may only run in callbacks"));
            }
            let object_id = object_arg(lua, &mut args)?;
            let name = string_arg(lua, &mut args, "value name")?;
            if name.is_empty() || name.len() > 64 || name.chars().any(char::is_control) {
                return Err(error("value name must be 1 to 64 printable bytes"));
            }
            let value = match args.pop_front() {
                Some(Value::Boolean(flag)) => f64::from(u8::from(flag)),
                Some(value) => number(value, "value")?,
                None => return Err(error("value must be a number or true/false")),
            };
            if !value.is_finite() {
                return Err(error("value must be finite"));
            }
            push(
                &queue,
                GameCommand::SetAnimatorValue {
                    object_id,
                    name,
                    value,
                },
            )
        })?,
    )?;

    let (queue, on) = (Rc::clone(commands), Rc::clone(active));
    engine.set(
        "set_animation_speed",
        lua.create_function(move |lua, mut args: MultiValue| {
            if !on.get() {
                return Err(error("engine commands may only run in callbacks"));
            }
            let object_id = object_arg(lua, &mut args)?;
            let speed = match args.pop_front() {
                Some(value) => speed_value(number(value, "speed")?)?,
                None => return Err(error("speed must be a number")),
            };
            push(&queue, GameCommand::SetAnimationSpeed { object_id, speed })
        })?,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{GameCommand, LuaRuntime};

    const HERO: &str = "3fa85f64-5717-4562-b3fc-2c963f66afa6";

    fn run(body: &str) -> Result<Vec<GameCommand>, crate::ScriptError> {
        let mut runtime = LuaRuntime::new().expect("runtime");
        runtime
            .load_script(
                "assets/scripts/main.lua",
                &format!("local hero = '{HERO}' return {{ start = function() {body} end }}"),
            )
            .expect("script loads");
        runtime.start()
    }

    #[test]
    fn play_animation_queues_defaults_and_options() {
        let out = run("engine.play_animation(hero, 'walk')
             engine.play_animation(hero, 'jump', { loop = false, speed = 1.5, fade = 0.25 })
             engine.set_animation_speed(hero, -1)")
        .expect("runs");
        assert_eq!(
            out,
            vec![
                GameCommand::PlayAnimation {
                    object_id: HERO.into(),
                    clip: "walk".into(),
                    looping: None,
                    speed: 1.0,
                    fade: 0.0,
                },
                GameCommand::PlayAnimation {
                    object_id: HERO.into(),
                    clip: "jump".into(),
                    looping: Some(false),
                    speed: 1.5,
                    fade: 0.25,
                },
                GameCommand::SetAnimationSpeed {
                    object_id: HERO.into(),
                    speed: -1.0,
                },
            ]
        );
    }

    #[test]
    fn play_animation_rejects_bad_arguments() {
        for call in [
            "engine.play_animation('hero', 'walk')",
            "engine.play_animation(hero, '')",
            "engine.play_animation(hero)",
            "engine.play_animation(hero, 'walk', 3)",
            "engine.play_animation(hero, 'walk', { loop = 1 })",
            "engine.play_animation(hero, 'walk', { speed = 0/0 })",
            "engine.play_animation(hero, 'walk', { speed = 101 })",
            "engine.play_animation(hero, 'walk', { fade = -1 })",
            "engine.play_animation(hero, 'walk', { fade = 11 })",
            "engine.set_animation_speed(hero)",
            "engine.set_animation_speed(hero, math.huge)",
        ] {
            assert!(run(call).is_err(), "{call} should fail");
        }
    }

    #[test]
    fn animation_events_reach_the_object_and_global_scripts_only() {
        let mut runtime = LuaRuntime::new().expect("runtime");
        let script =
            "return { on_animation_event = function(id, name) engine.log(id .. ':' .. name) end }";
        runtime
            .load_attached_script("assets/scripts/hero.lua", script, HERO)
            .expect("hero");
        runtime
            .load_attached_script(
                "assets/scripts/other.lua",
                script,
                "00000000-0000-4000-8000-000000000009",
            )
            .expect("other");
        runtime
            .load_script("assets/scripts/main.lua", script)
            .expect("global");
        runtime.start().expect("start");

        let out = runtime.on_animation_event(HERO, "step").expect("event");
        let line = GameCommand::Log(format!("{HERO}:step"));
        assert_eq!(out, vec![line.clone(), line]);
    }
}
