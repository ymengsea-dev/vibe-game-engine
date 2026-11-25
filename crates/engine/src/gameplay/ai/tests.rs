//! AI runtime tests: walking, senses and both brains, on a bare world.

use engine_ecs::components::Transform;
use engine_ecs::prelude::{Entity, World};
use engine_physics::{ColliderBuilder, PhysicsWorld, RigidBodyBuilder};
use engine_scene::{
    AiBrainData, AiData, AiState, BehaviourTreeData, BtNodeData, BtNodeKind, NavGridData, SceneAi,
    SceneNavGrid, SceneObjectId, SceneTags, SensesData, SimpleBrainData,
};
use engine_scripting::GameCommand;
use glam::{Quat, Vec3};

use super::*;

const DT: f32 = 1.0 / 30.0;

fn spawn(world: &mut World, id: &str, at: Vec3) -> Entity {
    world
        .spawn((
            SceneObjectId(id.into()),
            Transform::from(engine_utils::Transform::from_translation(at)),
        ))
        .id()
}

fn guard(world: &mut World, at: Vec3, ai: AiData) -> Entity {
    let entity = spawn(world, "guard", at);
    world.entity_mut(entity).insert(SceneAi(ai));
    entity
}

fn player(world: &mut World, at: Vec3) -> Entity {
    let entity = spawn(world, "player", at);
    world
        .entity_mut(entity)
        .insert(SceneTags(vec!["player".into()]));
    entity
}

fn place(world: &mut World, entity: Entity, at: Vec3) {
    if let Some(mut transform) = world.get_mut::<Transform>(entity) {
        transform.0.translation = at;
    }
}

fn at(world: &World, entity: Entity) -> Vec3 {
    super::position(world, entity).expect("position")
}

/// Runs one AI frame and applies its moves and turns; returns everything it
/// produced.
fn step(world: &mut World, physics: &PhysicsWorld, noises: &[Noise]) -> AiTick {
    let mut rng = engine_utils::Rng::new(1);
    let tick = tick(world, physics, noises, DT, &mut rng);
    let moves: Vec<GameCommand> = tick
        .commands
        .iter()
        .filter(|command| {
            matches!(
                command,
                GameCommand::SetPosition { .. } | GameCommand::SetRotation { .. }
            )
        })
        .cloned()
        .collect();
    crate::gameplay::apply_world_commands(world, &moves, &mut |_, path, _, _| {
        Err(crate::gameplay::GameplayError::Prefab(
            path.into(),
            "none".into(),
        ))
    })
    .expect("moves apply");
    tick
}

/// A 10x10 grid of 1-unit cells from the origin, with a wall along x = 5
/// from z = 0 to z = 8.
fn walled_grid(world: &mut World) {
    let mut grid = NavGridData::new(10, 10, 1.0, [0.0, 0.0]);
    for z in 0..8 {
        grid.blocked[z * 10 + 5] = true;
    }
    world.insert_resource(SceneNavGrid(grid));
}

fn walk_to(world: &mut World, goal: Vec3) {
    nav::apply_route_commands(
        world,
        &mut std::collections::HashSet::new(),
        &[GameCommand::MoveTo {
            object_id: "guard".into(),
            target: goal.to_array(),
            speed: None,
        }],
    );
}

#[test]
fn move_to_walks_around_a_wall_and_reports_arrival_once() {
    let mut world = World::new();
    let physics = PhysicsWorld::default();
    walled_grid(&mut world);
    let entity = guard(&mut world, Vec3::new(1.5, 0.0, 1.5), AiData::default());
    let goal = Vec3::new(8.5, 0.0, 1.5);
    walk_to(&mut world, goal);

    let mut arrivals = 0;
    for _ in 0..600 {
        let tick = step(&mut world, &physics, &[]);
        arrivals += tick
            .events
            .iter()
            .filter(|event| matches!(event, AiEvent::Arrived { .. }))
            .count();
        let here = at(&world, entity);
        assert!(
            !(here.x > 5.05 && here.x < 5.95 && here.z < 7.95),
            "walked through the wall at {here}"
        );
        if arrivals > 0 {
            break;
        }
    }
    assert_eq!(arrivals, 1);
    let end = at(&world, entity);
    assert!(end.distance(goal) <= 0.25, "ended at {end}");
    assert!(!nav::is_walking(&world, entity));
    let turned = super::rotation(&world, entity);
    assert!(
        turned.angle_between(Quat::IDENTITY) > 0.1,
        "faced where it walked"
    );
}

