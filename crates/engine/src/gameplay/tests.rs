//! Tests for the language-neutral gameplay runtime. Anything here is what a
//! Rust game gets without Lua.

use crate::ecs::components::{Name, Transform};
use crate::scene::{RuleVariables, Scene, SceneObjectId};
use engine_scripting::{GameCommand, ScriptQueries};

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
fn sliders_and_toggles_write_variables_and_bars_read_them() {
    use engine_scene::{SceneEntity, UiElementData};
    use engine_ui::{PointerInput, Ui, Widget};

    let element = |id: &str, data: UiElementData| SceneEntity {
        object_id: Some(id.into()),
        ui: Some(data),
        ..Default::default()
    };
    let scene = Scene {
        entities: vec![
            element(
                "slider",
                UiElementData {
                    anchor: engine_scene::UiAnchor::TopLeft,
                    size: [100.0, 20.0],
                    step: 0.1,
                    variable: "volume".into(),
                    ..UiElementData::slider()
                },
            ),
            element(
                "bar",
                UiElementData {
                    anchor: engine_scene::UiAnchor::BottomLeft,
                    variable: "volume".into(),
                    max: 1.0,
                    ..UiElementData::progress_bar()
                },
            ),
        ],
        ..Default::default()
    };
    let mut ui = Ui::new(400.0, 300.0);
    let nodes = scene.mount_ui(&mut ui);
    let node = |id: &str| {
        nodes
            .iter()
            .find_map(|(node, owner)| (owner == id).then_some(*node))
            .expect("mounted")
    };
    let mut world = crate::prelude::World::new();
    ui.layout();
    ui.interact(PointerInput {
        position: [30.0, 10.0],
        pressed: true,
        released: true,
    });
    store_ui_changes(&mut world, &mut ui);
    assert_eq!(
        world.resource::<RuleVariables>().get("volume"),
        0.3,
        "a tidy decimal, not a widened f32"
    );
    let bar_value = |ui: &Ui| match ui.node(node("bar")).expect("bar").widget {
        Widget::ProgressBar { value, .. } => value,
        _ => unreachable!(),
    };
    assert!((bar_value(&ui) - 0.3).abs() < 1e-6);

    // A script's change reaches both, and is not taken for the player's.
    world
        .resource_mut::<RuleVariables>()
        .set("volume", 0.9)
        .expect("set");
    store_ui_changes(&mut world, &mut ui);
    assert!((bar_value(&ui) - 0.9).abs() < 1e-6);
    assert!(matches!(
        ui.node(node("slider")).expect("slider").widget,
        Widget::Slider { value, .. } if (value - 0.9).abs() < 1e-6
    ));
    assert_eq!(world.resource::<RuleVariables>().get("volume"), 0.9);
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
    assert!(!valid_prefab_path("assets\\prefabs\\Crate.prefab"));
    assert!(!valid_prefab_path("assets/prefabs//Crate.prefab"));
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
fn moving_a_parented_object_warns_only_when_it_has_a_body() {
    #[derive(Clone, Default)]
    struct Log(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Log {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("log").extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut physics = crate::physics::PhysicsWorld::default();
    let (body, collider) = crate::ecs::components::RigidBody::spawn(
        &mut physics,
        crate::physics::RigidBodyBuilder::dynamic(),
        crate::physics::ColliderBuilder::ball(0.5),
    );
    let mut world = crate::prelude::World::new();
    let parent = world.spawn_empty().id();
    world.spawn((
        crate::scene::SceneObjectId("camera".into()),
        crate::ecs::components::Transform::default(),
        engine_ecs::prelude::ChildOf(parent),
    ));
    world.spawn((
        crate::scene::SceneObjectId("rider".into()),
        crate::ecs::components::Transform::default(),
        engine_ecs::prelude::ChildOf(parent),
        body,
        collider,
    ));

    let warned_for = |id: &str| {
        let log = Log::default();
        let writer = log.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .finish();
        let poses = tracing::subscriber::with_default(subscriber, || {
            commands::moved_body_poses(&world, &[id])
        });
        assert!(poses.is_empty(), "{id}");
        let text = String::from_utf8(log.0.lock().expect("log").clone()).expect("utf8");
        text.contains("moving a parented physics body is unsupported")
    };
    assert!(!warned_for("camera"));
    assert!(warned_for("rider"));
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

fn transform_of(world: &mut crate::prelude::World, id: &str) -> engine_utils::Transform {
    let entity = find_object(world, id).expect("object");
    world.get::<Transform>(entity).expect("transform").0
}

#[test]
fn rotation_and_scale_commands_update_transforms() {
    use crate::prelude::glam::Quat;
    let mut world = crate::prelude::World::new();
    world.spawn((SceneObjectId("actor".into()), Transform::default()));
    let quarter = Quat::from_rotation_y(90f32.to_radians());
    apply_commands(
        &mut world,
        vec![
            GameCommand::SetRotation {
                object_id: "actor".into(),
                rotation: quarter.to_array(),
            },
            GameCommand::Rotate {
                object_id: "actor".into(),
                delta: quarter.to_array(),
            },
            GameCommand::SetScale {
                object_id: "actor".into(),
                scale: [2.0, 2.0, 2.0],
            },
        ],
    );
    let t = transform_of(&mut world, "actor");
    assert!(
        t.rotation
            .abs_diff_eq(Quat::from_rotation_y(180f32.to_radians()), 1e-5)
    );
    assert_eq!(t.scale, crate::prelude::glam::Vec3::splat(2.0));
}

#[test]
fn rotating_a_body_syncs_physics_rotation() {
    use crate::prelude::glam::Quat;
    let mut physics = crate::physics::PhysicsWorld::default();
    let (body, collider) = crate::ecs::components::RigidBody::spawn(
        &mut physics,
        crate::physics::RigidBodyBuilder::dynamic(),
        crate::physics::ColliderBuilder::ball(0.5),
    );
    let mut world = crate::prelude::World::new();
    world.spawn((
        SceneObjectId("actor".into()),
        Transform::default(),
        body,
        collider,
    ));
    let quat = Quat::from_rotation_y(1.0);
    let commands = vec![GameCommand::SetRotation {
        object_id: "actor".into(),
        rotation: quat.to_array(),
    }];
    apply_commands(&mut world, commands.clone());
    let poses = commands::moved_body_poses(&world, &commands::moved_object_ids(&commands));
    assert_eq!(poses.len(), 1);
    commands::sync_moved_bodies(&mut physics, &poses);
    let actual = *physics.rapier.bodies.get(body.0).expect("body").rotation();
    assert!(actual.abs_diff_eq(quat, 1e-5));
}

#[test]
fn non_finite_rotation_and_zero_scale_are_ignored() {
    let mut world = crate::prelude::World::new();
    world.spawn((SceneObjectId("actor".into()), Transform::default()));
    let before = transform_of(&mut world, "actor");
    apply_commands(
        &mut world,
        vec![
            GameCommand::SetRotation {
                object_id: "actor".into(),
                rotation: [f32::NAN, 0.0, 0.0, 1.0],
            },
            GameCommand::Rotate {
                object_id: "actor".into(),
                delta: [0.0, 0.0, 0.0, 0.0],
            },
            GameCommand::SetScale {
                object_id: "actor".into(),
                scale: [0.0, 1.0, 1.0],
            },
        ],
    );
    assert_eq!(transform_of(&mut world, "actor"), before);
}

mod triggers {
    use super::*;
    use crate::scene::{AssetLibrary, BodyKind, ColliderData, ColliderShape};
    use engine_ecs::components::RigidBody;

    fn library() -> AssetLibrary {
        let dir = std::env::temp_dir().join(format!("vge-trigger-lib-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        AssetLibrary::index(&dir).expect("library")
    }

    fn spawn_built(
        world: &mut crate::prelude::World,
        physics: &mut crate::physics::PhysicsWorld,
        id: &str,
        data: &ColliderData,
        at: [f32; 3],
    ) -> RigidBody {
        let transform = engine_utils::Transform::from_translation(at.into());
        let (body, collider) = crate::app::build_collider(data, &transform, &library(), Some(id))
            .expect("collider builds");
        let (body, collider) = RigidBody::spawn(physics, body, collider);
        world.spawn((
            SceneObjectId(id.into()),
            Transform(transform),
            body,
            collider,
        ));
        body
    }

    /// A coin trigger at the origin and a kinematic capsule player far away.
    fn coin_scene() -> (
        crate::prelude::World,
        crate::physics::PhysicsWorld,
        RigidBody,
    ) {
        let mut world = crate::prelude::World::new();
        let mut physics = crate::physics::PhysicsWorld::default();
        let mut coin = ColliderData::new(ColliderShape::Ball { radius: 0.5 }, BodyKind::Fixed);
        coin.trigger = true;
        spawn_built(&mut world, &mut physics, "coin", &coin, [0.0, 0.0, 0.0]);
        let player = ColliderData::new(
            ColliderShape::Capsule {
                half_height: 0.5,
                radius: 0.3,
            },
            BodyKind::Kinematic,
        );
        let body = spawn_built(&mut world, &mut physics, "player", &player, [5.0, 0.0, 0.0]);
        (world, physics, body)
    }

    fn move_player(physics: &mut crate::physics::PhysicsWorld, body: RigidBody, x: f32) {
        if let Some(rb) = physics.rapier.bodies.get_mut(body.0) {
            rb.set_next_kinematic_translation([x, 0.0, 0.0].into());
        }
        physics.step(1.0 / 60.0).expect("step");
    }

    fn run(
        world: &crate::prelude::World,
        physics: &mut crate::physics::PhysicsWorld,
        body: RigidBody,
        tracker: &mut ContactTracker,
        xs: &[f32],
    ) -> Vec<CollisionEvent> {
        let mut all = Vec::new();
        for x in xs {
            move_player(physics, body, *x);
            all.extend(tracker.sample(world, physics));
        }
        all
    }

    #[test]
    fn fixed_trigger_sees_kinematic_character_once() {
        let (world, mut physics, body) = coin_scene();
        let mut tracker = ContactTracker::default();
        let approach = run(&world, &mut physics, body, &mut tracker, &[4.0, 2.0, 0.0]);
        assert!(approach.is_empty() || approach.len() == 1);
        let overlap = run(&world, &mut physics, body, &mut tracker, &[0.0; 5]);
        let all: Vec<_> = approach.into_iter().chain(overlap).collect();
        assert_eq!(all.len(), 1, "{all:?}");
        assert_eq!(all[0].kind, ContactKind::TriggerEnter);
        assert_eq!(all[0].first, "coin");
        assert_eq!(all[0].second, "player");
        let leave = run(&world, &mut physics, body, &mut tracker, &[3.0, 5.0, 6.0]);
        assert_eq!(leave.len(), 1, "{leave:?}");
        assert_eq!(leave[0].kind, ContactKind::TriggerExit);
    }

    #[test]
    fn move_character_walks_through_a_trigger() {
        let (world, mut physics, body) = coin_scene();
        physics.step(0.0).expect("refresh broad phase");
        let mut tracker = ContactTracker::default();
        let mut warned = std::collections::HashSet::new();
        let mut grounded = std::collections::HashMap::new();
        let mut events = Vec::new();
        for _ in 0..40 {
            bodies::apply_body_command(
                &world,
                &mut physics,
                &mut warned,
                &mut grounded,
                &GameCommand::MoveCharacter {
                    object_id: "player".into(),
                    delta: [-0.25, 0.0, 0.0],
                },
                1.0 / 60.0,
            );
            physics.step(1.0 / 60.0).expect("step");
            events.extend(tracker.sample(&world, &physics));
        }
        let x = physics
            .rapier
            .bodies
            .get(body.0)
            .expect("body")
            .translation()
            .x;
        // Trigger far side is at -0.5; the capsule radius is 0.3.
        assert!(x < -0.8, "trigger blocked the character at x = {x}");
        let enters = events
            .iter()
            .filter(|e| e.kind == ContactKind::TriggerEnter)
            .count();
        assert_eq!(enters, 1, "{events:?}");
    }

    #[test]
    fn triggers_never_report_as_collisions() {
        let (world, mut physics, body) = coin_scene();
        let mut tracker = ContactTracker::default();
        let events = run(
            &world,
            &mut physics,
            body,
            &mut tracker,
            &[2.0, 0.0, 0.0, 0.0, 3.0, 6.0],
        );
        assert!(!events.is_empty());
        assert!(
            events
                .iter()
                .all(|e| matches!(e.kind, ContactKind::TriggerEnter | ContactKind::TriggerExit)),
            "{events:?}"
        );
    }

    #[test]
    fn fixed_trigger_on_fixed_floor_reports_nothing() {
        let mut world = crate::prelude::World::new();
        let mut physics = crate::physics::PhysicsWorld::default();
        let floor = ColliderData::new(
            ColliderShape::Cuboid {
                half_extents: [20.0, 0.5, 20.0],
            },
            BodyKind::Fixed,
        );
        spawn_built(&mut world, &mut physics, "floor", &floor, [0.0, -0.5, 0.0]);
        let mut goal = ColliderData::new(ColliderShape::Ball { radius: 0.5 }, BodyKind::Fixed);
        goal.trigger = true;
        // Half sunk into the floor.
        spawn_built(&mut world, &mut physics, "goal", &goal, [0.0, 0.0, 0.0]);
        let mut tracker = ContactTracker::default();
        for _ in 0..5 {
            physics.step(1.0 / 60.0).expect("step");
            let events = tracker.sample(&world, &physics);
            assert!(events.is_empty(), "{events:?}");
        }
    }

    #[test]
    fn trigger_vs_trigger() {
        let mut world = crate::prelude::World::new();
        let mut physics = crate::physics::PhysicsWorld::default();
        for (id, kind, at) in [
            ("zone_a", BodyKind::Kinematic, [0.0, 0.0, 0.0]),
            ("zone_b", BodyKind::Kinematic, [0.2, 0.0, 0.0]),
        ] {
            let mut zone = ColliderData::new(ColliderShape::Ball { radius: 0.5 }, kind);
            zone.trigger = true;
            spawn_built(&mut world, &mut physics, id, &zone, at);
        }
        let mut tracker = ContactTracker::default();
        let mut enters = 0;
        for _ in 0..5 {
            physics.step(1.0 / 60.0).expect("step");
            enters += tracker
                .sample(&world, &physics)
                .iter()
                .filter(|e| e.kind == ContactKind::TriggerEnter)
                .count();
        }
        // Kinematic-kinematic pairs are active for triggers, so overlapping
        // zones report one enter (not one per frame).
        assert_eq!(enters, 1);
    }

    #[test]
    fn trigger_kind_names() {
        assert_eq!(ContactKind::TriggerEnter.name(), "trigger_enter");
        assert_eq!(ContactKind::TriggerExit.name(), "trigger_exit");
    }
}

fn spawn_body(
    world: &mut crate::prelude::World,
    physics: &mut crate::physics::PhysicsWorld,
    id: &str,
    body: crate::physics::RigidBodyBuilder,
) -> crate::physics::RigidBodyHandle {
    let (rb, collider) = crate::ecs::components::RigidBody::spawn(
        physics,
        body,
        crate::physics::ColliderBuilder::ball(0.5).mass(1.0),
    );
    let handle = rb.0;
    world.spawn((
        crate::scene::SceneObjectId(id.into()),
        crate::ecs::components::Transform::default(),
        rb,
        collider,
    ));
    handle
}

fn linvel(
    physics: &crate::physics::PhysicsWorld,
    handle: crate::physics::RigidBodyHandle,
) -> glam::Vec3 {
    physics.rapier.bodies.get(handle).expect("body").linvel()
}

#[test]
fn velocity_and_impulse_move_a_dynamic_body() {
    let mut physics = crate::physics::PhysicsWorld::default();
    let mut world = crate::prelude::World::new();
    let handle = spawn_body(
        &mut world,
        &mut physics,
        "ball",
        crate::physics::RigidBodyBuilder::dynamic(),
    );
    let mut warned = std::collections::HashSet::new();
    let mut grounded = std::collections::HashMap::new();
    let mut run = |physics: &mut crate::physics::PhysicsWorld, command: GameCommand| {
        bodies::apply_body_command(
            &world,
            physics,
            &mut warned,
            &mut grounded,
            &command,
            1.0 / 60.0,
        );
    };
    run(
        &mut physics,
        GameCommand::AddImpulse {
            object_id: "ball".into(),
            impulse: [0.0, 0.0, 2.0],
        },
    );
    assert!((linvel(&physics, handle).z - 2.0).abs() < 1e-4);
    run(
        &mut physics,
        GameCommand::SetVelocity {
            object_id: "ball".into(),
            velocity: [3.0, 0.0, 0.0],
        },
    );
    let v = linvel(&physics, handle);
    assert!(
        (v - glam::Vec3::new(3.0, 0.0, 0.0)).length() < 1e-4,
        "{v:?}"
    );
    run(
        &mut physics,
        GameCommand::AddForce {
            object_id: "ball".into(),
            force: [0.0, 0.0, 60.0],
            duration: 0.5,
        },
    );
    assert!((linvel(&physics, handle).z - 30.0).abs() < 1e-3);
}

#[test]
fn wrong_kind_physics_commands_warn_once_and_skip() {
    let mut physics = crate::physics::PhysicsWorld::default();
    let mut world = crate::prelude::World::new();
    spawn_body(
        &mut world,
        &mut physics,
        "fixed_id",
        crate::physics::RigidBodyBuilder::fixed(),
    );
    let dynamic = spawn_body(
        &mut world,
        &mut physics,
        "dyn",
        crate::physics::RigidBodyBuilder::dynamic(),
    );
    let batch = [
        GameCommand::AddImpulse {
            object_id: "fixed_id".into(),
            impulse: [1.0, 0.0, 0.0],
        },
        GameCommand::SetVelocity {
            object_id: "missing".into(),
            velocity: [1.0, 0.0, 0.0],
        },
        GameCommand::AddImpulse {
            object_id: "dyn".into(),
            impulse: [1.0, 0.0, 0.0],
        },
    ];
    let mut warned = std::collections::HashSet::new();
    let mut grounded = std::collections::HashMap::new();
    for command in &batch {
        bodies::apply_body_command(
            &world,
            &mut physics,
            &mut warned,
            &mut grounded,
            command,
            1.0 / 60.0,
        );
    }
    assert!((linvel(&physics, dynamic).x - 1.0).abs() < 1e-4);
    let expected: std::collections::HashSet<(String, &'static str)> = [
        ("fixed_id".to_owned(), "add_impulse"),
        ("missing".to_owned(), "set_velocity"),
    ]
    .into_iter()
    .collect();
    assert_eq!(warned, expected);
    for command in &batch {
        bodies::apply_body_command(
            &world,
            &mut physics,
            &mut warned,
            &mut grounded,
            command,
            1.0 / 60.0,
        );
    }
    assert_eq!(warned, expected);
    assert!((linvel(&physics, dynamic).x - 2.0).abs() < 1e-4);
}

#[test]
fn body_commands_after_a_spawn_in_the_same_batch_reach_the_new_body() {
    // Real `apply` needs a GPU, so this follows its order by hand: world
    // commands spawn "bullet", the collider step gives it a body, then the
    // batch's body commands run.
    let id = "00000000-0000-4000-8000-0000000000b1";
    let batch = vec![
        GameCommand::SpawnEmpty {
            object_id: id.into(),
            name: "Bullet".into(),
            position: [0.0; 3],
        },
        GameCommand::AddImpulse {
            object_id: id.into(),
            impulse: [0.0, 0.0, 2.0],
        },
    ];
    let mut world = crate::prelude::World::new();
    let mut physics = crate::physics::PhysicsWorld::default();
    apply_commands(&mut world, batch.clone());
    let (rb, collider) = crate::ecs::components::RigidBody::spawn(
        &mut physics,
        crate::physics::RigidBodyBuilder::dynamic(),
        crate::physics::ColliderBuilder::ball(0.5).mass(1.0),
    );
    let handle = rb.0;
    let entity = find_object(&world, id).expect("spawned");
    world.entity_mut(entity).insert((rb, collider));
    let mut warned = std::collections::HashSet::new();
    let mut grounded = std::collections::HashMap::new();
    bodies::apply_body_commands(
        &world,
        &mut physics,
        &mut warned,
        &mut grounded,
        &batch,
        1.0 / 60.0,
    );
    assert!((linvel(&physics, handle).z - 2.0).abs() < 1e-4);
    assert!(warned.is_empty());
}

fn spawn_collider(
    world: &mut crate::prelude::World,
    physics: &mut crate::physics::PhysicsWorld,
    id: &str,
    body: crate::physics::RigidBodyBuilder,
    collider: crate::physics::ColliderBuilder,
) -> crate::physics::RigidBodyHandle {
    let (rb, collider) = crate::ecs::components::RigidBody::spawn(physics, body, collider);
    let handle = rb.0;
    world.spawn((
        crate::scene::SceneObjectId(id.into()),
        crate::ecs::components::Transform::default(),
        rb,
        collider,
    ));
    handle
}

fn floor(world: &mut crate::prelude::World, physics: &mut crate::physics::PhysicsWorld) {
    // Top surface at y = 0.
    spawn_collider(
        world,
        physics,
        "floor",
        crate::physics::RigidBodyBuilder::fixed().translation(glam::Vec3::new(0.0, -0.5, 0.0)),
        crate::physics::ColliderBuilder::cuboid(20.0, 0.5, 20.0),
    );
}

fn queries<'a>(
    world: &'a crate::prelude::World,
    physics: &'a crate::physics::PhysicsWorld,
    grounded: &'a std::collections::HashMap<String, bool>,
) -> GameplayQueries<'a> {
    GameplayQueries {
        world,
        physics,
        grounded: Some(grounded),
    }
}

#[test]
fn move_character_slides_and_reports_grounded() {
    let mut physics = crate::physics::PhysicsWorld::default();
    let mut world = crate::prelude::World::new();
    floor(&mut world, &mut physics);
    let body = spawn_collider(
        &mut world,
        &mut physics,
        "hero",
        crate::physics::RigidBodyBuilder::kinematic_position_based()
            .translation(glam::Vec3::new(0.0, 2.0, 0.0)),
        crate::physics::ColliderBuilder::capsule_y(0.5, 0.3),
    );
    physics.step(0.0).expect("refresh broad phase");
    let mut warned = std::collections::HashSet::new();
    let mut grounded = std::collections::HashMap::new();
    let mut last_y = 2.0;
    for _ in 0..3 {
        bodies::apply_body_command(
            &world,
            &mut physics,
            &mut warned,
            &mut grounded,
            &GameCommand::MoveCharacter {
                object_id: "hero".into(),
                delta: [0.0, -1.0, 0.0],
            },
            1.0 / 60.0,
        );
        physics.step(1.0 / 60.0).expect("step");
        let y = physics
            .rapier
            .bodies
            .get(body)
            .expect("body")
            .translation()
            .y;
        assert!(y <= last_y + 1e-4, "moved up: {last_y} -> {y}");
        last_y = y;
    }
    assert_eq!(grounded.get("hero"), Some(&true));
    // Started at 2.0; the capsule extends 0.8 below its center, so it rests
    // at y ~ 0.8: it moved down and stopped on the floor.
    let y = last_y;
    assert!(
        (0.79..0.95).contains(&y),
        "expected to rest on the floor: {y}"
    );
    assert!(warned.is_empty());
    assert!(queries(&world, &physics, &grounded).is_grounded("hero"));
}

#[test]
fn move_character_on_non_kinematic_warns_once() {
    let mut physics = crate::physics::PhysicsWorld::default();
    let mut world = crate::prelude::World::new();
    spawn_body(
        &mut world,
        &mut physics,
        "crate",
        crate::physics::RigidBodyBuilder::dynamic(),
    );
    let command = GameCommand::MoveCharacter {
        object_id: "crate".into(),
        delta: [1.0, 0.0, 0.0],
    };
    let mut warned = std::collections::HashSet::new();
    let mut grounded = std::collections::HashMap::new();
    for _ in 0..3 {
        bodies::apply_body_command(
            &world,
            &mut physics,
            &mut warned,
            &mut grounded,
            &command,
            1.0 / 60.0,
        );
    }
    assert_eq!(warned.len(), 1);
    assert!(warned.contains(&("crate".to_owned(), "move_character")));
    assert!(grounded.is_empty());
}

#[test]
fn is_grounded_uses_a_short_ray_for_other_bodies() {
    let mut physics = crate::physics::PhysicsWorld::default();
    let mut world = crate::prelude::World::new();
    floor(&mut world, &mut physics);
    spawn_collider(
        &mut world,
        &mut physics,
        "resting",
        crate::physics::RigidBodyBuilder::dynamic().translation(glam::Vec3::new(0.0, 0.6, 0.0)),
        crate::physics::ColliderBuilder::cuboid(0.5, 0.5, 0.5),
    );
    spawn_collider(
        &mut world,
        &mut physics,
        "airborne",
        crate::physics::RigidBodyBuilder::fixed().translation(glam::Vec3::new(5.0, 5.0, 0.0)),
        crate::physics::ColliderBuilder::cuboid(0.5, 0.5, 0.5),
    );
    for _ in 0..30 {
        physics.step(1.0 / 60.0).expect("step");
    }
    let grounded = std::collections::HashMap::new();
    let q = queries(&world, &physics, &grounded);
    assert!(q.is_grounded("resting"));
    assert!(!q.is_grounded("airborne"));
    assert!(!q.is_grounded("missing"));
}

#[test]
fn raycast_ignores_triggers_and_maps_object_ids() {
    let mut physics = crate::physics::PhysicsWorld::default();
    let mut world = crate::prelude::World::new();
    spawn_collider(
        &mut world,
        &mut physics,
        "wall",
        crate::physics::RigidBodyBuilder::fixed().translation(glam::Vec3::new(0.0, 0.0, -5.0)),
        crate::physics::ColliderBuilder::cuboid(2.0, 2.0, 0.5),
    );
    spawn_collider(
        &mut world,
        &mut physics,
        "coin",
        crate::physics::RigidBodyBuilder::fixed().translation(glam::Vec3::new(0.0, 0.0, -2.0)),
        crate::physics::ColliderBuilder::ball(0.5).sensor(true),
    );
    physics.step(0.0).expect("refresh broad phase");
    let grounded = std::collections::HashMap::new();
    let hit = queries(&world, &physics, &grounded)
        .raycast([0.0; 3], [0.0, 0.0, -1.0], 100.0, None)
        .expect("hit");
    assert_eq!(hit.object_id.as_deref(), Some("wall"));
    assert!((hit.distance - 4.5).abs() < 1e-3);
}

#[test]
fn raycast_from_inside_a_collider_hits_it() {
    let mut physics = crate::physics::PhysicsWorld::default();
    let mut world = crate::prelude::World::new();
    spawn_collider(
        &mut world,
        &mut physics,
        "room",
        crate::physics::RigidBodyBuilder::fixed(),
        crate::physics::ColliderBuilder::cuboid(3.0, 3.0, 3.0),
    );
    physics.step(0.0).expect("refresh broad phase");
    let grounded = std::collections::HashMap::new();
    let hit = queries(&world, &physics, &grounded)
        .raycast([0.0; 3], [1.0, 0.0, 0.0], 10.0, None)
        .expect("hit");
    assert_eq!(hit.object_id.as_deref(), Some("room"));
    assert_eq!(hit.distance, 0.0);
}

#[test]
fn two_moves_before_one_step_add_up() {
    let (mut world, mut physics) = (
        crate::prelude::World::new(),
        crate::physics::PhysicsWorld::default(),
    );
    let body = spawn_collider(
        &mut world,
        &mut physics,
        "hero",
        crate::physics::RigidBodyBuilder::kinematic_position_based(),
        crate::physics::ColliderBuilder::capsule_y(0.5, 0.3),
    );
    physics.step(0.0).expect("refresh broad phase");
    let (mut warned, mut grounded) = (
        std::collections::HashSet::new(),
        std::collections::HashMap::new(),
    );
    for _ in 0..2 {
        bodies::apply_body_command(
            &world,
            &mut physics,
            &mut warned,
            &mut grounded,
            &GameCommand::MoveCharacter {
                object_id: "hero".into(),
                delta: [0.5, 0.0, 0.0],
            },
            1.0 / 60.0,
        );
    }
    physics.step(1.0 / 60.0).expect("step");
    let x = physics
        .rapier
        .bodies
        .get(body)
        .expect("body")
        .translation()
        .x;
    assert!((x - 1.0).abs() < 1e-3, "x = {x}");
}

#[test]
fn moves_without_a_step_between_both_count() {
    let (mut world, mut physics) = (
        crate::prelude::World::new(),
        crate::physics::PhysicsWorld::default(),
    );
    floor(&mut world, &mut physics);
    let body = spawn_collider(
        &mut world,
        &mut physics,
        "hero",
        crate::physics::RigidBodyBuilder::kinematic_position_based()
            .translation(glam::Vec3::new(0.0, 2.0, 0.0)),
        crate::physics::ColliderBuilder::capsule_y(0.5, 0.3),
    );
    physics.step(0.0).expect("refresh broad phase");
    let (mut warned, mut grounded) = (
        std::collections::HashSet::new(),
        std::collections::HashMap::new(),
    );
    for delta in [[1.0, 0.0, 0.0], [0.0, -0.2, 0.0]] {
        bodies::apply_body_command(
            &world,
            &mut physics,
            &mut warned,
            &mut grounded,
            &GameCommand::MoveCharacter {
                object_id: "hero".into(),
                delta,
            },
            1.0 / 60.0,
        );
    }
    physics.step(1.0 / 60.0).expect("step");
    let t = physics.rapier.bodies.get(body).expect("body").translation();
    assert!(
        (t.x - 1.0).abs() < 1e-3 && (t.y - 1.8).abs() < 1e-3,
        "{t:?}"
    );
}

#[test]
fn raycast_ignore_skips_the_casters_own_collider() {
    let mut physics = crate::physics::PhysicsWorld::default();
    let mut world = crate::prelude::World::new();
    spawn_collider(
        &mut world,
        &mut physics,
        "player",
        crate::physics::RigidBodyBuilder::kinematic_position_based(),
        crate::physics::ColliderBuilder::capsule_y(0.5, 0.3),
    );
    spawn_collider(
        &mut world,
        &mut physics,
        "wall",
        crate::physics::RigidBodyBuilder::fixed().translation(glam::Vec3::new(5.0, 0.0, 0.0)),
        crate::physics::ColliderBuilder::cuboid(0.5, 2.0, 2.0),
    );
    physics.step(0.0).expect("refresh broad phase");
    let grounded = std::collections::HashMap::new();
    let q = queries(&world, &physics, &grounded);
    let hit = q
        .raycast([0.0; 3], [1.0, 0.0, 0.0], 10.0, None)
        .expect("hit");
    assert_eq!(hit.object_id.as_deref(), Some("player"));
    assert_eq!(hit.distance, 0.0);
    let hit = q
        .raycast([0.0; 3], [1.0, 0.0, 0.0], 10.0, Some("player"))
        .expect("hit");
    assert_eq!(hit.object_id.as_deref(), Some("wall"));
    assert!((hit.distance - 4.5).abs() < 1e-3);
    let hit = q
        .raycast([0.0; 3], [1.0, 0.0, 0.0], 10.0, Some("nobody"))
        .expect("hit");
    assert_eq!(hit.object_id.as_deref(), Some("player"));
}

// --- game state and pause -------------------------------------------

use crate::scene::{GameState, GameStateName};

fn set_state(state: &str) -> GameCommand {
    GameCommand::SetGameState {
        state: state.into(),
    }
}

#[test]
fn game_state_lists_match() {
    let scene_names: Vec<&str> = GameStateName::ALL.iter().map(|s| s.name()).collect();
    assert_eq!(scene_names, engine_scripting::GAME_STATES);
}

#[test]
fn set_game_state_pauses_the_simulation_this_frame() {
    let mut world = crate::prelude::World::new();
    state::init_state(&mut world, None);
    assert!(!state::simulation_paused(&world));
    let change = state::apply_state_commands(&mut world, &[set_state("paused")]);
    assert_eq!(
        change,
        Some((GameStateName::Playing, GameStateName::Paused))
    );
    assert_eq!(world.resource::<GameState>().current, GameStateName::Paused);
    assert!(state::simulation_paused(&world));
    state::apply_state_commands(&mut world, &[set_state("playing")]);
    assert!(!state::simulation_paused(&world));
}

#[test]
fn setting_the_current_state_again_is_not_a_change() {
    let mut world = crate::prelude::World::new();
    state::init_state(&mut world, None);
    assert_eq!(
        state::apply_state_commands(&mut world, &[set_state("playing")]),
        None
    );
    // Unknown names are ignored; the net change is first old to last new.
    let change = state::apply_state_commands(
        &mut world,
        &[
            set_state("menu"),
            set_state("nonsense"),
            set_state("game_over"),
        ],
    );
    assert_eq!(
        change,
        Some((GameStateName::Playing, GameStateName::GameOver))
    );
}

#[test]
fn state_survives_scene_change_and_new_scene_starts_paused() {
    let mut world = crate::prelude::World::new();
    state::init_state(&mut world, None);
    state::apply_state_commands(&mut world, &[set_state("paused")]);
    world.spawn(SceneObjectId("old".into()));
    clear_world(&mut world, &mut crate::prelude::RenderAssets::new());
    // The next scene says "playing", but a state already exists.
    state::init_state(&mut world, Some(GameStateName::Playing));
    assert_eq!(world.resource::<GameState>().current, GameStateName::Paused);
    assert!(state::simulation_paused(&world));
}

#[test]
fn start_state_is_used_only_when_no_state_exists() {
    let mut world = crate::prelude::World::new();
    let first = state::init_state(&mut world, Some(GameStateName::Menu));
    assert_eq!(first, GameStateName::Menu);
    let again = state::init_state(&mut world, Some(GameStateName::GameOver));
    assert_eq!(again, GameStateName::Menu);
    let mut empty = crate::prelude::World::new();
    assert_eq!(state::init_state(&mut empty, None), GameStateName::Playing);
}

#[test]
fn take_state_change_reports_the_last_change_once() {
    let mut runtime = GameplayRuntime::new(Scene::default(), None);
    let mut world = crate::prelude::World::new();
    state::init_state(&mut world, None);
    assert_eq!(runtime.take_state_change(), None);
    runtime.record_state_changes(&mut world, &[set_state("paused"), set_state("menu")]);
    assert_eq!(
        runtime.take_state_change(),
        Some((GameStateName::Playing, GameStateName::Menu))
    );
    assert_eq!(runtime.take_state_change(), None);
    assert_eq!(runtime.game_state(&world), GameStateName::Menu);
}

#[test]
fn resume_after_pause_runs_no_catch_up_steps() {
    let mut ticker = engine_core::FixedTimestep::from_hz(60.0).expect("ticker");
    let mut steps = 0;
    for _ in 0..120 {
        steps += crate::app::steps_for_frame(true, &mut ticker, 1.0 / 60.0);
    }
    assert_eq!(steps, 0);
    let after = crate::app::steps_for_frame(false, &mut ticker, 1.0 / 60.0);
    assert!(after <= 1, "resume produced {after} steps");
    // A long frame while paused still leaves nothing banked.
    crate::app::steps_for_frame(true, &mut ticker, 2.0);
    assert!(crate::app::steps_for_frame(false, &mut ticker, 1.0 / 60.0) <= 1);
}

#[test]
fn net_unchanged_batch_reports_no_change() {
    let mut world = crate::prelude::World::new();
    state::init_state(&mut world, None);
    let batch = [set_state("paused"), set_state("playing")];
    assert_eq!(state::apply_state_commands(&mut world, &batch), None);
    let mut runtime = GameplayRuntime::new(Scene::default(), None);
    runtime.record_state_changes(&mut world, &batch);
    assert_eq!(runtime.take_state_change(), None);
}

#[test]
fn changes_merge_across_batches_until_taken() {
    let mut world = crate::prelude::World::new();
    state::init_state(&mut world, None);
    let mut runtime = GameplayRuntime::new(Scene::default(), None);
    runtime.record_state_changes(&mut world, &[set_state("paused")]);
    runtime.record_state_changes(&mut world, &[set_state("game_over")]);
    assert_eq!(
        runtime.take_state_change(),
        Some((GameStateName::Playing, GameStateName::GameOver))
    );
    runtime.record_state_changes(&mut world, &[set_state("playing")]);
    runtime.record_state_changes(&mut world, &[set_state("game_over")]);
    assert_eq!(runtime.take_state_change(), None);
    runtime.record_state_changes(&mut world, &[set_state("paused")]);
    runtime.record_state_changes(&mut world, &[set_state("game_over")]);
    assert_eq!(runtime.take_state_change(), None);
}

/// The parts of `replace_scene` that need no window, in the same order.
fn leave_scene(
    world: &mut crate::prelude::World,
    physics: &mut crate::physics::PhysicsWorld,
) -> (
    std::collections::HashSet<crate::prelude::Entity>,
    Vec<engine_scene::SceneEntity>,
) {
    let keep = kept_objects(world);
    let carried = Scene::capture_entities(world, &keep);
    scenes::clear_physics_except(world, physics, &keep);
    scenes::clear_world_except(world, &mut crate::prelude::RenderAssets::new(), &keep);
    (keep, carried)
}

#[test]
fn a_kept_object_survives_with_its_children_and_body() {
    let mut world = crate::prelude::World::new();
    let mut physics = crate::physics::PhysicsWorld::default();
    let kept_body = spawn_body(
        &mut world,
        &mut physics,
        "kept",
        crate::physics::RigidBodyBuilder::dynamic(),
    );
    let gone_body = spawn_body(
        &mut world,
        &mut physics,
        "gone",
        crate::physics::RigidBodyBuilder::dynamic(),
    );
    let kept = find_object(&world, "kept").unwrap();
    world
        .entity_mut(kept)
        .insert((engine_scene::KeepBetweenScenes, Name::new("Kept")));
    let child = world
        .spawn((
            SceneObjectId("child".into()),
            Name::new("Child"),
            engine_ecs::prelude::ChildOf(kept),
        ))
        .id();

    let (keep, carried) = leave_scene(&mut world, &mut physics);

    assert_eq!(keep.len(), 2);
    assert!(world.get_entity(kept).is_ok() && world.get_entity(child).is_ok());
    assert!(find_object(&world, "gone").is_none());
    assert!(physics.rapier.bodies.get(kept_body).is_some());
    assert!(physics.rapier.bodies.get(gone_body).is_none());
    // The child's parent index points into the carried list.
    let child_data = carried
        .iter()
        .find(|entity| entity.object_id.as_deref() == Some("child"))
        .unwrap();
    let parent = child_data.parent.unwrap();
    assert_eq!(carried[parent].object_id.as_deref(), Some("kept"));
}

#[test]
fn only_a_top_level_object_is_kept_and_nothing_kept_resets_physics() {
    let mut world = crate::prelude::World::new();
    let mut physics = crate::physics::PhysicsWorld::default();
    let body = spawn_body(
        &mut world,
        &mut physics,
        "a",
        crate::physics::RigidBodyBuilder::dynamic(),
    );
    let parent = world.spawn(SceneObjectId("parent".into())).id();
    world.spawn((
        SceneObjectId("child".into()),
        engine_scene::KeepBetweenScenes,
        engine_ecs::prelude::ChildOf(parent),
    ));
    let (keep, carried) = leave_scene(&mut world, &mut physics);
    assert!(keep.is_empty() && carried.is_empty());
    assert!(physics.rapier.bodies.get(body).is_none());
    assert!(find_object(&world, "child").is_none());
}

#[test]
fn tags_find_every_object_carrying_them() {
    let mut world = crate::prelude::World::new();
    world.spawn((
        SceneObjectId("a".into()),
        engine_scene::SceneTags(vec!["enemy".into(), "boss".into()]),
    ));
    world.spawn((
        SceneObjectId("b".into()),
        engine_scene::SceneTags(vec!["enemy".into()]),
    ));
    world.spawn(SceneObjectId("c".into()));
    let mut enemies = find_objects_by_tag(&world, "enemy");
    enemies.sort();
    assert_eq!(enemies, ["a", "b"]);
    assert!(find_objects_by_tag(&world, "nothing").is_empty());
}

#[test]
fn the_next_scene_follows_the_order_and_skips_the_loading_screen() {
    let order: Vec<String> = ["scenes/a.ron", "scenes/loading.ron", "scenes/b.ron"]
        .map(str::to_owned)
        .to_vec();
    assert_eq!(
        scenes::scene_after(&order, "scenes/a.ron").as_deref(),
        Some("scenes/b.ron")
    );
    assert_eq!(scenes::scene_after(&order, "scenes/b.ron"), None);
    assert_eq!(scenes::scene_after(&order, "scenes/other.ron"), None);
}

#[test]
fn an_exported_game_reads_its_scene_order_back() {
    let root = std::env::temp_dir().join(format!("vge-order-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let source = SceneSource::Export(root.clone());
    assert!(
        source.scene_order().order.is_empty(),
        "a missing file is no order"
    );
    let saved = engine_project::build::ExportedSceneOrder {
        start: "scenes/a.ron".into(),
        order: vec!["scenes/a.ron".into(), "scenes/b.ron".into()],
    };
    std::fs::write(
        root.join(engine_project::build::SCENE_ORDER_FILE),
        ron::to_string(&saved).unwrap(),
    )
    .unwrap();
    assert_eq!(source.scene_order(), saved);
    let _ = std::fs::remove_dir_all(root);
}
