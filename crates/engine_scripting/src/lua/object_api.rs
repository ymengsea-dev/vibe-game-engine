//! Lua `engine.*` object control: `destroy`, `set_visible`, `set_enabled`,
//! `is_visible` and `is_enabled`.
//!
//! Reads see the scene captured when the callback began plus this callback's
//! own writes; a destroyed object reads as gone.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use mlua::{Lua, MultiValue, Table, Value};

use super::{MAX_COMMANDS, SceneSnapshot, string_arg};
use crate::GameCommand;

type Commands = Rc<RefCell<Vec<GameCommand>>>;
type Scene = Rc<RefCell<SceneSnapshot>>;

fn error(message: &str) -> mlua::Error {
    mlua::Error::RuntimeError(message.to_owned())
}

fn id_arg(lua: &Lua, args: &mut MultiValue) -> mlua::Result<String> {
    let id = string_arg(lua, args, "object ID")?;
    if id.is_empty() || id.len() > 64 {
        return Err(error("object ID must be nonempty and at most 64 bytes"));
    }
    Ok(id)
}

fn flag_arg(args: &mut MultiValue, what: &str) -> mlua::Result<bool> {
    match args.pop_front() {
        Some(Value::Boolean(flag)) => Ok(flag),
        _ => Err(error(&format!("{what} must be a boolean"))),
    }
}

fn check_ready(on: &Cell<bool>, queue: &RefCell<Vec<GameCommand>>) -> mlua::Result<()> {
    if !on.get() {
        return Err(error("engine commands may only run in callbacks"));
    }
    if queue.borrow().len() >= MAX_COMMANDS {
        return Err(error("Lua command limit exceeded"));
    }
    Ok(())
}

