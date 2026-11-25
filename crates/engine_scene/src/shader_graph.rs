//! Shader graphs: a material made of nodes instead of fixed settings.
//!
//! A graph is a list of nodes joined by wires, ending in one Output node
//! whose inputs are the surface's base colour, metallic, roughness, normal,
//! glow, alpha and vertex offset. [`compile`] turns it into WGSL functions
//! (`material_surface` and `material_vertex_offset`) that the renderer
//! splices into its PBR shader; [`Evaluator`] runs the same graph on the
//! CPU for previews and tests, so both agree on what each node does.
//!
//! Pins carry a float or a 2-, 3- or 4-wide vector. A wire between pins of
//! different widths converts: a float spreads to every component, a wider
//! vector drops its last components, and a narrower one is padded with `0`
//! (and `1` for the fourth). Maths nodes take the width of their widest
//! input.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

/// Most nodes one graph may have.
pub const MAX_GRAPH_NODES: usize = 256;
/// Most named parameters one graph may expose.
pub const MAX_GRAPH_PARAMETERS: usize = 7;
/// The longest parameter name, in bytes.
pub const MAX_PARAMETER_NAME: usize = 32;

/// A shader graph as saved in a material file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ShaderGraphData {
    /// The nodes, each with an ID unique in the graph.
    #[serde(default)]
    pub nodes: Vec<ShaderNodeData>,
    /// The wires, each from an output pin to an input pin.
    #[serde(default)]
    pub wires: Vec<ShaderWire>,
}

/// One node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShaderNodeData {
    /// Unique in the graph; wires refer to it.
    pub id: u32,
    /// What it does.
    pub kind: ShaderNodeKind,
    /// Where it sits in the graph window.
    #[serde(default)]
    pub position: [f32; 2],
}

/// A wire from output pin `from_pin` of node `from` to input pin `to_pin`
/// of node `to`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShaderWire {
    /// The node the value comes from.
    pub from: u32,
    /// Which of its outputs.
    pub from_pin: u8,
    /// The node the value goes to.
    pub to: u32,
    /// Which of its inputs.
    pub to_pin: u8,
}

/// The images a graph can read: the material file's five image slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GraphImage {
    /// The Colour slot (read as a colour).
    Colour,
    /// The Glow slot (read as a colour).
    Glow,
    /// The Bumps slot (read as data).
    Bumps,
    /// The Shadowing slot (read as data).
    Shadowing,
    /// The Metal / rough slot (read as data).
    MetalRough,
}

impl GraphImage {
    /// Every slot, in the order the Inspector lists them.
    pub const ALL: [GraphImage; 5] = [
        GraphImage::Colour,
        GraphImage::Bumps,
        GraphImage::MetalRough,
        GraphImage::Shadowing,
        GraphImage::Glow,
    ];

    /// Its name in the editor.
    pub fn label(self) -> &'static str {
        match self {
            GraphImage::Colour => "Colour image",
            GraphImage::Glow => "Glow image",
            GraphImage::Bumps => "Bumps image",
            GraphImage::Shadowing => "Shadowing image",
            GraphImage::MetalRough => "Metal / rough image",
        }
    }

    /// The texture's name in the shader.
    fn texture(self) -> &'static str {
        match self {
            GraphImage::Colour => "base_color_texture",
            GraphImage::Glow => "emissive_map",
            GraphImage::Bumps => "normal_map",
            GraphImage::Shadowing => "occlusion_map",
            GraphImage::MetalRough => "metallic_roughness_map",
        }
    }
}

/// What a node does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ShaderNodeKind {
    /// The surface: every graph has exactly one.
    Output,
    /// A fixed number.
    Number {
        /// Its value.
        value: f32,
    },
    /// A fixed colour (linear RGBA).
    Colour {
        /// Its value.
        value: [f32; 4],
    },
    /// A fixed 3-wide vector.
    Vector {
        /// Its value.
        value: [f32; 3],
    },
    /// Seconds since the game (or Studio) started.
    Time,
    /// The surface's texture coordinates, after the material's repeat and
    /// shift.
    Uv,
    /// Where the surface is in the world.
    WorldPosition,
    /// The surface's normal in the world.
    Normal,
    /// From the surface towards the camera.
    ViewDirection,
    /// A colour read from one of the material's images.
    Texture {
        /// Which image.
        image: GraphImage,
    },
    /// `a + b`.
    Add,
    /// `a - b`.
    Subtract,
    /// `a * b`.
    Multiply,
    /// `a / b`.
    Divide,
    /// From `a` to `b` by `t`.
    Lerp,
    /// `x` kept between `min` and `max`.
    Clamp,
    /// `a` to the power `b`.
    Power,
    /// Sine.
    Sin,
    /// Cosine.
    Cos,
    /// Dot product.
    Dot,
    /// Length 1, same direction.
    Normalise,
    /// A vector's four components.
    Split,
    /// Four numbers as one vector.
    Combine,
    /// Bright at the surface's edges, seen from the camera.
    Fresnel,
    /// Smooth random values, `0` to `1`.
    Noise,
    /// `0` below `edge`, `1` from it.
    Step,
    /// From `0` at `edge 0` to `1` at `edge 1`, smoothly.
    Smoothstep,
    /// `x` moved from one range to another.
    Remap,
    /// A value the Inspector and scripts can set.
    Parameter {
        /// Its name, unique in the graph.
        name: String,
        /// Its value until something sets it.
        default: [f32; 4],
        /// A colour; otherwise a number (the first component).
        colour: bool,
    },
}

/// A pin's width: `1` is a float, `2`–`4` a vector.
pub type Width = u8;

/// A width decided by the inputs: the widest of them.
const ANY: Width = 0;

/// One input pin.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InputPin {
    /// Its name.
    pub name: &'static str,
    /// Its width, or [`ANY`] for maths inputs.
    pub width: Width,
    /// Its value when nothing is wired to it.
    pub default: [f32; 4],
}

const fn pin(name: &'static str, width: Width, default: [f32; 4]) -> InputPin {
    InputPin {
        name,
        width,
        default,
    }
}

const ZERO: [f32; 4] = [0.0; 4];
const ONE: [f32; 4] = [1.0; 4];

/// The Output node's inputs, in pin order.
pub const OUTPUT_PINS: [InputPin; 7] = [
    pin("Base colour", 3, [0.8, 0.8, 0.8, 1.0]),
    pin("Metallic", 1, ZERO),
    pin("Roughness", 1, [0.5; 4]),
    pin("Normal", 3, [0.0, 0.0, 1.0, 0.0]),
    pin("Glow", 3, ZERO),
    pin("Alpha", 1, ONE),
    pin("Vertex offset", 3, ZERO),
];
/// The Output node's vertex offset pin.
const VERTEX_OFFSET_PIN: usize = 6;

