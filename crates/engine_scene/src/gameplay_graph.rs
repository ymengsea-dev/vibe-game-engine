//! Gameplay graphs (`.graph` files): Blueprint-style visual programs an
//! object runs alongside, or instead of, a Lua or Rust script.
//!
//! A graph is nodes joined by wires. White **flow** wires say what happens
//! next, starting from an event node (Start, Every frame, Key pressed…);
//! coloured **data** wires carry values of one [`PinType`] into the nodes
//! that need them. An unwired data input uses the value stored on the node
//! ([`GameplayNodeData::inputs`]) or its kind's default.
//!
//! This module holds the file format and the checks the editor and the
//! game share. The game runs graphs in `engine::gameplay`, which turns
//! their actions into the same commands Lua and Rust games send.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::SceneError;
use crate::format::RuleCompare;

/// The file extension of a gameplay graph.
pub const GAMEPLAY_GRAPH_EXTENSION: &str = "graph";
/// Most nodes one graph may have.
pub const MAX_GAMEPLAY_NODES: usize = 512;
/// Most variables one graph may have.
pub const MAX_GRAPH_VARIABLES: usize = 64;
/// Most comments one graph may have.
pub const MAX_GRAPH_COMMENTS: usize = 64;
/// Most times one For loop may run its body.
pub const MAX_LOOP_COUNT: i64 = 1000;
/// The longest name (variable, event, key, path, text value) in bytes.
pub const MAX_GRAPH_TEXT: usize = 1024;

/// What a pin carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PinType {
    /// What happens next (white wires).
    Flow,
    /// True or false.
    Bool,
    /// A number.
    Number,
    /// Text.
    Text,
    /// Three numbers: a position, direction or size.
    Vector,
    /// A scene object, by its ID; empty is the graph's own object.
    Object,
}

impl PinType {
    /// Its name in the editor.
    pub fn label(self) -> &'static str {
        match self {
            PinType::Flow => "Flow",
            PinType::Bool => "True / false",
            PinType::Number => "Number",
            PinType::Text => "Text",
            PinType::Vector => "Vector",
            PinType::Object => "Object",
        }
    }

    /// The types a variable can have.
    pub const VALUES: [PinType; 5] = [
        PinType::Bool,
        PinType::Number,
        PinType::Text,
        PinType::Vector,
        PinType::Object,
    ];
}

/// A value on a data pin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PinValue {
    /// True or false.
    Bool(bool),
    /// A number.
    Number(f64),
    /// Text.
    Text(String),
    /// Three numbers.
    Vector([f32; 3]),
    /// A scene object's ID; empty is the graph's own object.
    Object(String),
}

impl PinValue {
    /// Its type.
    pub fn pin_type(&self) -> PinType {
        match self {
            PinValue::Bool(_) => PinType::Bool,
            PinValue::Number(_) => PinType::Number,
            PinValue::Text(_) => PinType::Text,
            PinValue::Vector(_) => PinType::Vector,
            PinValue::Object(_) => PinType::Object,
        }
    }

    /// The zero value of `ty` (false, 0, empty, the origin, this object).
    pub fn default_of(ty: PinType) -> PinValue {
        match ty {
            PinType::Bool | PinType::Flow => PinValue::Bool(false),
            PinType::Number => PinValue::Number(0.0),
            PinType::Text => PinValue::Text(String::new()),
            PinType::Vector => PinValue::Vector([0.0; 3]),
            PinType::Object => PinValue::Object(String::new()),
        }
    }

    /// Short text for the editor and for `To text`.
    pub fn display(&self) -> String {
        match self {
            PinValue::Bool(value) => if *value { "true" } else { "false" }.into(),
            PinValue::Number(value) => crate::format_variable(*value),
            PinValue::Text(text) => text.clone(),
            PinValue::Vector([x, y, z]) => format!("{x}, {y}, {z}"),
            PinValue::Object(id) if id.is_empty() => "this object".into(),
            PinValue::Object(id) => id.clone(),
        }
    }

    fn finite(&self) -> bool {
        match self {
            PinValue::Number(value) => value.is_finite(),
            PinValue::Vector(value) => value.iter().all(|v| v.is_finite()),
            PinValue::Text(text) | PinValue::Object(text) => text.len() <= MAX_GRAPH_TEXT,
            PinValue::Bool(_) => true,
        }
    }
}

/// One pin: its name and what it carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphPin {
    /// Its name.
    pub name: &'static str,
    /// What it carries.
    pub ty: PinType,
}

const fn pin(name: &'static str, ty: PinType) -> GraphPin {
    GraphPin { name, ty }
}

const FLOW_IN: GraphPin = pin("", PinType::Flow);
const THEN: GraphPin = pin("Then", PinType::Flow);
const OBJECT: GraphPin = pin("Object", PinType::Object);

/// Which contact an object's Touch event reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TouchKind {
    /// Began touching a solid object.
    Starts,
    /// Stopped touching a solid object.
    Stops,
    /// Entered a trigger, or a trigger it is.
    Enters,
    /// Left a trigger.
    Leaves,
}

impl TouchKind {
    /// Every kind.
    pub const ALL: [TouchKind; 4] = [
        TouchKind::Starts,
        TouchKind::Stops,
        TouchKind::Enters,
        TouchKind::Leaves,
    ];

    /// Its name in the editor.
    pub fn label(self) -> &'static str {
        match self {
            TouchKind::Starts => "Starts touching",
            TouchKind::Stops => "Stops touching",
            TouchKind::Enters => "Trigger entered",
            TouchKind::Leaves => "Trigger left",
        }
    }
}

/// What an object's AI did, for the AI event node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GraphAiEvent {
    /// It noticed a target.
    TargetSeen,
    /// It lost its target.
    TargetLost,
    /// It heard a noise.
    NoiseHeard,
    /// It reached where it was walking to.
    MoveFinished,
    /// Its Simple brain changed state.
    StateChanged,
}