pub(super) fn register(
    lua: &Lua,
    engine: &Table,
    commands: &Commands,
    scene: &Scene,
    active: &Rc<Cell<bool>>,
) -> mlua::Result<()> {
    let (queue, snapshot, on) = (Rc::clone(commands), Rc::clone(scene), Rc::clone(active));
    engine.set(
        "destroy",
        lua.create_function(move |lua, mut args: MultiValue| {
            check_ready(&on, &queue)?;
            let object_id = id_arg(lua, &mut args)?;
            snapshot
                .borrow_mut()
                .pending_destroyed
                .insert(object_id.clone());
            queue.borrow_mut().push(GameCommand::Destroy { object_id });
            Ok(())
        })?,
    )?;

    let (queue, snapshot, on) = (Rc::clone(commands), Rc::clone(scene), Rc::clone(active));
    engine.set(
        "set_visible",
        lua.create_function(move |lua, mut args: MultiValue| {
            check_ready(&on, &queue)?;
            let object_id = id_arg(lua, &mut args)?;
            let visible = flag_arg(&mut args, "visible")?;
            snapshot
                .borrow_mut()
                .pending_visible
                .insert(object_id.clone(), visible);
            queue
                .borrow_mut()
                .push(GameCommand::SetVisible { object_id, visible });
            Ok(())
        })?,
    )?;

    let (queue, snapshot, on) = (Rc::clone(commands), Rc::clone(scene), Rc::clone(active));
    engine.set(
        "set_enabled",
        lua.create_function(move |lua, mut args: MultiValue| {
            check_ready(&on, &queue)?;
            let object_id = id_arg(lua, &mut args)?;
            let enabled = flag_arg(&mut args, "enabled")?;
            snapshot
                .borrow_mut()
                .pending_enabled
                .insert(object_id.clone(), enabled);
            queue
                .borrow_mut()
                .push(GameCommand::SetEnabled { object_id, enabled });
            Ok(())
        })?,
    )?;

    let snapshot = Rc::clone(scene);
    engine.set(
        "is_visible",
        lua.create_function(move |lua, mut args: MultiValue| {
            let object_id = id_arg(lua, &mut args)?;
            Ok(snapshot.borrow().is_visible(&object_id))
        })?,
    )?;

    let snapshot = Rc::clone(scene);
    engine.set(
        "is_enabled",
        lua.create_function(move |lua, mut args: MultiValue| {
            let object_id = id_arg(lua, &mut args)?;
            Ok(snapshot.borrow().is_enabled(&object_id))
        })?,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{GameCommand, LuaRuntime, SnapshotObject};

    fn object(id: &str, visible: bool, enabled: bool) -> SnapshotObject {
        SnapshotObject {
            id: id.into(),
            name: None,
            position: Some([1.0, 2.0, 3.0]),
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0; 3],
            velocity: None,
            visible,
            enabled,
        }
    }

    fn runtime(body: &str) -> LuaRuntime {
        let mut runtime = LuaRuntime::new().expect("runtime");
        runtime.set_scene_objects([object("a", true, true), object("off", false, false)]);
        runtime
            .load_script(
                "assets/scripts/main.lua",
                &format!("return {{ start = function() {body} end }}"),
            )
            .expect("script loads");
        runtime
    }

    #[test]
    fn object_calls_queue_commands() {
        let out = runtime(
            "engine.destroy('a') engine.set_visible('a', false) engine.set_enabled('a', true)",
        )
        .start()
        .expect("start");
        assert_eq!(
            out,
            vec![
                GameCommand::Destroy {
                    object_id: "a".into()
                },
                GameCommand::SetVisible {
                    object_id: "a".into(),
                    visible: false
                },
                GameCommand::SetEnabled {
                    object_id: "a".into(),
                    enabled: true
                },
            ]
        );
    }

    #[test]
    fn object_reads_are_pending_aware() {
        let out = runtime(
            "engine.log(tostring(engine.is_visible('a')) .. tostring(engine.is_enabled('off')) .. tostring(engine.is_visible('nope')))
             engine.set_visible('a', false)
             engine.set_enabled('off', true)
             engine.log(tostring(engine.is_visible('a')) .. tostring(engine.is_enabled('off')))
             engine.destroy('a')
             engine.log(tostring(engine.get_position('a')) .. tostring(engine.is_enabled('a')) .. tostring(engine.is_visible('a')))
             engine.set_visible('nope', true)
             engine.set_enabled('nope', true)
             engine.log(tostring(engine.is_visible('nope')) .. tostring(engine.is_enabled('nope')))",
        )
        .start()
        .expect("start");
        let logs: Vec<_> = out
            .into_iter()
            .filter_map(|c| match c {
                GameCommand::Log(text) => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(
            logs,
            ["truefalsefalse", "falsetrue", "nilfalsefalse", "falsefalse"]
        );
    }

    #[test]
    fn object_calls_validate_arguments() {
        for body in [
            "engine.destroy('')",
            "engine.set_visible('a', 1)",
            "engine.set_visible('a')",
            "engine.set_enabled('a', 'yes')",
            "engine.is_visible('')",
        ] {
            assert!(runtime(body).start().is_err(), "{body}");
        }
        let mut rt = LuaRuntime::new().expect("runtime");
        rt.load_script("assets/scripts/main.lua", "engine.destroy('a') return {}")
            .expect_err("outside a callback");
    }

    #[test]
    fn object_calls_respect_the_command_budget() {
        let body = "for _ = 1, 1025 do engine.set_visible('a', true) end";
        assert!(runtime(body).start().is_err());
    }

    #[test]
    fn scripts_on_disabled_objects_are_skipped() {
        let mut rt = LuaRuntime::new().expect("runtime");
        rt.set_scene_objects([object("a", true, true), object("b", true, false)]);
        let src = |name: &str| format!("return {{ update = function() engine.log('{name}') end }}");
        rt.load_attached_script("assets/scripts/a.lua", &src("a"), "a")
            .expect("a");
        rt.load_attached_script("assets/scripts/b.lua", &src("b"), "b")
            .expect("b");
        rt.load_script("assets/scripts/g.lua", &src("g"))
            .expect("global");
        let logs: Vec<_> = rt
            .update(0.1)
            .expect("update")
            .into_iter()
            .filter_map(|c| match c {
                GameCommand::Log(text) => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(logs, ["a", "g"]);
    }

    fn logs(commands: Vec<GameCommand>) -> Vec<String> {
        commands
            .into_iter()
            .filter_map(|c| match c {
                GameCommand::Log(text) => Some(text),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_script_that_destroys_its_object_gets_no_further_update() {
        let mut rt = LuaRuntime::new().expect("runtime");
        rt.set_scene_objects([object("a", true, true), object("b", true, true)]);
        let src = "return { update = function() engine.log(engine.object_id()) end, \
                   on_collision = function() engine.destroy(engine.object_id()) end }";
        for id in ["a", "b"] {
            rt.load_attached_script("assets/scripts/x.lua", src, id)
                .expect("script");
        }
        assert_eq!(logs(rt.update(0.1).expect("update")), ["a", "b"]);
        let commands = rt.on_collision("enter", "a", "b").expect("collision");
        let destroyed: Vec<String> = commands
            .iter()
            .filter_map(|c| match c {
                GameCommand::Destroy { object_id } => Some(object_id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(destroyed, ["a", "b"]);
        // The host reports what the engine destroyed; the scripts go.
        rt.forget_objects(&destroyed[..1]);
        rt.set_scene_objects([object("b", true, true)]);
        assert_eq!(logs(rt.update(0.1).expect("update")), ["b"]);
        rt.forget_objects(&["b".to_owned()]);
        assert!(logs(rt.update(0.1).expect("update")).is_empty());
    }

    #[test]
    fn a_destroyed_spawned_objects_script_is_dropped() {
        let mut rt = LuaRuntime::new().expect("runtime");
        let src = "return { update = function() engine.log(tostring(engine.object_id())) end }";
        rt.load_attached_script("assets/scripts/x.lua", src, "bullet")
            .expect("script");
        rt.load_script("assets/scripts/g.lua", src).expect("global");
        rt.forget_objects(&["bullet".to_owned(), "missing".to_owned()]);
        // The global script stays; only the bullet's script is gone.
        assert_eq!(logs(rt.update(0.1).expect("update")), ["nil"]);
    }

    #[test]
    fn start_runs_once_on_first_enable_before_the_first_update() {
        let mut rt = LuaRuntime::new().expect("runtime");
        rt.set_scene_objects([object("b", true, false)]);
        let src = "return { start = function() engine.log('start') end, \
                   update = function() engine.log('update') end }";
        rt.load_attached_script("assets/scripts/b.lua", src, "b")
            .expect("script");
        assert!(logs(rt.start().expect("start")).is_empty());
        assert!(logs(rt.update(0.1).expect("update")).is_empty());
        rt.set_scene_objects([object("b", true, true)]);
        assert_eq!(logs(rt.update(0.1).expect("update")), ["start", "update"]);
        assert_eq!(logs(rt.update(0.1).expect("update")), ["update"]);
        // Disabling and enabling again does not start it twice.
        rt.set_scene_objects([object("b", true, false)]);
        assert!(logs(rt.update(0.1).expect("update")).is_empty());
        rt.set_scene_objects([object("b", true, true)]);
        assert_eq!(logs(rt.update(0.1).expect("update")), ["update"]);
    }

    #[test]
    fn an_enabled_script_is_not_started_by_update_alone() {
        let mut rt = LuaRuntime::new().expect("runtime");
        rt.set_scene_objects([object("a", true, true)]);
        let src = "return { start = function() engine.log('start') end, \
                   update = function() engine.log('update') end }";
        rt.load_attached_script("assets/scripts/a.lua", src, "a")
            .expect("script");
        assert_eq!(logs(rt.update(0.1).expect("update")), ["update"]);
    }
}