impl ShaderNodeKind {
    /// The name shown on the node and in the add menu.
    pub fn label(&self) -> String {
        match self {
            ShaderNodeKind::Output => "Surface".into(),
            ShaderNodeKind::Number { .. } => "Number".into(),
            ShaderNodeKind::Colour { .. } => "Colour".into(),
            ShaderNodeKind::Vector { .. } => "Vector".into(),
            ShaderNodeKind::Time => "Time".into(),
            ShaderNodeKind::Uv => "UV".into(),
            ShaderNodeKind::WorldPosition => "World position".into(),
            ShaderNodeKind::Normal => "Normal".into(),
            ShaderNodeKind::ViewDirection => "View direction".into(),
            ShaderNodeKind::Texture { image } => image.label().into(),
            ShaderNodeKind::Add => "Add".into(),
            ShaderNodeKind::Subtract => "Subtract".into(),
            ShaderNodeKind::Multiply => "Multiply".into(),
            ShaderNodeKind::Divide => "Divide".into(),
            ShaderNodeKind::Lerp => "Lerp".into(),
            ShaderNodeKind::Clamp => "Clamp".into(),
            ShaderNodeKind::Power => "Power".into(),
            ShaderNodeKind::Sin => "Sin".into(),
            ShaderNodeKind::Cos => "Cos".into(),
            ShaderNodeKind::Dot => "Dot".into(),
            ShaderNodeKind::Normalise => "Normalise".into(),
            ShaderNodeKind::Split => "Split".into(),
            ShaderNodeKind::Combine => "Combine".into(),
            ShaderNodeKind::Fresnel => "Fresnel".into(),
            ShaderNodeKind::Noise => "Noise".into(),
            ShaderNodeKind::Step => "Step".into(),
            ShaderNodeKind::Smoothstep => "Smoothstep".into(),
            ShaderNodeKind::Remap => "Remap".into(),
            ShaderNodeKind::Parameter { name, .. } => format!("Parameter: {name}"),
        }
    }

    /// The input pins.
    pub fn inputs(&self) -> Vec<InputPin> {
        let two = || vec![pin("A", ANY, ZERO), pin("B", ANY, ZERO)];
        match self {
            ShaderNodeKind::Output => OUTPUT_PINS.to_vec(),
            ShaderNodeKind::Number { .. }
            | ShaderNodeKind::Colour { .. }
            | ShaderNodeKind::Vector { .. }
            | ShaderNodeKind::Time
            | ShaderNodeKind::Uv
            | ShaderNodeKind::WorldPosition
            | ShaderNodeKind::Normal
            | ShaderNodeKind::ViewDirection
            | ShaderNodeKind::Parameter { .. } => Vec::new(),
            ShaderNodeKind::Texture { .. } => vec![pin("UV", 2, ZERO)],
            ShaderNodeKind::Add | ShaderNodeKind::Subtract | ShaderNodeKind::Dot => two(),
            ShaderNodeKind::Multiply | ShaderNodeKind::Divide | ShaderNodeKind::Power => {
                vec![pin("A", ANY, ONE), pin("B", ANY, ONE)]
            }
            ShaderNodeKind::Lerp => vec![
                pin("A", ANY, ZERO),
                pin("B", ANY, ONE),
                pin("T", ANY, [0.5; 4]),
            ],
            ShaderNodeKind::Clamp => vec![
                pin("X", ANY, ZERO),
                pin("Min", ANY, ZERO),
                pin("Max", ANY, ONE),
            ],
            ShaderNodeKind::Sin | ShaderNodeKind::Cos | ShaderNodeKind::Normalise => {
                vec![pin("X", ANY, ZERO)]
            }
            ShaderNodeKind::Split => vec![pin("Vector", 4, ZERO)],
            ShaderNodeKind::Combine => vec![
                pin("X", 1, ZERO),
                pin("Y", 1, ZERO),
                pin("Z", 1, ZERO),
                pin("W", 1, ONE),
            ],
            ShaderNodeKind::Fresnel => vec![pin("Power", 1, [5.0; 4])],
            ShaderNodeKind::Noise => vec![pin("Position", 3, ZERO), pin("Scale", 1, ONE)],
            ShaderNodeKind::Step => vec![pin("Edge", ANY, [0.5; 4]), pin("X", ANY, ZERO)],
            ShaderNodeKind::Smoothstep => vec![
                pin("Edge 0", ANY, ZERO),
                pin("Edge 1", ANY, ONE),
                pin("X", ANY, ZERO),
            ],
            ShaderNodeKind::Remap => vec![
                pin("X", ANY, ZERO),
                pin("From min", ANY, ZERO),
                pin("From max", ANY, ONE),
                pin("To min", ANY, ZERO),
                pin("To max", ANY, ONE),
            ],
        }
    }

    /// The output pins' names; their widths depend on the inputs (see
    /// [`compile`]).
    pub fn outputs(&self) -> Vec<&'static str> {
        match self {
            ShaderNodeKind::Output => Vec::new(),
            ShaderNodeKind::Split => vec!["X", "Y", "Z", "W"],
            ShaderNodeKind::Texture { .. } => vec!["Colour"],
            _ => vec!["Out"],
        }
    }

    /// Every node the add menu offers, with default settings.
    pub fn catalogue() -> Vec<ShaderNodeKind> {
        let mut kinds = vec![
            ShaderNodeKind::Number { value: 1.0 },
            ShaderNodeKind::Colour { value: ONE },
            ShaderNodeKind::Vector {
                value: [0.0, 1.0, 0.0],
            },
            ShaderNodeKind::Parameter {
                name: "Amount".into(),
                default: ONE,
                colour: false,
            },
            ShaderNodeKind::Parameter {
                name: "Tint".into(),
                default: ONE,
                colour: true,
            },
            ShaderNodeKind::Time,
            ShaderNodeKind::Uv,
            ShaderNodeKind::WorldPosition,
            ShaderNodeKind::Normal,
            ShaderNodeKind::ViewDirection,
        ];
        kinds.extend(
            GraphImage::ALL
                .into_iter()
                .map(|image| ShaderNodeKind::Texture { image }),
        );
        kinds.extend([
            ShaderNodeKind::Add,
            ShaderNodeKind::Subtract,
            ShaderNodeKind::Multiply,
            ShaderNodeKind::Divide,
            ShaderNodeKind::Lerp,
            ShaderNodeKind::Clamp,
            ShaderNodeKind::Power,
            ShaderNodeKind::Sin,
            ShaderNodeKind::Cos,
            ShaderNodeKind::Dot,
            ShaderNodeKind::Normalise,
            ShaderNodeKind::Split,
            ShaderNodeKind::Combine,
            ShaderNodeKind::Fresnel,
            ShaderNodeKind::Noise,
            ShaderNodeKind::Step,
            ShaderNodeKind::Smoothstep,
            ShaderNodeKind::Remap,
        ]);
        kinds
    }
}

impl ShaderGraphData {
    /// A graph holding only the Surface node.
    pub fn new() -> Self {
        Self {
            nodes: vec![ShaderNodeData {
                id: 0,
                kind: ShaderNodeKind::Output,
                position: [520.0, 80.0],
            }],
            wires: Vec::new(),
        }
    }

    /// The node with `id`.
    pub fn node(&self, id: u32) -> Option<&ShaderNodeData> {
        self.nodes.iter().find(|node| node.id == id)
    }

    /// An ID no node has yet.
    pub fn free_id(&self) -> u32 {
        self.nodes.iter().map(|node| node.id + 1).max().unwrap_or(0)
    }

    /// Adds a wire, replacing any wire already going into the same input.
    /// Returns whether the graph changed.
    pub fn connect(&mut self, wire: ShaderWire) -> bool {
        if self.wires.contains(&wire) || wire.from == wire.to {
            return false;
        }
        self.wires
            .retain(|other| !(other.to == wire.to && other.to_pin == wire.to_pin));
        self.wires.push(wire);
        true
    }