impl GraphAiEvent {
    /// Every event.
    pub const ALL: [GraphAiEvent; 5] = [
        GraphAiEvent::TargetSeen,
        GraphAiEvent::TargetLost,
        GraphAiEvent::NoiseHeard,
        GraphAiEvent::MoveFinished,
        GraphAiEvent::StateChanged,
    ];

    /// Its name in the editor.
    pub fn label(self) -> &'static str {
        match self {
            GraphAiEvent::TargetSeen => "Sees a target",
            GraphAiEvent::TargetLost => "Loses its target",
            GraphAiEvent::NoiseHeard => "Hears a noise",
            GraphAiEvent::MoveFinished => "Arrives",
            GraphAiEvent::StateChanged => "Changes state",
        }
    }
}

/// What a node does. Settings that never change while the game runs (key
/// names, event names, paths) are stored here; values that may come from
/// wires are input pins.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GameplayNodeKind {
    // Events.
    /// Once, when the scene starts.
    Start,
    /// Every frame; outputs the seconds since the last one.
    EveryFrame,
    /// When a key goes down.
    KeyPressed {
        /// Key name, as Lua's `engine.key_pressed` takes it.
        key: String,
    },
    /// When a key comes up.
    KeyReleased {
        /// Key name.
        key: String,
    },
    /// When this object touches or stops touching another.
    Touch {
        /// Which contact.
        kind: TouchKind,
    },
    /// When a named event is sent (by a graph, a rule or a script).
    NamedEvent {
        /// The event's name.
        name: String,
    },
    /// Every so many seconds, or once.
    Timer {
        /// Seconds.
        seconds: f32,
        /// Again and again; otherwise once.
        repeat: bool,
    },
    /// When a UI button is clicked.
    UiClicked {
        /// The button's object ID.
        button: String,
    },
    /// When one of the game's variables changes.
    VariableChanged {
        /// The variable.
        name: String,
    },
    /// When this object's AI does something.
    Ai {
        /// What.
        event: GraphAiEvent,
    },
    /// When this object's animation passes a named moment.
    AnimationEvent {
        /// The clip event's name.
        name: String,
    },
    /// When this graph (or another object's) calls it with Call event.
    CustomEvent {
        /// The event's name.
        name: String,
    },

    // Flow.
    /// True or False, by a condition.
    Branch,
    /// Three things in order.
    Sequence,
    /// The body once per number from First to Last.
    ForLoop,
    /// Then, after some seconds.
    Delay,
    /// Then, only the first time (until Reset).
    DoOnce,
    /// Lets flow through while open.
    Gate {
        /// Open when the scene starts.
        start_open: bool,
    },

    // Values.
    /// A fixed value of any type.
    Value {
        /// The value.
        value: PinValue,
    },
    /// The graph's own object.
    SelfObject,

    // Maths and logic.
    /// `A + B`.
    Add,
    /// `A − B`.
    Subtract,
    /// `A × B`.
    Multiply,
    /// `A ÷ B` (0 when B is 0).
    Divide,
    /// Compares two numbers.
    Compare {
        /// How.
        compare: RuleCompare,
    },
    /// Both true.
    And,
    /// Either true.
    Or,
    /// The opposite.
    Not,
    /// A random number from Min to Max.
    Random,

    // Vectors.
    /// Three numbers as a vector.
    MakeVector,
    /// A vector's three numbers.
    SplitVector,
    /// `A + B`, for vectors.
    AddVectors,
    /// A vector times a number.
    ScaleVector,
    /// How far apart two points are.
    Distance,

    // Text.
    /// Two texts one after the other.
    JoinText,
    /// A number as text.
    NumberToText,

    // Queries.
    /// Where an object is in the world.
    PositionOf,
    /// The first object with a tag, and how many have it.
    FindByTag,
    /// Whether an object is still there.
    Exists,
    /// Whether a key is held down.
    KeyDown {
        /// Key name.
        key: String,
    },
    /// −1, 0 or 1 from two keys, for movement.
    Axis {
        /// The key for −1.
        negative: String,
        /// The key for 1.
        positive: String,
    },
    /// What a straight line from a point first hits.
    Raycast,
    /// Whether nothing solid stands between two objects.
    LineOfSight,

    // Variables.
    /// One of this graph's variables.
    GetVariable {
        /// The variable.
        name: String,
    },
    /// Changes one of this graph's variables.
    SetVariable {
        /// The variable.
        name: String,
    },
    /// One of the game's shared number variables (the ones rules and
    /// scripts use).
    GetGameVariable {
        /// The variable.
        name: String,
    },
    /// Changes one of the game's shared variables.
    SetGameVariable {
        /// The variable.
        name: String,
    },

    // Actions.
    /// Writes a line to the Console.
    Log,
    /// Moves an object to a position.
    SetPosition,
    /// Moves an object by an amount.
    Translate,
    /// Turns an object by degrees around X, Y and Z.
    Rotate,
    /// Shows or hides an object.
    SetVisible,
    /// Removes an object.
    Destroy,
    /// Places a copy of a prefab.
    SpawnPrefab {
        /// The prefab's path, under `assets/`.
        path: String,
    },
    /// Plays an object's sound.
    PlaySound,
    /// Plays an animation clip on an object.
    PlayAnimation,
    /// Changes a UI label's or button's text.
    SetText,
    /// Changes to another scene.
    ChangeScene {
        /// The scene's path, `scenes/…ron`.
        path: String,
    },
    /// Gives a body a push.
    AddImpulse,
    /// Sets a body's speed and direction.
    SetVelocity,
    /// Walks an object to a place.
    MoveTo,
    /// Sends a named event to every graph, rule and script.
    SendEvent,
    /// Runs a Custom event in an object's graph.
    CallEvent,
    /// Switches the game state.
    SetGameState {
        /// `playing`, `paused` or another game state.
        state: String,
    },
    /// Shakes the camera.
    ShakeCamera,
}

