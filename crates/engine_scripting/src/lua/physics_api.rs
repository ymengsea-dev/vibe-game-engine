//! Lua `engine.*` velocity, force, impulse, character-move and query calls.
//!
//! `get_velocity` reads the dynamic body's linear velocity captured when the
//! callback began (plus this callback's own `set_velocity`). The rest queue
//! commands that the host applies to the physics world.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use glam::Vec3;
use mlua::{Function, Lua, MultiValue, Table, Value};

use super::{MAX_COMMANDS, SceneSnapshot, string_arg, vec3};
use crate::{GameCommand, ScriptQueries};

/// Longest ray a script may cast.
const MAX_RAY_DISTANCE: f32 = 10_000.0;

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
    force_seconds: &Rc<Cell<f32>>,
) -> mlua::Result<QueryStubs> {
    let read_scene = Rc::clone(scene);
    engine.set(
        "get_velocity",
        lua.create_function(move |lua, object_id: String| {
            let velocity = read_scene.borrow().velocity(&object_id);
            vec3::to_lua(lua, velocity.map_or(Vec3::ZERO, Vec3::from_array))
        })?,
    )?;

    for name in ["set_velocity", "add_force", "add_impulse", "move_character"] {
        let (commands, scene, active) = (Rc::clone(commands), Rc::clone(scene), Rc::clone(active));
        let force_seconds = Rc::clone(force_seconds);
        engine.set(
            name,
            lua.create_function(move |lua, mut args: MultiValue| {
                if !active.get() {
                    return Err(error("engine commands may only run in callbacks"));
                }
                let object_id = string_arg(lua, &mut args, "object ID")?;
                if object_id.is_empty() || object_id.len() > 64 {
                    return Err(error("object ID must be nonempty and at most 64 bytes"));
                }
                let value = vec3::vector_arg(&mut args, "vector")?.to_array();
                let mut pending = commands.borrow_mut();
                if pending.len() >= MAX_COMMANDS {
                    return Err(error("Lua command limit exceeded"));
                }
                match name {
                    "set_velocity" => {
                        let mut snapshot = scene.borrow_mut();
                        if snapshot.velocity(&object_id).is_some() {
                            snapshot.pending_velocities.insert(object_id.clone(), value);
                        }
                        pending.push(GameCommand::SetVelocity {
                            object_id,
                            velocity: value,
                        });
                    }
                    "add_force" => pending.push(GameCommand::AddForce {
                        object_id,
                        force: value,
                        duration: force_seconds.get(),
                    }),
                    "move_character" => pending.push(GameCommand::MoveCharacter {
                        object_id,
                        delta: value,
                    }),
                    _ => pending.push(GameCommand::AddImpulse {
                        object_id,
                        impulse: value,
                    }),
                }
                Ok(())
            })?,
        )?;
    }
    // Outside `with_queries` there is no world to ask: no hit, not grounded.
    let stubs = QueryStubs {
        engine: engine.clone(),
        raycast: lua.create_function(|_, _: MultiValue| Ok(Value::Nil))?,
        is_grounded: lua.create_function(|_, _: MultiValue| Ok(false))?,
    };
    engine.set("raycast", stubs.raycast.clone())?;
    engine.set("is_grounded", stubs.is_grounded.clone())?;
    Ok(stubs)
}

/// The `engine` table and the functions `engine.raycast` and
/// `engine.is_grounded` hold outside `with_queries`. The runtime keeps them so
/// they are never read back from a table or global a script may have changed.
#[derive(Clone)]
pub(super) struct QueryStubs {
    engine: Table,
    raycast: Function,
    is_grounded: Function,
}