#[test]
fn without_a_grid_move_to_goes_straight_and_stop_moving_stops() {
    let mut world = World::new();
    let physics = PhysicsWorld::default();
    let entity = guard(&mut world, Vec3::ZERO, AiData::default());
    walk_to(&mut world, Vec3::new(0.0, 0.0, -9.0));
    step(&mut world, &physics, &[]);
    let after_one = at(&world, entity);
    assert!(after_one.z < 0.0 && after_one.x.abs() < 1e-4);
    nav::apply_route_commands(
        &mut world,
        &mut std::collections::HashSet::new(),
        &[GameCommand::StopMoving {
            object_id: "guard".into(),
        }],
    );
    step(&mut world, &physics, &[]);
    assert_eq!(at(&world, entity), after_one);
}

#[test]
fn walking_on_a_baked_hill_follows_its_heights() {
    let mut world = World::new();
    let physics = PhysicsWorld::default();
    let mut grid = NavGridData::new(4, 1, 1.0, [0.0, 0.0]);
    grid.heights = vec![0.0, 1.0, 2.0, 3.0];
    world.insert_resource(SceneNavGrid(grid));
    let entity = guard(&mut world, Vec3::new(0.5, 0.0, 0.5), AiData::default());
    walk_to(&mut world, Vec3::new(3.5, 3.0, 0.5));
    for _ in 0..60 {
        step(&mut world, &physics, &[]);
    }
    assert!((at(&world, entity).y - 3.0).abs() < 1e-4);
}

#[test]
fn a_kinematic_walker_moves_through_its_character_controller() {
    let mut world = World::new();
    let mut physics = PhysicsWorld::default();
    let entity = guard(&mut world, Vec3::new(0.0, 1.0, 0.0), AiData::default());
    let body = physics
        .rapier
        .bodies
        .insert(RigidBodyBuilder::kinematic_position_based().translation(Vec3::new(0.0, 1.0, 0.0)));
    world
        .entity_mut(entity)
        .insert(engine_ecs::components::RigidBody(body));
    walk_to(&mut world, Vec3::new(5.0, 1.0, 0.0));
    let tick = step(&mut world, &physics, &[]);
    let delta = tick
        .commands
        .iter()
        .find_map(|command| match command {
            GameCommand::MoveCharacter { delta, .. } => Some(*delta),
            _ => None,
        })
        .expect("moves the character");
    assert!(delta[0] > 0.0 && delta[1] < 0.0);
    assert!(
        !tick
            .commands
            .iter()
            .any(|command| matches!(command, GameCommand::SetPosition { .. }))
    );
}

fn watcher() -> AiData {
    AiData {
        senses: Some(SensesData {
            sight_range: 10.0,
            sight_angle: 90.0,
            hearing_radius: 2.0,
            forget_after: 1.0,
            ..SensesData::default()
        }),
        ..AiData::default()
    }
}

fn memory(world: &World, entity: Entity) -> AiMemory {
    world.get::<AiMemory>(entity).cloned().unwrap_or_default()
}

#[test]
fn senses_see_ahead_hear_close_and_forget_after_a_while() {
    let mut world = World::new();
    let physics = PhysicsWorld::default();
    let entity = guard(&mut world, Vec3::ZERO, watcher());
    // Facing -Z: a target at +Z is behind it.
    let target = player(&mut world, Vec3::new(0.0, 0.0, 5.0));
    let tick = step(&mut world, &physics, &[]);
    assert!(
        tick.events.is_empty(),
        "nothing behind it: {:?}",
        tick.events
    );

    place(&mut world, target, Vec3::new(0.0, 0.0, -5.0));
    let tick = step(&mut world, &physics, &[]);
    assert_eq!(
        tick.events,
        [AiEvent::TargetSeen {
            object_id: "guard".into(),
            target_id: "player".into(),
        }]
    );
    assert!(memory(&world, entity).sees);
    assert!(
        step(&mut world, &physics, &[]).events.is_empty(),
        "seen once"
    );

    place(&mut world, target, Vec3::new(0.0, 0.0, 1.5));
    step(&mut world, &physics, &[]);
    let heard = memory(&world, entity);
    assert!(heard.hears && !heard.sees, "heard behind it");

    place(&mut world, target, Vec3::new(0.0, 0.0, 50.0));
    let mut lost = Vec::new();
    for _ in 0..40 {
        lost.extend(step(&mut world, &physics, &[]).events);
    }
    assert_eq!(
        lost,
        [AiEvent::TargetLost {
            object_id: "guard".into(),
            target_id: "player".into(),
        }]
    );
}