/// What kind of node, for colours and the add menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeGroup {
    /// Starts flow.
    Event,
    /// Steers flow.
    Flow,
    /// Pure values and maths.
    Data,
    /// Reads the game.
    Query,
    /// Graph and game variables.
    Variable,
    /// Changes the game.
    Action,
}

impl NodeGroup {
    /// Its name in the add menu.
    pub fn label(self) -> &'static str {
        match self {
            NodeGroup::Event => "Events",
            NodeGroup::Flow => "Flow",
            NodeGroup::Data => "Values and maths",
            NodeGroup::Query => "Questions",
            NodeGroup::Variable => "Variables",
            NodeGroup::Action => "Actions",
        }
    }
}

impl GameplayNodeKind {
    /// The name shown on the node and in the add menu.
    pub fn label(&self) -> String {
        use GameplayNodeKind as K;
        match self {
            K::Start => "Start".into(),
            K::EveryFrame => "Every frame".into(),
            K::KeyPressed { .. } => "Key pressed".into(),
            K::KeyReleased { .. } => "Key released".into(),
            K::Touch { kind } => kind.label().into(),
            K::NamedEvent { .. } => "Named event".into(),
            K::Timer { .. } => "Timer".into(),
            K::UiClicked { .. } => "Button clicked".into(),
            K::VariableChanged { .. } => "Variable changed".into(),
            K::Ai { event } => format!("AI: {}", event.label()),
            K::AnimationEvent { .. } => "Animation event".into(),
            K::CustomEvent { .. } => "Custom event".into(),
            K::Branch => "Branch".into(),
            K::Sequence => "Sequence".into(),
            K::ForLoop => "For loop".into(),
            K::Delay => "Delay".into(),
            K::DoOnce => "Do once".into(),
            K::Gate { .. } => "Gate".into(),
            K::Value { value } => format!("{} value", value.pin_type().label()),
            K::SelfObject => "This object".into(),
            K::Add => "Add".into(),
            K::Subtract => "Subtract".into(),
            K::Multiply => "Multiply".into(),
            K::Divide => "Divide".into(),
            K::Compare { compare } => format!("Compare {}", compare.symbol()),
            K::And => "And".into(),
            K::Or => "Or".into(),
            K::Not => "Not".into(),
            K::Random => "Random number".into(),
            K::MakeVector => "Make vector".into(),
            K::SplitVector => "Split vector".into(),
            K::AddVectors => "Add vectors".into(),
            K::ScaleVector => "Scale vector".into(),
            K::Distance => "Distance".into(),
            K::JoinText => "Join text".into(),
            K::NumberToText => "Number to text".into(),
            K::PositionOf => "Position of".into(),
            K::FindByTag => "Find by tag".into(),
            K::Exists => "Exists".into(),
            K::KeyDown { .. } => "Key held".into(),
            K::Axis { .. } => "Key axis".into(),
            K::Raycast => "Raycast".into(),
            K::LineOfSight => "Line of sight".into(),
            K::GetVariable { .. } => "Get variable".into(),
            K::SetVariable { .. } => "Set variable".into(),
            K::GetGameVariable { .. } => "Get game variable".into(),
            K::SetGameVariable { .. } => "Set game variable".into(),
            K::Log => "Log".into(),
            K::SetPosition => "Set position".into(),
            K::Translate => "Move by".into(),
            K::Rotate => "Rotate".into(),
            K::SetVisible => "Show or hide".into(),
            K::Destroy => "Destroy".into(),
            K::SpawnPrefab { .. } => "Spawn prefab".into(),
            K::PlaySound => "Play sound".into(),
            K::PlayAnimation => "Play animation".into(),
            K::SetText => "Set UI text".into(),
            K::ChangeScene { .. } => "Change scene".into(),
            K::AddImpulse => "Push".into(),
            K::SetVelocity => "Set velocity".into(),
            K::MoveTo => "Walk to".into(),
            K::SendEvent => "Send event".into(),
            K::CallEvent => "Call event".into(),
            K::SetGameState { .. } => "Set game state".into(),
            K::ShakeCamera => "Shake camera".into(),
        }
    }

    /// Its group.
    pub fn group(&self) -> NodeGroup {
        use GameplayNodeKind as K;
        match self {
            K::Start
            | K::EveryFrame
            | K::KeyPressed { .. }
            | K::KeyReleased { .. }
            | K::Touch { .. }
            | K::NamedEvent { .. }
            | K::Timer { .. }
            | K::UiClicked { .. }
            | K::VariableChanged { .. }
            | K::Ai { .. }
            | K::AnimationEvent { .. }
            | K::CustomEvent { .. } => NodeGroup::Event,
            K::Branch | K::Sequence | K::ForLoop | K::Delay | K::DoOnce | K::Gate { .. } => {
                NodeGroup::Flow
            }
            K::Value { .. }
            | K::SelfObject
            | K::Add
            | K::Subtract
            | K::Multiply
            | K::Divide
            | K::Compare { .. }
            | K::And
            | K::Or
            | K::Not
            | K::Random
            | K::MakeVector
            | K::SplitVector
            | K::AddVectors
            | K::ScaleVector
            | K::Distance
            | K::JoinText
            | K::NumberToText => NodeGroup::Data,
            K::PositionOf
            | K::FindByTag
            | K::Exists
            | K::KeyDown { .. }
            | K::Axis { .. }
            | K::Raycast
            | K::LineOfSight => NodeGroup::Query,
            K::GetVariable { .. }
            | K::SetVariable { .. }
            | K::GetGameVariable { .. }
            | K::SetGameVariable { .. } => NodeGroup::Variable,
            _ => NodeGroup::Action,
        }
    }

    /// Whether flow runs through it (it has flow pins).
    pub fn is_flow(&self) -> bool {
        self.inputs()
            .iter()
            .chain(self.outputs().iter())
            .any(|pin| pin.ty == PinType::Flow)
    }

