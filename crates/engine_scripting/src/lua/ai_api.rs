//! Lua `engine.*` calls for in-game AI: `move_to`, `stop_moving`,
//! `make_noise` and `set_ai_state` queue commands; `find_path`,
//! `line_of_sight` and `get_ai_state` ask the world while a callback runs
//! (see [`super::physics_api::with_queries`]).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use glam::Vec3;
use mlua::{Lua, MultiValue, Table, Value};

use super::{MAX_COMMANDS, string_arg, vec3};
use crate::{AI_STATES, GameCommand, ScriptQueries};

/// The largest distance or speed a script may give AI.
const MAX_AI_DISTANCE: f64 = 10_000.0;

type Commands = Rc<RefCell<Vec<GameCommand>>>;

fn error(message: &str) -> mlua::Error {
    mlua::Error::RuntimeError(message.to_owned())
}

fn object_arg(lua: &Lua, args: &mut MultiValue) -> mlua::Result<String> {
    let object_id = string_arg(lua, args, "object ID")?;
    if uuid::Uuid::parse_str(&object_id).is_err() {
        return Err(error("object ID must be a scene object ID"));
    }
    Ok(object_id)
}

fn distance(value: Option<Value>, what: &str) -> mlua::Result<f32> {
    let value = match value {
        Some(Value::Number(n)) => n,
        Some(Value::Integer(n)) => n as f64,
        _ => return Err(error(&format!("{what} must be a number"))),
    };
    if !value.is_finite() || !(0.0..=MAX_AI_DISTANCE).contains(&value) {
        return Err(error(&format!("{what} must be between 0 and 10000")));
    }
    Ok(value as f32)
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
        "move_to",
        lua.create_function(move |lua, mut args: MultiValue| {
            if !on.get() {
                return Err(error("engine commands may only run in callbacks"));
            }
            let object_id = object_arg(lua, &mut args)?;
            let target = vec3::vector_arg(&mut args, "position")?;
            let speed = match args.pop_front() {
                None | Some(Value::Nil) => None,
                Some(Value::Table(options)) => match options.get::<Value>("speed")? {
                    Value::Nil => None,
                    value => Some(distance(Some(value), "speed")?),
                },
                Some(_) => return Err(error("move_to options must be a table")),
            };
            push(
                &queue,
                GameCommand::MoveTo {
                    object_id,
                    target: target.to_array(),
                    speed,
                },
            )
        })?,
    )?;

    let (queue, on) = (Rc::clone(commands), Rc::clone(active));
    engine.set(
        "stop_moving",
        lua.create_function(move |lua, mut args: MultiValue| {
            if !on.get() {
                return Err(error("engine commands may only run in callbacks"));
            }
            let object_id = object_arg(lua, &mut args)?;
            push(&queue, GameCommand::StopMoving { object_id })
        })?,
    )?;

    let (queue, on) = (Rc::clone(commands), Rc::clone(active));
    engine.set(
        "make_noise",
        lua.create_function(move |_, mut args: MultiValue| {
            if !on.get() {
                return Err(error("engine commands may only run in callbacks"));
            }
            let position = vec3::vector_arg(&mut args, "position")?;
            let radius = distance(args.pop_front(), "radius")?;
            push(
                &queue,
                GameCommand::MakeNoise {
                    position: position.to_array(),
                    radius,
                },
            )
        })?,
    )?;

    let (queue, on) = (Rc::clone(commands), Rc::clone(active));
    engine.set(
        "set_ai_state",
        lua.create_function(move |lua, mut args: MultiValue| {
            if !on.get() {
                return Err(error("engine commands may only run in callbacks"));
            }
            let object_id = object_arg(lua, &mut args)?;
            let state = string_arg(lua, &mut args, "state")?.to_ascii_lowercase();
            if !AI_STATES.contains(&state.as_str()) {
                return Err(error(
                    "state must be \"idle\", \"patrol\", \"chase\" or \"attack\"",
                ));
            }
            push(&queue, GameCommand::SetAiState { object_id, state })
        })?,
    )
}