#[test]
fn a_wall_blocks_sight_but_not_hearing() {
    let mut world = World::new();
    let mut physics = PhysicsWorld::default();
    let entity = guard(&mut world, Vec3::ZERO, watcher());
    player(&mut world, Vec3::new(0.0, 1.6, -5.0));
    physics
        .rapier
        .colliders
        .insert(ColliderBuilder::cuboid(3.0, 3.0, 0.2).translation(Vec3::new(0.0, 1.0, -2.5)));
    physics.step(0.0).expect("refresh broad phase");
    let tick = step(&mut world, &physics, &[]);
    assert!(tick.events.is_empty());
    assert!(!memory(&world, entity).sees);

    let mut world = World::new();
    let entity = guard(&mut world, Vec3::ZERO, watcher());
    player(&mut world, Vec3::new(0.0, 0.0, -1.5));
    let mut physics = PhysicsWorld::default();
    physics
        .rapier
        .colliders
        .insert(ColliderBuilder::cuboid(3.0, 3.0, 0.1).translation(Vec3::new(0.0, 1.0, -0.8)));
    physics.step(0.0).expect("refresh broad phase");
    step(&mut world, &physics, &[]);
    let close = memory(&world, entity);
    assert!(close.hears && !close.sees);
}

#[test]
fn noises_are_heard_within_their_radius() {
    let mut world = World::new();
    let physics = PhysicsWorld::default();
    guard(&mut world, Vec3::ZERO, watcher());
    let far = Noise {
        position: Vec3::new(20.0, 0.0, 0.0),
        radius: 5.0,
    };
    assert!(step(&mut world, &physics, &[far]).events.is_empty());
    let loud = Noise {
        position: Vec3::new(8.0, 0.0, 0.0),
        radius: 10.0,
    };
    assert_eq!(
        step(&mut world, &physics, &[loud]).events,
        [AiEvent::NoiseHeard {
            object_id: "guard".into(),
            position: [8.0, 0.0, 0.0],
        }]
    );
}

fn brain(brain: SimpleBrainData) -> AiData {
    AiData {
        brain: Some(AiBrainData::Simple(brain)),
        ..watcher()
    }
}

fn states(events: &[AiEvent]) -> Vec<&'static str> {
    events
        .iter()
        .filter_map(|event| match event {
            AiEvent::StateChanged { state, .. } => Some(*state),
            _ => None,
        })
        .collect()
}

#[test]
fn a_simple_brain_patrols_chases_attacks_and_goes_back() {
    let mut world = World::new();
    let physics = PhysicsWorld::default();
    let entity = guard(
        &mut world,
        Vec3::ZERO,
        brain(SimpleBrainData {
            start: AiState::Patrol,
            patrol_points: vec![[0.0, 0.0, -3.0], [0.0, 0.0, 0.0]],
            patrol_wait: 0.0,
            attack_range: 1.5,
            attack_cooldown: 0.5,
            clips: engine_scene::AiClips {
                walk: "Walk".into(),
                run: "Run".into(),
                ..Default::default()
            },
            ..SimpleBrainData::default()
        }),
    );
    let target = player(&mut world, Vec3::new(40.0, 0.0, 0.0));

    let first = step(&mut world, &physics, &[]);
    assert_eq!(states(&first.events), ["patrol"]);
    let mut walked = first.commands.clone();
    for _ in 0..90 {
        walked.extend(step(&mut world, &physics, &[]).commands);
    }
    assert!(walked.iter().any(|command| matches!(
        command,
        GameCommand::PlayAnimation { clip, .. } if clip == "Walk"
    )));
    assert!(nav::is_walking(&world, entity) || memory(&world, entity).walking_patrol);

    // In view: chase it.
    let facing = super::facing(&world, entity);
    let ahead = at(&world, entity) + facing * 6.0;
    place(&mut world, target, ahead);
    let chase = step(&mut world, &physics, &[]);
    assert_eq!(states(&chase.events), ["chase"], "{:?}", chase.events);
    assert!(chase.commands.iter().any(|command| matches!(
        command,
        GameCommand::PlayAnimation { clip, .. } if clip == "Run"
    )));

    // Close: attack, then again after the cooldown only.
    let close = at(&world, entity) + facing * 1.0;
    place(&mut world, target, close);
    let mut attacks = 0;
    let mut events = Vec::new();
    for _ in 0..30 {
        let tick = step(&mut world, &physics, &[]);
        attacks += tick
            .commands
            .iter()
            .filter(
                |command| matches!(command, GameCommand::EmitEvent { name } if name == "attack"),
            )
            .count();
        events.extend(tick.events);
    }
    assert_eq!(states(&events), ["attack"]);
    assert_eq!(attacks, 2, "one at once, one after 0.5 s of the 1 s");

    // Gone: lost after a second, back to patrolling.
    place(&mut world, target, Vec3::new(500.0, 0.0, 0.0));
    let mut events = Vec::new();
    for _ in 0..60 {
        events.extend(step(&mut world, &physics, &[]).events);
    }
    assert_eq!(states(&events), ["chase", "patrol"]);
}