    /// The input pins, flow first. `variables` gives the graph's variable
    /// types, for Set variable.
    pub fn inputs_with(&self, variables: &[GraphVariable]) -> Vec<GraphPin> {
        use GameplayNodeKind as K;
        use PinType::{Bool, Number, Object, Text, Vector};
        let number2 = || vec![pin("A", Number), pin("B", Number)];
        match self {
            K::Start
            | K::EveryFrame
            | K::KeyPressed { .. }
            | K::KeyReleased { .. }
            | K::Touch { .. }
            | K::NamedEvent { .. }
            | K::Timer { .. }
            | K::UiClicked { .. }
            | K::VariableChanged { .. }
            | K::Ai { .. }
            | K::AnimationEvent { .. }
            | K::CustomEvent { .. }
            | K::Value { .. }
            | K::SelfObject
            | K::KeyDown { .. }
            | K::Axis { .. }
            | K::GetVariable { .. }
            | K::GetGameVariable { .. } => Vec::new(),
            K::Branch => vec![FLOW_IN, pin("Condition", Bool)],
            K::Sequence => vec![FLOW_IN],
            K::ForLoop => vec![FLOW_IN, pin("First", Number), pin("Last", Number)],
            K::Delay => vec![FLOW_IN, pin("Seconds", Number)],
            K::DoOnce => vec![FLOW_IN, pin("Reset", PinType::Flow)],
            K::Gate { .. } => vec![
                FLOW_IN,
                pin("Open", PinType::Flow),
                pin("Close", PinType::Flow),
                pin("Toggle", PinType::Flow),
            ],
            K::Add | K::Subtract | K::Multiply | K::Divide | K::Compare { .. } => number2(),
            K::And | K::Or => vec![pin("A", Bool), pin("B", Bool)],
            K::Not => vec![pin("Value", Bool)],
            K::Random => vec![pin("Min", Number), pin("Max", Number)],
            K::MakeVector => vec![pin("X", Number), pin("Y", Number), pin("Z", Number)],
            K::SplitVector => vec![pin("Vector", Vector)],
            K::AddVectors => vec![pin("A", Vector), pin("B", Vector)],
            K::ScaleVector => vec![pin("Vector", Vector), pin("By", Number)],
            K::Distance => vec![pin("A", Vector), pin("B", Vector)],
            K::JoinText => vec![pin("A", Text), pin("B", Text)],
            K::NumberToText => vec![pin("Number", Number)],
            K::PositionOf | K::Exists => vec![OBJECT],
            K::FindByTag => vec![pin("Tag", Text)],
            K::Raycast => vec![
                pin("From", Vector),
                pin("Direction", Vector),
                pin("Distance", Number),
            ],
            K::LineOfSight => vec![pin("From", Object), pin("To", Object)],
            K::SetVariable { name } => {
                let ty = variables
                    .iter()
                    .find(|variable| variable.name == *name)
                    .map_or(Number, |variable| variable.value.pin_type());
                vec![FLOW_IN, pin("Value", ty)]
            }
            K::SetGameVariable { .. } => vec![FLOW_IN, pin("Value", Number)],
            K::Log => vec![FLOW_IN, pin("Text", Text)],
            K::SetPosition => vec![FLOW_IN, OBJECT, pin("Position", Vector)],
            K::Translate => vec![FLOW_IN, OBJECT, pin("By", Vector)],
            K::Rotate => vec![FLOW_IN, OBJECT, pin("Degrees", Vector)],
            K::SetVisible => vec![FLOW_IN, OBJECT, pin("Visible", Bool)],
            K::Destroy | K::PlaySound => vec![FLOW_IN, OBJECT],
            K::SpawnPrefab { .. } => vec![FLOW_IN, pin("Position", Vector)],
            K::PlayAnimation => vec![FLOW_IN, OBJECT, pin("Clip", Text)],
            K::SetText => vec![FLOW_IN, pin("Element", Object), pin("Text", Text)],
            K::ChangeScene { .. } | K::SetGameState { .. } => vec![FLOW_IN],
            K::AddImpulse => vec![FLOW_IN, OBJECT, pin("Impulse", Vector)],
            K::SetVelocity => vec![FLOW_IN, OBJECT, pin("Velocity", Vector)],
            K::MoveTo => vec![FLOW_IN, OBJECT, pin("To", Vector)],
            K::SendEvent => vec![FLOW_IN, pin("Name", Text)],
            K::CallEvent => vec![FLOW_IN, OBJECT, pin("Name", Text)],
            K::ShakeCamera => vec![FLOW_IN, pin("Strength", Number), pin("Seconds", Number)],
        }
    }

    /// [`Self::inputs_with`] for a graph without variables.
    pub fn inputs(&self) -> Vec<GraphPin> {
        self.inputs_with(&[])
    }

