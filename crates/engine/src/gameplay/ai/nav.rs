//! Walking: routes over the scene's nav grid, and each frame's step along
//! them.
//!
//! The grid lies on the ground (X and Z, heights along Y) or, in a 2D
//! scene, on the screen (X and Y, characters keep their depth). Routes are
//! worked out in "grid space": `(x, height, z)` along the grid.

use std::collections::HashSet;

use engine_ai::{Clearance, NavGrid, Solid, Span, find_layered_route, follow_path};
use engine_ecs::components::{Disabled, GlobalTransform, RigidBody, Transform};
use engine_ecs::prelude::{Entity, Without, World};
use engine_physics::PhysicsWorld;
use engine_scene::{
    BodyKind, NavAgentData, NavPlane, SCREEN_TO_GRID, SceneAi, SceneCollider, SceneNavGrid,
    collider_solid,
};
use engine_scripting::GameCommand;
use glam::{Mat4, Vec2, Vec3};

use super::{AiEvent, AiTick};

/// The scene's nav grid, ready for pathfinding. Built from [`SceneNavGrid`]
/// the first time AI runs in a scene; scene changes remove it.
#[derive(bevy_ecs::resource::Resource, Debug, Clone)]
pub(in crate::gameplay) struct ActiveNavGrid {
    /// The pathfinder's grid.
    pub grid: NavGrid,
    /// The plane it lies on.
    pub plane: NavPlane,
    /// The space a character needs, for moving bodies that block.
    pub clearance: Clearance,
}

/// How far in front of and behind the screen a moving body blocks a 2D
/// grid.
const SCREEN_DEPTH: f32 = 10_000.0;

/// Where an object is walking.
#[derive(bevy_ecs::component::Component, Debug, Clone, PartialEq)]
pub(super) struct NavRoute {
    /// Where it is going.
    pub goal: Vec3,
    /// The corners still ahead, ending at the goal, in grid space.
    pub waypoints: Vec<Vec3>,
    /// Its speed for this route, or the agent's own.
    pub speed: Option<f32>,
}

impl NavRoute {
    /// The corners on the grid's plane.
    fn corners(&self) -> Vec<Vec2> {
        self.waypoints
            .iter()
            .map(|point| Vec2::new(point.x, point.z))
            .collect()
    }
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
    world.insert_resource(grid);
}

/// The pathfinder's grid for scene data, or `None` if the data is malformed.
pub(in crate::gameplay) fn nav_grid(data: &engine_scene::NavGridData) -> Option<ActiveNavGrid> {
    let upper = data
        .upper
        .iter()
        .map(|span| Span {
            cell: span.cell,
            height: span.height,
            blocked: span.blocked,
        })
        .collect();
    let grid = NavGrid::from_cells(
        data.width,
        data.height,
        data.cell_size,
        Vec2::from_array(data.origin),
        &data.blocked,
    )
    .and_then(|grid| grid.with_heights(data.heights.clone()))
    .and_then(|grid| grid.with_layers(upper, data.climb));
    let settings = data.bake.unwrap_or_default();
    let clearance = match data.plane {
        NavPlane::Ground => Clearance {
            radius: settings.agent_radius,
            step: settings.step_height,
            height: settings.agent_height,
        },
        NavPlane::Screen => Clearance {
            radius: settings.agent_radius,
            step: 0.0,
            height: 2.0 * SCREEN_DEPTH,
        },
    };
    match grid {
        Ok(grid) => Some(ActiveNavGrid {
            grid,
            plane: data.plane,
            clearance,
        }),
        Err(error) => {
            tracing::warn!(%error, "the scene's nav grid cannot be used");
            None
        }
    }
}

/// The plane walking happens on: the scene grid's, else the ground.
pub(super) fn plane(world: &World) -> NavPlane {
    world
        .get_resource::<ActiveNavGrid>()
        .map(|active| active.plane)
        .or_else(|| {
            world
                .get_resource::<SceneNavGrid>()
                .map(|grid| grid.0.plane)
        })
        .unwrap_or_default()
}

/// A world position on the walking plane.
pub(super) fn flat_on(plane: NavPlane, point: Vec3) -> Vec2 {
    match plane {
        NavPlane::Ground => Vec2::new(point.x, point.z),
        NavPlane::Screen => Vec2::new(point.x, point.y),
    }
}

/// How far apart two objects are on the walking plane.
pub(super) fn plane_distance(world: &World, a: Vec3, b: Vec3) -> f32 {
    let plane = plane(world);
    flat_on(plane, a).distance(flat_on(plane, b))
}

/// A world position in grid space.
fn to_grid(plane: NavPlane, point: Vec3) -> Vec3 {
    match plane {
        NavPlane::Ground => point,
        NavPlane::Screen => Vec3::new(point.x, 0.0, point.y),
    }
}

/// A grid-space position in the world; on the screen it keeps `keep`'s
/// depth.
fn from_grid(plane: NavPlane, point: Vec3, keep: Vec3) -> Vec3 {
    match plane {
        NavPlane::Ground => point,
        NavPlane::Screen => Vec3::new(point.x, point.z, keep.z),
    }
}