    /// Removes the node `id` and its wires. The Surface node stays.
    pub fn remove(&mut self, id: u32) -> bool {
        if self
            .node(id)
            .is_none_or(|node| node.kind == ShaderNodeKind::Output)
        {
            return false;
        }
        self.nodes.retain(|node| node.id != id);
        self.wires.retain(|wire| wire.from != id && wire.to != id);
        true
    }

    /// The named parameters, in the order they get their slots, with their
    /// defaults and whether each is a colour.
    pub fn parameters(&self) -> Vec<(String, [f32; 4], bool)> {
        let mut seen = HashSet::new();
        self.nodes
            .iter()
            .filter_map(|node| match &node.kind {
                ShaderNodeKind::Parameter {
                    name,
                    default,
                    colour,
                } if seen.insert(name.clone()) => Some((name.clone(), *default, *colour)),
                _ => None,
            })
            .collect()
    }

    /// Rejects graphs no file should hold: too many nodes or parameters,
    /// numbers that are not finite, bad names. A graph that passes may
    /// still fail to [`compile`] while it is being built.
    pub fn validate(&self) -> Result<(), String> {
        if self.nodes.len() > MAX_GRAPH_NODES {
            return Err(format!(
                "a shader graph has at most {MAX_GRAPH_NODES} nodes"
            ));
        }
        if self.wires.len() > MAX_GRAPH_NODES * 8 {
            return Err("the shader graph has too many wires".into());
        }
        let mut ids = HashSet::new();
        for node in &self.nodes {
            if !ids.insert(node.id) {
                return Err(format!("two shader graph nodes share ID {}", node.id));
            }
            if !node.position.iter().all(|value| value.is_finite()) {
                return Err("a shader graph node's position is not finite".into());
            }
            let finite = match &node.kind {
                ShaderNodeKind::Number { value } => value.is_finite(),
                ShaderNodeKind::Colour { value } => value.iter().all(|v| v.is_finite()),
                ShaderNodeKind::Vector { value } => value.iter().all(|v| v.is_finite()),
                ShaderNodeKind::Parameter { name, default, .. } => {
                    if name.trim().is_empty() || name.len() > MAX_PARAMETER_NAME {
                        return Err(format!(
                            "parameter names have 1 to {MAX_PARAMETER_NAME} bytes"
                        ));
                    }
                    default.iter().all(|v| v.is_finite())
                }
                _ => true,
            };
            if !finite {
                return Err(format!(
                    "{} holds a number that is not finite",
                    node.kind.label()
                ));
            }
        }
        if self.parameters().len() > MAX_GRAPH_PARAMETERS {
            return Err(format!(
                "a shader graph has at most {MAX_GRAPH_PARAMETERS} parameters"
            ));
        }
        Ok(())
    }
}

/// Why a graph does not compile, and the node to blame when there is one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphError {
    /// The node at fault.
    pub node: Option<u32>,
    /// What is wrong, for the user.
    pub message: String,
}

impl GraphError {
    fn at(node: u32, message: impl Into<String>) -> Self {
        Self {
            node: Some(node),
            message: message.into(),
        }
    }

    fn graph(message: impl Into<String>) -> Self {
        Self {
            node: None,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for GraphError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

/// A compiled graph.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledGraph {
    /// WGSL defining `material_surface(input: SurfaceInput) -> SurfaceOutput`
    /// and `material_vertex_offset(input: SurfaceInput) -> vec3<f32>`.
    pub source: String,
    /// The parameters: name, default and whether a colour. Parameter `i`
    /// reads `graph_params.values[i + 1]`; slot `0` holds the time.
    pub parameters: Vec<(String, [f32; 4], bool)>,
}

/// Which shader stage code is made for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Vertex,
    Fragment,
}

/// The graph's structure, checked: nodes by ID and the wire into each
/// input.
struct Checked<'a> {
    nodes: HashMap<u32, &'a ShaderNodeData>,
    inputs: HashMap<(u32, u8), (u32, u8)>,
    output: u32,
    parameters: Vec<(String, [f32; 4], bool)>,
}

fn check(graph: &ShaderGraphData) -> Result<Checked<'_>, GraphError> {
    graph.validate().map_err(GraphError::graph)?;
    let nodes: HashMap<u32, &ShaderNodeData> =
        graph.nodes.iter().map(|node| (node.id, node)).collect();
    let mut outputs = graph
        .nodes
        .iter()
        .filter(|node| node.kind == ShaderNodeKind::Output);
    let output = match (outputs.next(), outputs.next()) {
        (Some(node), None) => node.id,
        (None, _) => return Err(GraphError::graph("the graph has no Surface node")),
        (Some(_), Some(extra)) => {
            return Err(GraphError::at(extra.id, "a graph has one Surface node"));
        }
    };
    let mut inputs = HashMap::new();
    for wire in &graph.wires {
        let (Some(from), Some(to)) = (nodes.get(&wire.from), nodes.get(&wire.to)) else {
            return Err(GraphError::graph("a wire joins a node that is not there"));
        };
        if usize::from(wire.from_pin) >= from.kind.outputs().len() {
            return Err(GraphError::at(
                from.id,
                "a wire leaves a pin this node lacks",
            ));
        }
        if usize::from(wire.to_pin) >= to.kind.inputs().len() {
            return Err(GraphError::at(
                to.id,
                "a wire reaches a pin this node lacks",
            ));
        }
        if inputs
            .insert((wire.to, wire.to_pin), (wire.from, wire.from_pin))
            .is_some()
        {
            return Err(GraphError::at(to.id, "two wires go into one input"));
        }
    }
    Ok(Checked {
        nodes,
        inputs,
        output,
        parameters: graph.parameters(),
    })
}

/// A WGSL float literal.
fn float(value: f32) -> String {
    let text = format!("{value:?}");
    if text.contains('.') || text.contains('e') {
        text
    } else {
        format!("{text}.0")
    }
}

fn wgsl_type(width: Width) -> String {
    match width {
        1 => "f32".into(),
        n => format!("vec{n}<f32>"),
    }
}

/// `expr` of width `from` as width `to`.
fn convert(expr: &str, from: Width, to: Width) -> String {
    match (from, to) {
        (a, b) if a == b => expr.to_owned(),
        (1, n) => format!("vec{n}<f32>({expr})"),
        (_, 1) => format!("({expr}).x"),
        (4, 3) | (3, 2) | (4, 2) => {
            let swizzle = ["", "", "xy", "xyz"][usize::from(to)];
            format!("({expr}).{swizzle}")
        }
        (2, 3) => format!("vec3<f32>({expr}, 0.0)"),
        (2, 4) => format!("vec4<f32>({expr}, 0.0, 1.0)"),
        (3, 4) => format!("vec4<f32>({expr}, 1.0)"),
        _ => expr.to_owned(),
    }
}

fn literal(value: [f32; 4], width: Width) -> String {
    match width {
        1 => float(value[0]),
        n => {
            let parts: Vec<String> = value[..usize::from(n)].iter().map(|v| float(*v)).collect();
            format!("vec{n}<f32>({})", parts.join(", "))
        }
    }
}

/// A value in generated code: an expression and its width.
#[derive(Debug, Clone)]
struct Code {
    expr: String,
    width: Width,
}

