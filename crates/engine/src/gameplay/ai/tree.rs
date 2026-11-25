//! Running behaviour trees.
//!
//! The tree runs from its root every frame, so conditions always see the
//! present: a Selector re-checks its children from the first one, which lets
//! "Has target?" interrupt a patrol. A Sequence remembers which child was
//! still running and carries on from it, so "walk, then wait" does not walk
//! again every frame, but it re-checks the condition nodes before that
//! child, and stops when one fails.
//!
//! Any node that did not run this frame forgets its progress (a Wait starts
//! over), and a walk whose node stopped running is stopped.

use std::collections::HashMap;

use engine_ecs::prelude::{Entity, World};
use engine_scene::{
    AiBrainData, BehaviourTreeData, BtNodeKind, BtNodeRun, BtTrace, NavAgentData, RuleVariables,
};
use engine_scripting::GameCommand;
use glam::Vec3;

use super::nav::{self, flat};
use super::{AiMemory, AiTick};

/// How often "Move to target" works out a new route to a moving target.
const TARGET_REPATH: f32 = 0.5;
/// How far a target may drift from the end of a route before it is redone.
const TARGET_DRIFT: f32 = 0.75;

/// What a node answered this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::gameplay) enum Status {
    Success,
    Failure,
    Running,
}

impl From<Status> for BtNodeRun {
    fn from(status: Status) -> Self {
        match status {
            Status::Success => Self::Success,
            Status::Failure => Self::Failure,
            Status::Running => Self::Running,
        }
    }
}

/// A tree's progress between frames.
#[derive(Debug, Clone, Default)]
pub(in crate::gameplay) struct TreeMemory {
    /// Per node: what it keeps while it runs.
    nodes: HashMap<usize, NodeState>,
    /// The node whose walk is under way.
    walking: Option<usize>,
    /// Per Patrol node: the point it walks to next. Kept while other
    /// branches run, so a patrol carries on where it stopped.
    patrol: HashMap<usize, usize>,
    /// The nodes the last frame visited and what each answered, for Studio.
    pub(in crate::gameplay) trace: BtTrace,
}

/// What one node keeps while it runs.
#[derive(Debug, Clone, Copy, Default)]
struct NodeState {
    /// Seconds waited, or until the next re-route.
    timer: f32,
    /// A Sequence's running child.
    index: usize,
    /// A walk has been started.
    started: bool,
    /// Ran this frame.
    ticked: bool,
}

/// Everything one tree run reads and changes.
struct Run<'a> {
    world: &'a mut World,
    entity: Entity,
    object_id: &'a str,
    agent: NavAgentData,
    tree: &'a BehaviourTreeData,
    memory: &'a mut AiMemory,
    dt: f32,
    rng: &'a mut engine_utils::Rng,
    out: &'a mut AiTick,
}

/// Runs every behaviour tree for one frame.
pub(super) fn update(world: &mut World, dt: f32, rng: &mut engine_utils::Rng, out: &mut AiTick) {
    for (entity, object_id, ai) in super::agents(world) {
        let Some(AiBrainData::Tree(tree)) = &ai.brain else {
            continue;
        };
        if tree.nodes.is_empty() {
            continue;
        }
        let mut memory = world.get::<AiMemory>(entity).cloned().unwrap_or_default();
        for state in memory.tree.nodes.values_mut() {
            state.ticked = false;
        }
        memory.tree.trace.clear();
        Run {
            world: &mut *world,
            entity,
            object_id: &object_id,
            agent: ai.agent,
            tree,
            memory: &mut memory,
            dt,
            rng: &mut *rng,
            out: &mut *out,
        }
        .tick(0, 0);
        // Forget what nodes that did not run were doing.
        memory.tree.nodes.retain(|_, state| state.ticked);
        if let Some(walking) = memory.tree.walking
            && !memory.tree.nodes.contains_key(&walking)
        {
            memory.tree.walking = None;
            nav::stop(world, entity);
        }
        world.entity_mut(entity).insert(memory);
    }
}