fn raycast(lua: &Lua, mut args: MultiValue, queries: &dyn ScriptQueries) -> mlua::Result<Value> {
    let origin = vec3::vector_arg(&mut args, "origin")?;
    let direction = vec3::vector_arg(&mut args, "direction")?;
    let max = match args.pop_front() {
        Some(Value::Number(n)) => n,
        Some(Value::Integer(n)) => n as f64,
        _ => return Err(error("max distance must be a positive number")),
    };
    if direction.length() < 1e-6 {
        return Err(error("direction must be non-zero"));
    }
    if !max.is_finite() || max <= 0.0 {
        return Err(error("max distance must be a positive number"));
    }
    let max = (max as f32).min(MAX_RAY_DISTANCE);
    let direction = direction.normalize_or_zero();
    let ignore = match args.pop_front() {
        None | Some(Value::Nil) => None,
        Some(Value::String(id)) => {
            let id = id.to_str()?.to_string();
            if id.is_empty() || id.len() > 64 {
                return Err(error("ignore ID must be nonempty and at most 64 bytes"));
            }
            Some(id)
        }
        Some(_) => return Err(error("ignore must be an object ID string or nil")),
    };
    let Some(hit) = queries.raycast(
        origin.to_array(),
        direction.to_array(),
        max,
        ignore.as_deref(),
    ) else {
        return Ok(Value::Nil);
    };
    let table = lua.create_table()?;
    if let Some(id) = hit.object_id {
        table.set("object", id)?;
    }
    table.set("point", vec3::to_lua(lua, Vec3::from_array(hit.point))?)?;
    table.set("normal", vec3::to_lua(lua, Vec3::from_array(hit.normal))?)?;
    table.set("distance", f64::from(hit.distance))?;
    Ok(Value::Table(table))
}

