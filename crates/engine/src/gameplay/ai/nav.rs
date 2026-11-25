//! Walking: routes over the scene's nav grid, and each frame's step along
//! them.

use std::collections::HashSet;

use engine_ai::{NavGrid, find_route, follow_path};
use engine_ecs::components::RigidBody;
use engine_ecs::prelude::{Entity, World};
use engine_physics::PhysicsWorld;
use engine_scene::{NavAgentData, SceneAi, SceneNavGrid};
use engine_scripting::GameCommand;
use glam::{Vec2, Vec3};

use super::{AiEvent, AiTick};

/// The scene's nav grid, ready for pathfinding. Built from [`SceneNavGrid`]
/// the first time AI runs in a scene; scene changes remove it.
#[derive(bevy_ecs::resource::Resource, Debug, Clone)]
pub(in crate::gameplay) struct ActiveNavGrid(pub NavGrid);

/// Where an object is walking.
#[derive(bevy_ecs::component::Component, Debug, Clone, PartialEq)]
pub(super) struct NavRoute {
    /// Where it is going.
    pub goal: Vec3,
    /// The corners still ahead, ending at the goal, on the ground plane.
    pub waypoints: Vec<Vec2>,
    /// Its speed for this route, or the agent's own.
    pub speed: Option<f32>,
}

/// How fast a kinematic walker is pulled down each second, so it follows
/// slopes and steps off ledges (the character controller snaps it to the
/// ground below).
const WALK_GRAVITY: f32 = 4.0;

/// Builds [`ActiveNavGrid`] from the scene's grid when it is missing.
pub(super) fn refresh_active_grid(world: &mut World) {
    if world.contains_resource::<ActiveNavGrid>() {
        return;
    }
    let Some(grid) = world
        .get_resource::<SceneNavGrid>()
        .and_then(|data| nav_grid(&data.0))
    else {
        return;
    };
    world.insert_resource(ActiveNavGrid(grid));
}

/// The pathfinder's grid for scene data, or `None` if the data is malformed.
pub(in crate::gameplay) fn nav_grid(data: &engine_scene::NavGridData) -> Option<NavGrid> {
    let grid = NavGrid::from_cells(
        data.width,
        data.height,
        data.cell_size,
        Vec2::from_array(data.origin),
        &data.blocked,
    )
    .and_then(|grid| grid.with_heights(data.heights.clone()));
    match grid {
        Ok(grid) => Some(grid),
        Err(error) => {
            tracing::warn!(%error, "the scene's nav grid cannot be used");
            None
        }
    }
}

/// A route from `from` to `to` in world space, as corner positions ending at
/// the goal (or the closest reachable cell). Without a grid the route is
/// straight there; with one but no way through it is `None`.
pub(in crate::gameplay) fn find_world_route(
    grid: Option<&NavGrid>,
    from: Vec3,
    to: Vec3,
) -> Option<Vec<Vec3>> {
    let Some(grid) = grid else {
        return Some(vec![to]);
    };
    let route = find_route(grid, flat(from), flat(to))?;
    let last = route.len().saturating_sub(1);
    Some(
        route
            .iter()
            .enumerate()
            .map(|(index, point)| {
                let y = if index == last && *point == flat(to) {
                    to.y
                } else {
                    grid.height_at(*point)
                };
                Vec3::new(point.x, y, point.y)
            })
            .collect(),
    )
}

/// Starts `entity` walking to `goal`, replacing any route it had. Returns
/// whether a route was found.
pub(super) fn start_route(
    world: &mut World,
    entity: Entity,
    goal: Vec3,
    speed: Option<f32>,
) -> bool {
    let Some(from) = super::position(world, entity) else {
        return false;
    };
    let grid = world.get_resource::<ActiveNavGrid>().map(|grid| &grid.0);
    match find_world_route(grid, from, goal) {
        Some(route) => {
            let waypoints = route.iter().map(|point| flat(*point)).collect();
            world.entity_mut(entity).insert(NavRoute {
                goal,
                waypoints,
                speed,
            });
            true
        }
        None => {
            world.entity_mut(entity).remove::<NavRoute>();
            false
        }
    }
}

/// Stops `entity` walking.
pub(super) fn stop(world: &mut World, entity: Entity) {
    world.entity_mut(entity).remove::<NavRoute>();
}

/// Whether `entity` is walking somewhere.
pub(super) fn is_walking(world: &World, entity: Entity) -> bool {
    world.get::<NavRoute>(entity).is_some()
}

/// Where `entity` is walking to, if anywhere.
pub(super) fn goal(world: &World, entity: Entity) -> Option<Vec3> {
    world.get::<NavRoute>(entity).map(|route| route.goal)
}