/// Generates the code for one stage.
struct Writer<'a> {
    checked: &'a Checked<'a>,
    stage: Stage,
    lines: Vec<String>,
    /// Each node's outputs once written.
    done: HashMap<u32, Vec<Code>>,
    /// Nodes being written, to spot cycles.
    active: HashSet<u32>,
}

impl Writer<'_> {
    /// The value going into input `pin` of `node`: its wire's, or the
    /// default, with its own width (or the wire's, for maths inputs).
    fn input(&mut self, node: &ShaderNodeData, pin: usize) -> Result<Code, GraphError> {
        let pins = node.kind.inputs();
        let spec = pins[pin];
        let wired = self.checked.inputs.get(&(node.id, pin as u8)).copied();
        let value = match wired {
            Some((from, from_pin)) => {
                let outputs = self.node_outputs(from)?;
                outputs
                    .get(usize::from(from_pin))
                    .cloned()
                    .ok_or_else(|| GraphError::at(from, "this node has no such output"))?
            }
            None => self.default_input(node, pin, spec),
        };
        Ok(match spec.width {
            ANY => value,
            width => Code {
                expr: convert(&value.expr, value.width, width),
                width,
            },
        })
    }

    /// An input's value when nothing is wired to it.
    fn default_input(&self, node: &ShaderNodeData, pin: usize, spec: InputPin) -> Code {
        match (&node.kind, pin) {
            (ShaderNodeKind::Texture { .. }, 0) => Code {
                expr: "input.uv".into(),
                width: 2,
            },
            (ShaderNodeKind::Noise, 0) => Code {
                expr: "input.world_position".into(),
                width: 3,
            },
            _ => {
                let width = if spec.width == ANY { 1 } else { spec.width };
                Code {
                    expr: literal(spec.default, width),
                    width,
                }
            }
        }
    }

    /// Writes `id` (and what it needs) and returns its outputs.
    fn node_outputs(&mut self, id: u32) -> Result<Vec<Code>, GraphError> {
        if let Some(done) = self.done.get(&id) {
            return Ok(done.clone());
        }
        if !self.active.insert(id) {
            return Err(GraphError::at(id, "this node feeds back into itself"));
        }
        let node = *self
            .checked
            .nodes
            .get(&id)
            .ok_or_else(|| GraphError::graph("a wire joins a node that is not there"))?;
        let outputs = self.write_node(node)?;
        self.active.remove(&id);
        self.done.insert(id, outputs.clone());
        Ok(outputs)
    }

    /// Binds `expr` to a new name and returns it.
    fn bind(&mut self, id: u32, pin: usize, expr: String, width: Width) -> Code {
        let name = format!("n{id}_{pin}");
        self.lines
            .push(format!("    let {name}: {} = {expr};", wgsl_type(width)));
        Code { expr: name, width }
    }

    /// The inputs of `node`, all as the widest of them.
    fn widest(&mut self, node: &ShaderNodeData) -> Result<(Vec<String>, Width), GraphError> {
        let count = node.kind.inputs().len();
        let values: Vec<Code> = (0..count)
            .map(|pin| self.input(node, pin))
            .collect::<Result<_, _>>()?;
        let width = values.iter().map(|value| value.width).max().unwrap_or(1);
        Ok((
            values
                .iter()
                .map(|value| convert(&value.expr, value.width, width))
                .collect(),
            width,
        ))
    }

    fn write_node(&mut self, node: &ShaderNodeData) -> Result<Vec<Code>, GraphError> {
        let id = node.id;
        let one = |this: &mut Self, expr: String, width: Width| vec![this.bind(id, 0, expr, width)];
        Ok(match &node.kind {
            ShaderNodeKind::Output => {
                return Err(GraphError::at(id, "the Surface node has no outputs"));
            }
            ShaderNodeKind::Number { value } => one(self, float(*value), 1),
            ShaderNodeKind::Colour { value } => one(self, literal(*value, 4), 4),
            ShaderNodeKind::Vector { value } => {
                one(self, literal([value[0], value[1], value[2], 0.0], 3), 3)
            }
            ShaderNodeKind::Time => one(self, "input.time".into(), 1),
            ShaderNodeKind::Uv => one(self, "input.uv".into(), 2),
            ShaderNodeKind::WorldPosition => one(self, "input.world_position".into(), 3),
            ShaderNodeKind::Normal => one(self, "input.normal".into(), 3),
            ShaderNodeKind::ViewDirection => one(self, "input.view_direction".into(), 3),
            ShaderNodeKind::Texture { image } => {
                let uv = self.input(node, 0)?;
                let texture = image.texture();
                let expr = match self.stage {
                    Stage::Fragment => {
                        format!("textureSample({texture}, base_color_sampler, {})", uv.expr)
                    }
                    Stage::Vertex => format!(
                        "textureSampleLevel({texture}, base_color_sampler, {}, 0.0)",
                        uv.expr
                    ),
                };
                one(self, expr, 4)
            }
            ShaderNodeKind::Add
            | ShaderNodeKind::Subtract
            | ShaderNodeKind::Multiply
            | ShaderNodeKind::Divide => {
                let (values, width) = self.widest(node)?;
                let operator = match node.kind {
                    ShaderNodeKind::Add => "+",
                    ShaderNodeKind::Subtract => "-",
                    ShaderNodeKind::Multiply => "*",
                    _ => "/",
                };
                one(
                    self,
                    format!("({} {operator} {})", values[0], values[1]),
                    width,
                )
            }
            ShaderNodeKind::Lerp => {
                let (values, width) = self.widest(node)?;
                one(
                    self,
                    format!("mix({}, {}, {})", values[0], values[1], values[2]),
                    width,
                )
            }
            ShaderNodeKind::Clamp => {
                let (values, width) = self.widest(node)?;
                one(
                    self,
                    format!("clamp({}, {}, {})", values[0], values[1], values[2]),
                    width,
                )
            }
            ShaderNodeKind::Power => {
                let (values, width) = self.widest(node)?;
                one(self, format!("pow({}, {})", values[0], values[1]), width)
            }
            ShaderNodeKind::Sin | ShaderNodeKind::Cos => {
                let value = self.input(node, 0)?;
                let function = if node.kind == ShaderNodeKind::Sin {
                    "sin"
                } else {
                    "cos"
                };
                one(self, format!("{function}({})", value.expr), value.width)
            }
            ShaderNodeKind::Dot => {
                let (values, width) = self.widest(node)?;
                let expr = if width == 1 {
                    format!("({} * {})", values[0], values[1])
                } else {
                    format!("dot({}, {})", values[0], values[1])
                };
                one(self, expr, 1)
            }
            ShaderNodeKind::Normalise => {
                let value = self.input(node, 0)?;
                let expr = if value.width == 1 {
                    format!("sign({})", value.expr)
                } else {
                    format!("graph_normalise{}({})", value.width, value.expr)
                };
                one(self, expr, value.width)
            }
            ShaderNodeKind::Split => {
                let value = self.input(node, 0)?;
                let vector = self.bind(id, 9, value.expr, 4);
                ["x", "y", "z", "w"]
                    .iter()
                    .map(|part| Code {
                        expr: format!("{}.{part}", vector.expr),
                        width: 1,
                    })
                    .collect()
            }
            ShaderNodeKind::Combine => {
                let parts: Vec<String> = (0..4)
                    .map(|pin| self.input(node, pin).map(|value| value.expr))
                    .collect::<Result<_, _>>()?;
                one(self, format!("vec4<f32>({})", parts.join(", ")), 4)
            }
            ShaderNodeKind::Fresnel => {
                let power = self.input(node, 0)?;
                one(
                    self,
                    format!(
                        "pow(1.0 - clamp(dot(input.normal, input.view_direction), 0.0, 1.0), max({}, 0.0))",
                        power.expr
                    ),
                    1,
                )
            }
            ShaderNodeKind::Noise => {
                let position = self.input(node, 0)?;
                let scale = self.input(node, 1)?;
                one(
                    self,
                    format!("graph_noise({} * {})", position.expr, scale.expr),
                    1,
                )
            }
            ShaderNodeKind::Step => {
                let (values, width) = self.widest(node)?;
                one(self, format!("step({}, {})", values[0], values[1]), width)
            }
            ShaderNodeKind::Smoothstep => {
                let (values, width) = self.widest(node)?;
                one(
                    self,
                    format!("smoothstep({}, {}, {})", values[0], values[1], values[2]),
                    width,
                )
            }
            ShaderNodeKind::Remap => {
                let (v, width) = self.widest(node)?;
                one(
                    self,
                    format!(
                        "mix({}, {}, ({} - {}) / ({} - {}))",
                        v[3], v[4], v[0], v[1], v[2], v[1]
                    ),
                    width,
                )
            }
            ShaderNodeKind::Parameter { name, colour, .. } => {
                let slot = self
                    .checked
                    .parameters
                    .iter()
                    .position(|(other, ..)| other == name)
                    .ok_or_else(|| GraphError::at(id, "this parameter has no slot"))?
                    + 1;
                if *colour {
                    one(self, format!("graph_params.values[{slot}]"), 4)
                } else {
                    one(self, format!("graph_params.values[{slot}].x"), 1)
                }
            }
        })
    }
}