/// Runs `f` with working `engine.raycast` and `engine.is_grounded` backed by
/// `queries`, then puts the stubs back. The functions are scoped, so a script
/// that keeps one and calls it later gets a Lua error.
pub(super) fn with_queries<R>(
    lua: &Lua,
    stubs: &QueryStubs,
    queries: &dyn ScriptQueries,
    f: impl FnOnce() -> R,
) -> mlua::Result<R> {
    let engine = &stubs.engine;
    let restore = || -> mlua::Result<()> {
        engine.set("raycast", stubs.raycast.clone())?;
        engine.set("is_grounded", stubs.is_grounded.clone())
    };
    let outcome = lua.scope(|scope| {
        engine.set(
            "raycast",
            scope.create_function(|lua, args: MultiValue| raycast(lua, args, queries))?,
        )?;
        engine.set(
            "is_grounded",
            scope.create_function(|lua, mut args: MultiValue| {
                let object_id = string_arg(lua, &mut args, "object ID")?;
                if object_id.is_empty() || object_id.len() > 64 {
                    return Err(error("object ID must be nonempty and at most 64 bytes"));
                }
                Ok(queries.is_grounded(&object_id))
            })?,
        )?;
        Ok(f())
    });
    // The scoped functions die with the scope; always put the stubs back so
    // later plain callbacks see nil and false, whatever happened above.
    let restored = restore();
    let result = outcome?;
    restored?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use crate::{GameCommand, LuaRuntime, ScriptQueries, ScriptRayHit, SnapshotObject};

    fn runtime(velocity: Option<[f32; 3]>, id: &str, body: &str) -> LuaRuntime {
        let mut runtime = LuaRuntime::new().expect("runtime");
        runtime.set_scene_objects([SnapshotObject {
            id: id.into(),
            name: None,
            position: Some([0.0; 3]),
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0; 3],
            velocity,
            visible: true,
            enabled: true,
            tags: Vec::new(),
        }]);
        runtime
            .load_script(
                "assets/scripts/main.lua",
                &format!("return {{ start = function() {body} end }}"),
            )
            .expect("script loads");
        runtime
    }

    fn logs(commands: &[GameCommand]) -> Vec<String> {
        commands
            .iter()
            .filter_map(|c| match c {
                GameCommand::Log(m) => Some(m.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn velocity_calls_queue_commands_and_read_back() {
        let mut rt = runtime(
            Some([1.0, 0.0, 0.0]),
            "actor",
            "engine.log(tostring(engine.get_velocity('actor')))
             engine.set_velocity('actor', vec3(0, 5, 0))
             engine.log(tostring(engine.get_velocity('actor')))
             engine.add_force('actor', 0, 10, 0)
             engine.add_impulse('actor', vec3(1, 0, 0))",
        );
        let out = rt.start().expect("start");
        assert!(
            matches!(
                &out[..],
                [
                    GameCommand::Log(_),
                    GameCommand::SetVelocity {
                        velocity: [0.0, 5.0, 0.0],
                        ..
                    },
                    GameCommand::Log(_),
                    GameCommand::AddForce {
                        force: [0.0, 10.0, 0.0],
                        ..
                    },
                    GameCommand::AddImpulse {
                        impulse: [1.0, 0.0, 0.0],
                        ..
                    },
                ]
            ),
            "{out:?}"
        );
        assert_eq!(
            logs(&out),
            vec!["vec3(1, 0, 0)".to_owned(), "vec3(0, 5, 0)".to_owned()]
        );
    }

    #[test]
    fn velocity_of_non_dynamic_object_is_zero() {
        let mut rt = runtime(
            None,
            "wall",
            "engine.log(tostring(engine.get_velocity('wall')))",
        );
        let out = rt.start().expect("start");
        assert_eq!(logs(&out), vec!["vec3(0, 0, 0)".to_owned()]);
    }

    #[test]
    fn velocity_calls_reject_bad_arguments() {
        let mut rt = runtime(None, "wall", "engine.add_impulse('wall', 0, 0, 0/0)");
        assert!(rt.start().is_err());
        let mut rt = runtime(None, "wall", "engine.set_velocity('', vec3(1, 0, 0))");
        assert!(rt.start().is_err());
    }

    struct FakeQueries {
        hit: Option<ScriptRayHit>,
        grounded: bool,
        seen_max: Cell<f32>,
        seen_ignore: std::cell::RefCell<Option<String>>,
    }

    impl ScriptQueries for FakeQueries {
        fn raycast(
            &self,
            _: [f32; 3],
            _: [f32; 3],
            max: f32,
            ignore: Option<&str>,
        ) -> Option<ScriptRayHit> {
            self.seen_max.set(max);
            *self.seen_ignore.borrow_mut() = ignore.map(str::to_owned);
            self.hit.clone()
        }
        fn is_grounded(&self, _: &str) -> bool {
            self.grounded
        }
    }

    fn fake(hit: Option<ScriptRayHit>, grounded: bool) -> FakeQueries {
        FakeQueries {
            hit,
            grounded,
            seen_max: Cell::new(0.0),
            seen_ignore: std::cell::RefCell::new(None),
        }
    }

    fn wall_hit() -> ScriptRayHit {
        ScriptRayHit {
            object_id: Some("wall".into()),
            point: [0.0, 0.0, -5.0],
            normal: [0.0, 0.0, 1.0],
            distance: 5.0,
        }
    }

    #[test]
    fn raycast_returns_hit_table_inside_with_queries() {
        let mut rt = runtime(
            None,
            "a",
            "local h = engine.raycast(vec3(0,0,0), vec3(0,0,-1), 100)
             engine.log(h.object .. ' ' .. h.distance .. ' ' .. tostring(h.point))
             engine.log(tostring(engine.is_grounded('a')))",
        );
        let queries = fake(Some(wall_hit()), true);
        let out = rt
            .with_queries(&queries, |rt| rt.start())
            .expect("scope")
            .expect("start");
        assert_eq!(
            logs(&out),
            vec!["wall 5.0 vec3(0, 0, -5)".to_owned(), "true".to_owned()]
        );
    }

    #[test]
    fn raycast_rejects_bad_arguments_and_is_nil_outside_callbacks() {
        let queries = fake(Some(wall_hit()), true);
        for (call, message) in [
            ("vec3(0,0,0), 100", "direction must be non-zero"),
            ("vec3(0,0,-1), 0", "max distance must be a positive number"),
            (
                "vec3(0,0,-1), 0/0",
                "max distance must be a positive number",
            ),
            ("vec3(0,0,-1), -3", "max distance must be a positive number"),
        ] {
            let mut rt = runtime(None, "a", &format!("engine.raycast(vec3(0,0,0), {call})"));
            let error = rt
                .with_queries(&queries, |rt| rt.start())
                .expect("scope")
                .expect_err("bad ray");
            assert!(error.to_string().contains(message), "{call}: {error}");
        }

        let mut rt = runtime(
            None,
            "a",
            "engine.log(tostring(engine.raycast(vec3(0,0,0), vec3(0,0,-1), 5)))
             engine.log(tostring(engine.is_grounded('a')))",
        );
        let out = rt.start().expect("plain start");
        assert_eq!(logs(&out), vec!["nil".to_owned(), "false".to_owned()]);

        // A stored scoped function fails cleanly once its scope has ended.
        let mut rt = LuaRuntime::new().expect("runtime");
        rt.load_script(
            "assets/scripts/main.lua",
            "return {
               start = function() kept = engine.raycast end,
               update = function() kept(vec3(0,0,0), vec3(0,0,-1), 5) end,
             }",
        )
        .expect("loads");
        rt.with_queries(&queries, |rt| rt.start())
            .expect("scope")
            .expect("start");
        assert!(rt.update(0.016).is_err());
    }

    #[test]
    fn scripts_overwriting_query_functions_cannot_break_with_queries() {
        let mut rt = LuaRuntime::new().expect("runtime");
        rt.load_script(
            "assets/scripts/main.lua",
            "return {
               start = function() engine.raycast = 1; engine.is_grounded = nil end,
               update = function()
                 local h = engine.raycast(vec3(0,0,0), vec3(0,0,-1), 5)
                 engine.log(tostring(h and h.object))
               end,
             }",
        )
        .expect("loads");
        let queries = fake(Some(wall_hit()), true);
        rt.start().expect("start overwrites the functions");
        // The next scoped callback gets a working raycast again.
        let out = rt
            .with_queries(&queries, |rt| rt.update(0.016))
            .expect("scope still works")
            .expect("update");
        assert_eq!(logs(&out), vec!["wall".to_owned()]);
        // A later plain callback sees the nil stub, not a dead function.
        let out = rt.update(0.016).expect("plain update");
        assert_eq!(logs(&out), vec!["nil".to_owned()]);
    }

    #[test]
    fn raycast_caps_max_distance() {
        let mut rt = runtime(None, "a", "engine.raycast(vec3(0,0,0), vec3(0,0,-1), 1e9)");
        let queries = fake(None, false);
        rt.with_queries(&queries, |rt| rt.start())
            .expect("scope")
            .expect("start");
        assert_eq!(queries.seen_max.get(), 10_000.0);
    }

    #[test]
    fn move_character_queues_command() {
        let mut rt = runtime(None, "a", "engine.move_character('a', vec3(0, -1, 2))");
        let out = rt.start().expect("start");
        assert_eq!(
            out,
            vec![GameCommand::MoveCharacter {
                object_id: "a".into(),
                delta: [0.0, -1.0, 2.0]
            }]
        );
    }

    #[test]
    fn raycast_passes_the_optional_ignore_id() {
        let queries = fake(Some(wall_hit()), false);
        let mut rt = runtime(
            None,
            "a",
            "engine.raycast(vec3(0,0,0), vec3(0,0,-1), 5, 'player')",
        );
        rt.with_queries(&queries, |rt| rt.start().expect("start"))
            .expect("queries");
        assert_eq!(queries.seen_ignore.borrow().as_deref(), Some("player"));
        let mut rt = runtime(
            None,
            "a",
            "engine.raycast(vec3(0,0,0), vec3(0,0,-1), 5, nil)",
        );
        rt.with_queries(&queries, |rt| rt.start().expect("start"))
            .expect("queries");
        assert_eq!(*queries.seen_ignore.borrow(), None);
        let mut rt = runtime(None, "a", "engine.raycast(vec3(0,0,0), vec3(0,0,-1), 5, 7)");
        assert!(
            rt.with_queries(&queries, |rt| rt.start())
                .expect("queries")
                .is_err()
        );
    }

    fn force_durations(out: &[GameCommand]) -> Vec<f32> {
        out.iter()
            .filter_map(|c| match c {
                GameCommand::AddForce { duration, .. } => Some(*duration),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn add_force_duration_follows_the_callback() {
        let mut rt = LuaRuntime::new().expect("runtime");
        rt.set_fixed_step(0.02);
        rt.load_script(
            "assets/scripts/main.lua",
            "return {
                start = function() engine.add_force('a', 0, 1, 0) end,
                update = function(dt) engine.add_force('a', 0, 1, 0) end,
                fixed_update = function(dt) engine.add_force('a', 0, 1, 0) end,
                on_event = function() engine.add_force('a', 0, 1, 0) end,
            }",
        )
        .expect("loads");
        assert_eq!(force_durations(&rt.start().expect("start")), vec![0.02]);
        assert_eq!(
            force_durations(&rt.update(0.25).expect("update")),
            vec![0.25]
        );
        assert_eq!(
            force_durations(&rt.fixed_update(0.01).expect("fixed")),
            vec![0.01]
        );
        assert_eq!(
            force_durations(&rt.on_event("k", "v").expect("event")),
            vec![0.02]
        );
    }
}
