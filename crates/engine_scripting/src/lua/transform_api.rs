//! Lua `engine.*` rotation, scale and facing calls.
//!
//! Rotations are Euler degrees `vec3(pitch, yaw, roll)` applied yaw, pitch,
//! then roll. Forward is -Z, right is +X, up is +Y.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use glam::{EulerRot, Mat3, Quat, Vec3};
use mlua::{Lua, MultiValue, Table, Value};

use super::{MAX_COMMANDS, SceneSnapshot, string_arg, vec3};
use crate::GameCommand;

type Commands = Rc<RefCell<Vec<GameCommand>>>;
type Scene = Rc<RefCell<SceneSnapshot>>;

fn error(message: &str) -> mlua::Error {
    mlua::Error::RuntimeError(message.to_owned())
}

/// Checks the callback is active, then consumes and validates the object ID.
fn command_id(lua: &Lua, args: &mut MultiValue, active: &Cell<bool>) -> mlua::Result<String> {
    if !active.get() {
        return Err(error("engine commands may only run in callbacks"));
    }
    let object_id = string_arg(lua, args, "object ID")?;
    if object_id.is_empty() || object_id.len() > 64 {
        return Err(error("object ID must be nonempty and at most 64 bytes"));
    }
    Ok(object_id)
}

fn degrees_to_quat(degrees: Vec3) -> Quat {
    Quat::from_euler(
        EulerRot::YXZ,
        degrees.y.to_radians(),
        degrees.x.to_radians(),
        degrees.z.to_radians(),
    )
}

fn quat_to_degrees(rotation: Quat) -> Vec3 {
    let (yaw, pitch, roll) = rotation.normalize().to_euler(EulerRot::YXZ);
    // Snap float noise so identity-like angles read as 0, not -0.0 or 1e-6.
    let clean = |radians: f32| {
        let degrees = radians.to_degrees();
        if degrees.abs() < 1e-4 { 0.0 } else { degrees }
    };
    Vec3::new(clean(pitch), clean(yaw), clean(roll))
}

/// Returns a finite quaternion or a Lua error naming the bad input.
fn checked(rotation: Quat) -> mlua::Result<Quat> {
    if rotation.is_finite() {
        Ok(rotation.normalize())
    } else {
        Err(error("rotation must be finite"))
    }
}

/// Reads a rotation as `Quat`; the second step pushes the command.
fn push(commands: &Commands, command: impl FnOnce() -> GameCommand) -> mlua::Result<()> {
    let mut pending = commands.borrow_mut();
    if pending.len() >= MAX_COMMANDS {
        return Err(error("Lua command limit exceeded"));
    }
    pending.push(command());
    Ok(())
}

