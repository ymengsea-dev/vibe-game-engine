//! Tests for the language-neutral gameplay runtime. Anything here is what a
//! Rust game gets without Lua.

use crate::ecs::components::{Name, Transform};
use crate::scene::{RuleVariables, Scene, SceneObjectId};
use engine_scripting::GameCommand;

use super::*;

/// Applies world commands with no asset package; prefab requests fail.
fn apply_commands(world: &mut crate::prelude::World, commands: Vec<GameCommand>) {
    apply_world_commands(world, &commands, &mut |_, path, _, _| {
        Err(GameplayError::Prefab(path.into(), "no assets".into()))
    })
    .expect("command batch");
}

fn position(world: &mut crate::prelude::World, id: &str) -> [f32; 3] {
    let entity = find_object(world, id).expect("object");
    world
        .get::<Transform>(entity)
        .expect("transform")
        .0
        .translation
        .to_array()
}

#[test]
fn rust_code_can_spawn_move_and_set_variables_with_commands() {
    let mut world = crate::prelude::World::new();
    let id = "00000000-0000-4000-8000-000000000001";
    apply_commands(
        &mut world,
        vec![
            GameCommand::SpawnEmpty {
                object_id: id.into(),
                name: "Coin".into(),
                position: [1.0, 2.0, 3.0],
            },
            GameCommand::Translate {
                object_id: id.into(),
                delta: [1.0, 0.0, 0.0],
            },
            GameCommand::SetVariable {
                name: "coins".into(),
                value: 4.0,
            },
        ],
    );
    assert_eq!(position(&mut world, id), [2.0, 2.0, 3.0]);
    let entity = find_object(&world, id).expect("spawned");
    assert_eq!(
        world.get::<Name>(entity).map(|name| name.0.as_str()),
        Some("Coin")
    );
    assert_eq!(world.resource::<RuleVariables>().get("coins"), 4.0);
}

#[test]
fn invalid_requests_are_skipped_without_stopping_the_batch() {
    let mut world = crate::prelude::World::new();
    world.spawn((SceneObjectId("actor".into()), Transform::default()));
    apply_commands(
        &mut world,
        vec![
            GameCommand::SpawnEmpty {
                object_id: "not-a-uuid".into(),
                name: "Bad".into(),
                position: [0.0; 3],
            },
            GameCommand::SetPosition {
                object_id: "actor".into(),
                position: [f32::NAN, 0.0, 0.0],
            },
            GameCommand::Translate {
                object_id: "missing".into(),
                delta: [1.0, 0.0, 0.0],
            },
            GameCommand::SetVariable {
                name: String::new(),
                value: 1.0,
            },
            GameCommand::SetPosition {
                object_id: "actor".into(),
                position: [7.0, 0.0, 0.0],
            },
        ],
    );
    assert_eq!(position(&mut world, "actor"), [7.0, 0.0, 0.0]);
    assert_eq!(
        world
            .iter_entities()
            .filter(|e| e.get::<Name>().is_some())
            .count(),
        0
    );
}

#[test]
fn a_failed_prefab_spawn_stops_the_batch_with_an_error() {
    let mut world = crate::prelude::World::new();
    let result = apply_world_commands(
        &mut world,
        &[GameCommand::SpawnPrefab {
            object_id: "00000000-0000-4000-8000-000000000002".into(),
            path: "assets/prefabs/Crate.prefab".into(),
            position: [0.0; 3],
        }],
        &mut |_, path, _, _| Err(GameplayError::Prefab(path.into(), "no assets".into())),
    );
    assert!(
        matches!(result, Err(GameplayError::Prefab(path, _)) if path == "assets/prefabs/Crate.prefab")
    );
}

#[test]
fn scene_requests_queue_the_first_target_and_need_a_source() {
    let mut runtime = GameplayRuntime::new(Scene::default(), None);
    runtime.request_scene_change("scenes/a.ron");
    runtime.request_scene_change("scenes/b.ron");
    assert_eq!(
        runtime.take_scene_request().as_deref(),
        Some("scenes/a.ron")
    );
    assert_eq!(runtime.take_scene_request(), None);
    assert!(matches!(
        runtime.load_scene("scenes/a.ron"),
        Err(GameplayError::NoSceneSource(_))
    ));
}

#[test]
fn scene_and_prefab_paths_must_stay_inside_their_folders() {
    assert!(valid_scene_path("scenes/level2.ron"));
    assert!(valid_scene_path("scenes/world/level3.ron"));
    for bad in [
        "../scenes/x.ron",
        "/scenes/x.ron",
        "scenes\\x.ron",
        "scenes//x.ron",
        "assets/x.ron",
        "scenes/x.txt",
    ] {
        assert!(!valid_scene_path(bad), "{bad}");
    }
    assert!(valid_prefab_path("assets/prefabs/Crate.prefab"));
    for bad in [
        "assets/prefabs/../scripts/main.lua",
        "assets/Crate.prefab",
        "/assets/prefabs/a.prefab",
    ] {
        assert!(!valid_prefab_path(bad), "{bad}");
    }
}

#[test]
fn collision_events_name_their_kind_and_report_the_other_side() {
    let contact = CollisionEvent {
        kind: ContactKind::Enter,
        first: "coin".into(),
        second: "player".into(),
    };
    assert_eq!(contact.kind.name(), "enter");
    assert_eq!(ContactKind::Exit.name(), "exit");
    assert!(contact.involves("player"));
    assert_eq!(contact.other("player"), Some("coin"));
    assert_eq!(contact.other("coin"), Some("player"));
    assert_eq!(contact.other("wall"), None);
}

#[test]
fn moved_body_is_teleported_before_the_next_physics_step() {
    let mut physics = crate::physics::PhysicsWorld::default();
    let (body, collider) = crate::ecs::components::RigidBody::spawn(
        &mut physics,
        crate::physics::RigidBodyBuilder::dynamic(),
        crate::physics::ColliderBuilder::ball(0.5),
    );
    let mut world = crate::prelude::World::new();
    world.spawn((
        crate::scene::SceneObjectId("actor".into()),
        crate::ecs::components::Transform::default(),
        body,
        collider,
    ));
    apply_commands(
        &mut world,
        vec![
            GameCommand::SetPosition {
                object_id: "actor".into(),
                position: [4.0, 5.0, 6.0],
            },
            GameCommand::Translate {
                object_id: "actor".into(),
                delta: [1.0, 0.0, 0.0],
            },
        ],
    );
    let poses = commands::moved_body_poses(&world, &["actor"]);
    commands::sync_moved_bodies(&mut physics, &poses);
    assert_eq!(
        physics
            .rapier
            .bodies
            .get(body.0)
            .expect("body")
            .translation(),
        crate::prelude::glam::Vec3::new(5.0, 5.0, 6.0)
    );
    physics.step(1.0 / 60.0).expect("next physics step");
    assert!(
        (physics
            .rapier
            .bodies
            .get(body.0)
            .expect("body")
            .translation()
            .x
            - 5.0)
            .abs()
            < 1e-3
    );
}