#[test]
fn a_simple_brain_walks_to_a_noise_and_scripts_can_force_a_state() {
    let mut world = World::new();
    let physics = PhysicsWorld::default();
    let entity = guard(&mut world, Vec3::ZERO, brain(SimpleBrainData::default()));
    step(&mut world, &physics, &[]);
    let noise = Noise {
        position: Vec3::new(3.0, 0.0, 0.0),
        radius: 10.0,
    };
    step(&mut world, &physics, &[noise]);
    step(&mut world, &physics, &[]);
    assert!(nav::is_walking(&world, entity), "went to look");

    apply_state_commands(
        &mut world,
        &[GameCommand::SetAiState {
            object_id: "guard".into(),
            state: "patrol".into(),
        }],
    );
    let tick = step(&mut world, &physics, &[]);
    assert_eq!(states(&tick.events), ["patrol"]);
    assert_eq!(super::ai_state(&world, entity), Some("patrol"));
}

fn tree_guard(world: &mut World, tree: BehaviourTreeData) -> Entity {
    guard(
        world,
        Vec3::ZERO,
        AiData {
            brain: Some(AiBrainData::Tree(tree)),
            ..watcher()
        },
    )
}

#[test]
fn the_starter_tree_waits_until_it_has_a_target_then_chases() {
    let mut world = World::new();
    let physics = PhysicsWorld::default();
    let entity = tree_guard(&mut world, BehaviourTreeData::default());
    let target = player(&mut world, Vec3::new(0.0, 0.0, 30.0));
    for _ in 0..5 {
        step(&mut world, &physics, &[]);
    }
    assert!(!nav::is_walking(&world, entity));

    place(&mut world, target, Vec3::new(0.0, 0.0, -8.0));
    for _ in 0..10 {
        step(&mut world, &physics, &[]);
    }
    assert!(
        at(&world, entity).z < -1.0,
        "chased: {}",
        at(&world, entity)
    );
}

#[test]
fn a_tree_sequence_walks_then_waits_before_walking_on() {
    let node = |kind, children: Vec<usize>| BtNodeData {
        kind,
        children,
        graph_position: None,
    };
    let tree = BehaviourTreeData {
        nodes: vec![
            node(BtNodeKind::Sequence, vec![1, 2]),
            node(
                BtNodeKind::Patrol {
                    points: vec![[0.0, 0.0, -1.0], [0.0, 0.0, 0.0]],
                },
                vec![],
            ),
            node(BtNodeKind::Wait { seconds: 0.5 }, vec![]),
        ],
    };
    let mut world = World::new();
    let physics = PhysicsWorld::default();
    let entity = tree_guard(&mut world, tree);
    let mut reached_first = None;
    for frame in 0..60 {
        step(&mut world, &physics, &[]);
        if reached_first.is_none() && (at(&world, entity).z + 1.0).abs() <= 0.25 {
            reached_first = Some(frame);
        }
    }
    let reached = reached_first.expect("reached the first point");
    // Then it waits about 0.5 s (15 frames) there before walking back.
    let z_after_wait = {
        let mut world = World::new();
        let entity = tree_guard(
            &mut world,
            BehaviourTreeData {
                nodes: vec![
                    node(BtNodeKind::Sequence, vec![1, 2]),
                    node(
                        BtNodeKind::Patrol {
                            points: vec![[0.0, 0.0, -1.0], [0.0, 0.0, 0.0]],
                        },
                        vec![],
                    ),
                    node(BtNodeKind::Wait { seconds: 0.5 }, vec![]),
                ],
            },
        );
        for _ in 0..(reached + 10) {
            step(&mut world, &physics, &[]);
        }
        at(&world, entity).z
    };
    assert!(
        (z_after_wait + 1.0).abs() <= 0.25,
        "still waiting at the first point 10 frames later: {z_after_wait}"
    );
}