fn limit_check(commands: &Commands) -> mlua::Result<()> {
    if commands.borrow().len() >= MAX_COMMANDS {
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
    // set_rotation / rotate
    for (name, additive) in [("set_rotation", false), ("rotate", true)] {
        let (commands, scene, active) = (Rc::clone(commands), Rc::clone(scene), Rc::clone(active));
        engine.set(
            name,
            lua.create_function(move |lua, mut args: MultiValue| {
                let object_id = command_id(lua, &mut args, &active)?;
                let degrees = vec3::vector_arg(&mut args, "rotation")?;
                let quat = checked(degrees_to_quat(degrees))?;
                limit_check(&commands)?;
                {
                    let mut snapshot = scene.borrow_mut();
                    if let Some(current) = snapshot.rotation(&object_id) {
                        let next = if additive { current * quat } else { quat };
                        if next.is_finite() {
                            snapshot
                                .pending_rotations
                                .insert(object_id.clone(), next.normalize().to_array());
                        }
                    }
                }
                push(&commands, || {
                    if additive {
                        GameCommand::Rotate {
                            object_id,
                            delta: quat.to_array(),
                        }
                    } else {
                        GameCommand::SetRotation {
                            object_id,
                            rotation: quat.to_array(),
                        }
                    }
                })
            })?,
        )?;
    }

    let read_scene = Rc::clone(scene);
    engine.set(
        "get_rotation",
        lua.create_function(move |lua, object_id: String| {
            read_scene
                .borrow()
                .rotation(&object_id)
                .map(|rotation| vec3::to_lua(lua, quat_to_degrees(rotation)))
                .transpose()
        })?,
    )?;

    let read_scene = Rc::clone(scene);
    engine.set(
        "get_scale",
        lua.create_function(move |lua, object_id: String| {
            read_scene
                .borrow()
                .scale(&object_id)
                .map(|scale| vec3::to_lua(lua, scale))
                .transpose()
        })?,
    )?;

    let (set_commands, set_scene, set_active) =
        (Rc::clone(commands), Rc::clone(scene), Rc::clone(active));
    engine.set(
        "set_scale",
        lua.create_function(move |lua, mut args: MultiValue| {
            let object_id = command_id(lua, &mut args, &set_active)?;
            let scale = match args.front() {
                Some(Value::Number(_) | Value::Integer(_)) if args.len() == 1 => {
                    let single = match args.pop_front() {
                        Some(Value::Number(n)) => n,
                        Some(Value::Integer(n)) => n as f64,
                        _ => 0.0,
                    };
                    let v = Vec3::splat(single as f32);
                    if !v.is_finite() {
                        return Err(error("scale must be finite"));
                    }
                    v
                }
                _ => vec3::vector_arg(&mut args, "scale")?,
            };
            if scale.to_array().contains(&0.0) {
                return Err(error("scale must be non-zero"));
            }
            limit_check(&set_commands)?;
            {
                let mut snapshot = set_scene.borrow_mut();
                if snapshot.scale(&object_id).is_some() {
                    snapshot
                        .pending_scales
                        .insert(object_id.clone(), scale.to_array());
                }
            }
            push(&set_commands, || GameCommand::SetScale {
                object_id,
                scale: scale.to_array(),
            })
        })?,
    )?;

    let (look_commands, look_scene, look_active) =
        (Rc::clone(commands), Rc::clone(scene), Rc::clone(active));
    engine.set(
        "look_at",
        lua.create_function(move |lua, mut args: MultiValue| {
            let object_id = command_id(lua, &mut args, &look_active)?;
            let target = vec3::vector_arg(&mut args, "target")?;
            let up = if args.is_empty() || matches!(args.front(), Some(Value::Nil)) {
                Vec3::Y
            } else {
                vec3::vector_arg(&mut args, "up")?
            };
            limit_check(&look_commands)?;
            let mut snapshot = look_scene.borrow_mut();
            let Some(position) = snapshot.position(&object_id) else {
                return Ok(());
            };
            let direction = target - Vec3::from_array(position);
            if direction.length() < 1e-6 {
                return Ok(());
            }
            let forward = direction.normalize();
            let side = forward.cross(up);
            if side.length() < 1e-6 {
                return Ok(());
            }
            let right = side.normalize();
            let true_up = right.cross(forward);
            let rotation = Quat::from_mat3(&Mat3::from_cols(right, true_up, -forward)).normalize();
            if !rotation.is_finite() {
                return Ok(());
            }
            snapshot
                .pending_rotations
                .insert(object_id.clone(), rotation.to_array());
            drop(snapshot);
            push(&look_commands, || GameCommand::SetRotation {
                object_id,
                rotation: rotation.to_array(),
            })
        })?,
    )?;

    for (name, axis) in [
        ("forward", Vec3::NEG_Z),
        ("right", Vec3::X),
        ("up", Vec3::Y),
    ] {
        let read_scene = Rc::clone(scene);
        engine.set(
            name,
            lua.create_function(move |lua, object_id: String| {
                read_scene
                    .borrow()
                    .rotation(&object_id)
                    .map(|rotation| vec3::to_lua(lua, rotation * axis))
                    .transpose()
            })?,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{GameCommand, LuaRuntime};
    use glam::Quat;

    fn run(body: &str) -> Result<Vec<GameCommand>, crate::ScriptError> {
        let mut runtime = LuaRuntime::new().expect("runtime");
        runtime.set_scene_snapshot([("actor".to_owned(), None, Some([0.0, 0.0, 0.0]))]);
        runtime
            .load_script(
                "assets/scripts/main.lua",
                &format!("return {{ start = function() {body} end }}"),
            )
            .expect("script loads");
        runtime.start()
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

    fn rotations(commands: &[GameCommand]) -> Vec<Quat> {
        commands
            .iter()
            .filter_map(|c| match c {
                GameCommand::SetRotation { rotation, .. } => Some(Quat::from_array(*rotation)),
                _ => None,
            })
            .collect()
    }

    fn parse(line: &str) -> Vec<f32> {
        line.split(',')
            .map(|v| v.parse().expect("number"))
            .collect()
    }

    #[test]
    fn rotation_calls_queue_commands_and_read_back_pending() {
        let out = run("engine.set_rotation('actor', vec3(0, 90, 0))
             local r = engine.get_rotation('actor')
             engine.log(string.format('%.1f,%.1f,%.1f', r.x, r.y, r.z))
             engine.set_scale('actor', 3)
             local s = engine.get_scale('actor')
             engine.log(string.format('%.1f,%.1f,%.1f', s.x, s.y, s.z))")
        .expect("start");
        assert_eq!(logs(&out), ["0.0,90.0,0.0", "3.0,3.0,3.0"]);
        let first = rotations(&out)[0];
        assert!(first.abs_diff_eq(Quat::from_rotation_y(90f32.to_radians()), 1e-5));
    }

    #[test]
    fn rotate_composes_locally_and_missing_objects_read_nil() {
        let out = run("engine.rotate('actor', vec3(0, 90, 0))
             engine.rotate('actor', vec3(0, 90, 0))
             local r = engine.get_rotation('actor')
             engine.log(string.format('%.1f', math.abs(r.y)))
             engine.log(tostring(engine.get_rotation('nope')))
             engine.log(tostring(engine.forward('nope')))")
        .expect("start");
        assert_eq!(logs(&out), ["180.0", "nil", "nil"]);
    }

    #[test]
    fn facing_directions_follow_rotation() {
        let out = run("engine.set_rotation('actor', vec3(0, 90, 0))
             local f = engine.forward('actor')
             engine.log(string.format('%.4f,%.4f,%.4f', f.x, f.y, f.z))
             local r = engine.right('actor')
             engine.log(string.format('%.4f,%.4f,%.4f', r.x, r.y, r.z))
             local u = engine.up('actor')
             engine.log(string.format('%.4f,%.4f,%.4f', u.x, u.y, u.z))")
        .expect("start");
        let l = logs(&out);
        let (f, r, u) = (parse(&l[0]), parse(&l[1]), parse(&l[2]));
        for (got, want) in [
            (f, [-1.0, 0.0, 0.0]),
            (r, [0.0, 0.0, -1.0]),
            (u, [0.0, 1.0, 0.0]),
        ] {
            for (a, b) in got.iter().zip(want) {
                assert!((a - b).abs() < 1e-4, "{got:?} vs {want:?}");
            }
        }
    }

    #[test]
    fn rotation_round_trips_at_steep_pitch() {
        let out = run("engine.set_rotation('actor', vec3(90, 30, 0))
             local r = engine.get_rotation('actor')
             engine.set_rotation('actor', r)")
        .expect("start");
        let q = rotations(&out);
        assert_eq!(q.len(), 2);
        assert!(q.iter().all(|q| q.is_finite()));
        assert!(q[0].dot(q[1]).abs() > 0.9999, "{:?}", q);
    }

    #[test]
    fn look_at_points_forward_at_target() {
        let out = run("engine.look_at('actor', vec3(10, 0, 0))
             local f = engine.forward('actor')
             engine.log(string.format('%.4f,%.4f,%.4f', f.x, f.y, f.z))")
        .expect("start");
        let f = parse(&logs(&out)[0]);
        assert!((f[0] - 1.0).abs() < 1e-4 && f[1].abs() < 1e-4 && f[2].abs() < 1e-4);
        assert_eq!(rotations(&out).len(), 1);
    }

    #[test]
    fn look_at_treats_a_nil_up_like_a_missing_one() {
        let out = run("engine.look_at('actor', vec3(10, 0, 0), nil)").expect("nil up is fine");
        assert_eq!(rotations(&out).len(), 1);
    }

    #[test]
    fn look_at_degenerate_targets_do_nothing() {
        let out = run("engine.look_at('actor', engine.get_position('actor'))
             engine.look_at('actor', vec3(0, 5, 0))")
        .expect("no error");
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn set_scale_accepts_number_or_vec3_and_rejects_zero() {
        let out = run("engine.set_scale('actor', 2) engine.set_scale('actor', vec3(1, 2, 3))")
            .expect("start");
        assert_eq!(
            out,
            [
                GameCommand::SetScale {
                    object_id: "actor".into(),
                    scale: [2.0; 3]
                },
                GameCommand::SetScale {
                    object_id: "actor".into(),
                    scale: [1.0, 2.0, 3.0]
                },
            ]
        );
        for body in [
            "engine.set_scale('actor', 0)",
            "engine.set_scale('actor', vec3(1, 0, 1))",
        ] {
            let err = run(body).expect_err("zero scale").to_string();
            assert!(err.contains("scale must be non-zero"), "{err}");
        }
    }
}
