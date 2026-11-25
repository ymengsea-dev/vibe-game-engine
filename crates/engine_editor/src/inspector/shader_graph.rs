//! The Shader Graph window: a material file's graph on a node canvas, the
//! add-node menu with search, the selected node's settings, and what is
//! wrong when the graph does not compile.
//!
//! The graph lives in the material file, so every change here saves the
//! file like any other material edit.

use engine_scene::{
    GraphImage, MAX_GRAPH_NODES, MaterialData, ShaderGraphData, ShaderNodeData, ShaderNodeKind,
    ShaderWire,
};

use super::{choice, note, number, property_row};
use crate::node_canvas::{self, CanvasEvent, CanvasNode, CanvasWire};
use crate::theme;

/// Size of the canvas.
const CANVAS: egui::Vec2 = egui::vec2(760.0, 420.0);

fn window_id(key: &str) -> egui::Id {
    egui::Id::new(("shader_graph", key))
}

/// Opens the window for the material file `key` (its path).
pub(crate) fn open(ctx: &egui::Context, key: &str) {
    ctx.data_mut(|data| data.insert_temp(window_id(key), true));
}

/// A graph doing what `data`'s standard settings do: its colour image
/// times its colour, and its metallic and roughness.
pub(crate) fn starter_graph(data: &MaterialData) -> ShaderGraphData {
    let mut graph = ShaderGraphData::new();
    let node = |id, kind, x, y| ShaderNodeData {
        id,
        kind,
        position: [x, y],
    };
    graph.nodes.extend([
        node(
            1,
            ShaderNodeKind::Texture {
                image: GraphImage::Colour,
            },
            40.0,
            20.0,
        ),
        node(
            2,
            ShaderNodeKind::Colour {
                value: data.base_color,
            },
            40.0,
            130.0,
        ),
        node(3, ShaderNodeKind::Multiply, 280.0, 60.0),
        node(
            4,
            ShaderNodeKind::Number {
                value: data.metallic,
            },
            280.0,
            190.0,
        ),
        node(
            5,
            ShaderNodeKind::Number {
                value: data.roughness,
            },
            280.0,
            280.0,
        ),
    ]);
    for (from, to, to_pin) in [(1, 3, 0), (2, 3, 1), (3, 0, 0), (4, 0, 1), (5, 0, 2)] {
        graph.wires.push(ShaderWire {
            from,
            from_pin: 0,
            to,
            to_pin,
        });
    }
    graph
}

/// The colour bar of a node: values, scene inputs, images, maths and the
/// Surface differ.
fn accent(kind: &ShaderNodeKind) -> egui::Color32 {
    let palette = theme::NIGHT;
    match kind {
        ShaderNodeKind::Output => palette.highlight,
        ShaderNodeKind::Number { .. }
        | ShaderNodeKind::Colour { .. }
        | ShaderNodeKind::Vector { .. }
        | ShaderNodeKind::Parameter { .. } => palette.purple,
        ShaderNodeKind::Time
        | ShaderNodeKind::Uv
        | ShaderNodeKind::WorldPosition
        | ShaderNodeKind::Normal
        | ShaderNodeKind::ViewDirection => palette.info,
        ShaderNodeKind::Texture { .. } => palette.success,
        _ => palette.accent,
    }
}

fn short(value: f32) -> String {
    let text = format!("{value:.3}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text.is_empty() || text == "-" || text == "-0" {
        "0".into()
    } else {
        text.to_owned()
    }
}

/// The canvas title and detail line of a node.
fn title_and_detail(kind: &ShaderNodeKind) -> (String, Option<String>) {
    match kind {
        ShaderNodeKind::Number { value } => ("Number".into(), Some(short(*value))),
        ShaderNodeKind::Colour { value } => (
            "Colour".into(),
            Some(format!(
                "{}, {}, {}",
                short(value[0]),
                short(value[1]),
                short(value[2])
            )),
        ),
        ShaderNodeKind::Vector { value } => (
            "Vector".into(),
            Some(format!(
                "{}, {}, {}",
                short(value[0]),
                short(value[1]),
                short(value[2])
            )),
        ),
        ShaderNodeKind::Parameter { name, .. } => ("Parameter".into(), Some(name.clone())),
        other => (other.label(), None),
    }
}