/// Applies `MoveTo` and `StopMoving` commands.
pub(in crate::gameplay) fn apply_route_commands(
    world: &mut World,
    warned: &mut HashSet<(String, &'static str)>,
    commands: &[GameCommand],
) {
    refresh_active_grid(world);
    for command in commands {
        match command {
            GameCommand::MoveTo {
                object_id,
                target,
                speed,
            } => {
                let target = Vec3::from_array(*target);
                if !target.is_finite() || speed.is_some_and(|s| !s.is_finite() || s < 0.0) {
                    warn_once(warned, object_id, "value is not usable");
                    continue;
                }
                let Some(entity) = super::super::find_object(world, object_id) else {
                    warn_once(warned, object_id, "object was not found");
                    continue;
                };
                if !start_route(world, entity, target, *speed) {
                    warn_once(warned, object_id, "no walkable way there");
                }
            }
            GameCommand::StopMoving { object_id } => {
                if let Some(entity) = super::super::find_object(world, object_id) {
                    stop(world, entity);
                }
            }
            _ => {}
        }
    }
}

/// The velocity an AI object moved with last frame, on the ground plane,
/// for its neighbours' avoidance.
#[derive(bevy_ecs::component::Component, Debug, Clone, Copy, PartialEq, Default)]
pub(super) struct WalkVelocity(pub Vec2);

/// One walker's plan for this frame, before avoidance.
struct Step {
    entity: Entity,
    object_id: String,
    position: Vec3,
    agent: NavAgentData,
    route: NavRoute,
    /// Where its path takes it this frame, and how many corners it passes.
    path_next: Vec2,
    path_reached: usize,
}

/// Takes this frame's step for every walking object, steering round other
/// AI objects (see [`engine_ai::avoid`]).
pub(super) fn walk(world: &mut World, physics: &PhysicsWorld, dt: f32, out: &mut AiTick) {
    let walkers: Vec<(Entity, NavRoute)> = world
        .query_filtered::<(Entity, &NavRoute), engine_ecs::prelude::Without<engine_ecs::components::Disabled>>()
        .iter(world)
        .map(|(entity, route)| (entity, route.clone()))
        .collect();
    // A grid baked on flat ground stores no heights: objects keep their own.
    let has_heights = world.contains_resource::<ActiveNavGrid>()
        && world
            .get_resource::<SceneNavGrid>()
            .is_some_and(|grid| !grid.0.heights.is_empty());
    let mut steps = Vec::with_capacity(walkers.len());
    for (entity, route) in walkers {
        let (Some(object_id), Some(position)) = (
            super::object_id(world, entity),
            super::position(world, entity),
        ) else {
            continue;
        };
        let agent = world
            .get::<SceneAi>(entity)
            .map_or_else(NavAgentData::default, |ai| ai.0.agent);
        let here = flat(position);
        if here.distance(flat(route.goal)) <= agent.stopping_distance.max(1e-3)
            || route.waypoints.is_empty()
        {
            world.entity_mut(entity).remove::<NavRoute>();
            world.entity_mut(entity).insert(WalkVelocity::default());
            out.events.push(AiEvent::Arrived { object_id });
            continue;
        }
        let speed = route.speed.unwrap_or(agent.speed);
        let (path_next, path_reached) = follow_path(here, &route.waypoints, speed, dt);
        steps.push(Step {
            entity,
            object_id,
            position,
            agent,
            route,
            path_next,
            path_reached,
        });
    }
    let velocities = steered_velocities(world, &steps, dt);

    for (step, velocity) in steps.into_iter().zip(velocities) {
        let Step {
            entity,
            object_id,
            position,
            agent,
            mut route,
            path_next,
            path_reached,
        } = step;
        let here = flat(position);
        let speed = route.speed.unwrap_or(agent.speed);
        let next = match velocity {
            Some(velocity) => here + velocity * dt,
            None => path_next,
        };
        if velocity.is_none() {
            route
                .waypoints
                .drain(..path_reached.min(route.waypoints.len()));
        } else {
            // Steered off the path: a corner counts as passed once it is
            // within reach.
            let reach = agent.radius.max(speed * dt).max(0.05);
            while route.waypoints.len() > 1
                && route
                    .waypoints
                    .first()
                    .is_some_and(|corner| corner.distance(next) <= reach)
            {
                route.waypoints.remove(0);
            }
        }
        let step = next - here;
        let moved = if dt > 0.0 { step / dt } else { Vec2::ZERO };
        let body = body_kind(world, physics, entity);
        let ground = if has_heights {
            world
                .get_resource::<ActiveNavGrid>()
                .map(|grid| grid.0.height_at(next))
        } else {
            None
        };
        match body {
            Some(Body::Kinematic) => out.commands.push(GameCommand::MoveCharacter {
                object_id: object_id.clone(),
                delta: [step.x, -WALK_GRAVITY * dt, step.y],
            }),
            Some(Body::Dynamic(velocity_y)) => {
                out.commands.push(GameCommand::SetVelocity {
                    object_id: object_id.clone(),
                    velocity: [moved.x, velocity_y, moved.y],
                });
            }
            None => out.commands.push(GameCommand::SetPosition {
                object_id: object_id.clone(),
                position: [next.x, ground.unwrap_or(position.y), next.y],
            }),
        }
        if agent.turn_speed > 0.0
            && let Some(turned) = super::turn_toward(
                super::rotation(world, entity),
                Vec3::new(step.x, 0.0, step.y),
                agent.turn_speed * dt,
            )
        {
            out.commands.push(GameCommand::SetRotation {
                object_id,
                rotation: turned.to_array(),
            });
        }
        world
            .entity_mut(entity)
            .insert((route, WalkVelocity(moved)));
    }
}

/// The steered velocity of each walker in `steps`, or `None` where it just
/// follows its path: avoidance is off for it, or nobody is near. Other AI
/// objects standing still are obstacles. A steered step that would leave
/// walkable ground falls back to the path.
fn steered_velocities(world: &mut World, steps: &[Step], dt: f32) -> Vec<Option<Vec2>> {
    if dt <= 0.0 || !steps.iter().any(|step| step.agent.avoid_others) {
        return vec![None; steps.len()];
    }
    let walking: HashSet<Entity> = steps.iter().map(|step| step.entity).collect();
    let mut agents: Vec<engine_ai::AvoidAgent> = steps
        .iter()
        .map(|step| {
            let here = flat(step.position);
            let speed = step.route.speed.unwrap_or(step.agent.speed);
            engine_ai::AvoidAgent {
                position: here,
                velocity: world
                    .get::<WalkVelocity>(step.entity)
                    .map_or(Vec2::ZERO, |velocity| velocity.0),
                preferred: (step.path_next - here) / dt,
                radius: step.agent.radius,
                max_speed: speed,
                steers: step.agent.avoid_others,
            }
        })
        .collect();
    let standing: Vec<(Entity, f32)> = world
        .query_filtered::<(Entity, &SceneAi), engine_ecs::prelude::Without<engine_ecs::components::Disabled>>()
        .iter(world)
        .filter(|(entity, _)| !walking.contains(entity))
        .map(|(entity, ai)| (entity, ai.0.agent.radius))
        .collect();
    for (entity, radius) in standing {
        if let Some(position) = super::position(world, entity) {
            agents.push(engine_ai::AvoidAgent {
                position: flat(position),
                velocity: Vec2::ZERO,
                preferred: Vec2::ZERO,
                radius,
                max_speed: 0.0,
                steers: false,
            });
        }
    }
    let chosen = engine_ai::avoid(&agents);
    let grid = world.get_resource::<ActiveNavGrid>().map(|grid| &grid.0);
    steps
        .iter()
        .zip(&agents)
        .zip(chosen)
        .map(|((step, agent), velocity)| {
            if !step.agent.avoid_others || velocity == agent.preferred {
                return None;
            }
            let next = agent.position + velocity * dt;
            let walkable = grid.is_none_or(|grid| grid.is_walkable(grid.world_to_coord(next)));
            walkable.then_some(velocity)
        })
        .collect()
}

/// How an object's body moves, when it has one.
enum Body {
    Kinematic,
    /// With its current vertical speed.
    Dynamic(f32),
}

fn body_kind(world: &World, physics: &PhysicsWorld, entity: Entity) -> Option<Body> {
    let handle = world.get::<RigidBody>(entity)?.0;
    let body = physics.rapier.bodies.get(handle)?;
    if body.is_kinematic() {
        Some(Body::Kinematic)
    } else if body.is_dynamic() {
        Some(Body::Dynamic(body.linvel().y))
    } else {
        None
    }
}

/// A world position on the ground plane, `(x, z)`.
pub(super) fn flat(point: Vec3) -> Vec2 {
    Vec2::new(point.x, point.z)
}

fn warn_once(warned: &mut HashSet<(String, &'static str)>, object_id: &str, reason: &str) {
    if warned.insert((object_id.to_owned(), "move_to")) {
        tracing::warn!(%object_id, reason, "move_to skipped");
    }
}