/// Compiles `graph` into WGSL for the renderer's graph template.
///
/// # Errors
///
/// [`GraphError`] naming the node at fault: a cycle, two Surface nodes,
/// a wire to a missing pin, too many parameters…
pub fn compile(graph: &ShaderGraphData) -> Result<CompiledGraph, GraphError> {
    let checked = check(graph)?;
    let output = *checked
        .nodes
        .get(&checked.output)
        .ok_or_else(|| GraphError::graph("the graph has no Surface node"))?;

    let mut fragment = Writer {
        checked: &checked,
        stage: Stage::Fragment,
        lines: Vec::new(),
        done: HashMap::new(),
        active: HashSet::new(),
    };
    let surface: Vec<Code> = (0..VERTEX_OFFSET_PIN)
        .map(|pin| fragment.input(output, pin))
        .collect::<Result<_, _>>()?;
    let mut vertex = Writer {
        checked: &checked,
        stage: Stage::Vertex,
        lines: Vec::new(),
        done: HashMap::new(),
        active: HashSet::new(),
    };
    let offset = vertex.input(output, VERTEX_OFFSET_PIN)?;

    let mut source = String::from("fn material_surface(input: SurfaceInput) -> SurfaceOutput {\n");
    for line in &fragment.lines {
        source.push_str(line);
        source.push('\n');
    }
    source.push_str("    var out: SurfaceOutput;\n");
    for (field, value) in [
        "base_color",
        "metallic",
        "roughness",
        "normal",
        "emissive",
        "alpha",
    ]
    .iter()
    .zip(&surface)
    {
        source.push_str(&format!("    out.{field} = {};\n", value.expr));
    }
    source.push_str("    return out;\n}\n\n");
    source.push_str("fn material_vertex_offset(input: SurfaceInput) -> vec3<f32> {\n");
    for line in &vertex.lines {
        source.push_str(line);
        source.push('\n');
    }
    source.push_str(&format!("    return {};\n}}\n", offset.expr));
    Ok(CompiledGraph {
        source,
        parameters: checked.parameters,
    })
}

/// What a graph's nodes see at one point of the surface.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SurfacePoint {
    /// Texture coordinates.
    pub uv: [f32; 2],
    /// World position.
    pub world_position: [f32; 3],
    /// World normal, length 1.
    pub normal: [f32; 3],
    /// Towards the camera, length 1.
    pub view_direction: [f32; 3],
    /// Seconds.
    pub time: f32,
}

/// What a graph makes of one point of the surface.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SurfaceValues {
    /// Base colour (linear RGB).
    pub base_color: [f32; 3],
    /// Metallic.
    pub metallic: f32,
    /// Roughness.
    pub roughness: f32,
    /// Normal in the surface's own space (`0, 0, 1` is flat).
    pub normal: [f32; 3],
    /// Glow (linear RGB).
    pub emissive: [f32; 3],
    /// Alpha.
    pub alpha: f32,
    /// Vertex offset.
    pub vertex_offset: [f32; 3],
}

/// A value on the CPU: up to four components and its width.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Value {
    v: [f32; 4],
    width: Width,
}

impl Value {
    fn new(v: [f32; 4], width: Width) -> Self {
        Self { v, width }
    }

    /// As `width`, converting like the generated code.
    fn as_width(self, width: Width) -> Value {
        let mut v = self.v;
        match (self.width, width) {
            (a, b) if a == b => {}
            (1, _) => v = [self.v[0]; 4],
            (from, to) if from > to => {}
            (2, 3) => v[2] = 0.0,
            (2, 4) => {
                v[2] = 0.0;
                v[3] = 1.0;
            }
            (3, 4) => v[3] = 1.0,
            _ => {}
        }
        let mut out = Value::new(v, width);
        for slot in usize::from(width)..4 {
            out.v[slot] = 0.0;
        }
        out
    }

    fn map(self, f: impl Fn(f32) -> f32) -> Value {
        let mut out = self;
        for slot in 0..usize::from(self.width) {
            out.v[slot] = f(self.v[slot]);
        }
        out
    }
}

/// A random value in `0..1` for a lattice point, matching `graph_hash` in
/// [`engine_renderer::GRAPH_HELPERS`].
fn hash(p: [f32; 3]) -> f32 {
    let fract = |x: f32| x - x.floor();
    let q = [
        fract(p[0] * std::f32::consts::FRAC_1_PI + 0.1) * 17.0,
        fract(p[1] * std::f32::consts::FRAC_1_PI + 0.2) * 17.0,
        fract(p[2] * std::f32::consts::FRAC_1_PI + 0.3) * 17.0,
    ];
    fract(q[0] * q[1] * q[2] * (q[0] + q[1] + q[2]))
}

/// Smooth value noise in `0..1`, matching `graph_noise` in
/// [`engine_renderer::GRAPH_HELPERS`].
pub fn value_noise(p: [f32; 3]) -> f32 {
    let i = p.map(f32::floor);
    let f = [p[0] - i[0], p[1] - i[1], p[2] - i[2]];
    let u = f.map(|t| t * t * (3.0 - 2.0 * t));
    let at = |dx: f32, dy: f32, dz: f32| hash([i[0] + dx, i[1] + dy, i[2] + dz]);
    let mix = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let low = mix(
        mix(at(0.0, 0.0, 0.0), at(1.0, 0.0, 0.0), u[0]),
        mix(at(0.0, 1.0, 0.0), at(1.0, 1.0, 0.0), u[0]),
        u[1],
    );
    let high = mix(
        mix(at(0.0, 0.0, 1.0), at(1.0, 0.0, 1.0), u[0]),
        mix(at(0.0, 1.0, 1.0), at(1.0, 1.0, 1.0), u[0]),
        u[1],
    );
    mix(low, high, u[2])
}