/// A short line on what each kind of node is for, shown with its settings.
fn describe(kind: &ShaderNodeKind) -> &'static str {
    match kind {
        ShaderNodeKind::Output => {
            "What the surface looks like. Unwired inputs keep their defaults: grey, half rough, flat, no glow, solid."
        }
        ShaderNodeKind::Number { .. } => "A fixed number.",
        ShaderNodeKind::Colour { .. } => "A fixed colour.",
        ShaderNodeKind::Vector { .. } => "Three fixed numbers, such as a direction.",
        ShaderNodeKind::Time => "Seconds since the game started; animates whatever it feeds.",
        ShaderNodeKind::Uv => {
            "Where on its images the surface is, after the material's Repeat and Shift."
        }
        ShaderNodeKind::WorldPosition => "Where the surface is in the world.",
        ShaderNodeKind::Normal => "Which way the surface faces, in the world.",
        ShaderNodeKind::ViewDirection => "From the surface towards the camera.",
        ShaderNodeKind::Texture { .. } => {
            "Reads one of the material's images. Unwired UV uses the surface's own."
        }
        ShaderNodeKind::Add => "A + B.",
        ShaderNodeKind::Subtract => "A − B.",
        ShaderNodeKind::Multiply => "A × B, part by part.",
        ShaderNodeKind::Divide => "A ÷ B, part by part.",
        ShaderNodeKind::Lerp => "From A at T = 0 to B at T = 1.",
        ShaderNodeKind::Clamp => "X kept between Min and Max.",
        ShaderNodeKind::Power => "A to the power B.",
        ShaderNodeKind::Sin => "Sine: swings from −1 to 1. Feed it Time for a pulse.",
        ShaderNodeKind::Cos => "Cosine: like Sin, a quarter turn later.",
        ShaderNodeKind::Dot => "How much A and B point the same way.",
        ShaderNodeKind::Normalise => "The same direction with length 1.",
        ShaderNodeKind::Split => "A vector's four parts, one per output.",
        ShaderNodeKind::Combine => "Four numbers as one vector or colour.",
        ShaderNodeKind::Fresnel => {
            "Bright where the surface turns away from the camera: rims and edges."
        }
        ShaderNodeKind::Noise => {
            "Smooth random values from 0 to 1. Unwired Position uses the world position."
        }
        ShaderNodeKind::Step => "0 below Edge, 1 from it.",
        ShaderNodeKind::Smoothstep => "From 0 at Edge 0 to 1 at Edge 1, smoothly.",
        ShaderNodeKind::Remap => "X moved from one range to another.",
        ShaderNodeKind::Parameter { .. } => {
            "A value each object can change in its Inspector, and scripts with set_material_value."
        }
    }
}

/// The add menu: a search field over every node kind. Adds the picked one
/// at `at` and selects it.
fn add_menu(
    ui: &mut egui::Ui,
    id: egui::Id,
    graph: &mut ShaderGraphData,
    selected: &mut Option<u32>,
    changed: &mut bool,
    at: [f32; 2],
) {
    let search_id = id.with("search");
    let mut search = ui
        .ctx()
        .data(|data| data.get_temp::<String>(search_id))
        .unwrap_or_default();
    let field = crate::widgets::text_field(ui, &mut search, "Search nodes");
    if field.changed() {
        ui.ctx()
            .data_mut(|data| data.insert_temp(search_id, search.clone()));
    }
    field.request_focus();
    let query = search.to_lowercase();
    let mut picked = None;
    egui::ScrollArea::vertical()
        .max_height(280.0)
        .show(ui, |ui| {
            for kind in ShaderNodeKind::catalogue() {
                let label = kind.label();
                if !query.is_empty() && !label.to_lowercase().contains(&query) {
                    continue;
                }
                if ui.button(&label).clicked() {
                    picked = Some(kind);
                }
            }
        });
    if let Some(kind) = picked {
        if graph.nodes.len() < MAX_GRAPH_NODES {
            let kind = match kind {
                ShaderNodeKind::Parameter {
                    name,
                    default,
                    colour,
                } => ShaderNodeKind::Parameter {
                    name: free_parameter_name(graph, &name),
                    default,
                    colour,
                },
                other => other,
            };
            let node = graph.free_id();
            graph.nodes.push(ShaderNodeData {
                id: node,
                kind,
                position: at,
            });
            *selected = Some(node);
            *changed = true;
        }
        ui.ctx().data_mut(|data| data.remove::<String>(search_id));
        ui.close();
    }
}

