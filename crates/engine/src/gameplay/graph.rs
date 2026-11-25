//! Runs gameplay graphs (`.graph` files, see
//! [`engine_scene::GameplayGraphData`]): one instance per object that has
//! one, with its own variables, Do once and Gate state, timers and delays.
//!
//! Flow starts at an event node and follows white wires; data inputs are
//! pulled through coloured wires when a node needs them. Like Lua scripts,
//! a graph only reads the world and queues [`GameCommand`]s, which the
//! runtime applies after the run, so a move shows in what the graph reads
//! from the next event on. Each event may run at most [`STEP_LIMIT`] node
//! steps; a graph that goes past it stops there with a warning naming the
//! node.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use engine_ecs::prelude::World;
use engine_scene::{
    GameplayGraphData, GameplayNodeData, GameplayNodeKind, GraphAiEvent, MAX_LOOP_COUNT, PinType,
    PinValue, TouchKind,
};
use engine_scripting::{GameCommand, ScriptQueries};

use super::commands::{find_object, find_objects_by_tag};

/// Most node steps one event may run, data nodes included.
pub(super) const STEP_LIMIT: u32 = 10_000;
/// Deepest chain of data nodes one input may pull through.
const MAX_DATA_DEPTH: u32 = 64;
/// Most delays one object may have waiting.
const MAX_DELAYS: usize = 256;
/// Most Call event hops one event may make, so graphs calling each
/// other can't run forever.
const MAX_CALL_HOPS: usize = 64;
/// How many frames a node that ran stays in the editor's live view.
const TRACE_FRAMES: u64 = 30;