/// `engine.find_path(from, to)`: the corners of a walkable route, ending at
/// `to`, or nil when there is no way there.
pub(super) fn find_path(
    lua: &Lua,
    mut args: MultiValue,
    queries: &dyn ScriptQueries,
) -> mlua::Result<Value> {
    let from = vec3::vector_arg(&mut args, "from")?;
    let to = vec3::vector_arg(&mut args, "to")?;
    let Some(points) = queries.find_path(from.to_array(), to.to_array()) else {
        return Ok(Value::Nil);
    };
    let table = lua.create_table()?;
    for (index, point) in points.into_iter().enumerate() {
        table.set(index + 1, vec3::to_lua(lua, Vec3::from_array(point))?)?;
    }
    Ok(Value::Table(table))
}

/// `engine.line_of_sight(from, to, ignore?)`: whether nothing solid stands
/// between two points.
pub(super) fn line_of_sight(
    lua: &Lua,
    mut args: MultiValue,
    queries: &dyn ScriptQueries,
) -> mlua::Result<bool> {
    let from = vec3::vector_arg(&mut args, "from")?;
    let to = vec3::vector_arg(&mut args, "to")?;
    let ignore = match args.pop_front() {
        None | Some(Value::Nil) => None,
        Some(value) => {
            let mut rest = MultiValue::from_vec(vec![value]);
            Some(string_arg(lua, &mut rest, "ignore")?)
        }
    };
    Ok(queries.line_of_sight(from.to_array(), to.to_array(), ignore.as_deref()))
}

/// `engine.get_ai_state(id)`: a Simple brain's state, or nil.
pub(super) fn get_ai_state(
    lua: &Lua,
    mut args: MultiValue,
    queries: &dyn ScriptQueries,
) -> mlua::Result<Option<String>> {
    let object_id = string_arg(lua, &mut args, "object ID")?;
    Ok(queries.ai_state(&object_id))
}

#[cfg(test)]
mod tests {
    use crate::{GameCommand, LuaRuntime, ScriptQueries, ScriptRayHit};

    const GUARD: &str = "00000000-0000-4000-8000-000000000001";

    fn runtime(body: &str) -> LuaRuntime {
        let mut runtime = LuaRuntime::new().expect("runtime");
        runtime
            .load_script(
                "assets/scripts/main.lua",
                &format!("return {{ start = function() {body} end }}"),
            )
            .expect("script loads");
        runtime
    }

    #[test]
    fn ai_commands_queue_with_their_options() {
        let mut lua = runtime(&format!(
            "engine.move_to('{GUARD}', vec3(1, 0, 2))
             engine.move_to('{GUARD}', vec3(3, 0, 4), {{ speed = 6 }})
             engine.stop_moving('{GUARD}')
             engine.make_noise(vec3(0, 1, 0), 12)
             engine.set_ai_state('{GUARD}', 'Chase')"
        ));
        let commands = lua.start().expect("start runs");
        assert_eq!(
            commands,
            [
                GameCommand::MoveTo {
                    object_id: GUARD.into(),
                    target: [1.0, 0.0, 2.0],
                    speed: None,
                },
                GameCommand::MoveTo {
                    object_id: GUARD.into(),
                    target: [3.0, 0.0, 4.0],
                    speed: Some(6.0),
                },
                GameCommand::StopMoving {
                    object_id: GUARD.into(),
                },
                GameCommand::MakeNoise {
                    position: [0.0, 1.0, 0.0],
                    radius: 12.0,
                },
                GameCommand::SetAiState {
                    object_id: GUARD.into(),
                    state: "chase".into(),
                },
            ]
        );
    }

    #[test]
    fn ai_commands_reject_bad_arguments() {
        for call in [
            "engine.move_to('not-an-id', vec3(0, 0, 0))".to_owned(),
            format!("engine.move_to('{GUARD}', 5)"),
            format!("engine.move_to('{GUARD}', vec3(0, 0, 0), {{ speed = -1 }})"),
            "engine.make_noise(vec3(0, 0, 0), 1/0)".to_owned(),
            format!("engine.set_ai_state('{GUARD}', 'sleep')"),
        ] {
            let mut lua = runtime(&call);
            assert!(lua.start().is_err(), "{call} should fail");
        }
    }