#[test]
fn ai_state_names_match_the_scripting_list() {
    let names: Vec<&str> = AiState::ALL.iter().map(|state| state.name()).collect();
    assert_eq!(names, engine_scripting::AI_STATES);
}

#[test]
fn a_tree_reports_which_nodes_ran_and_what_they_answered() {
    use engine_scene::BtNodeRun;
    let node = |kind, children: Vec<usize>| BtNodeData {
        kind,
        children,
        graph_position: None,
    };
    // Selector: "has target?" fails, so it goes on to waiting, which runs.
    let tree = BehaviourTreeData {
        nodes: vec![
            node(BtNodeKind::Selector, vec![1, 2]),
            node(BtNodeKind::HasTarget, vec![]),
            node(BtNodeKind::Wait { seconds: 5.0 }, vec![]),
        ],
    };
    let mut world = World::new();
    let physics = PhysicsWorld::default();
    let entity = tree_guard(&mut world, tree);
    assert!(super::behaviour_tree_trace(&world, entity).is_empty());
    step(&mut world, &physics, &[]);
    assert_eq!(
        super::behaviour_tree_trace(&world, entity),
        [
            (0, BtNodeRun::Running),
            (1, BtNodeRun::Failure),
            (2, BtNodeRun::Running),
        ]
    );
    // Each frame replaces the last one's trace.
    step(&mut world, &physics, &[]);
    assert_eq!(super::behaviour_tree_trace(&world, entity).len(), 3);
}

/// Two walkers sent at each other along the same line; returns the closest
/// they came, and where each ended.
fn head_on(avoid_others: bool) -> (f32, Vec3, Vec3) {
    let mut world = World::new();
    let physics = PhysicsWorld::default();
    let walker = |avoid_others| AiData {
        agent: engine_scene::NavAgentData {
            avoid_others,
            ..engine_scene::NavAgentData::default()
        },
        ..AiData::default()
    };
    let a = spawn(&mut world, "a", Vec3::new(-6.0, 0.0, 0.0));
    world.entity_mut(a).insert(SceneAi(walker(avoid_others)));
    let b = spawn(&mut world, "b", Vec3::new(6.0, 0.0, 0.0));
    world.entity_mut(b).insert(SceneAi(walker(avoid_others)));
    nav::apply_route_commands(
        &mut world,
        &mut std::collections::HashSet::new(),
        &[
            GameCommand::MoveTo {
                object_id: "a".into(),
                target: [6.0, 0.0, 0.0],
                speed: None,
            },
            GameCommand::MoveTo {
                object_id: "b".into(),
                target: [-6.0, 0.0, 0.0],
                speed: None,
            },
        ],
    );
    let mut closest = f32::INFINITY;
    for _ in 0..240 {
        step(&mut world, &physics, &[]);
        closest = closest.min(at(&world, a).distance(at(&world, b)));
    }
    (closest, at(&world, a), at(&world, b))
}

#[test]
fn walkers_steer_round_each_other_and_still_arrive() {
    let (closest, a, b) = head_on(true);
    assert!(closest >= 0.7, "came within {closest}");
    assert!(a.distance(Vec3::new(6.0, 0.0, 0.0)) < 0.5, "a arrived: {a}");
    assert!(
        b.distance(Vec3::new(-6.0, 0.0, 0.0)) < 0.5,
        "b arrived: {b}"
    );

    // With avoidance off they keep to their line and walk through each other.
    let (closest, _, _) = head_on(false);
    assert!(closest < 0.2, "kept to the path: {closest}");
}