/// `base`, or `base 2`, `base 3`… whichever no parameter has yet.
fn free_parameter_name(graph: &ShaderGraphData, base: &str) -> String {
    let taken: Vec<String> = graph
        .parameters()
        .into_iter()
        .map(|(name, ..)| name)
        .collect();
    if !taken.iter().any(|name| name == base) {
        return base.to_owned();
    }
    (2..)
        .map(|n| format!("{base} {n}"))
        .find(|name| !taken.contains(name))
        .unwrap_or_else(|| base.to_owned())
}

/// The settings of the selected node.
fn node_fields(ui: &mut egui::Ui, node: &mut ShaderNodeData) -> bool {
    let mut changed = false;
    note(ui, describe(&node.kind));
    match &mut node.kind {
        ShaderNodeKind::Number { value } => {
            property_row(ui, "Value", |ui| {
                changed |= number(ui, egui::DragValue::new(value).speed(0.01)).changed();
            });
        }
        ShaderNodeKind::Colour { value } => {
            property_row(ui, "Colour", |ui| {
                changed |= ui.color_edit_button_rgba_unmultiplied(value).changed();
            });
        }
        ShaderNodeKind::Vector { value } => {
            property_row(ui, "Value", |ui| {
                for part in value.iter_mut() {
                    changed |= number(ui, egui::DragValue::new(part).speed(0.01)).changed();
                }
            });
        }
        ShaderNodeKind::Texture { image } => {
            property_row(ui, "Image", |ui| {
                choice("shader-graph-image", ui)
                    .selected_text(image.label())
                    .show_ui(ui, |ui| {
                        for option in GraphImage::ALL {
                            if ui
                                .selectable_label(*image == option, option.label())
                                .clicked()
                                && *image != option
                            {
                                *image = option;
                                changed = true;
                            }
                        }
                    });
            });
        }
        ShaderNodeKind::Parameter {
            name,
            default,
            colour,
        } => {
            property_row(ui, "Name", |ui| {
                let mut text = name.clone();
                if crate::widgets::text_field(ui, &mut text, "Parameter name").changed() {
                    let text: String = text
                        .chars()
                        .take(engine_scene::MAX_PARAMETER_NAME)
                        .collect();
                    if !text.trim().is_empty() {
                        *name = text;
                        changed = true;
                    }
                }
            });
            property_row(ui, "Colour", |ui| {
                changed |= crate::widgets::checkbox(ui, &theme::NIGHT, colour, "")
                    .on_hover_text("A colour; otherwise a number")
                    .changed();
            });
            property_row(ui, "Default", |ui| {
                if *colour {
                    changed |= ui.color_edit_button_rgba_unmultiplied(default).changed();
                } else {
                    changed |=
                        number(ui, egui::DragValue::new(&mut default[0]).speed(0.01)).changed();
                }
            });
        }
        _ => {}
    }
    changed
}