    /// The output pins, flow first. `variables` gives the graph's variable
    /// types, for Get variable.
    pub fn outputs_with(&self, variables: &[GraphVariable]) -> Vec<GraphPin> {
        use GameplayNodeKind as K;
        use PinType::{Bool, Flow, Number, Object, Text, Vector};
        let out = |ty| vec![pin("Out", ty)];
        match self {
            K::Start
            | K::KeyPressed { .. }
            | K::KeyReleased { .. }
            | K::NamedEvent { .. }
            | K::Timer { .. }
            | K::UiClicked { .. }
            | K::AnimationEvent { .. }
            | K::CustomEvent { .. }
            | K::Delay
            | K::DoOnce => vec![THEN],
            K::EveryFrame => vec![THEN, pin("Seconds", Number)],
            K::Touch { .. } => vec![THEN, pin("Other", Object)],
            K::VariableChanged { .. } => vec![THEN, pin("Value", Number)],
            K::Ai { .. } => vec![THEN, pin("Target", Object)],
            K::Branch => vec![pin("True", Flow), pin("False", Flow)],
            K::Sequence => vec![
                pin("Then 1", Flow),
                pin("Then 2", Flow),
                pin("Then 3", Flow),
            ],
            K::ForLoop => vec![pin("Body", Flow), pin("Done", Flow), pin("Index", Number)],
            K::Gate { .. } => vec![pin("Exit", Flow)],
            K::Value { value } => out(value.pin_type()),
            K::SelfObject => out(Object),
            K::Add
            | K::Subtract
            | K::Multiply
            | K::Divide
            | K::Random
            | K::Distance
            | K::Axis { .. } => out(Number),
            K::Compare { .. } | K::And | K::Or | K::Not | K::Exists | K::KeyDown { .. } => {
                out(Bool)
            }
            K::LineOfSight => out(Bool),
            K::MakeVector | K::AddVectors | K::ScaleVector | K::PositionOf => out(Vector),
            K::SplitVector => vec![pin("X", Number), pin("Y", Number), pin("Z", Number)],
            K::JoinText | K::NumberToText => out(Text),
            K::FindByTag => vec![pin("Object", Object), pin("Count", Number)],
            K::Raycast => vec![
                pin("Hit", Bool),
                pin("Object", Object),
                pin("Point", Vector),
            ],
            K::GetVariable { name } => {
                let ty = variables
                    .iter()
                    .find(|variable| variable.name == *name)
                    .map_or(Number, |variable| variable.value.pin_type());
                out(ty)
            }
            K::GetGameVariable { .. } => out(Number),
            K::SpawnPrefab { .. } => vec![THEN, pin("Spawned", Object)],
            K::SetVariable { .. }
            | K::SetGameVariable { .. }
            | K::Log
            | K::SetPosition
            | K::Translate
            | K::Rotate
            | K::SetVisible
            | K::Destroy
            | K::PlaySound
            | K::PlayAnimation
            | K::SetText
            | K::ChangeScene { .. }
            | K::AddImpulse
            | K::SetVelocity
            | K::MoveTo
            | K::SendEvent
            | K::CallEvent
            | K::SetGameState { .. }
            | K::ShakeCamera => vec![THEN],
        }
    }

    /// [`Self::outputs_with`] for a graph without variables.
    pub fn outputs(&self) -> Vec<GraphPin> {
        self.outputs_with(&[])
    }

    /// The value an unwired data input `pin` has when the node stores none.
    pub fn default_input(&self, pin: usize, ty: PinType) -> PinValue {
        use GameplayNodeKind as K;
        let number = |value| PinValue::Number(value);
        match (self, pin) {
            (K::ForLoop, 2) => number(10.0),
            (K::Delay, 1) => number(1.0),
            (K::Multiply, _) | (K::Divide, 1) => number(1.0),
            (K::Random, 1) => number(1.0),
            (K::ScaleVector, 1) => number(1.0),
            (K::SetVisible, 2) => PinValue::Bool(true),
            (K::Raycast, 1) => PinValue::Vector([0.0, -1.0, 0.0]),
            (K::Raycast, 2) => number(100.0),
            (K::ShakeCamera, 1) => number(0.3),
            (K::ShakeCamera, 2) => number(0.5),
            _ => PinValue::default_of(ty),
        }
    }

    /// Every node the add menu offers, with default settings.
    pub fn catalogue() -> Vec<GameplayNodeKind> {
        use GameplayNodeKind as K;
        let mut kinds = vec![
            K::Start,
            K::EveryFrame,
            K::KeyPressed {
                key: "Space".into(),
            },
            K::KeyReleased {
                key: "Space".into(),
            },
        ];
        kinds.extend(TouchKind::ALL.into_iter().map(|kind| K::Touch { kind }));
        kinds.extend([
            K::NamedEvent {
                name: "my_event".into(),
            },
            K::Timer {
                seconds: 1.0,
                repeat: true,
            },
            K::UiClicked {
                button: String::new(),
            },
            K::VariableChanged {
                name: "score".into(),
            },
        ]);
        kinds.extend(GraphAiEvent::ALL.into_iter().map(|event| K::Ai { event }));
        kinds.extend([
            K::AnimationEvent {
                name: "footstep".into(),
            },
            K::CustomEvent {
                name: "my_event".into(),
            },
            K::Branch,
            K::Sequence,
            K::ForLoop,
            K::Delay,
            K::DoOnce,
            K::Gate { start_open: true },
            K::Value {
                value: PinValue::Number(0.0),
            },
            K::Value {
                value: PinValue::Bool(true),
            },
            K::Value {
                value: PinValue::Text(String::new()),
            },
            K::Value {
                value: PinValue::Vector([0.0; 3]),
            },
            K::SelfObject,
            K::Add,
            K::Subtract,
            K::Multiply,
            K::Divide,
            K::Compare {
                compare: RuleCompare::Equal,
            },
            K::And,
            K::Or,
            K::Not,
            K::Random,
            K::MakeVector,
            K::SplitVector,
            K::AddVectors,
            K::ScaleVector,
            K::Distance,
            K::JoinText,
            K::NumberToText,
            K::PositionOf,
            K::FindByTag,
            K::Exists,
            K::KeyDown {
                key: "Space".into(),
            },
            K::Axis {
                negative: "A".into(),
                positive: "D".into(),
            },
            K::Raycast,
            K::LineOfSight,
            K::GetVariable {
                name: String::new(),
            },
            K::SetVariable {
                name: String::new(),
            },
            K::GetGameVariable {
                name: "score".into(),
            },
            K::SetGameVariable {
                name: "score".into(),
            },
            K::Log,
            K::SetPosition,
            K::Translate,
            K::Rotate,
            K::SetVisible,
            K::Destroy,
            K::SpawnPrefab {
                path: String::new(),
            },
            K::PlaySound,
            K::PlayAnimation,
            K::SetText,
            K::ChangeScene {
                path: String::new(),
            },
            K::AddImpulse,
            K::SetVelocity,
            K::MoveTo,
            K::SendEvent,
            K::CallEvent,
            K::SetGameState {
                state: "paused".into(),
            },
            K::ShakeCamera,
        ]);
        kinds
    }