/// Runs a graph on the CPU, for previews and tests. Images are read
/// through a callback; parameters take their defaults unless set.
pub struct Evaluator<'a> {
    checked: Checked<'a>,
    values: BTreeMap<String, [f32; 4]>,
}

impl<'a> Evaluator<'a> {
    /// Prepares `graph`.
    ///
    /// # Errors
    ///
    /// The same [`GraphError`]s as [`compile`].
    pub fn new(graph: &'a ShaderGraphData) -> Result<Self, GraphError> {
        compile(graph)?;
        let checked = check(graph)?;
        let values = checked
            .parameters
            .iter()
            .map(|(name, default, _)| (name.clone(), *default))
            .collect();
        Ok(Self { checked, values })
    }

    /// Sets parameter `name`, if the graph has it.
    pub fn set(&mut self, name: &str, value: [f32; 4]) {
        if let Some(slot) = self.values.get_mut(name) {
            *slot = value;
        }
    }

    /// The surface at `point`; `image(slot, uv)` reads an image in linear
    /// RGBA.
    pub fn evaluate(
        &self,
        point: &SurfacePoint,
        image: &dyn Fn(GraphImage, [f32; 2]) -> [f32; 4],
    ) -> SurfaceValues {
        let mut cache = HashMap::new();
        let mut input = |pin: usize| {
            let spec = OUTPUT_PINS[pin];
            self.input(self.checked.output, pin, spec, point, image, &mut cache)
                .as_width(spec.width)
                .v
        };
        let base = input(0);
        let normal = input(3);
        let emissive = input(4);
        let offset = input(6);
        SurfaceValues {
            base_color: [base[0], base[1], base[2]],
            metallic: input(1)[0],
            roughness: input(2)[0],
            normal: [normal[0], normal[1], normal[2]],
            emissive: [emissive[0], emissive[1], emissive[2]],
            alpha: input(5)[0],
            vertex_offset: [offset[0], offset[1], offset[2]],
        }
    }

    fn input(
        &self,
        node: u32,
        pin: usize,
        spec: InputPin,
        point: &SurfacePoint,
        image: &dyn Fn(GraphImage, [f32; 2]) -> [f32; 4],
        cache: &mut HashMap<u32, Vec<Value>>,
    ) -> Value {
        let value = match self.checked.inputs.get(&(node, pin as u8)) {
            Some(&(from, from_pin)) => self
                .outputs(from, point, image, cache)
                .get(usize::from(from_pin))
                .copied()
                .unwrap_or(Value::new([0.0; 4], 1)),
            None => {
                let kind = self.checked.nodes.get(&node).map(|node| &node.kind);
                match (kind, pin) {
                    (Some(ShaderNodeKind::Texture { .. }), 0) => {
                        Value::new([point.uv[0], point.uv[1], 0.0, 0.0], 2)
                    }
                    (Some(ShaderNodeKind::Noise), 0) => {
                        let p = point.world_position;
                        Value::new([p[0], p[1], p[2], 0.0], 3)
                    }
                    _ => Value::new(spec.default, if spec.width == ANY { 1 } else { spec.width })
                        .as_width(if spec.width == ANY { 1 } else { spec.width }),
                }
            }
        };
        if spec.width == ANY {
            value
        } else {
            value.as_width(spec.width)
        }
    }

    fn outputs(
        &self,
        id: u32,
        point: &SurfacePoint,
        image: &dyn Fn(GraphImage, [f32; 2]) -> [f32; 4],
        cache: &mut HashMap<u32, Vec<Value>>,
    ) -> Vec<Value> {
        if let Some(done) = cache.get(&id) {
            return done.clone();
        }
        let Some(node) = self.checked.nodes.get(&id).copied() else {
            return Vec::new();
        };
        let pins = node.kind.inputs();
        let inputs: Vec<Value> = pins
            .iter()
            .enumerate()
            .map(|(pin, spec)| self.input(id, pin, *spec, point, image, cache))
            .collect();
        let widest = || {
            let width = inputs.iter().map(|value| value.width).max().unwrap_or(1);
            (
                inputs
                    .iter()
                    .map(|value| value.as_width(width))
                    .collect::<Vec<_>>(),
                width,
            )
        };
        let zip = |a: Value, b: Value, f: &dyn Fn(f32, f32) -> f32| {
            let mut out = a;
            for slot in 0..usize::from(a.width) {
                out.v[slot] = f(a.v[slot], b.v[slot]);
            }
            out
        };
        let vec3 = |p: [f32; 3]| Value::new([p[0], p[1], p[2], 0.0], 3);
        let out = match &node.kind {
            ShaderNodeKind::Output => Vec::new(),
            ShaderNodeKind::Number { value } => vec![Value::new([*value, 0.0, 0.0, 0.0], 1)],
            ShaderNodeKind::Colour { value } => vec![Value::new(*value, 4)],
            ShaderNodeKind::Vector { value } => vec![vec3(*value)],
            ShaderNodeKind::Time => vec![Value::new([point.time, 0.0, 0.0, 0.0], 1)],
            ShaderNodeKind::Uv => vec![Value::new([point.uv[0], point.uv[1], 0.0, 0.0], 2)],
            ShaderNodeKind::WorldPosition => vec![vec3(point.world_position)],
            ShaderNodeKind::Normal => vec![vec3(point.normal)],
            ShaderNodeKind::ViewDirection => vec![vec3(point.view_direction)],
            ShaderNodeKind::Texture { image: slot } => {
                vec![Value::new(
                    image(*slot, [inputs[0].v[0], inputs[0].v[1]]),
                    4,
                )]
            }
            ShaderNodeKind::Add => {
                let (v, _) = widest();
                vec![zip(v[0], v[1], &|a, b| a + b)]
            }
            ShaderNodeKind::Subtract => {
                let (v, _) = widest();
                vec![zip(v[0], v[1], &|a, b| a - b)]
            }
            ShaderNodeKind::Multiply => {
                let (v, _) = widest();
                vec![zip(v[0], v[1], &|a, b| a * b)]
            }
            ShaderNodeKind::Divide => {
                let (v, _) = widest();
                vec![zip(v[0], v[1], &|a, b| a / b)]
            }
            ShaderNodeKind::Power => {
                let (v, _) = widest();
                vec![zip(v[0], v[1], &f32::powf)]
            }
            ShaderNodeKind::Lerp => {
                let (v, _) = widest();
                let t = v[2];
                let mut out = zip(v[0], v[1], &|a, b| a - b);
                for slot in 0..usize::from(out.width) {
                    out.v[slot] = v[0].v[slot] + (v[1].v[slot] - v[0].v[slot]) * t.v[slot];
                }
                vec![out]
            }
            ShaderNodeKind::Clamp => {
                let (v, _) = widest();
                let mut out = v[0];
                for slot in 0..usize::from(out.width) {
                    out.v[slot] = v[0].v[slot].max(v[1].v[slot]).min(v[2].v[slot]);
                }
                vec![out]
            }
            ShaderNodeKind::Sin => vec![inputs[0].map(f32::sin)],
            ShaderNodeKind::Cos => vec![inputs[0].map(f32::cos)],
            ShaderNodeKind::Dot => {
                let (v, width) = widest();
                let dot: f32 = (0..usize::from(width)).map(|s| v[0].v[s] * v[1].v[s]).sum();
                vec![Value::new([dot, 0.0, 0.0, 0.0], 1)]
            }
            ShaderNodeKind::Normalise => {
                let value = inputs[0];
                if value.width == 1 {
                    vec![value.map(f32::signum)]
                } else {
                    let length = (0..usize::from(value.width))
                        .map(|s| value.v[s] * value.v[s])
                        .sum::<f32>()
                        .sqrt();
                    if length < 1e-6 {
                        vec![value]
                    } else {
                        vec![value.map(|x| x / length)]
                    }
                }
            }
            ShaderNodeKind::Split => (0..4)
                .map(|slot| Value::new([inputs[0].v[slot], 0.0, 0.0, 0.0], 1))
                .collect(),
            ShaderNodeKind::Combine => vec![Value::new(
                [
                    inputs[0].v[0],
                    inputs[1].v[0],
                    inputs[2].v[0],
                    inputs[3].v[0],
                ],
                4,
            )],
            ShaderNodeKind::Fresnel => {
                let n = point.normal;
                let d = point.view_direction;
                let facing = (n[0] * d[0] + n[1] * d[1] + n[2] * d[2]).clamp(0.0, 1.0);
                vec![Value::new(
                    [(1.0 - facing).powf(inputs[0].v[0].max(0.0)), 0.0, 0.0, 0.0],
                    1,
                )]
            }
            ShaderNodeKind::Noise => {
                let p = inputs[0].v;
                let scale = inputs[1].v[0];
                vec![Value::new(
                    [
                        value_noise([p[0] * scale, p[1] * scale, p[2] * scale]),
                        0.0,
                        0.0,
                        0.0,
                    ],
                    1,
                )]
            }
            ShaderNodeKind::Step => {
                let (v, _) = widest();
                vec![zip(v[0], v[1], &|edge, x| if x < edge { 0.0 } else { 1.0 })]
            }
            ShaderNodeKind::Smoothstep => {
                let (v, _) = widest();
                let mut out = v[2];
                for slot in 0..usize::from(out.width) {
                    let (e0, e1, x) = (v[0].v[slot], v[1].v[slot], v[2].v[slot]);
                    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
                    out.v[slot] = t * t * (3.0 - 2.0 * t);
                }
                vec![out]
            }
            ShaderNodeKind::Remap => {
                let (v, _) = widest();
                let mut out = v[0];
                for slot in 0..usize::from(out.width) {
                    let t = (v[0].v[slot] - v[1].v[slot]) / (v[2].v[slot] - v[1].v[slot]);
                    out.v[slot] = v[3].v[slot] + (v[4].v[slot] - v[3].v[slot]) * t;
                }
                vec![out]
            }
            ShaderNodeKind::Parameter { name, colour, .. } => {
                let value = self.values.get(name).copied().unwrap_or([0.0; 4]);
                if *colour {
                    vec![Value::new(value, 4)]
                } else {
                    vec![Value::new([value[0], 0.0, 0.0, 0.0], 1)]
                }
            }
        };
        cache.insert(id, out.clone());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: u32, kind: ShaderNodeKind) -> ShaderNodeData {
        ShaderNodeData {
            id,
            kind,
            position: [0.0, 0.0],
        }
    }