/// Draws the window for `key` while it is open. Returns whether the graph
/// changed; the caller saves the material file.
pub(crate) fn show(ctx: &egui::Context, key: &str, name: &str, data: &mut MaterialData) -> bool {
    let id = window_id(key);
    let mut visible = ctx.data(|data| data.get_temp::<bool>(id).unwrap_or(false));
    let Some(graph) = data.graph.as_mut() else {
        return false;
    };
    if !visible {
        return false;
    }
    let selected_id = id.with("selected");
    let mut selected = ctx
        .data(|data| data.get_temp::<u32>(selected_id))
        .filter(|node| graph.node(*node).is_some());
    let mut changed = false;
    let compiled = engine_scene::compile_shader_graph(graph);
    let error = compiled.as_ref().err().cloned();
    let canvas_id = id.with("canvas");

    egui::Window::new(format!("Shader graph — {name}"))
        .id(id)
        .open(&mut visible)
        .default_size([800.0, 700.0])
        .min_width(620.0)
        .frame(
            egui::Frame::NONE
                .fill(theme::NIGHT.panel)
                .stroke(egui::Stroke::new(1.0, theme::NIGHT.rule))
                .corner_radius(egui::CornerRadius::same(10))
                .inner_margin(egui::Margin::same(14)),
        )
        .show(ctx, |ui| {
            super::inspector_style(ui.style_mut());
            ui.label("Wire nodes into Surface. Drag from an output to an input to join them; drag a wire off an input to move or remove it.");
            ui.weak("Right-click empty space to add a node; Delete removes the selected one.");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_enabled_ui(graph.nodes.len() < MAX_GRAPH_NODES, |ui| {
                    ui.menu_button("+ Node", |ui| {
                        theme::style_context_menu(ui);
                        let at = node_canvas::centre(ui.ctx(), canvas_id, CANVAS);
                        add_menu(ui, id, graph, &mut selected, &mut changed, at);
                    });
                });
                let removable = selected
                    .and_then(|node| graph.node(node))
                    .is_some_and(|node| node.kind != ShaderNodeKind::Output);
                if ui
                    .add_enabled(
                        removable,
                        egui::Button::new(egui::RichText::new("Delete").color(theme::NIGHT.danger)),
                    )
                    .clicked()
                    && let Some(node) = selected
                    && graph.remove(node)
                {
                    selected = None;
                    changed = true;
                }
                if ui.button("Reset view").clicked() {
                    node_canvas::reset_view(ui.ctx(), canvas_id);
                }
            });
            ui.add_space(8.0);
            match &error {
                Some(error) => {
                    ui.colored_label(theme::NIGHT.danger, format!("Not drawn yet: {error}."));
                }
                None => {
                    ui.weak("Compiles. Objects using this material show it in the Scene View and the game.");
                }
            }
            ui.add_space(6.0);

            let nodes: Vec<CanvasNode> = graph
                .nodes
                .iter()
                .map(|node| {
                    let (title, detail) = title_and_detail(&node.kind);
                    CanvasNode {
                        id: node.id,
                        position: node.position,
                        title,
                        detail,
                        inputs: node
                            .kind
                            .inputs()
                            .iter()
                            .map(|pin| pin.name.to_owned())
                            .collect(),
                        outputs: node
                            .kind
                            .outputs()
                            .iter()
                            .map(|name| (*name).to_owned())
                            .collect(),
                        accent: accent(&node.kind),
                        warning: error.as_ref().and_then(|error| error.node) == Some(node.id),
                        input_colors: Vec::new(),
                        output_colors: Vec::new(),
                        lit: false,
                    }
                })
                .collect();
            let wires: Vec<CanvasWire> = graph
                .wires
                .iter()
                .map(|wire| CanvasWire {
                    from: wire.from,
                    from_pin: wire.from_pin,
                    to: wire.to,
                    to_pin: wire.to_pin,
                })
                .collect();
            let size = egui::vec2(ui.available_width().max(560.0), CANVAS.y);
            let mut added = None;
            let events = node_canvas::show(
                ui,
                canvas_id,
                size,
                &node_canvas::CanvasContent {
                    nodes: &nodes,
                    wires: &wires,
                    comments: &[],
                    selected,
                    selected_comment: None,
                },
                &mut |ui, at| {
                    let mut copy = graph.clone();
                    let mut pick = selected;
                    let mut did = false;
                    add_menu(ui, id, &mut copy, &mut pick, &mut did, at);
                    if did {
                        added = Some((copy, pick));
                    }
                },
            );
            if let Some((copy, pick)) = added {
                *graph = copy;
                selected = pick;
                changed = true;
            }
            for event in events {
                match event {
                    CanvasEvent::Moved { node, position } => {
                        if let Some(target) = graph.nodes.iter_mut().find(|n| n.id == node) {
                            target.position = position;
                            changed = true;
                        }
                    }
                    CanvasEvent::Selected(node) => selected = node,
                    CanvasEvent::Connected(wire) => {
                        changed |= graph.connect(ShaderWire {
                            from: wire.from,
                            from_pin: wire.from_pin,
                            to: wire.to,
                            to_pin: wire.to_pin,
                        });
                    }
                    CanvasEvent::Disconnected { to, to_pin } => {
                        let before = graph.wires.len();
                        graph
                            .wires
                            .retain(|wire| !(wire.to == to && wire.to_pin == to_pin));
                        changed |= graph.wires.len() != before;
                    }
                    CanvasEvent::Delete(node) => {
                        if graph.remove(node) {
                            selected = None;
                            changed = true;
                        }
                    }
                    // A shader graph has no comments.
                    CanvasEvent::CommentSelected(_)
                    | CanvasEvent::CommentMoved { .. }
                    | CanvasEvent::CommentResized { .. } => {}
                }
            }

            ui.add_space(10.0);
            egui::ScrollArea::vertical()
                .id_salt(id.with("details"))
                .max_height(200.0)
                .show(ui, |ui| match selected {
                    Some(node) => {
                        if let Some(target) = graph.nodes.iter_mut().find(|n| n.id == node) {
                            changed |= node_fields(ui, target);
                        }
                    }
                    None => note(ui, "Click a node to change its settings."),
                });
        });
    ctx.data_mut(|data| {
        data.insert_temp(id, visible);
        match selected {
            Some(node) => {
                data.insert_temp(selected_id, node);
            }
            None => data.remove::<u32>(selected_id),
        }
    });
    changed
}

