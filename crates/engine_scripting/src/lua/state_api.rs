//! Lua `engine.get_state` and `engine.set_state`.
//!
//! `get_state` reads the state captured when the callback began, plus this
//! callback's own `set_state`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use mlua::{Lua, MultiValue, Table};

use super::{MAX_COMMANDS, SceneSnapshot, string_arg};
use crate::{GAME_STATES, GameCommand};

type Commands = Rc<RefCell<Vec<GameCommand>>>;
type Scene = Rc<RefCell<SceneSnapshot>>;

fn error(message: &str) -> mlua::Error {
    mlua::Error::RuntimeError(message.to_owned())
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
        "get_state",
        lua.create_function(move |_, _: MultiValue| Ok(read_scene.borrow().state()))?,
    )?;

    let (queue, scene_for_set, on) = (Rc::clone(commands), Rc::clone(scene), Rc::clone(active));
    engine.set(
        "set_state",
        lua.create_function(move |lua, mut args: MultiValue| {
            if !on.get() {
                return Err(error("engine commands may only run in callbacks"));
            }
            let name = string_arg(lua, &mut args, "state name")?;
            if !GAME_STATES.contains(&name.as_str()) {
                return Err(error(&format!(
                    "unknown state '{name}'; use one of {}",
                    GAME_STATES.join(", ")
                )));
            }
            let mut pending = queue.borrow_mut();
            if pending.len() >= MAX_COMMANDS {
                return Err(error("Lua command limit exceeded"));
            }
            scene_for_set.borrow_mut().pending_state = Some(name.clone());
            pending.push(GameCommand::SetGameState { state: name });
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
        runtime
            .load_script(
                "assets/scripts/main.lua",
                &format!("return {{ start = function() {body} end }}"),
            )
            .expect("script loads");
        runtime
    }

    #[test]
    fn set_state_validates_and_reads_back() {
        let mut rt = runtime(
            "engine.log(engine.get_state())
             engine.set_state('paused')
             engine.log(engine.get_state())",
        );
        rt.set_game_state("menu");
        assert_eq!(
            rt.start().expect("start"),
            vec![
                GameCommand::Log("menu".into()),
                GameCommand::SetGameState {
                    state: "paused".into()
                },
                GameCommand::Log("paused".into()),
            ]
        );
        let error = runtime("engine.set_state('Paused')")
            .start()
            .expect_err("unknown name")
            .to_string();
        for name in crate::GAME_STATES {
            assert!(error.contains(name), "{error}");
        }
        assert!(runtime("engine.set_state(3)").start().is_err());
        let mut outside = LuaRuntime::new().expect("runtime");
        outside
            .load_script(
                "assets/scripts/main.lua",
                "engine.set_state('menu') return {}",
            )
            .expect_err("outside a callback");
        // Default state reads as playing.
        let out = runtime("engine.log(engine.get_state())")
            .start()
            .expect("start");
        assert_eq!(out, vec![GameCommand::Log("playing".into())]);
    }

    #[test]
    fn on_state_changed_reaches_every_script() {
        let mut rt = LuaRuntime::new().expect("runtime");
        let src = |name: &str| {
            format!(
                "return {{ on_state_changed = function(old, new) engine.log('{name}:' .. old .. '>' .. new .. ':' .. engine.get_state()) end }}"
            )
        };
        rt.load_script("assets/scripts/g.lua", &src("g"))
            .expect("g");
        rt.load_attached_script("assets/scripts/a.lua", &src("a"), "a")
            .expect("a");
        rt.set_game_state("paused");
        let logs: Vec<_> = rt
            .on_state_changed("playing", "paused")
            .expect("callback")
            .into_iter()
            .filter_map(|c| match c {
                GameCommand::Log(text) => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(logs, ["g:playing>paused:paused", "a:playing>paused:paused"]);
    }
}