    /// The texts this node's settings hold, for length checks.
    fn texts(&self) -> Vec<&str> {
        use GameplayNodeKind as K;
        match self {
            K::KeyPressed { key } | K::KeyReleased { key } | K::KeyDown { key } => vec![key],
            K::NamedEvent { name }
            | K::VariableChanged { name }
            | K::AnimationEvent { name }
            | K::CustomEvent { name }
            | K::GetVariable { name }
            | K::SetVariable { name }
            | K::GetGameVariable { name }
            | K::SetGameVariable { name } => vec![name],
            K::UiClicked { button } => vec![button],
            K::Axis { negative, positive } => vec![negative, positive],
            K::SpawnPrefab { path } | K::ChangeScene { path } => vec![path],
            K::SetGameState { state } => vec![state],
            _ => Vec::new(),
        }
    }
}

/// One node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameplayNodeData {
    /// Unique in the graph; wires refer to it.
    pub id: u32,
    /// What it does.
    pub kind: GameplayNodeKind,
    /// Where it sits in the graph window.
    #[serde(default)]
    pub position: [f32; 2],
    /// Values of unwired data inputs, by pin index. A pin with none uses
    /// [`GameplayNodeKind::default_input`].
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<u8, PinValue>,
}

/// A wire from output `from_pin` of node `from` to input `to_pin` of `to`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GameplayWire {
    /// The node the flow or value comes from.
    pub from: u32,
    /// Which of its outputs.
    pub from_pin: u8,
    /// The node it goes to.
    pub to: u32,
    /// Which of its inputs.
    pub to_pin: u8,
}

/// A variable of the graph, kept per object while the game runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphVariable {
    /// Its name, unique in the graph.
    pub name: String,
    /// Its starting value, which also fixes its type.
    pub value: PinValue,
}

/// A note on the canvas, drawn behind the nodes it sits under.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphComment {
    /// What it says.
    pub text: String,
    /// Top-left corner.
    pub position: [f32; 2],
    /// Width and height.
    pub size: [f32; 2],
}

/// A gameplay graph as saved in a `.graph` file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GameplayGraphData {
    /// The nodes.
    #[serde(default)]
    pub nodes: Vec<GameplayNodeData>,
    /// The wires.
    #[serde(default)]
    pub wires: Vec<GameplayWire>,
    /// The graph's variables.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variables: Vec<GraphVariable>,
    /// Notes on the canvas.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<GraphComment>,
}

/// Something wrong with a graph, and the node it is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphProblem {
    /// The node, when there is one.
    pub node: Option<u32>,
    /// What is wrong, for the user.
    pub message: String,
}

impl std::fmt::Display for GraphProblem {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl GameplayGraphData {
    /// A graph with a Start node and an Every frame node.
    pub fn new() -> Self {
        let node = |id, kind, x| GameplayNodeData {
            id,
            kind,
            position: [x, 40.0],
            inputs: BTreeMap::new(),
        };
        Self {
            nodes: vec![
                node(0, GameplayNodeKind::Start, 40.0),
                node(1, GameplayNodeKind::EveryFrame, 280.0),
            ],
            ..Self::default()
        }
    }

    /// The node with `id`.
    pub fn node(&self, id: u32) -> Option<&GameplayNodeData> {
        self.nodes.iter().find(|node| node.id == id)
    }

    /// An ID no node has yet.
    pub fn free_id(&self) -> u32 {
        self.nodes.iter().map(|node| node.id + 1).max().unwrap_or(0)
    }

    /// The input pins of `node`.
    pub fn inputs_of(&self, node: &GameplayNodeData) -> Vec<GraphPin> {
        node.kind.inputs_with(&self.variables)
    }

    /// The output pins of `node`.
    pub fn outputs_of(&self, node: &GameplayNodeData) -> Vec<GraphPin> {
        node.kind.outputs_with(&self.variables)
    }

    fn pin_types(&self, wire: &GameplayWire) -> Option<(PinType, PinType)> {
        let from = self.node(wire.from)?;
        let to = self.node(wire.to)?;
        let out = self.outputs_of(from).get(usize::from(wire.from_pin))?.ty;
        let input = self.inputs_of(to).get(usize::from(wire.to_pin))?.ty;
        Some((out, input))
    }

    /// Adds a wire when its pins carry the same type. A data input and a
    /// flow output take one wire each, so an older one there is replaced.
    ///
    /// # Errors
    ///
    /// Why the wire can't be made: a missing pin or different types.
    pub fn connect(&mut self, wire: GameplayWire) -> Result<bool, String> {
        if wire.from == wire.to {
            return Err("a node can't be wired to itself".into());
        }
        let Some((out, input)) = self.pin_types(&wire) else {
            return Err("there is no such pin".into());
        };
        if out != input {
            return Err(format!("{} can't go into {}", out.label(), input.label()));
        }
        if self.wires.contains(&wire) {
            return Ok(false);
        }
        if out == PinType::Flow {
            self.wires
                .retain(|other| !(other.from == wire.from && other.from_pin == wire.from_pin));
        } else {
            self.wires
                .retain(|other| !(other.to == wire.to && other.to_pin == wire.to_pin));
        }
        self.wires.push(wire);
        Ok(true)
    }

    /// Removes node `id` and its wires.
    pub fn remove(&mut self, id: u32) -> bool {
        let before = self.nodes.len();
        self.nodes.retain(|node| node.id != id);
        self.wires.retain(|wire| wire.from != id && wire.to != id);
        self.nodes.len() != before
    }