impl Run<'_> {
    fn state(&mut self, node: usize) -> &mut NodeState {
        let state = self.memory.tree.nodes.entry(node).or_default();
        state.ticked = true;
        state
    }

    fn tick(&mut self, node: usize, depth: usize) -> Status {
        // Recorded in visiting order; the answer is filled in on the way out.
        let at = self.memory.tree.trace.len();
        let traced = self.tree.nodes.get(node).is_some() && at < engine_scene::MAX_BT_NODES * 4;
        if traced && let Ok(index) = u16::try_from(node) {
            self.memory.tree.trace.push((index, BtNodeRun::Running));
        }
        let status = self.tick_node(node, depth);
        if traced && let Some(entry) = self.memory.tree.trace.get_mut(at) {
            entry.1 = status.into();
        }
        status
    }

    fn tick_node(&mut self, node: usize, depth: usize) -> Status {
        let Some(data) = self.tree.nodes.get(node) else {
            return Status::Failure;
        };
        if depth > engine_scene::MAX_BT_NODES {
            return Status::Failure;
        }
        self.state(node);
        let kind = data.kind.clone();
        let children = data.children.clone();
        match kind {
            BtNodeKind::Sequence => self.sequence(node, &children, depth),
            BtNodeKind::Selector => {
                for &child in &children {
                    match self.tick(child, depth + 1) {
                        Status::Failure => continue,
                        other => return other,
                    }
                }
                Status::Failure
            }
            BtNodeKind::Inverter => match children.first().map(|&c| self.tick(c, depth + 1)) {
                Some(Status::Success) => Status::Failure,
                Some(Status::Failure) | None => Status::Success,
                Some(Status::Running) => Status::Running,
            },
            BtNodeKind::CanSeeTarget => check(self.memory.sees),
            BtNodeKind::CanHearTarget => check(self.memory.hears),
            BtNodeKind::HasTarget => check(self.memory.target.is_some()),
            BtNodeKind::TargetWithin { distance } => {
                let here = super::position(self.world, self.entity);
                check(match (here, self.target_position()) {
                    (Some(here), Some(at)) => flat(here).distance(flat(at)) <= distance,
                    _ => false,
                })
            }
            BtNodeKind::Variable(condition) => {
                let value = self
                    .world
                    .get_resource::<RuleVariables>()
                    .map_or(0.0, |variables| variables.get(&condition.variable));
                check(condition.compare.holds(value, condition.value))
            }
            BtNodeKind::Chance { percent } => check(self.rng.next_f32() * 100.0 < percent),
            BtNodeKind::MoveToTarget { speed } => self.move_to_target(node, speed),
            BtNodeKind::Patrol { points } => self.patrol(node, &points),
            BtNodeKind::MoveTo { position } => self.walk_to(node, Vec3::from_array(position)),
            BtNodeKind::Wait { seconds } => {
                let dt = self.dt;
                let state = self.state(node);
                state.timer += dt;
                if state.timer >= seconds {
                    state.timer = 0.0;
                    Status::Success
                } else {
                    Status::Running
                }
            }
            BtNodeKind::PlayAnimation { clip } => {
                if !clip.is_empty() {
                    self.out.commands.push(GameCommand::PlayAnimation {
                        object_id: self.object_id.to_owned(),
                        clip,
                        looping: None,
                        speed: 1.0,
                        fade: 0.2,
                    });
                }
                Status::Success
            }
            BtNodeKind::SendEvent { name } => {
                self.out.commands.push(GameCommand::EmitEvent { name });
                Status::Success
            }
            BtNodeKind::SetVariable { name, value } => {
                self.out
                    .commands
                    .push(GameCommand::SetVariable { name, value });
                Status::Success
            }
            BtNodeKind::FaceTarget => {
                let (Some(here), Some(at)) = (
                    super::position(self.world, self.entity),
                    self.target_position(),
                ) else {
                    return Status::Failure;
                };
                if let Some(turned) = super::turn_toward(
                    super::rotation(self.world, self.entity),
                    at - here,
                    if self.agent.turn_speed > 0.0 {
                        self.agent.turn_speed * self.dt
                    } else {
                        180.0
                    },
                ) {
                    self.out.commands.push(GameCommand::SetRotation {
                        object_id: self.object_id.to_owned(),
                        rotation: turned.to_array(),
                    });
                }
                Status::Success
            }
            BtNodeKind::Stop => {
                nav::stop(self.world, self.entity);
                self.memory.tree.walking = None;
                Status::Success
            }
        }
    }

    fn sequence(&mut self, node: usize, children: &[usize], depth: usize) -> Status {
        let resume = self.state(node).index.min(children.len());
        // Conditions before the running child are checked again.
        for &child in &children[..resume] {
            let is_condition = self.tree.nodes.get(child).is_some_and(|data| {
                matches!(
                    data.kind,
                    BtNodeKind::CanSeeTarget
                        | BtNodeKind::CanHearTarget
                        | BtNodeKind::HasTarget
                        | BtNodeKind::TargetWithin { .. }
                        | BtNodeKind::Variable(_)
                        | BtNodeKind::Inverter
                )
            });
            if is_condition && self.tick(child, depth + 1) == Status::Failure {
                self.state(node).index = 0;
                return Status::Failure;
            }
        }
        for (offset, &child) in children[resume..].iter().enumerate() {
            match self.tick(child, depth + 1) {
                Status::Success => continue,
                Status::Failure => {
                    self.state(node).index = 0;
                    return Status::Failure;
                }
                Status::Running => {
                    self.state(node).index = resume + offset;
                    return Status::Running;
                }
            }
        }
        self.state(node).index = 0;
        Status::Success
    }

    fn target_position(&self) -> Option<Vec3> {
        let live = self.memory.target.as_deref().and_then(|id| {
            let entity = super::super::find_object(self.world, id)?;
            super::position(self.world, entity)
        });
        if self.memory.sees || self.memory.hears {
            live.or(self.memory.last_known)
        } else {
            self.memory
                .last_known
                .filter(|_| self.memory.target.is_some())
        }
    }

    fn move_to_target(&mut self, node: usize, speed: Option<f32>) -> Status {
        let (Some(here), Some(at)) = (
            super::position(self.world, self.entity),
            self.target_position(),
        ) else {
            return Status::Failure;
        };
        if flat(here).distance(flat(at)) <= self.agent.stopping_distance.max(0.1) {
            nav::stop(self.world, self.entity);
            return Status::Success;
        }
        let dt = self.dt;
        let state = self.state(node);
        state.timer -= dt;
        let due = state.timer <= 0.0;
        let drifted =
            nav::goal(self.world, self.entity).is_none_or(|goal| goal.distance(at) > TARGET_DRIFT);
        if due || drifted || self.memory.tree.walking != Some(node) {
            if !nav::start_route(self.world, self.entity, at, speed) {
                return Status::Failure;
            }
            self.state(node).timer = TARGET_REPATH;
            self.memory.tree.walking = Some(node);
        }
        Status::Running
    }

    fn walk_to(&mut self, node: usize, goal: Vec3) -> Status {
        let started = self.state(node).started;
        if started {
            if self.memory.tree.walking == Some(node) && nav::is_walking(self.world, self.entity) {
                return Status::Running;
            }
            self.state(node).started = false;
            self.memory.tree.walking = None;
            return Status::Success;
        }
        if !nav::start_route(self.world, self.entity, goal, None) {
            return Status::Failure;
        }
        self.state(node).started = true;
        self.memory.tree.walking = Some(node);
        Status::Running
    }

    fn patrol(&mut self, node: usize, points: &[[f32; 3]]) -> Status {
        if points.is_empty() {
            return Status::Failure;
        }
        let index = self.memory.tree.patrol.get(&node).copied().unwrap_or(0) % points.len();
        let status = self.walk_to(node, Vec3::from_array(points[index]));
        if status == Status::Success {
            self.memory
                .tree
                .patrol
                .insert(node, (index + 1) % points.len());
        }
        status
    }
}

fn check(holds: bool) -> Status {
    if holds {
        Status::Success
    } else {
        Status::Failure
    }
}