/// The route in grid space, or straight there without a grid.
fn grid_route(active: Option<&ActiveNavGrid>, from: Vec3, to: Vec3) -> Option<Vec<Vec3>> {
    let Some(active) = active else {
        return Some(vec![to]);
    };
    find_layered_route(
        &active.grid,
        to_grid(active.plane, from),
        to_grid(active.plane, to),
    )
}

/// A route from `from` to `to` in world space, as corner positions ending at
/// the goal (or the closest reachable cell). Without a grid the route is
/// straight there; with one but no way through it is `None`.
pub(in crate::gameplay) fn find_world_route(
    active: Option<&ActiveNavGrid>,
    from: Vec3,
    to: Vec3,
) -> Option<Vec<Vec3>> {
    let route = grid_route(active, from, to)?;
    let Some(active) = active else {
        return Some(route);
    };
    let goal = to_grid(active.plane, to);
    Some(
        route
            .into_iter()
            .map(|point| {
                if point == goal {
                    to
                } else {
                    from_grid(active.plane, point, from)
                }
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
    let active = world.get_resource::<ActiveNavGrid>();
    let route = match active {
        Some(active) => grid_route(Some(active), from, goal),
        None => Some(vec![to_grid(NavPlane::Ground, goal)]),
    };
    match route {
        Some(waypoints) => {
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
        .query_filtered::<(Entity, &NavRoute), Without<Disabled>>()
        .iter(world)
        .map(|(entity, route)| (entity, route.clone()))
        .collect();
    let plane = plane(world);
    // A grid baked on flat ground stores no heights: objects keep their own.
    let has_heights = world
        .get_resource::<ActiveNavGrid>()
        .is_some_and(|active| active.grid.has_heights());
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
        let here = flat_on(plane, position);
        if here.distance(flat_on(plane, route.goal)) <= agent.stopping_distance.max(1e-3)
            || route.waypoints.is_empty()
        {
            world.entity_mut(entity).remove::<NavRoute>();
            world.entity_mut(entity).insert(WalkVelocity::default());
            out.events.push(AiEvent::Arrived { object_id });
            continue;
        }
        let speed = route.speed.unwrap_or(agent.speed);
        let (path_next, path_reached) = follow_path(here, &route.corners(), speed, dt);
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
    let velocities = steered_velocities(world, plane, &steps, dt);

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
        let here = flat_on(plane, position);
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
                    .is_some_and(|corner| Vec2::new(corner.x, corner.z).distance(next) <= reach)
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
                .map(|active| active.grid.floor_height(next, to_grid(plane, position).y))
        } else {
            None
        };
        match (body, plane) {
            (Some(Body::Kinematic), NavPlane::Ground) => {
                out.commands.push(GameCommand::MoveCharacter {
                    object_id: object_id.clone(),
                    delta: [step.x, -WALK_GRAVITY * dt, step.y],
                })
            }
            (Some(Body::Kinematic), NavPlane::Screen) => {
                out.commands.push(GameCommand::MoveCharacter {
                    object_id: object_id.clone(),
                    delta: [step.x, step.y, 0.0],
                })
            }
            (Some(Body::Dynamic(velocity)), _) => {
                let world_velocity = match plane {
                    NavPlane::Ground => [moved.x, velocity.y, moved.y],
                    NavPlane::Screen => [moved.x, moved.y, velocity.z],
                };
                out.commands.push(GameCommand::SetVelocity {
                    object_id: object_id.clone(),
                    velocity: world_velocity,
                });
            }
            (None, _) => {
                let height = ground.unwrap_or(to_grid(plane, position).y);
                let at = from_grid(plane, Vec3::new(next.x, height, next.y), position);
                out.commands.push(GameCommand::SetPosition {
                    object_id: object_id.clone(),
                    position: at.to_array(),
                })
            }
        }
        // 2D characters keep facing the screen.
        if agent.turn_speed > 0.0
            && plane == NavPlane::Ground
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
fn steered_velocities(
    world: &mut World,
    plane: NavPlane,
    steps: &[Step],
    dt: f32,
) -> Vec<Option<Vec2>> {
    if dt <= 0.0 || !steps.iter().any(|step| step.agent.avoid_others) {
        return vec![None; steps.len()];
    }
    let walking: HashSet<Entity> = steps.iter().map(|step| step.entity).collect();
    let mut agents: Vec<engine_ai::AvoidAgent> = steps
        .iter()
        .map(|step| {
            let here = flat_on(plane, step.position);
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
        .query_filtered::<(Entity, &SceneAi), Without<Disabled>>()
        .iter(world)
        .filter(|(entity, _)| !walking.contains(entity))
        .map(|(entity, ai)| (entity, ai.0.agent.radius))
        .collect();
    for (entity, radius) in standing {
        if let Some(position) = super::position(world, entity) {
            agents.push(engine_ai::AvoidAgent {
                position: flat_on(plane, position),
                velocity: Vec2::ZERO,
                preferred: Vec2::ZERO,
                radius,
                max_speed: 0.0,
                steers: false,
            });
        }
    }
    let chosen = engine_ai::avoid(&agents);
    let grid = world
        .get_resource::<ActiveNavGrid>()
        .map(|active| &active.grid);
    steps
        .iter()
        .zip(&agents)
        .zip(chosen)
        .map(|((step, agent), velocity)| {
            if !step.agent.avoid_others || velocity == agent.preferred {
                return None;
            }
            let next = agent.position + velocity * dt;
            let height = to_grid(plane, step.position).y;
            let walkable =
                grid.is_none_or(|grid| grid.is_node_walkable(grid.node_at(next, height)));
            walkable.then_some(velocity)
        })
        .collect()
}

/// Marks where moving bodies with Blocks paths on stand, and sends every
/// walker whose way one now blocks on a new route. Mesh colliders don't
/// block while moving.
pub(super) fn update_moving_blockers(world: &mut World) {
    let Some(plane) = world
        .get_resource::<ActiveNavGrid>()
        .map(|active| active.plane)
    else {
        return;
    };
    let to_grid_space = match plane {
        NavPlane::Ground => Mat4::IDENTITY,
        // Lifted so every depth lies in the space above the grid's floor.
        NavPlane::Screen => {
            Mat4::from_translation(Vec3::new(0.0, SCREEN_DEPTH, 0.0)) * SCREEN_TO_GRID
        }
    };
    let blockers: Vec<(Entity, engine_scene::ColliderShape)> = world
        .query_filtered::<(Entity, &SceneCollider), (Without<Disabled>, Without<SceneAi>)>()
        .iter(world)
        .filter(|(_, collider)| {
            collider.0.blocks_walking && !collider.0.trigger && collider.0.body != BodyKind::Fixed
        })
        .map(|(entity, collider)| (entity, collider.0.shape.clone()))
        .collect();
    let solids: Vec<Solid> = blockers
        .iter()
        .filter_map(|(entity, shape)| {
            let to_world = world
                .get::<GlobalTransform>(*entity)
                .map(|global| global.0.to_matrix())
                .or_else(|| {
                    world
                        .get::<Transform>(*entity)
                        .map(|local| local.0.to_matrix())
                })?;
            collider_solid(shape, to_world, to_grid_space, || None)
        })
        .collect();
    let changed = match world.get_resource_mut::<ActiveNavGrid>() {
        Some(mut active) => {
            let clearance = active.clearance;
            active.grid.block_moving(&solids, &clearance)
        }
        None => false,
    };
    if !changed {
        return;
    }
    let walkers: Vec<(Entity, NavRoute)> = world
        .query_filtered::<(Entity, &NavRoute), Without<Disabled>>()
        .iter(world)
        .map(|(entity, route)| (entity, route.clone()))
        .collect();
    for (entity, route) in walkers {
        let Some(position) = super::position(world, entity) else {
            continue;
        };
        let blocked = world
            .get_resource::<ActiveNavGrid>()
            .is_some_and(|active| route_blocked(active, position, &route.waypoints));
        if blocked {
            start_route(world, entity, route.goal, route.speed);
        }
    }
}

/// Whether something now stands in the way of the corners ahead of a
/// walker at `position`. The leg from where it stands only counts when that
/// floor is open: a walker pushing a crate is not blocked by it.
fn route_blocked(active: &ActiveNavGrid, position: Vec3, waypoints: &[Vec3]) -> bool {
    let grid = &active.grid;
    let node = |point: Vec3| grid.node_at(Vec2::new(point.x, point.z), point.y);
    let here = node(to_grid(active.plane, position));
    let start = grid.is_node_walkable(here).then_some(here);
    let mut legs = start.into_iter().chain(waypoints.iter().map(|p| node(*p)));
    let Some(mut from) = legs.next() else {
        return false;
    };
    for to in legs {
        if !engine_ai::node_line_of_sight(grid, from, to) {
            return true;
        }
        from = to;
    }
    false
}

/// How an object's body moves, when it has one.
enum Body {
    Kinematic,
    /// With its current velocity.
    Dynamic(Vec3),
}

fn body_kind(world: &World, physics: &PhysicsWorld, entity: Entity) -> Option<Body> {
    let handle = world.get::<RigidBody>(entity)?.0;
    let body = physics.rapier.bodies.get(handle)?;
    if body.is_kinematic() {
        Some(Body::Kinematic)
    } else if body.is_dynamic() {
        let velocity = body.linvel();
        Some(Body::Dynamic(Vec3::new(velocity.x, velocity.y, velocity.z)))
    } else {
        None
    }
}

fn warn_once(warned: &mut HashSet<(String, &'static str)>, object_id: &str, reason: &str) {
    if warned.insert((object_id.to_owned(), "move_to")) {
        tracing::warn!(%object_id, reason, "move_to skipped");
    }
}