/// Something a graph reacts to.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum GraphEvent<'a> {
    /// The scene started; objects in the set already ran their Start.
    Start(Option<&'a HashSet<String>>),
    /// A frame passed (seconds).
    EveryFrame(f32),
    /// A key went down.
    KeyPressed(&'a str),
    /// A key came up.
    KeyReleased(&'a str),
    /// `owner` touched or stopped touching `other`.
    Touch {
        kind: TouchKind,
        owner: &'a str,
        other: &'a str,
    },
    /// A named event was sent.
    Named(&'a str),
    /// A UI button was clicked.
    UiClicked(&'a str),
    /// One of the game's variables changed to this value.
    VariableChanged(&'a str, f64),
    /// `owner`'s AI did something, maybe about `target`.
    Ai {
        owner: &'a str,
        event: GraphAiEvent,
        target: Option<&'a str>,
    },
    /// `owner`'s animation passed a named moment.
    Animation { owner: &'a str, name: &'a str },
    /// A Custom event in `owner`'s graph was called.
    Custom { owner: &'a str, name: &'a str },
}

/// What a graph may read while it runs.
pub(super) struct GraphWorld<'a> {
    pub(super) world: &'a World,
    pub(super) queries: &'a dyn ScriptQueries,
    /// Whether a key (by name) is held.
    pub(super) key_held: &'a dyn Fn(&str) -> bool,
    /// The game's shared variables.
    pub(super) variables: Option<&'a engine_scene::RuleVariables>,
}

/// What one run produced.
#[derive(Debug, Default)]
pub(super) struct GraphOutput {
    /// Commands for the runtime to apply, in order.
    pub(super) commands: Vec<GameCommand>,
}

/// One object's graph and its state.
struct Instance {
    owner: String,
    graph: Arc<GameplayGraphData>,
    nodes: HashMap<u32, usize>,
    locals: HashMap<String, PinValue>,
    /// Data outputs of flow nodes and events, from when they last ran.
    outputs: HashMap<(u32, u8), PinValue>,
    once: HashSet<u32>,
    gates: HashMap<u32, bool>,
    /// Seconds each timer has counted, and the timers that are done.
    timers: HashMap<u32, f32>,
    timers_done: HashSet<u32>,
    /// Waiting delays: seconds left and the Delay node.
    delays: Vec<(f32, u32)>,
    /// The frame each node last ran on, for the editor's live view.
    trace: HashMap<u32, u64>,
    /// The runner's frame count, while the instance runs.
    clock: u64,
}

impl Instance {
    fn new(owner: String, graph: Arc<GameplayGraphData>) -> Self {
        let nodes = graph
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.id, index))
            .collect();
        let locals = graph
            .variables
            .iter()
            .map(|variable| (variable.name.clone(), variable.value.clone()))
            .collect();
        let gates = graph
            .nodes
            .iter()
            .filter_map(|node| match node.kind {
                GameplayNodeKind::Gate { start_open } => Some((node.id, start_open)),
                _ => None,
            })
            .collect();
        Self {
            owner,
            graph,
            nodes,
            locals,
            outputs: HashMap::new(),
            once: HashSet::new(),
            gates,
            timers: HashMap::new(),
            timers_done: HashSet::new(),
            delays: Vec::new(),
            trace: HashMap::new(),
            clock: 0,
        }
    }

    fn node(&self, id: u32) -> Option<&GameplayNodeData> {
        self.nodes
            .get(&id)
            .and_then(|&index| self.graph.nodes.get(index))
    }

    /// The events in this graph `event` starts, with the data their
    /// outputs carry.
    fn starts(&self, event: &GraphEvent<'_>) -> Vec<(u32, Vec<PinValue>)> {
        use GameplayNodeKind as K;
        let mine = |owner: &str| owner == self.owner;
        self.graph
            .nodes
            .iter()
            .filter_map(|node| {
                let payload = match (&node.kind, event) {
                    (K::Start, GraphEvent::Start(except)) => {
                        if except.is_some_and(|except| except.contains(&self.owner)) {
                            return None;
                        }
                        Vec::new()
                    }
                    (K::EveryFrame, GraphEvent::EveryFrame(dt)) => {
                        vec![PinValue::Number(f64::from(*dt))]
                    }
                    (K::KeyPressed { key }, GraphEvent::KeyPressed(name))
                    | (K::KeyReleased { key }, GraphEvent::KeyReleased(name))
                        if key.eq_ignore_ascii_case(name) =>
                    {
                        Vec::new()
                    }
                    (
                        K::Touch { kind },
                        GraphEvent::Touch {
                            kind: happened,
                            owner,
                            other,
                        },
                    ) if kind == happened && mine(owner) => {
                        vec![PinValue::Object((*other).to_owned())]
                    }
                    (K::NamedEvent { name }, GraphEvent::Named(sent)) if name == sent => Vec::new(),
                    (K::UiClicked { button }, GraphEvent::UiClicked(clicked))
                        if button == clicked =>
                    {
                        Vec::new()
                    }
                    (K::VariableChanged { name }, GraphEvent::VariableChanged(changed, value))
                        if name == changed =>
                    {
                        vec![PinValue::Number(*value)]
                    }
                    (
                        K::Ai { event: wanted },
                        GraphEvent::Ai {
                            owner,
                            event,
                            target,
                        },
                    ) if wanted == event && mine(owner) => {
                        vec![PinValue::Object(target.unwrap_or_default().to_owned())]
                    }
                    (
                        K::AnimationEvent { name },
                        GraphEvent::Animation {
                            owner,
                            name: passed,
                        },
                    ) if name == passed && mine(owner) => Vec::new(),
                    (
                        K::CustomEvent { name },
                        GraphEvent::Custom {
                            owner,
                            name: called,
                        },
                    ) if name == called && mine(owner) => Vec::new(),
                    _ => return None,
                };
                Some((node.id, payload))
            })
            .collect()
    }

    fn record(&mut self, node: u32) {
        self.trace.insert(node, self.clock);
    }
}

/// A step of flow waiting to run.
enum Frame {
    /// Follow the flow wire out of this output.
    Follow(u32, u8),
    /// The next turn of a For loop.
    Loop { node: u32, index: i64, last: i64 },
}

/// Why a run stopped early.
struct Stopped {
    node: u32,
    reason: &'static str,
}

/// One run of one instance: what it may read and what it queues.
struct Run<'r, 'w> {
    env: &'r GraphWorld<'w>,
    out: &'r mut GraphOutput,
    calls: &'r mut Vec<(String, String)>,
    budget: &'r mut u32,
    rng: &'r mut engine_utils::Rng,
    /// Game variables set during this event, so later reads see them.
    set_variables: &'r mut HashMap<String, f64>,
}

impl Run<'_, '_> {
    fn spend(&mut self, node: u32) -> Result<(), Stopped> {
        if *self.budget == 0 {
            return Err(Stopped {
                node,
                reason: "ran more than 10,000 steps for one event (a loop that never ends?)",
            });
        }
        *self.budget -= 1;
        Ok(())
    }

    /// Runs the flow from `start`'s output `pin`.
    fn flow(&mut self, instance: &mut Instance, start: u32, pin: u8) -> Result<(), Stopped> {
        let mut stack = vec![Frame::Follow(start, pin)];
        while let Some(frame) = stack.pop() {
            match frame {
                Frame::Follow(node, pin) => {
                    let next = instance.graph.wires.iter().find(|wire| {
                        wire.from == node && wire.from_pin == pin && {
                            instance
                                .node(wire.to)
                                .and_then(|to| {
                                    instance
                                        .graph
                                        .inputs_of(to)
                                        .get(usize::from(wire.to_pin))
                                        .copied()
                                })
                                .is_some_and(|input| input.ty == PinType::Flow)
                        }
                    });
                    if let Some(wire) = next.copied() {
                        self.execute(instance, wire.to, wire.to_pin, &mut stack)?;
                    }
                }
                Frame::Loop { node, index, last } => {
                    self.spend(node)?;
                    if index <= last {
                        instance
                            .outputs
                            .insert((node, 2), PinValue::Number(index as f64));
                        stack.push(Frame::Loop {
                            node,
                            index: index + 1,
                            last,
                        });
                        stack.push(Frame::Follow(node, 0));
                    } else {
                        stack.push(Frame::Follow(node, 1));
                    }
                }
            }
        }
        Ok(())
    }

    fn object(&self, instance: &Instance, value: PinValue) -> String {
        match value {
            PinValue::Object(id) | PinValue::Text(id) if !id.is_empty() => id,
            _ => instance.owner.clone(),
        }
    }

    fn input(&mut self, instance: &Instance, node: u32, pin: u8) -> Result<PinValue, Stopped> {
        self.input_at(instance, node, pin, 0)
    }

    fn number(&mut self, instance: &Instance, node: u32, pin: u8) -> Result<f64, Stopped> {
        Ok(as_number(&self.input(instance, node, pin)?))
    }

    fn vector(&mut self, instance: &Instance, node: u32, pin: u8) -> Result<[f32; 3], Stopped> {
        Ok(as_vector(&self.input(instance, node, pin)?))
    }

    fn text(&mut self, instance: &Instance, node: u32, pin: u8) -> Result<String, Stopped> {
        Ok(as_text(&self.input(instance, node, pin)?))
    }

    fn target(&mut self, instance: &Instance, node: u32, pin: u8) -> Result<String, Stopped> {
        let value = self.input(instance, node, pin)?;
        Ok(self.object(instance, value))
    }

    /// The value going into data input `pin` of `node`.
    fn input_at(
        &mut self,
        instance: &Instance,
        node: u32,
        pin: u8,
        depth: u32,
    ) -> Result<PinValue, Stopped> {
        let Some(data) = instance.node(node) else {
            return Ok(PinValue::Number(0.0));
        };
        let ty = instance
            .graph
            .inputs_of(data)
            .get(usize::from(pin))
            .map_or(PinType::Number, |input| input.ty);
        let wire = instance
            .graph
            .wires
            .iter()
            .find(|wire| wire.to == node && wire.to_pin == pin);
        match wire {
            Some(wire) => self.output(instance, wire.from, wire.from_pin, depth + 1),
            None => Ok(data
                .inputs
                .get(&pin)
                .cloned()
                .unwrap_or_else(|| data.kind.default_input(usize::from(pin), ty))),
        }
    }

    /// The value of output `pin` of `node`: what a flow node or event
    /// last gave, or a data node worked out now.
    fn output(
        &mut self,
        instance: &Instance,
        node: u32,
        pin: u8,
        depth: u32,
    ) -> Result<PinValue, Stopped> {
        use GameplayNodeKind as K;
        let Some(data) = instance.node(node) else {
            return Ok(PinValue::Number(0.0));
        };
        if data.kind.is_flow() {
            let ty = instance
                .graph
                .outputs_of(data)
                .get(usize::from(pin))
                .map_or(PinType::Number, |output| output.ty);
            return Ok(instance
                .outputs
                .get(&(node, pin))
                .cloned()
                .unwrap_or_else(|| PinValue::default_of(ty)));
        }
        if depth > MAX_DATA_DEPTH {
            return Err(Stopped {
                node,
                reason: "pulls data through too many nodes (or round in a circle)",
            });
        }
        self.spend(node)?;
        let mut get = |this: &mut Self, pin: u8| this.input_at(instance, node, pin, depth);
        let num =
            |this: &mut Self,
             pin: u8,
             get: &mut dyn FnMut(&mut Self, u8) -> Result<PinValue, Stopped>| {
                get(this, pin).map(|value| as_number(&value))
            };
        let world = self.env.world;
        Ok(match &data.kind {
            K::Value { value } => value.clone(),
            K::SelfObject => PinValue::Object(instance.owner.clone()),
            K::Add => PinValue::Number(num(self, 0, &mut get)? + num(self, 1, &mut get)?),
            K::Subtract => PinValue::Number(num(self, 0, &mut get)? - num(self, 1, &mut get)?),
            K::Multiply => PinValue::Number(num(self, 0, &mut get)? * num(self, 1, &mut get)?),
            K::Divide => {
                let (a, b) = (num(self, 0, &mut get)?, num(self, 1, &mut get)?);
                PinValue::Number(if b == 0.0 { 0.0 } else { a / b })
            }
            K::Compare { compare } => {
                let (a, b) = (num(self, 0, &mut get)?, num(self, 1, &mut get)?);
                PinValue::Bool(compare.holds(a, b))
            }
            K::And => PinValue::Bool(as_bool(&get(self, 0)?) && as_bool(&get(self, 1)?)),
            K::Or => PinValue::Bool(as_bool(&get(self, 0)?) || as_bool(&get(self, 1)?)),
            K::Not => PinValue::Bool(!as_bool(&get(self, 0)?)),
            K::Random => {
                let (min, max) = (num(self, 0, &mut get)?, num(self, 1, &mut get)?);
                let t = f64::from(self.rng.next_f32());
                PinValue::Number(min + (max - min) * t)
            }
            K::MakeVector => PinValue::Vector([
                num(self, 0, &mut get)? as f32,
                num(self, 1, &mut get)? as f32,
                num(self, 2, &mut get)? as f32,
            ]),
            K::SplitVector => {
                let vector = as_vector(&get(self, 0)?);
                PinValue::Number(f64::from(
                    vector.get(usize::from(pin)).copied().unwrap_or(0.0),
                ))
            }
            K::AddVectors => {
                let (a, b) = (as_vector(&get(self, 0)?), as_vector(&get(self, 1)?));
                PinValue::Vector([a[0] + b[0], a[1] + b[1], a[2] + b[2]])
            }
            K::ScaleVector => {
                let (v, by) = (as_vector(&get(self, 0)?), num(self, 1, &mut get)? as f32);
                PinValue::Vector([v[0] * by, v[1] * by, v[2] * by])
            }
            K::Distance => {
                let a = glam::Vec3::from_array(as_vector(&get(self, 0)?));
                let b = glam::Vec3::from_array(as_vector(&get(self, 1)?));
                PinValue::Number(f64::from(a.distance(b)))
            }
            K::JoinText => {
                let (a, b) = (as_text(&get(self, 0)?), as_text(&get(self, 1)?));
                let mut joined = a + &b;
                truncate(&mut joined);
                PinValue::Text(joined)
            }
            K::NumberToText => {
                PinValue::Text(engine_scene::format_variable(num(self, 0, &mut get)?))
            }
            K::PositionOf => {
                let value = get(self, 0)?;
                let id = self.object(instance, value);
                PinValue::Vector(
                    super::rule_runtime::object_position(world, &id).unwrap_or([0.0; 3]),
                )
            }
            K::FindByTag => {
                let tag = as_text(&get(self, 0)?);
                let found = find_objects_by_tag(world, &tag);
                if pin == 1 {
                    PinValue::Number(found.len() as f64)
                } else {
                    PinValue::Object(found.into_iter().next().unwrap_or_default())
                }
            }
            K::Exists => {
                let value = get(self, 0)?;
                let id = self.object(instance, value);
                PinValue::Bool(find_object(world, &id).is_some())
            }
            K::KeyDown { key } => PinValue::Bool((self.env.key_held)(key)),
            K::Axis { negative, positive } => {
                let held = self.env.key_held;
                PinValue::Number(f64::from(
                    i8::from(held(positive)) - i8::from(held(negative)),
                ))
            }
            K::Raycast => {
                let from = as_vector(&get(self, 0)?);
                let direction = glam::Vec3::from_array(as_vector(&get(self, 1)?));
                let distance = num(self, 2, &mut get)? as f32;
                let hit =
                    if direction.length_squared() > 1e-12 && distance.is_finite() && distance > 0.0
                    {
                        self.env.queries.raycast(
                            from,
                            direction.normalize().to_array(),
                            distance.min(10_000.0),
                            Some(&instance.owner),
                        )
                    } else {
                        None
                    };
                match (pin, hit) {
                    (0, hit) => PinValue::Bool(hit.is_some()),
                    (1, hit) => {
                        PinValue::Object(hit.and_then(|hit| hit.object_id).unwrap_or_default())
                    }
                    (_, hit) => PinValue::Vector(hit.map_or([0.0; 3], |hit| hit.point)),
                }
            }
            K::LineOfSight => {
                let from = get(self, 0)?;
                let from = self.object(instance, from);
                let to = get(self, 1)?;
                let to = self.object(instance, to);
                let positions = (
                    super::rule_runtime::object_position(world, &from),
                    super::rule_runtime::object_position(world, &to),
                );
                PinValue::Bool(match positions {
                    (Some(a), Some(b)) => self.env.queries.line_of_sight(a, b, Some(&from)),
                    _ => false,
                })
            }
            K::GetVariable { name } => instance
                .locals
                .get(name)
                .cloned()
                .unwrap_or(PinValue::Number(0.0)),
            K::GetGameVariable { name } => PinValue::Number(
                self.set_variables
                    .get(name)
                    .copied()
                    .or_else(|| self.env.variables.map(|variables| variables.get(name)))
                    .unwrap_or(0.0),
            ),
            _ => PinValue::Number(0.0),
        })
    }

    /// Runs flow node `id`, entered through input `entry`, queuing what
    /// follows on `stack`.
    fn execute(
        &mut self,
        instance: &mut Instance,
        id: u32,
        entry: u8,
        stack: &mut Vec<Frame>,
    ) -> Result<(), Stopped> {
        use GameplayNodeKind as K;
        self.spend(id)?;
        let Some(kind) = instance.node(id).map(|node| node.kind.clone()) else {
            return Ok(());
        };
        instance.record(id);
        let then = Frame::Follow(id, 0);
        match kind {
            K::Branch => {
                let condition = as_bool(&self.input(instance, id, 1)?);
                stack.push(Frame::Follow(id, if condition { 0 } else { 1 }));
            }
            K::Sequence => {
                stack.extend([
                    Frame::Follow(id, 2),
                    Frame::Follow(id, 1),
                    Frame::Follow(id, 0),
                ]);
            }
            K::ForLoop => {
                let first = self.number(instance, id, 1)?;
                let last = self.number(instance, id, 2)?;
                if first.is_finite() && last.is_finite() {
                    let first = first.round() as i64;
                    let last = (last.round() as i64).min(first + MAX_LOOP_COUNT - 1);
                    stack.push(Frame::Loop {
                        node: id,
                        index: first,
                        last,
                    });
                }
            }
            K::Delay => {
                let seconds = self.number(instance, id, 1)?;
                if seconds.is_finite() && instance.delays.len() < MAX_DELAYS {
                    instance.delays.push((seconds.max(0.0) as f32, id));
                }
            }
            K::DoOnce => {
                if entry == 1 {
                    instance.once.remove(&id);
                } else if instance.once.insert(id) {
                    stack.push(then);
                }
            }
            K::Gate { start_open } => {
                let open = instance.gates.entry(id).or_insert(start_open);
                match entry {
                    0 if *open => stack.push(then),
                    1 => *open = true,
                    2 => *open = false,
                    3 => *open = !*open,
                    _ => {}
                }
            }
            K::SetVariable { name } => {
                let value = self.input(instance, id, 1)?;
                if instance.locals.contains_key(&name) {
                    instance.locals.insert(name, value);
                }
                stack.push(then);
            }
            K::SetGameVariable { name } => {
                let value = self.number(instance, id, 1)?;
                if value.is_finite() {
                    self.set_variables.insert(name.clone(), value);
                    self.out
                        .commands
                        .push(GameCommand::SetVariable { name, value });
                }
                stack.push(then);
            }
            K::SpawnPrefab { path } => {
                let position = self.vector(instance, id, 1)?;
                let object_id = uuid::Uuid::new_v4().to_string();
                instance
                    .outputs
                    .insert((id, 1), PinValue::Object(object_id.clone()));
                self.out.commands.push(GameCommand::SpawnPrefab {
                    object_id,
                    path,
                    position,
                });
                stack.push(then);
            }
            K::CallEvent => {
                let target = self.target(instance, id, 1)?;
                let name = self.text(instance, id, 2)?;
                self.calls.push((target, name));
                stack.push(then);
            }
            other => {
                if let Some(command) = self.action(instance, id, &other)? {
                    self.out.commands.push(command);
                }
                stack.push(then);
            }
        }
        Ok(())
    }

    /// The command an action node sends.
    fn action(
        &mut self,
        instance: &Instance,
        id: u32,
        kind: &GameplayNodeKind,
    ) -> Result<Option<GameCommand>, Stopped> {
        use GameplayNodeKind as K;
        Ok(Some(match kind {
            K::Log => GameCommand::Log(self.text(instance, id, 1)?),
            K::SetPosition => GameCommand::SetPosition {
                object_id: self.target(instance, id, 1)?,
                position: self.vector(instance, id, 2)?,
            },
            K::Translate => GameCommand::Translate {
                object_id: self.target(instance, id, 1)?,
                delta: self.vector(instance, id, 2)?,
            },
            K::Rotate => {
                let object_id = self.target(instance, id, 1)?;
                let [x, y, z] = self.vector(instance, id, 2)?;
                let delta = glam::Quat::from_euler(
                    glam::EulerRot::YXZ,
                    y.to_radians(),
                    x.to_radians(),
                    z.to_radians(),
                );
                GameCommand::Rotate {
                    object_id,
                    delta: delta.to_array(),
                }
            }
            K::SetVisible => GameCommand::SetVisible {
                object_id: self.target(instance, id, 1)?,
                visible: as_bool(&self.input(instance, id, 2)?),
            },
            K::Destroy => GameCommand::Destroy {
                object_id: self.target(instance, id, 1)?,
            },
            K::PlaySound => GameCommand::PlayAudio {
                object_id: self.target(instance, id, 1)?,
            },
            K::PlayAnimation => GameCommand::PlayAnimation {
                object_id: self.target(instance, id, 1)?,
                clip: self.text(instance, id, 2)?,
                looping: None,
                speed: 1.0,
                fade: 0.2,
            },
            K::SetText => GameCommand::UiSetText {
                object_id: self.target(instance, id, 1)?,
                text: self.text(instance, id, 2)?,
            },
            K::ChangeScene { path } => GameCommand::ChangeScene { path: path.clone() },
            K::AddImpulse => GameCommand::AddImpulse {
                object_id: self.target(instance, id, 1)?,
                impulse: self.vector(instance, id, 2)?,
            },
            K::SetVelocity => GameCommand::SetVelocity {
                object_id: self.target(instance, id, 1)?,
                velocity: self.vector(instance, id, 2)?,
            },
            K::MoveTo => GameCommand::MoveTo {
                object_id: self.target(instance, id, 1)?,
                target: self.vector(instance, id, 2)?,
                speed: None,
            },
            K::SendEvent => GameCommand::EmitEvent {
                name: self.text(instance, id, 1)?,
            },
            K::SetGameState { state } => GameCommand::SetGameState {
                state: state.clone(),
            },
            K::ShakeCamera => GameCommand::CameraShake {
                strength: self.number(instance, id, 1)? as f32,
                seconds: self.number(instance, id, 2)? as f32,
            },
            _ => return Ok(None),
        }))
    }
}

fn truncate(text: &mut String) {
    if text.len() > engine_scene::MAX_GRAPH_TEXT {
        let mut end = engine_scene::MAX_GRAPH_TEXT;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
}

fn as_number(value: &PinValue) -> f64 {
    match value {
        PinValue::Number(value) => *value,
        PinValue::Bool(value) => f64::from(u8::from(*value)),
        PinValue::Text(text) => text.trim().parse().unwrap_or(0.0),
        PinValue::Vector(vector) => f64::from(vector[0]),
        PinValue::Object(_) => 0.0,
    }
}

fn as_bool(value: &PinValue) -> bool {
    match value {
        PinValue::Bool(value) => *value,
        PinValue::Number(value) => *value != 0.0,
        PinValue::Text(text) | PinValue::Object(text) => !text.is_empty(),
        PinValue::Vector(vector) => vector.iter().any(|v| *v != 0.0),
    }
}

fn as_vector(value: &PinValue) -> [f32; 3] {
    match value {
        PinValue::Vector(vector) => *vector,
        PinValue::Number(value) => [*value as f32; 3],
        _ => [0.0; 3],
    }
}

fn as_text(value: &PinValue) -> String {
    value.display()
}

/// Every object's graph.
pub(super) struct GraphRunner {
    instances: Vec<Instance>,
    /// Parsed graph files, by asset ID (`None` for files that failed).
    loaded: HashMap<String, Option<Arc<GameplayGraphData>>>,
    rng: engine_utils::Rng,
    /// Keys held on the last frame, for Key released.
    held: HashSet<&'static str>,
    /// Frames counted by [`Self::update`].
    frame: u64,
    /// Game variables as last seen, for Variable changed.
    seen_variables: HashMap<String, f64>,
}

impl Default for GraphRunner {
    fn default() -> Self {
        Self {
            instances: Vec::new(),
            loaded: HashMap::new(),
            rng: engine_utils::Rng::new(0x6772_6170_6873),
            held: HashSet::new(),
            frame: 0,
            seen_variables: HashMap::new(),
        }
    }
}

impl GraphRunner {
    /// Whether any object has a graph.
    pub(super) fn is_empty(&self) -> bool {
        self.instances.is_empty()
    }

    /// Adds instances for the objects of `scene` that have a graph and
    /// none yet; returns how many were added. `read` gives a graph file's
    /// bytes by asset ID.
    pub(super) fn adopt(
        &mut self,
        scene: &engine_scene::Scene,
        read: &dyn Fn(&str) -> Option<Vec<u8>>,
    ) -> usize {
        let have: HashSet<String> = self
            .instances
            .iter()
            .map(|instance| instance.owner.clone())
            .collect();
        let mut added = 0;
        for entity in &scene.entities {
            let (Some(owner), Some(graph)) = (&entity.object_id, &entity.gameplay_graph) else {
                continue;
            };
            if have.contains(owner) {
                continue;
            }
            let loaded = self
                .loaded
                .entry(graph.id.clone())
                .or_insert_with(|| {
                    let bytes = read(&graph.id)?;
                    let text = String::from_utf8(bytes).ok()?;
                    match GameplayGraphData::from_ron(&text) {
                        Ok(data) => Some(Arc::new(data)),
                        Err(error) => {
                            tracing::warn!(%error, asset = %graph.id, "gameplay graph can't be read");
                            None
                        }
                    }
                })
                .clone();
            match loaded {
                Some(data) => {
                    for problem in data.problems() {
                        tracing::warn!(
                            object = %owner,
                            node = ?problem.node,
                            "gameplay graph: {problem}"
                        );
                    }
                    self.instances.push(Instance::new(owner.clone(), data));
                    added += 1;
                }
                None => {
                    tracing::warn!(object = %owner, asset = %graph.id, "gameplay graph is missing")
                }
            }
        }
        added
    }

    /// Drops the graphs of destroyed objects.
    pub(super) fn forget(&mut self, destroyed: &[String]) {
        if destroyed.is_empty() {
            return;
        }
        let gone: HashSet<&str> = destroyed.iter().map(String::as_str).collect();
        self.instances
            .retain(|instance| !gone.contains(instance.owner.as_str()));
    }

    /// Drops every instance, for a new scene.
    pub(super) fn clear(&mut self) {
        self.instances.clear();
        self.held.clear();
        self.seen_variables.clear();
    }

    /// Runs every graph's nodes for `event`, then the Custom events they
    /// called.
    pub(super) fn dispatch(&mut self, env: &GraphWorld<'_>, event: &GraphEvent<'_>) -> GraphOutput {
        let mut out = GraphOutput::default();
        let mut budget = STEP_LIMIT;
        let mut set_variables = HashMap::new();
        let mut calls = Vec::new();
        self.run_event(
            env,
            event,
            &mut out,
            &mut budget,
            &mut set_variables,
            &mut calls,
        );
        let mut hops = 0;
        while !calls.is_empty() && hops < MAX_CALL_HOPS {
            hops += 1;
            let pending = std::mem::take(&mut calls);
            for (owner, name) in pending {
                let event = GraphEvent::Custom {
                    owner: &owner,
                    name: &name,
                };
                self.run_event(
                    env,
                    &event,
                    &mut out,
                    &mut budget,
                    &mut set_variables,
                    &mut calls,
                );
            }
        }
        if !calls.is_empty() {
            tracing::warn!(
                "gameplay graphs called each other more than {MAX_CALL_HOPS} times in a row"
            );
        }
        out
    }

    fn run_event(
        &mut self,
        env: &GraphWorld<'_>,
        event: &GraphEvent<'_>,
        out: &mut GraphOutput,
        budget: &mut u32,
        set_variables: &mut HashMap<String, f64>,
        calls: &mut Vec<(String, String)>,
    ) {
        for instance in &mut self.instances {
            instance.clock = self.frame;
            for (node, payload) in instance.starts(event) {
                instance.record(node);
                for (pin, value) in payload.into_iter().enumerate() {
                    instance.outputs.insert((node, pin as u8 + 1), value);
                }
                let mut run = Run {
                    env,
                    out,
                    calls,
                    budget,
                    rng: &mut self.rng,
                    set_variables,
                };
                if let Err(stopped) = run.flow(instance, node, 0) {
                    tracing::warn!(
                        object = %instance.owner,
                        node = stopped.node,
                        "gameplay graph stopped: {}",
                        stopped.reason
                    );
                    return;
                }
            }
        }
    }

    /// One frame: key presses and releases, Every frame, timers, delays
    /// and variable changes, in that order. Keys run while paused; the
    /// rest stand still.
    pub(super) fn update(
        &mut self,
        env: &GraphWorld<'_>,
        pressed: &[&'static str],
        held: &[&'static str],
        dt: f32,
        paused: bool,
    ) -> GraphOutput {
        let mut out = GraphOutput::default();
        if self.instances.is_empty() {
            return out;
        }
        self.frame += 1;
        let mut merge = |this: &mut Self, event: GraphEvent<'_>| {
            out.commands.extend(this.dispatch(env, &event).commands);
        };
        for key in pressed {
            merge(self, GraphEvent::KeyPressed(key));
        }
        let now: HashSet<&'static str> = held.iter().copied().collect();
        let released: Vec<&'static str> = self.held.difference(&now).copied().collect();
        for key in released {
            merge(self, GraphEvent::KeyReleased(key));
        }
        self.held = now;
        if paused {
            return out;
        }
        merge(self, GraphEvent::EveryFrame(dt));

        // Timers and delays that are due, per object.
        let mut due: Vec<(usize, u32, u8)> = Vec::new();
        for (index, instance) in self.instances.iter_mut().enumerate() {
            instance.clock = self.frame;
            let timers: Vec<(u32, f32, bool)> = instance
                .graph
                .nodes
                .iter()
                .filter_map(|node| match node.kind {
                    GameplayNodeKind::Timer { seconds, repeat } => Some((node.id, seconds, repeat)),
                    _ => None,
                })
                .collect();
            for (id, seconds, repeat) in timers {
                if instance.timers_done.contains(&id) {
                    continue;
                }
                let counted = instance.timers.entry(id).or_insert(0.0);
                *counted += dt;
                if *counted >= seconds.max(0.01) {
                    *counted = 0.0;
                    if !repeat {
                        instance.timers_done.insert(id);
                    }
                    due.push((index, id, 0));
                }
            }
            let mut index_delay = 0;
            while index_delay < instance.delays.len() {
                instance.delays[index_delay].0 -= dt;
                if instance.delays[index_delay].0 <= 0.0 {
                    let (_, node) = instance.delays.remove(index_delay);
                    due.push((index, node, 0));
                } else {
                    index_delay += 1;
                }
            }
        }
        let mut budget = STEP_LIMIT;
        let mut set_variables = HashMap::new();
        let mut calls = Vec::new();
        for (index, node, pin) in due {
            let Some(instance) = self.instances.get_mut(index) else {
                continue;
            };
            instance.record(node);
            let mut run = Run {
                env,
                out: &mut out,
                calls: &mut calls,
                budget: &mut budget,
                rng: &mut self.rng,
                set_variables: &mut set_variables,
            };
            if let Err(stopped) = run.flow(instance, node, pin) {
                tracing::warn!(
                    object = %instance.owner,
                    node = stopped.node,
                    "gameplay graph stopped: {}",
                    stopped.reason
                );
            }
        }
        for (owner, name) in calls {
            let event = GraphEvent::Custom {
                owner: &owner,
                name: &name,
            };
            out.commands.extend(self.dispatch(env, &event).commands);
        }

        // Variables changed since the last frame (by anything).
        if let Some(variables) = env.variables {
            let changed: Vec<(String, f64)> = variables
                .iter()
                .filter(|(name, value)| self.seen_variables.get(*name) != Some(value))
                .map(|(name, value)| (name.to_owned(), value))
                .collect();
            let first = self.seen_variables.is_empty();
            for (name, value) in &changed {
                self.seen_variables.insert(name.clone(), *value);
            }
            if !first {
                for (name, value) in changed {
                    out.commands.extend(
                        self.dispatch(env, &GraphEvent::VariableChanged(&name, value))
                            .commands,
                    );
                }
            }
        }
        out
    }

    /// The nodes `owner`'s graph ran in the last [`TRACE_FRAMES`] frames,
    /// smallest ID first.
    pub(super) fn trace(&self, owner: &str) -> Option<Vec<u32>> {
        let instance = self
            .instances
            .iter()
            .find(|instance| instance.owner == owner)?;
        let since = self.frame.saturating_sub(TRACE_FRAMES);
        let mut nodes: Vec<u32> = instance
            .trace
            .iter()
            .filter(|(_, frame)| **frame >= since)
            .map(|(node, _)| *node)
            .collect();
        nodes.sort_unstable();
        Some(nodes)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use engine_scene::{GameplayWire, GraphVariable, RuleCompare, RuleVariables};

    use super::*;

    struct NoQueries;

    impl ScriptQueries for NoQueries {
        fn raycast(
            &self,
            _origin: [f32; 3],
            _direction: [f32; 3],
            _max_distance: f32,
            _ignore: Option<&str>,
        ) -> Option<engine_scripting::ScriptRayHit> {
            None
        }

        fn is_grounded(&self, _object_id: &str) -> bool {
            false
        }
    }

    fn node(id: u32, kind: GameplayNodeKind) -> GameplayNodeData {
        GameplayNodeData {
            id,
            kind,
            position: [0.0, 0.0],
            inputs: BTreeMap::new(),
        }
    }

    fn with_input(mut data: GameplayNodeData, pin: u8, value: PinValue) -> GameplayNodeData {
        data.inputs.insert(pin, value);
        data
    }

    fn wire(from: u32, from_pin: u8, to: u32, to_pin: u8) -> GameplayWire {
        GameplayWire {
            from,
            from_pin,
            to,
            to_pin,
        }
    }

    fn runner(graph: GameplayGraphData) -> GraphRunner {
        let mut runner = GraphRunner::default();
        runner
            .instances
            .push(Instance::new("hero".into(), Arc::new(graph)));
        runner
    }

    fn run(runner: &mut GraphRunner, event: GraphEvent<'_>) -> Vec<GameCommand> {
        let world = World::new();
        let env = GraphWorld {
            world: &world,
            queries: &NoQueries,
            key_held: &|_| false,
            variables: None,
        };
        runner.dispatch(&env, &event).commands
    }

    fn log(text: &str) -> GameCommand {
        GameCommand::Log(text.into())
    }

    fn log_node(id: u32, text: &str) -> GameplayNodeData {
        with_input(
            node(id, GameplayNodeKind::Log),
            1,
            PinValue::Text(text.into()),
        )
    }

    #[test]
    fn start_runs_its_chain_in_order_and_actions_target_the_owner() {
        let graph = GameplayGraphData {
            nodes: vec![
                node(0, GameplayNodeKind::Start),
                log_node(1, "hello"),
                with_input(
                    node(2, GameplayNodeKind::Translate),
                    2,
                    PinValue::Vector([1.0, 0.0, 0.0]),
                ),
            ],
            wires: vec![wire(0, 0, 1, 0), wire(1, 0, 2, 0)],
            ..GameplayGraphData::default()
        };
        let mut runner = runner(graph);
        assert_eq!(
            run(&mut runner, GraphEvent::Start(None)),
            vec![
                log("hello"),
                GameCommand::Translate {
                    object_id: "hero".into(),
                    delta: [1.0, 0.0, 0.0],
                }
            ]
        );
        let except: HashSet<String> = ["hero".to_owned()].into();
        assert!(run(&mut runner, GraphEvent::Start(Some(&except))).is_empty());
    }

    #[test]
    fn branch_compare_and_maths_pull_data_through_wires() {
        // Start → Branch(2 + 3 > 4) → True: Log "big", False: Log "small".
        let graph = GameplayGraphData {
            nodes: vec![
                node(0, GameplayNodeKind::Start),
                node(1, GameplayNodeKind::Branch),
                with_input(
                    with_input(node(2, GameplayNodeKind::Add), 0, PinValue::Number(2.0)),
                    1,
                    PinValue::Number(3.0),
                ),
                with_input(
                    node(
                        3,
                        GameplayNodeKind::Compare {
                            compare: RuleCompare::Greater,
                        },
                    ),
                    1,
                    PinValue::Number(4.0),
                ),
                log_node(4, "big"),
                log_node(5, "small"),
            ],
            wires: vec![
                wire(0, 0, 1, 0),
                wire(2, 0, 3, 0),
                wire(3, 0, 1, 1),
                wire(1, 0, 4, 0),
                wire(1, 1, 5, 0),
            ],
            ..GameplayGraphData::default()
        };
        assert_eq!(
            run(&mut runner(graph), GraphEvent::Start(None)),
            vec![log("big")]
        );
    }

    #[test]
    fn loops_sequences_do_once_and_gates_steer_flow() {
        // Named "go" → Sequence: 1) For 1..3 → Log(index) 2) Do once → Log "once"
        // 3) Gate (closed) → Log "gate".
        let graph = GameplayGraphData {
            nodes: vec![
                node(0, GameplayNodeKind::NamedEvent { name: "go".into() }),
                node(1, GameplayNodeKind::Sequence),
                with_input(
                    with_input(node(2, GameplayNodeKind::ForLoop), 1, PinValue::Number(1.0)),
                    2,
                    PinValue::Number(3.0),
                ),
                node(3, GameplayNodeKind::NumberToText),
                node(4, GameplayNodeKind::Log),
                node(5, GameplayNodeKind::DoOnce),
                log_node(6, "once"),
                node(7, GameplayNodeKind::Gate { start_open: false }),
                log_node(8, "gate"),
            ],
            wires: vec![
                wire(0, 0, 1, 0),
                wire(1, 0, 2, 0),
                wire(2, 0, 4, 0),
                wire(2, 2, 3, 0),
                wire(3, 0, 4, 1),
                wire(1, 1, 5, 0),
                wire(5, 0, 6, 0),
                wire(1, 2, 7, 0),
                wire(7, 0, 8, 0),
            ],
            ..GameplayGraphData::default()
        };
        let mut runner = runner(graph);
        assert_eq!(
            run(&mut runner, GraphEvent::Named("go")),
            vec![log("1"), log("2"), log("3"), log("once")]
        );
        assert_eq!(
            run(&mut runner, GraphEvent::Named("go")),
            vec![log("1"), log("2"), log("3")],
            "Do once ran already; the gate is closed"
        );
    }

    #[test]
    fn a_loop_that_never_ends_stops_at_the_step_limit() {
        // Every frame → For 1..huge (capped at 1000) → Log.
        let graph = GameplayGraphData {
            nodes: vec![
                node(0, GameplayNodeKind::EveryFrame),
                with_input(
                    node(1, GameplayNodeKind::ForLoop),
                    2,
                    PinValue::Number(1e12),
                ),
                node(3, GameplayNodeKind::Log),
            ],
            wires: vec![wire(0, 0, 1, 0), wire(1, 0, 3, 0)],
            ..GameplayGraphData::default()
        };
        let commands = run(&mut runner(graph), GraphEvent::EveryFrame(0.016));
        assert_eq!(commands.len() as i64, MAX_LOOP_COUNT, "the loop is capped");

        // A loop in a loop: a million logs would pass the step limit.
        let graph = GameplayGraphData {
            nodes: vec![
                node(0, GameplayNodeKind::EveryFrame),
                with_input(
                    node(1, GameplayNodeKind::ForLoop),
                    2,
                    PinValue::Number(1000.0),
                ),
                with_input(
                    node(2, GameplayNodeKind::ForLoop),
                    2,
                    PinValue::Number(1000.0),
                ),
                node(3, GameplayNodeKind::Log),
            ],
            wires: vec![wire(0, 0, 1, 0), wire(1, 0, 2, 0), wire(2, 0, 3, 0)],
            ..GameplayGraphData::default()
        };
        let commands = run(&mut runner(graph), GraphEvent::EveryFrame(0.016));
        assert!(
            !commands.is_empty() && commands.len() < STEP_LIMIT as usize,
            "stopped after {} logs",
            commands.len()
        );

        // Two Custom events calling each other forever stop too.
        let graph = GameplayGraphData {
            nodes: vec![
                node(
                    0,
                    GameplayNodeKind::CustomEvent {
                        name: "ping".into(),
                    },
                ),
                with_input(
                    node(1, GameplayNodeKind::CallEvent),
                    2,
                    PinValue::Text("ping".into()),
                ),
            ],
            wires: vec![wire(0, 0, 1, 0)],
            ..GameplayGraphData::default()
        };
        let mut runner = runner(graph);
        let commands = run(
            &mut runner,
            GraphEvent::Custom {
                owner: "hero",
                name: "ping",
            },
        );
        assert!(commands.is_empty());
    }

    #[test]
    fn variables_delays_and_timers_keep_state_between_events() {
        // Every frame → Set coins = coins + 1; Timer 0.5 s → Log coins text;
        // Start → Delay 1 s → Log "later".
        let mut graph = GameplayGraphData {
            nodes: vec![
                node(0, GameplayNodeKind::EveryFrame),
                node(
                    1,
                    GameplayNodeKind::SetVariable {
                        name: "coins".into(),
                    },
                ),
                with_input(node(2, GameplayNodeKind::Add), 1, PinValue::Number(1.0)),
                node(
                    3,
                    GameplayNodeKind::GetVariable {
                        name: "coins".into(),
                    },
                ),
                node(
                    4,
                    GameplayNodeKind::Timer {
                        seconds: 0.5,
                        repeat: false,
                    },
                ),
                node(5, GameplayNodeKind::NumberToText),
                node(6, GameplayNodeKind::Log),
                node(7, GameplayNodeKind::Start),
                with_input(node(8, GameplayNodeKind::Delay), 1, PinValue::Number(1.0)),
                log_node(9, "later"),
            ],
            wires: vec![
                wire(0, 0, 1, 0),
                wire(3, 0, 2, 0),
                wire(2, 0, 1, 1),
                wire(4, 0, 6, 0),
                wire(3, 0, 5, 0),
                wire(5, 0, 6, 1),
                wire(7, 0, 8, 0),
                wire(8, 0, 9, 0),
            ],
            ..GameplayGraphData::default()
        };
        graph.variables.push(GraphVariable {
            name: "coins".into(),
            value: PinValue::Number(0.0),
        });
        assert!(graph.problems().is_empty(), "{:?}", graph.problems());
        let mut runner = runner(graph);
        let world = World::new();
        let env = GraphWorld {
            world: &world,
            queries: &NoQueries,
            key_held: &|_| false,
            variables: None,
        };
        assert!(
            runner
                .dispatch(&env, &GraphEvent::Start(None))
                .commands
                .is_empty()
        );
        let mut logs = Vec::new();
        for _ in 0..4 {
            logs.extend(runner.update(&env, &[], &[], 0.3, false).commands);
        }
        // Frame 2 (0.6 s): the timer fires after two increments; frame 4
        // (1.2 s): the delay is over.
        assert_eq!(logs, vec![log("2"), log("later")]);
        let paused = runner.update(&env, &[], &[], 0.3, true).commands;
        assert!(paused.is_empty(), "nothing counts while paused");
    }

    #[test]
    fn keys_raise_pressed_and_released_and_game_variables_are_read_back() {
        let graph = GameplayGraphData {
            nodes: vec![
                node(
                    0,
                    GameplayNodeKind::KeyPressed {
                        key: "Space".into(),
                    },
                ),
                with_input(
                    node(
                        1,
                        GameplayNodeKind::SetGameVariable {
                            name: "jumps".into(),
                        },
                    ),
                    1,
                    PinValue::Number(4.0),
                ),
                node(
                    2,
                    GameplayNodeKind::GetGameVariable {
                        name: "jumps".into(),
                    },
                ),
                node(3, GameplayNodeKind::NumberToText),
                node(4, GameplayNodeKind::Log),
                node(
                    5,
                    GameplayNodeKind::KeyReleased {
                        key: "Space".into(),
                    },
                ),
                log_node(6, "up"),
            ],
            wires: vec![
                wire(0, 0, 1, 0),
                wire(1, 0, 4, 0),
                wire(2, 0, 3, 0),
                wire(3, 0, 4, 1),
                wire(5, 0, 6, 0),
            ],
            ..GameplayGraphData::default()
        };
        let mut runner = runner(graph);
        let world = World::new();
        let variables = RuleVariables::default();
        let env = GraphWorld {
            world: &world,
            queries: &NoQueries,
            key_held: &|_| false,
            variables: Some(&variables),
        };
        let down = runner
            .update(&env, &["Space"], &["Space"], 0.1, false)
            .commands;
        assert_eq!(
            down,
            vec![
                GameCommand::SetVariable {
                    name: "jumps".into(),
                    value: 4.0,
                },
                log("4"),
            ]
        );
        let up = runner.update(&env, &[], &[], 0.1, false).commands;
        assert_eq!(up, vec![log("up")]);
    }
}