/// Selects `node` in `key`'s window.
#[cfg(test)]
pub(crate) fn select(ctx: &egui::Context, key: &str, node: u32) {
    ctx.data_mut(|data| data.insert_temp(window_id(key).with("selected"), node));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_starter_graph_compiles_to_the_standard_settings() {
        let data = MaterialData {
            base_color: [0.5, 0.25, 1.0, 1.0],
            metallic: 1.0,
            roughness: 0.2,
            ..MaterialData::default()
        };
        let graph = starter_graph(&data);
        engine_scene::compile_shader_graph(&graph).expect("compiles");
        let values = engine_scene::Evaluator::new(&graph)
            .expect("ready")
            .evaluate(
                &engine_scene::SurfacePoint {
                    uv: [0.0, 0.0],
                    world_position: [0.0; 3],
                    normal: [0.0, 0.0, 1.0],
                    view_direction: [0.0, 0.0, 1.0],
                    time: 0.0,
                },
                &|_, _| [1.0; 4],
            );
        assert_eq!(values.base_color, [0.5, 0.25, 1.0]);
        assert_eq!(values.metallic, 1.0);
        assert_eq!(values.roughness, 0.2);
    }

    #[test]
    fn new_parameters_get_names_no_other_has() {
        let mut graph = ShaderGraphData::new();
        assert_eq!(free_parameter_name(&graph, "Tint"), "Tint");
        graph.nodes.push(ShaderNodeData {
            id: 1,
            kind: ShaderNodeKind::Parameter {
                name: "Tint".into(),
                default: [1.0; 4],
                colour: true,
            },
            position: [0.0, 0.0],
        });
        assert_eq!(free_parameter_name(&graph, "Tint"), "Tint 2");
    }

    #[test]
    fn short_numbers_drop_trailing_zeros() {
        assert_eq!(short(1.0), "1");
        assert_eq!(short(0.25), "0.25");
        assert_eq!(short(-0.0001), "0");
    }
}