    /// Rejects files no graph should hold: too many nodes, repeated IDs,
    /// numbers that are not finite, texts too long, variables repeated.
    ///
    /// # Errors
    ///
    /// A message naming what is wrong.
    pub fn validate(&self) -> Result<(), String> {
        if self.nodes.len() > MAX_GAMEPLAY_NODES {
            return Err(format!(
                "a gameplay graph has at most {MAX_GAMEPLAY_NODES} nodes"
            ));
        }
        if self.wires.len() > MAX_GAMEPLAY_NODES * 8 {
            return Err("the gameplay graph has too many wires".into());
        }
        if self.variables.len() > MAX_GRAPH_VARIABLES {
            return Err(format!(
                "a gameplay graph has at most {MAX_GRAPH_VARIABLES} variables"
            ));
        }
        if self.comments.len() > MAX_GRAPH_COMMENTS {
            return Err(format!(
                "a gameplay graph has at most {MAX_GRAPH_COMMENTS} comments"
            ));
        }
        let mut ids = HashSet::new();
        for node in &self.nodes {
            if !ids.insert(node.id) {
                return Err(format!("two nodes share ID {}", node.id));
            }
            if !node.position.iter().all(|v| v.is_finite()) {
                return Err("a node's position is not finite".into());
            }
            if node
                .kind
                .texts()
                .iter()
                .any(|text| text.len() > MAX_GRAPH_TEXT)
            {
                return Err(format!("{}: a setting is too long", node.kind.label()));
            }
            let finite = match &node.kind {
                GameplayNodeKind::Value { value } => value.finite(),
                GameplayNodeKind::Timer { seconds, .. } => seconds.is_finite() && *seconds >= 0.0,
                _ => true,
            };
            if !finite || !node.inputs.values().all(PinValue::finite) {
                return Err(format!("{}: a number is not finite", node.kind.label()));
            }
        }
        let mut names = HashSet::new();
        for variable in &self.variables {
            if variable.name.trim().is_empty() || variable.name.len() > 64 {
                return Err("variable names have 1 to 64 bytes".into());
            }
            if !names.insert(variable.name.as_str()) {
                return Err(format!("two variables are called {}", variable.name));
            }
            if !variable.value.finite() {
                return Err(format!("{}: the value is not finite", variable.name));
            }
        }
        for comment in &self.comments {
            if comment.text.len() > MAX_GRAPH_TEXT
                || !comment
                    .position
                    .iter()
                    .chain(comment.size.iter())
                    .all(|v| v.is_finite())
            {
                return Err("a comment is too long or not finite".into());
            }
        }
        Ok(())
    }

    /// What would stop parts of the graph working: wires between pins of
    /// different types or to missing pins, two wires into one data input,
    /// data that feeds back into itself, and variables that don't exist.
    /// The game skips the affected nodes; the editor shows these.
    pub fn problems(&self) -> Vec<GraphProblem> {
        let mut problems = Vec::new();
        let at = |node: u32, message: String| GraphProblem {
            node: Some(node),
            message,
        };
        let mut into: HashMap<(u32, u8), u32> = HashMap::new();
        for wire in &self.wires {
            match self.pin_types(wire) {
                None => problems.push(at(wire.to, "a wire joins a pin that is not there".into())),
                Some((out, input)) if out != input => problems.push(at(
                    wire.to,
                    format!("{} is wired into {}", out.label(), input.label()),
                )),
                Some((PinType::Flow, _)) => {}
                Some(_) => {
                    if into.insert((wire.to, wire.to_pin), wire.from).is_some() {
                        problems.push(at(wire.to, "two wires go into one input".into()));
                    }
                }
            }
        }
        for node in &self.nodes {
            match &node.kind {
                GameplayNodeKind::GetVariable { name } | GameplayNodeKind::SetVariable { name }
                    if !self.variables.iter().any(|variable| variable.name == *name) =>
                {
                    problems.push(at(
                        node.id,
                        if name.is_empty() {
                            "pick a variable".into()
                        } else {
                            format!("there is no variable {name}")
                        },
                    ));
                }
                _ => {}
            }
        }
        // Data loops: a pure node whose inputs lead back to it.
        let pure: HashSet<u32> = self
            .nodes
            .iter()
            .filter(|node| !node.kind.is_flow())
            .map(|node| node.id)
            .collect();
        let mut state: HashMap<u32, u8> = HashMap::new();
        fn visit(
            id: u32,
            graph: &GameplayGraphData,
            pure: &HashSet<u32>,
            state: &mut HashMap<u32, u8>,
        ) -> Option<u32> {
            match state.get(&id) {
                Some(1) => return Some(id),
                Some(_) => return None,
                None => {}
            }
            state.insert(id, 1);
            for wire in graph.wires.iter().filter(|wire| wire.to == id) {
                if pure.contains(&wire.from)
                    && let Some(found) = visit(wire.from, graph, pure, state)
                {
                    return Some(found);
                }
            }
            state.insert(id, 2);
            None
        }
        for id in &pure {
            if let Some(found) = visit(*id, self, &pure, &mut state) {
                problems.push(at(found, "this value feeds back into itself".into()));
                break;
            }
        }
        problems
    }

    /// Parses and checks a graph file's text.
    ///
    /// # Errors
    ///
    /// [`SceneError::Resolve`] for text that is not a valid graph.
    pub fn from_ron(text: &str) -> Result<Self, SceneError> {
        let data: Self = ron::from_str(text)
            .map_err(|error| SceneError::Resolve(format!("gameplay graph: {error}")))?;
        data.validate()
            .map_err(|reason| SceneError::Resolve(format!("gameplay graph: {reason}")))?;
        Ok(data)
    }

    /// The file's text.
    ///
    /// # Errors
    ///
    /// [`SceneError::Resolve`] if it cannot be written as RON.
    pub fn to_ron(&self) -> Result<String, SceneError> {
        ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
            .map_err(|error| SceneError::Resolve(format!("gameplay graph: {error}")))
    }