    fn wire(from: u32, to: u32, to_pin: u8) -> ShaderWire {
        ShaderWire {
            from,
            from_pin: 0,
            to,
            to_pin,
        }
    }

    fn point() -> SurfacePoint {
        SurfacePoint {
            uv: [0.25, 0.75],
            world_position: [1.0, 2.0, 3.0],
            normal: [0.0, 0.0, 1.0],
            view_direction: [0.0, 0.0, 1.0],
            time: 2.0,
        }
    }

    /// The template the renderer wraps generated code in, reduced to what
    /// naga needs to check it.
    fn wrapped(source: &str) -> String {
        let helpers = engine_renderer::GRAPH_HELPERS;
        format!(
            r#"
@group(1) @binding(0) var base_color_texture: texture_2d<f32>;
@group(1) @binding(1) var base_color_sampler: sampler;
@group(1) @binding(3) var normal_map: texture_2d<f32>;
@group(1) @binding(4) var emissive_map: texture_2d<f32>;
@group(1) @binding(5) var occlusion_map: texture_2d<f32>;
@group(1) @binding(6) var metallic_roughness_map: texture_2d<f32>;
struct GraphParams {{ values: array<vec4<f32>, 8>, }};
@group(1) @binding(7) var<uniform> graph_params: GraphParams;
struct SurfaceInput {{
    uv: vec2<f32>,
    world_position: vec3<f32>,
    normal: vec3<f32>,
    view_direction: vec3<f32>,
    time: f32,
}};
struct SurfaceOutput {{
    base_color: vec3<f32>,
    metallic: f32,
    roughness: f32,
    normal: vec3<f32>,
    emissive: vec3<f32>,
    alpha: f32,
}};
{helpers}
{source}
@fragment
fn fs_main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {{
    let s = material_surface(SurfaceInput(uv, vec3<f32>(0.0), vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(0.0, 0.0, 1.0), 0.0));
    return vec4<f32>(s.base_color + s.emissive + s.normal * s.metallic * s.roughness, s.alpha);
}}
@vertex
fn vs_main(@location(0) uv: vec2<f32>) -> @builtin(position) vec4<f32> {{
    let o = material_vertex_offset(SurfaceInput(uv, vec3<f32>(0.0), vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(0.0, 0.0, 1.0), 0.0));
    return vec4<f32>(o, 1.0);
}}
"#
        )
    }

