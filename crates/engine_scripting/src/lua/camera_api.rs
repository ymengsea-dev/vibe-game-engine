//! Lua `engine.*` camera calls: `set_camera`, `get_camera`,
//! `set_follow_target` and `camera_shake`.
//!
//! `get_camera` reads the active camera captured when the callback began,
//! plus this callback's own `set_camera`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use mlua::{Lua, MultiValue, Table, Value};

use super::{MAX_COMMANDS, SceneSnapshot, string_arg};
use crate::GameCommand;

/// Longest shake a script may start, in seconds.
const MAX_SHAKE_SECONDS: f64 = 10.0;

type Commands = Rc<RefCell<Vec<GameCommand>>>;
type Scene = Rc<RefCell<SceneSnapshot>>;

fn error(message: &str) -> mlua::Error {
    mlua::Error::RuntimeError(message.to_owned())
}

fn id_arg(lua: &Lua, args: &mut MultiValue, what: &str) -> mlua::Result<String> {
    let id = string_arg(lua, args, what)?;
    if id.is_empty() || id.len() > 64 {
        return Err(error("object ID must be nonempty and at most 64 bytes"));
    }
    Ok(id)
}

fn number_arg(args: &mut MultiValue, what: &str) -> mlua::Result<f64> {
    match args.pop_front() {
        Some(Value::Number(n)) => Ok(n),
        Some(Value::Integer(n)) => Ok(n as f64),
        _ => Err(error(&format!("{what} must be a number"))),
    }
}

pub(super) fn register(
    lua: &Lua,
    engine: &Table,
    commands: &Commands,
    scene: &Scene,
    active: &Rc<Cell<bool>>,
) -> mlua::Result<()> {
    let read_scene = Rc::clone(scene);
    engine.set(
        "get_camera",
        lua.create_function(move |_, _: MultiValue| Ok(read_scene.borrow().camera()))?,
    )?;

    let (queue, scene_for_set, on) = (Rc::clone(commands), Rc::clone(scene), Rc::clone(active));
    engine.set(
        "set_camera",
        lua.create_function(move |lua, mut args: MultiValue| {
            if !on.get() {
                return Err(error("engine commands may only run in callbacks"));
            }
            let object_id = id_arg(lua, &mut args, "object ID")?;
            let mut pending = queue.borrow_mut();
            if pending.len() >= MAX_COMMANDS {
                return Err(error("Lua command limit exceeded"));
            }
            scene_for_set.borrow_mut().pending_camera = Some(object_id.clone());
            pending.push(GameCommand::SetCamera { object_id });
            Ok(())
        })?,
    )?;

    let (queue, on) = (Rc::clone(commands), Rc::clone(active));
    engine.set(
        "set_follow_target",
        lua.create_function(move |lua, mut args: MultiValue| {
            if !on.get() {
                return Err(error("engine commands may only run in callbacks"));
            }
            let camera_id = id_arg(lua, &mut args, "camera ID")?;
            let target_id = id_arg(lua, &mut args, "target ID")?;
            let mut pending = queue.borrow_mut();
            if pending.len() >= MAX_COMMANDS {
                return Err(error("Lua command limit exceeded"));
            }
            pending.push(GameCommand::SetFollowTarget {
                camera_id,
                target_id,
            });
            Ok(())
        })?,
    )?;

    let (queue, on) = (Rc::clone(commands), Rc::clone(active));
    engine.set(
        "camera_shake",
        lua.create_function(move |_, mut args: MultiValue| {
            if !on.get() {
                return Err(error("engine commands may only run in callbacks"));
            }
            let strength = number_arg(&mut args, "strength")?;
            let seconds = number_arg(&mut args, "seconds")?;
            if !strength.is_finite() || strength < 0.0 {
                return Err(error("strength must be a finite number, at least 0"));
            }
            if !seconds.is_finite() || !(0.0..=MAX_SHAKE_SECONDS).contains(&seconds) {
                return Err(error("seconds must be between 0 and 10"));
            }
            let mut pending = queue.borrow_mut();
            if pending.len() >= MAX_COMMANDS {
                return Err(error("Lua command limit exceeded"));
            }
            pending.push(GameCommand::CameraShake {
                strength: strength as f32,
                seconds: seconds as f32,
            });
            Ok(())
        })?,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{GameCommand, LuaRuntime};

    fn runtime(body: &str) -> LuaRuntime {
        let mut runtime = LuaRuntime::new().expect("runtime");
        runtime.set_active_camera(Some("cam1".into()));
        runtime
            .load_script(
                "assets/scripts/main.lua",
                &format!("return {{ start = function() {body} end }}"),
            )
            .expect("script loads");
        runtime
    }

    #[test]
    fn camera_calls_queue_commands_and_read_back() {
        let mut rt = runtime(
            "engine.log(tostring(engine.get_camera()))
             engine.set_camera('cam2')
             engine.log(engine.get_camera())
             engine.set_follow_target('cam2', 'hero')
             engine.camera_shake(0.5, 0.3)",
        );
        let out = rt.start().expect("start");
        assert_eq!(
            out,
            vec![
                GameCommand::Log("cam1".into()),
                GameCommand::SetCamera {
                    object_id: "cam2".into()
                },
                GameCommand::Log("cam2".into()),
                GameCommand::SetFollowTarget {
                    camera_id: "cam2".into(),
                    target_id: "hero".into()
                },
                GameCommand::CameraShake {
                    strength: 0.5,
                    seconds: 0.3
                },
            ]
        );
    }

    #[test]
    fn get_camera_is_nil_without_an_active_camera() {
        let mut rt = LuaRuntime::new().expect("runtime");
        rt.load_script(
            "assets/scripts/main.lua",
            "return { start = function() engine.log(tostring(engine.get_camera())) end }",
        )
        .expect("loads");
        assert_eq!(
            rt.start().expect("start"),
            vec![GameCommand::Log("nil".into())]
        );
    }

    #[test]
    fn camera_shake_validates_arguments() {
        for call in [
            "engine.camera_shake(-1, 1)",
            "engine.camera_shake(1, 11)",
            "engine.camera_shake(1, -0.1)",
            "engine.camera_shake(0/0, 1)",
            "engine.camera_shake(math.huge, 1)",
            "engine.camera_shake(1, 0/0)",
            "engine.camera_shake('x', 1)",
            "engine.set_camera('')",
        ] {
            let mut rt = runtime(call);
            assert!(rt.start().is_err(), "{call} should fail");
        }
        let mut rt = runtime("engine.camera_shake(0.5, 0.3) engine.camera_shake(2, 10)");
        assert_eq!(rt.start().expect("valid").len(), 2);
    }
}