    /// Reads a graph file.
    ///
    /// # Errors
    ///
    /// [`SceneError`] if it can't be read or isn't a valid graph.
    pub fn load(path: &Path) -> Result<Self, SceneError> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| SceneError::Resolve(format!("{}: {error}", path.display())))?;
        Self::from_ron(&text)
    }

    /// Writes a graph file, replacing it in one step.
    ///
    /// # Errors
    ///
    /// [`SceneError`] if it can't be written.
    pub fn save(&self, path: &Path) -> Result<(), SceneError> {
        crate::write_atomic(path, self.to_ron()?.as_bytes())
            .map_err(|error| SceneError::Resolve(format!("{}: {error}", path.display())))
    }
}

/// An object's gameplay graph, by asset ID; `None` until one is picked.
#[derive(bevy_ecs::component::Component, Debug, Clone, PartialEq)]
pub struct SceneGameplayGraph(pub Option<crate::AssetRef>);

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: u32, kind: GameplayNodeKind) -> GameplayNodeData {
        GameplayNodeData {
            id,
            kind,
            position: [0.0, 0.0],
            inputs: BTreeMap::new(),
        }
    }

    fn wire(from: u32, from_pin: u8, to: u32, to_pin: u8) -> GameplayWire {
        GameplayWire {
            from,
            from_pin,
            to,
            to_pin,
        }
    }

    #[test]
    fn every_node_has_pins_and_a_label() {
        for kind in GameplayNodeKind::catalogue() {
            assert!(!kind.label().is_empty());
            let pins = kind.inputs().len() + kind.outputs().len();
            assert!(pins > 0, "{} has no pins", kind.label());
            for (index, input) in kind.inputs().iter().enumerate() {
                if input.ty != PinType::Flow {
                    assert_eq!(kind.default_input(index, input.ty).pin_type(), input.ty);
                }
            }
        }
    }

    #[test]
    fn wires_need_matching_types_and_replace_their_input_or_flow_output() {
        let mut graph = GameplayGraphData::new();
        graph.nodes.extend([
            node(5, GameplayNodeKind::Log),
            node(6, GameplayNodeKind::Destroy),
            node(
                7,
                GameplayNodeKind::Value {
                    value: PinValue::Number(1.0),
                },
            ),
            node(
                8,
                GameplayNodeKind::Value {
                    value: PinValue::Text("hi".into()),
                },
            ),
        ]);
        assert!(
            graph.connect(wire(7, 0, 5, 1)).is_err(),
            "a number is not text"
        );
        assert_eq!(graph.connect(wire(8, 0, 5, 1)), Ok(true));
        assert_eq!(graph.connect(wire(0, 0, 5, 0)), Ok(true));
        // A flow output leads to one place.
        assert_eq!(graph.connect(wire(0, 0, 6, 0)), Ok(true));
        assert_eq!(
            graph.wires,
            vec![wire(8, 0, 5, 1), wire(0, 0, 6, 0)],
            "Start now leads to Destroy only"
        );
        // A flow input takes many.
        assert_eq!(graph.connect(wire(1, 0, 6, 0)), Ok(true));
        assert!(graph.problems().is_empty(), "{:?}", graph.problems());
        assert!(graph.remove(6));
        assert_eq!(graph.wires, vec![wire(8, 0, 5, 1)]);
    }

    #[test]
    fn problems_name_the_node() {
        let mut graph = GameplayGraphData::new();
        graph.nodes.extend([
            node(5, GameplayNodeKind::Add),
            node(6, GameplayNodeKind::Add),
            node(
                7,
                GameplayNodeKind::GetVariable {
                    name: "coins".into(),
                },
            ),
        ]);
        graph
            .wires
            .extend([wire(5, 0, 6, 0), wire(6, 0, 5, 0), wire(0, 0, 5, 0)]);
        let problems = graph.problems();
        assert!(
            problems.iter().any(|p| p.message.contains("feeds back")),
            "{problems:?}"
        );
        assert!(problems.iter().any(|p| p.node == Some(7)), "{problems:?}");
        assert!(
            problems
                .iter()
                .any(|p| p.message.contains("Flow is wired into")),
            "{problems:?}"
        );
    }

    #[test]
    fn variables_type_their_get_and_set_pins() {
        let mut graph = GameplayGraphData::new();
        graph.variables.push(GraphVariable {
            name: "target".into(),
            value: PinValue::Vector([1.0, 2.0, 3.0]),
        });
        let get = node(
            5,
            GameplayNodeKind::GetVariable {
                name: "target".into(),
            },
        );
        let set = node(
            6,
            GameplayNodeKind::SetVariable {
                name: "target".into(),
            },
        );
        assert_eq!(graph.outputs_of(&get)[0].ty, PinType::Vector);
        assert_eq!(graph.inputs_of(&set)[1].ty, PinType::Vector);
    }

    #[test]
    fn graphs_round_trip_and_bad_files_are_refused() {
        let mut graph = GameplayGraphData::new();
        let mut log = node(5, GameplayNodeKind::Log);
        log.inputs.insert(1, PinValue::Text("hello".into()));
        graph.nodes.push(log);
        graph.wires.push(wire(0, 0, 5, 0));
        graph.variables.push(GraphVariable {
            name: "coins".into(),
            value: PinValue::Number(0.0),
        });
        graph.comments.push(GraphComment {
            text: "Say hello".into(),
            position: [0.0, 0.0],
            size: [300.0, 120.0],
        });
        let text = graph.to_ron().expect("writes");
        assert_eq!(GameplayGraphData::from_ron(&text).expect("reads"), graph);

        graph.nodes.push(node(5, GameplayNodeKind::Start));
        assert!(graph.validate().is_err(), "two nodes share an ID");
        graph.nodes.pop();
        graph.nodes[2].inputs.insert(1, PinValue::Number(f64::NAN));
        assert!(graph.validate().is_err(), "not finite");
    }
}