    fn naga_check(source: &str) {
        let module = naga::front::wgsl::parse_str(&wrapped(source))
            .unwrap_or_else(|error| panic!("{error}\n{source}"));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|error| panic!("{error:?}\n{source}"));
    }

    #[test]
    fn a_new_graph_compiles_to_valid_wgsl_with_the_defaults() {
        let graph = ShaderGraphData::new();
        let compiled = compile(&graph).expect("compiles");
        assert!(compiled.source.contains("fn material_surface"));
        assert!(compiled.parameters.is_empty());
        naga_check(&compiled.source);
        let values = Evaluator::new(&graph)
            .expect("ready")
            .evaluate(&point(), &|_, _| [1.0; 4]);
        assert_eq!(values.base_color, [0.8, 0.8, 0.8]);
        assert_eq!(values.roughness, 0.5);
        assert_eq!(values.normal, [0.0, 0.0, 1.0]);
    }

    #[test]
    fn every_node_compiles_to_valid_wgsl_into_every_output() {
        for kind in ShaderNodeKind::catalogue() {
            for pin in 0..OUTPUT_PINS.len() as u8 {
                let mut graph = ShaderGraphData::new();
                graph.nodes.push(node(1, kind.clone()));
                for (input, _) in kind.inputs().iter().enumerate() {
                    let id = 10 + input as u32;
                    graph.nodes.push(node(id, ShaderNodeKind::Uv));
                    graph.wires.push(wire(id, 1, input as u8));
                }
                for output in 0..kind.outputs().len() as u8 {
                    if output == 0 {
                        graph.wires.push(ShaderWire {
                            from: 1,
                            from_pin: output,
                            to: 0,
                            to_pin: pin,
                        });
                    }
                }
                let compiled = compile(&graph)
                    .unwrap_or_else(|error| panic!("{} into pin {pin}: {error}", kind.label()));
                naga_check(&compiled.source);
                Evaluator::new(&graph)
                    .expect("ready")
                    .evaluate(&point(), &|_, _| [0.5; 4]);
            }
        }
    }

    #[test]
    fn maths_takes_the_widest_input_and_converts_the_rest() {
        let mut graph = ShaderGraphData::new();
        graph.nodes.extend([
            node(
                1,
                ShaderNodeKind::Colour {
                    value: [0.5, 0.25, 1.0, 1.0],
                },
            ),
            node(2, ShaderNodeKind::Number { value: 2.0 }),
            node(3, ShaderNodeKind::Multiply),
        ]);
        graph
            .wires
            .extend([wire(1, 3, 0), wire(2, 3, 1), wire(3, 0, 0)]);
        let compiled = compile(&graph).expect("compiles");
        assert!(
            compiled.source.contains("vec4<f32>(n2_0)"),
            "{}",
            compiled.source
        );
        naga_check(&compiled.source);
        let values = Evaluator::new(&graph)
            .expect("ready")
            .evaluate(&point(), &|_, _| [1.0; 4]);
        assert_eq!(values.base_color, [1.0, 0.5, 2.0]);
    }

    #[test]
    fn cycles_double_wires_and_bad_pins_are_reported_on_the_node() {
        let mut graph = ShaderGraphData::new();
        graph
            .nodes
            .extend([node(1, ShaderNodeKind::Add), node(2, ShaderNodeKind::Add)]);
        graph
            .wires
            .extend([wire(1, 2, 0), wire(2, 1, 0), wire(2, 0, 0)]);
        let error = compile(&graph).expect_err("a cycle");
        assert!(error.message.contains("feeds back"), "{error}");
        assert!(matches!(error.node, Some(1 | 2)));

        let mut graph = ShaderGraphData::new();
        graph.nodes.push(node(1, ShaderNodeKind::Time));
        graph.wires.push(ShaderWire {
            from: 1,
            from_pin: 3,
            to: 0,
            to_pin: 0,
        });
        assert_eq!(compile(&graph).expect_err("no such pin").node, Some(1));

        let mut graph = ShaderGraphData::new();
        graph.nodes.push(node(1, ShaderNodeKind::Output));
        assert!(compile(&graph).is_err(), "two Surface nodes");
        assert!(
            compile(&ShaderGraphData::default()).is_err(),
            "no Surface node"
        );
    }

    #[test]
    fn connect_replaces_the_wire_into_an_input_and_remove_keeps_the_surface() {
        let mut graph = ShaderGraphData::new();
        graph
            .nodes
            .extend([node(1, ShaderNodeKind::Time), node(2, ShaderNodeKind::Uv)]);
        assert!(graph.connect(wire(1, 0, 1)));
        assert!(graph.connect(wire(2, 0, 1)));
        assert_eq!(graph.wires, vec![wire(2, 0, 1)]);
        assert!(!graph.connect(wire(2, 2, 0)), "not into itself");
        assert!(!graph.remove(0), "the Surface node stays");
        assert!(graph.remove(2));
        assert!(graph.wires.is_empty());
        assert_eq!(graph.free_id(), 2);
    }

    #[test]
    fn parameters_get_slots_and_can_be_set() {
        let mut graph = ShaderGraphData::new();
        graph.nodes.extend([
            node(
                1,
                ShaderNodeKind::Parameter {
                    name: "Tint".into(),
                    default: [1.0, 0.0, 0.0, 1.0],
                    colour: true,
                },
            ),
            node(
                2,
                ShaderNodeKind::Parameter {
                    name: "Shine".into(),
                    default: [0.25; 4],
                    colour: false,
                },
            ),
        ]);
        graph.wires.extend([wire(1, 0, 0), wire(2, 0, 1)]);
        let compiled = compile(&graph).expect("compiles");
        assert_eq!(compiled.parameters.len(), 2);
        assert!(compiled.source.contains("graph_params.values[1]"));
        assert!(compiled.source.contains("graph_params.values[2].x"));
        naga_check(&compiled.source);
        let mut evaluator = Evaluator::new(&graph).expect("ready");
        assert_eq!(
            evaluator.evaluate(&point(), &|_, _| [1.0; 4]).metallic,
            0.25
        );
        evaluator.set("Tint", [0.0, 1.0, 0.0, 1.0]);
        assert_eq!(
            evaluator.evaluate(&point(), &|_, _| [1.0; 4]).base_color,
            [0.0, 1.0, 0.0]
        );

        for index in 0..=MAX_GRAPH_PARAMETERS as u32 {
            graph.nodes.push(node(
                10 + index,
                ShaderNodeKind::Parameter {
                    name: format!("P{index}"),
                    default: [0.0; 4],
                    colour: false,
                },
            ));
        }
        assert!(graph.validate().is_err(), "too many parameters");
    }

    #[test]
    fn textures_noise_and_fresnel_agree_with_their_definitions() {
        let mut graph = ShaderGraphData::new();
        graph.nodes.extend([
            node(
                1,
                ShaderNodeKind::Texture {
                    image: GraphImage::Colour,
                },
            ),
            node(2, ShaderNodeKind::Noise),
            node(3, ShaderNodeKind::Fresnel),
        ]);
        graph
            .wires
            .extend([wire(1, 0, 0), wire(2, 0, 1), wire(3, 0, 2)]);
        let evaluator = Evaluator::new(&graph).expect("ready");
        let values = evaluator.evaluate(&point(), &|image, uv| {
            assert_eq!(image, GraphImage::Colour);
            [uv[0], uv[1], 0.5, 1.0]
        });
        assert_eq!(values.base_color, [0.25, 0.75, 0.5]);
        assert_eq!(values.metallic, value_noise([1.0, 2.0, 3.0]));
        assert_eq!(values.roughness, 0.0, "facing the camera: no rim");
        assert!((0.0..=1.0).contains(&value_noise([12.3, -4.5, 6.7])));
        naga_check(&compile(&graph).expect("compiles").source);
    }

    #[test]
    fn the_vertex_offset_reads_images_without_derivatives() {
        let mut graph = ShaderGraphData::new();
        graph.nodes.push(node(
            1,
            ShaderNodeKind::Texture {
                image: GraphImage::Bumps,
            },
        ));
        graph.wires.push(wire(1, 0, 6));
        let compiled = compile(&graph).expect("compiles");
        assert!(compiled.source.contains("textureSampleLevel(normal_map"));
        naga_check(&compiled.source);
    }

    #[test]
    fn graphs_round_trip_and_bad_numbers_are_refused() {
        let mut graph = ShaderGraphData::new();
        graph
            .nodes
            .push(node(1, ShaderNodeKind::Number { value: 0.5 }));
        graph.wires.push(wire(1, 0, 1));
        let text = ron::to_string(&graph).expect("writes");
        assert_eq!(
            ron::from_str::<ShaderGraphData>(&text).expect("reads"),
            graph
        );
        graph.nodes[1].kind = ShaderNodeKind::Number { value: f32::NAN };
        assert!(graph.validate().is_err());
    }
}