    struct Map;

    impl ScriptQueries for Map {
        fn raycast(
            &self,
            _: [f32; 3],
            _: [f32; 3],
            _: f32,
            _: Option<&str>,
        ) -> Option<ScriptRayHit> {
            None
        }
        fn is_grounded(&self, _: &str) -> bool {
            false
        }
        fn find_path(&self, _: [f32; 3], to: [f32; 3]) -> Option<Vec<[f32; 3]>> {
            (to[0] >= 0.0).then(|| vec![[1.0, 0.0, 0.0], to])
        }
        fn line_of_sight(&self, _: [f32; 3], to: [f32; 3], ignore: Option<&str>) -> bool {
            to[2] < 5.0 || ignore == Some("wall")
        }
        fn ai_state(&self, object_id: &str) -> Option<String> {
            (object_id == GUARD).then(|| "patrol".to_owned())
        }
    }

    #[test]
    fn ai_queries_answer_inside_with_queries_only() {
        let mut lua = runtime(&format!(
            "local path = engine.find_path(vec3(0, 0, 0), vec3(4, 0, 0))
             engine.log(#path .. ' ' .. path[2].x)
             engine.log(tostring(engine.find_path(vec3(0, 0, 0), vec3(-4, 0, 0))))
             engine.log(tostring(engine.line_of_sight(vec3(0, 0, 0), vec3(0, 0, 9))))
             engine.log(tostring(engine.line_of_sight(vec3(0, 0, 0), vec3(0, 0, 9), 'wall')))
             engine.log(tostring(engine.get_ai_state('{GUARD}')))"
        ));
        let commands = lua
            .with_queries(&Map, |lua| lua.start())
            .expect("queries install")
            .expect("start runs");
        let logs: Vec<&str> = commands
            .iter()
            .filter_map(|command| match command {
                GameCommand::Log(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(logs, ["2 4.0", "nil", "false", "true", "patrol"]);

        let mut outside = runtime(
            "engine.log(tostring(engine.find_path(vec3(0, 0, 0), vec3(4, 0, 0))))
             engine.log(tostring(engine.line_of_sight(vec3(0, 0, 0), vec3(0, 0, 9))))",
        );
        let logs = outside.start().expect("start runs");
        assert_eq!(
            logs,
            [
                GameCommand::Log("nil".into()),
                GameCommand::Log("false".into())
            ]
        );
    }

    #[test]
    fn ai_callbacks_reach_the_object_and_global_scripts_only() {
        let mut runtime = LuaRuntime::new().expect("runtime");
        let script = "return {
            on_target_seen = function(id, target) engine.log('seen ' .. target) end,
            on_target_lost = function(id, target) engine.log('lost ' .. target) end,
            on_noise_heard = function(id, at) engine.log('noise ' .. at.x) end,
            on_move_finished = function(id) engine.log('arrived') end,
            on_ai_state_changed = function(id, state) engine.log('state ' .. state) end,
        }";
        runtime
            .load_attached_script("assets/scripts/guard.lua", script, GUARD)
            .expect("guard");
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
        let twice = |text: &str| vec![GameCommand::Log(text.into()), GameCommand::Log(text.into())];
        assert_eq!(
            runtime.on_target_seen(GUARD, "hero").expect("seen"),
            twice("seen hero")
        );
        assert_eq!(
            runtime.on_target_lost(GUARD, "hero").expect("lost"),
            twice("lost hero")
        );
        assert_eq!(
            runtime
                .on_noise_heard(GUARD, [2.0, 0.0, 0.0])
                .expect("noise"),
            twice("noise 2.0")
        );
        assert_eq!(
            runtime.on_move_finished(GUARD).expect("arrived"),
            twice("arrived")
        );
        assert_eq!(
            runtime.on_ai_state_changed(GUARD, "chase").expect("state"),
            twice("state chase")
        );
    }
}
