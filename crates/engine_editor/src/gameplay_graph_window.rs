//! The Gameplay graph window: a `.graph` file on the node canvas, with the
//! graph's variables beside it, the selected node's settings below, and
//! what would stop the graph working above.
//!
//! The graph is a file, so every change saves it straight away; the edit
//! becomes one undo step once the pointer is released (see
//! [`crate::EditorState::settle_gameplay_graph`]).

use engine_scene::{
    GameplayGraphData, GameplayNodeData, GameplayNodeKind, GameplayWire, GraphAiEvent,
    GraphComment, GraphVariable, MAX_GAMEPLAY_NODES, MAX_GRAPH_COMMENTS, MAX_GRAPH_VARIABLES,
    NodeGroup, PinType, PinValue, RuleCompare, TouchKind,
};

use crate::inspect::{number, property_row};
use crate::node_canvas::{self, CanvasComment, CanvasEvent, CanvasNode, CanvasWire};
use crate::state::EditorState;
use crate::theme;

/// Height of the canvas.
const CANVAS_HEIGHT: f32 = 440.0;
/// Width of the variables column.
const SIDE_WIDTH: f32 = 230.0;

fn window_id() -> egui::Id {
    egui::Id::new("gameplay-graph-window")
}

fn canvas_id() -> egui::Id {
    window_id().with("canvas")
}

/// What is selected on the canvas.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Selection {
    Node(u32),
    Comment(usize),
}

/// The colour of a pin carrying `ty`: flow is white, data by type.
pub(crate) fn pin_color(ty: PinType) -> egui::Color32 {
    let palette = theme::NIGHT;
    match ty {
        PinType::Flow => palette.text,
        PinType::Bool => palette.danger,
        PinType::Number => palette.success,
        PinType::Text => palette.purple,
        PinType::Vector => palette.highlight,
        PinType::Object => palette.info,
    }
}

/// The colour bar of a node, by group.
fn accent(group: NodeGroup) -> egui::Color32 {
    let palette = theme::NIGHT;
    match group {
        NodeGroup::Event => palette.danger,
        NodeGroup::Flow => palette.text,
        NodeGroup::Data => palette.success,
        NodeGroup::Query => palette.info,
        NodeGroup::Variable => palette.purple,
        NodeGroup::Action => palette.accent,
    }
}

/// The line under a node's title: its main setting.
fn detail(kind: &GameplayNodeKind) -> Option<String> {
    use GameplayNodeKind as K;
    let text = match kind {
        K::KeyPressed { key } | K::KeyReleased { key } | K::KeyDown { key } => key.clone(),
        K::NamedEvent { name }
        | K::VariableChanged { name }
        | K::AnimationEvent { name }
        | K::CustomEvent { name }
        | K::GetVariable { name }
        | K::SetVariable { name }
        | K::GetGameVariable { name }
        | K::SetGameVariable { name } => name.clone(),
        K::Timer { seconds, repeat } => {
            format!("{seconds} s{}", if *repeat { ", repeats" } else { "" })
        }
        K::Value { value } => value.display(),
        K::Axis { negative, positive } => format!("{negative} / {positive}"),
        K::SpawnPrefab { path } | K::ChangeScene { path } => path.clone(),
        K::SetGameState { state } => state.clone(),
        K::Gate { start_open } => if *start_open {
            "starts open"
        } else {
            "starts closed"
        }
        .into(),
        _ => return None,
    };
    (!text.is_empty()).then_some(text)
}

/// The scene's objects as `(name, object ID)`, for object pickers.
fn scene_objects(state: &mut EditorState) -> Vec<(String, String)> {
    let mut objects: Vec<(String, String)> = state
        .world
        .query::<(
            &engine_scene::SceneObjectId,
            Option<&engine_ecs::components::Name>,
        )>()
        .iter(&state.world)
        .map(|(id, name)| {
            (
                name.map_or_else(|| id.0.clone(), |name| name.0.clone()),
                id.0.clone(),
            )
        })
        .collect();
    objects.sort();
    objects
}

/// An object picker: "This object" (empty) or a scene object.
fn object_field(
    ui: &mut egui::Ui,
    salt: impl std::hash::Hash + std::fmt::Debug,
    id: &mut String,
    objects: &[(String, String)],
    empty: &str,
) -> bool {
    let mut changed = false;
    let shown = if id.is_empty() {
        empty.to_owned()
    } else {
        objects
            .iter()
            .find(|(_, object)| object == id)
            .map_or_else(|| "Missing object".to_owned(), |(name, _)| name.clone())
    };
    egui::ComboBox::from_id_salt(("graph-object", salt))
        .width(ui.available_width())
        .icon(crate::widgets::combo_chevron)
        .selected_text(shown)
        .show_ui(ui, |ui| {
            if ui.selectable_label(id.is_empty(), empty).clicked() && !id.is_empty() {
                id.clear();
                changed = true;
            }
            for (name, object) in objects {
                if ui.selectable_label(id == object, name).clicked() && id != object {
                    id.clone_from(object);
                    changed = true;
                }
            }
        });
    changed
}

/// Edits a value of its own type.
fn value_field(
    ui: &mut egui::Ui,
    salt: impl std::hash::Hash + std::fmt::Debug + Copy,
    value: &mut PinValue,
    objects: &[(String, String)],
) -> bool {
    let palette = theme::NIGHT;
    match value {
        PinValue::Bool(on) => crate::widgets::checkbox(ui, &palette, on, "").changed(),
        PinValue::Number(number_value) => {
            number(ui, egui::DragValue::new(number_value).speed(0.1)).changed()
        }
        PinValue::Text(text) => crate::widgets::text_field(ui, text, "Text").changed(),
        PinValue::Vector(vector) => {
            let mut changed = false;
            for (axis, part) in ["X ", "Y ", "Z "].into_iter().zip(vector.iter_mut()) {
                changed |= number(ui, egui::DragValue::new(part).speed(0.1).prefix(axis)).changed();
            }
            changed
        }
        PinValue::Object(id) => object_field(ui, salt, id, objects, "This object"),
    }
}

/// A key picker over the names scripts use.
fn key_field(ui: &mut egui::Ui, salt: &str, key: &mut String) -> bool {
    let mut changed = false;
    egui::ComboBox::from_id_salt(("graph-key", salt))
        .width(ui.available_width())
        .icon(crate::widgets::combo_chevron)
        .selected_text(key.as_str())
        .show_ui(ui, |ui| {
            for option in engine_scripting::ScriptKey::ALL {
                let name = option.name();
                if ui.selectable_label(key == name, name).clicked() && key != name {
                    *key = name.to_owned();
                    changed = true;
                }
            }
        });
    changed
}

/// A text setting, at most [`engine_scene::MAX_GRAPH_TEXT`] bytes.
fn text_setting(ui: &mut egui::Ui, label: &str, text: &mut String, hint: &str) -> bool {
    property_row(ui, label, |ui| {
        let changed = crate::widgets::text_field(ui, text, hint).changed();
        while text.len() > engine_scene::MAX_GRAPH_TEXT {
            text.pop();
        }
        changed
    })
}

/// The settings of a node's kind.
fn kind_settings(
    ui: &mut egui::Ui,
    kind: &mut GameplayNodeKind,
    variables: &[GraphVariable],
    objects: &[(String, String)],
) -> bool {
    use GameplayNodeKind as K;
    let mut changed = false;
    match kind {
        K::KeyPressed { key } | K::KeyReleased { key } | K::KeyDown { key } => {
            property_row(ui, "Key", |ui| changed |= key_field(ui, "key", key));
        }
        K::Axis { negative, positive } => {
            property_row(ui, "−1 key", |ui| {
                changed |= key_field(ui, "negative", negative)
            });
            property_row(ui, "1 key", |ui| {
                changed |= key_field(ui, "positive", positive)
            });
        }
        K::Touch { kind } => {
            property_row(ui, "When", |ui| {
                egui::ComboBox::from_id_salt("graph-touch")
                    .width(ui.available_width())
                    .icon(crate::widgets::combo_chevron)
                    .selected_text(kind.label())
                    .show_ui(ui, |ui| {
                        for option in TouchKind::ALL {
                            if ui
                                .selectable_label(*kind == option, option.label())
                                .clicked()
                                && *kind != option
                            {
                                *kind = option;
                                changed = true;
                            }
                        }
                    });
            });
        }
        K::Ai { event } => {
            property_row(ui, "When it", |ui| {
                egui::ComboBox::from_id_salt("graph-ai")
                    .width(ui.available_width())
                    .icon(crate::widgets::combo_chevron)
                    .selected_text(event.label())
                    .show_ui(ui, |ui| {
                        for option in GraphAiEvent::ALL {
                            if ui
                                .selectable_label(*event == option, option.label())
                                .clicked()
                                && *event != option
                            {
                                *event = option;
                                changed = true;
                            }
                        }
                    });
            });
        }
        K::NamedEvent { name } | K::CustomEvent { name } | K::AnimationEvent { name } => {
            changed |= text_setting(ui, "Name", name, "Event name");
        }
        K::VariableChanged { name } | K::GetGameVariable { name } | K::SetGameVariable { name } => {
            changed |= text_setting(ui, "Variable", name, "Game variable name");
        }
        K::GetVariable { name } | K::SetVariable { name } => {
            property_row(ui, "Variable", |ui| {
                egui::ComboBox::from_id_salt("graph-variable")
                    .width(ui.available_width())
                    .icon(crate::widgets::combo_chevron)
                    .selected_text(if name.is_empty() {
                        "Pick a variable"
                    } else {
                        name.as_str()
                    })
                    .show_ui(ui, |ui| {
                        for variable in variables {
                            if ui
                                .selectable_label(*name == variable.name, &variable.name)
                                .clicked()
                                && *name != variable.name
                            {
                                name.clone_from(&variable.name);
                                changed = true;
                            }
                        }
                    });
            });
        }
        K::Timer { seconds, repeat } => {
            property_row(ui, "Every", |ui| {
                changed |= number(
                    ui,
                    egui::DragValue::new(seconds)
                        .speed(0.05)
                        .range(0.01..=3600.0)
                        .suffix(" s"),
                )
                .changed();
            });
            property_row(ui, "Repeat", |ui| {
                changed |= crate::widgets::checkbox(ui, &theme::NIGHT, repeat, "").changed();
            });
        }
        K::UiClicked { button } => {
            property_row(ui, "Button", |ui| {
                changed |= object_field(ui, "button", button, objects, "Pick a button");
            });
        }
        K::Gate { start_open } => {
            property_row(ui, "Starts open", |ui| {
                changed |= crate::widgets::checkbox(ui, &theme::NIGHT, start_open, "").changed();
            });
        }
        K::Value { value } => {
            property_row(ui, "Value", |ui| {
                changed |= value_field(ui, "value", value, objects);
            });
        }
        K::Compare { compare } => {
            property_row(ui, "Test", |ui| {
                egui::ComboBox::from_id_salt("graph-compare")
                    .width(ui.available_width())
                    .icon(crate::widgets::combo_chevron)
                    .selected_text(format!("A {} B", compare.symbol()))
                    .show_ui(ui, |ui| {
                        for option in RuleCompare::ALL {
                            if ui
                                .selectable_label(
                                    *compare == option,
                                    format!("A {} B", option.symbol()),
                                )
                                .clicked()
                                && *compare != option
                            {
                                *compare = option;
                                changed = true;
                            }
                        }
                    });
            });
        }
        K::SpawnPrefab { path } => {
            changed |= text_setting(ui, "Prefab", path, "prefabs/coin.prefab");
        }
        K::ChangeScene { path } => {
            changed |= text_setting(ui, "Scene", path, "scenes/level2.ron");
        }
        K::SetGameState { state } => {
            property_row(ui, "State", |ui| {
                egui::ComboBox::from_id_salt("graph-state")
                    .width(ui.available_width())
                    .icon(crate::widgets::combo_chevron)
                    .selected_text(state.as_str())
                    .show_ui(ui, |ui| {
                        for option in engine_scripting::GAME_STATES {
                            if ui.selectable_label(state == option, option).clicked()
                                && state != option
                            {
                                *state = option.to_owned();
                                changed = true;
                            }
                        }
                    });
            });
        }
        _ => {}
    }
    changed
}

/// The selected node's settings, then a field for each unwired data input.
fn node_settings(
    ui: &mut egui::Ui,
    graph: &mut GameplayGraphData,
    id: u32,
    objects: &[(String, String)],
) -> bool {
    let variables = graph.variables.clone();
    let wired: Vec<u8> = graph
        .wires
        .iter()
        .filter(|wire| wire.to == id)
        .map(|wire| wire.to_pin)
        .collect();
    let Some(node) = graph.nodes.iter_mut().find(|node| node.id == id) else {
        return false;
    };
    let mut changed = kind_settings(ui, &mut node.kind, &variables, objects);
    let inputs = node.kind.inputs_with(&variables);
    for (index, pin) in inputs.iter().enumerate() {
        let Ok(pin_index) = u8::try_from(index) else {
            continue;
        };
        if pin.ty == PinType::Flow || wired.contains(&pin_index) {
            continue;
        }
        let mut value = node
            .inputs
            .get(&pin_index)
            .filter(|value| value.pin_type() == pin.ty)
            .cloned()
            .unwrap_or_else(|| node.kind.default_input(index, pin.ty));
        property_row(ui, pin.name, |ui| {
            if value_field(ui, ("input", index), &mut value, objects) {
                node.inputs.insert(pin_index, value);
                changed = true;
            }
        });
    }
    changed
}

/// The add menu: a search field over every node kind, grouped. Adds the
/// picked one at `at` and returns its ID.
fn add_menu(ui: &mut egui::Ui, graph: &mut GameplayGraphData, at: [f32; 2]) -> Option<u32> {
    let search_id = window_id().with("search");
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
        .max_height(340.0)
        .show(ui, |ui| {
            let mut group = None;
            for kind in GameplayNodeKind::catalogue() {
                let label = kind.label();
                if !query.is_empty() && !label.to_lowercase().contains(&query) {
                    continue;
                }
                if group != Some(kind.group()) {
                    group = Some(kind.group());
                    ui.label(
                        egui::RichText::new(kind.group().label())
                            .size(11.0)
                            .color(theme::NIGHT.subtle),
                    );
                }
                if ui.button(&label).clicked() {
                    picked = Some(kind);
                }
            }
        });
    let kind = picked?;
    ui.ctx().data_mut(|data| data.remove::<String>(search_id));
    ui.close();
    if graph.nodes.len() >= MAX_GAMEPLAY_NODES {
        return None;
    }
    let id = graph.free_id();
    graph.nodes.push(GameplayNodeData {
        id,
        kind,
        position: at,
        inputs: Default::default(),
    });
    Some(id)
}

/// The graph's variables: name, type and starting value of each.
fn variables_panel(
    ui: &mut egui::Ui,
    graph: &mut GameplayGraphData,
    objects: &[(String, String)],
) -> bool {
    let palette = theme::NIGHT;
    let mut changed = false;
    ui.label(egui::RichText::new("Variables").color(palette.muted));
    ui.label(
        egui::RichText::new("Kept per object while the game runs.")
            .size(11.0)
            .color(palette.subtle),
    );
    ui.add_space(4.0);
    let mut remove = None;
    let mut renames = Vec::new();
    for (index, variable) in graph.variables.iter_mut().enumerate() {
        ui.push_id(("variable", index), |ui| {
            ui.horizontal(|ui| {
                let mut name = variable.name.clone();
                ui.add_sized(
                    [ui.available_width() - 64.0, 22.0],
                    egui::TextEdit::singleline(&mut name),
                );
                if name != variable.name && !name.trim().is_empty() && name.len() <= 64 {
                    renames.push((variable.name.clone(), name.clone()));
                    variable.name = name;
                    changed = true;
                }
                if ui
                    .add(
                        egui::Button::new(egui::RichText::new("Remove").color(palette.danger))
                            .frame(false),
                    )
                    .on_hover_text("Remove this variable")
                    .clicked()
                {
                    remove = Some(index);
                }
            });
            ui.horizontal(|ui| {
                let ty = variable.value.pin_type();
                egui::ComboBox::from_id_salt("type")
                    .width(90.0)
                    .icon(crate::widgets::combo_chevron)
                    .selected_text(ty.label())
                    .show_ui(ui, |ui| {
                        for option in PinType::VALUES {
                            if ui.selectable_label(ty == option, option.label()).clicked()
                                && ty != option
                            {
                                variable.value = PinValue::default_of(option);
                                changed = true;
                            }
                        }
                    });
                if !matches!(variable.value, PinValue::Vector(_)) {
                    changed |=
                        value_field(ui, ("variable-value", index), &mut variable.value, objects);
                }
            });
            if matches!(variable.value, PinValue::Vector(_)) {
                ui.horizontal(|ui| {
                    changed |=
                        value_field(ui, ("variable-value", index), &mut variable.value, objects);
                });
            }
            ui.add_space(6.0);
        });
    }
    // Nodes follow a renamed variable.
    for (old, new) in renames {
        for node in &mut graph.nodes {
            if let GameplayNodeKind::GetVariable { name } | GameplayNodeKind::SetVariable { name } =
                &mut node.kind
                && *name == old
            {
                name.clone_from(&new);
            }
        }
    }
    if let Some(index) = remove {
        graph.variables.remove(index);
        changed = true;
    }
    if graph.variables.len() < MAX_GRAPH_VARIABLES
        && crate::widgets::pill_button(ui, &palette, "+ Variable", palette.accent, true).clicked()
    {
        let mut number = graph.variables.len() + 1;
        let name = loop {
            let candidate = format!("variable{number}");
            if !graph
                .variables
                .iter()
                .any(|variable| variable.name == candidate)
            {
                break candidate;
            }
            number += 1;
        };
        graph.variables.push(GraphVariable {
            name,
            value: PinValue::Number(0.0),
        });
        changed = true;
    }
    changed
}

/// A node copied with ⌘C or Copy, for Paste.
fn clipboard_id() -> egui::Id {
    window_id().with("clipboard")
}

/// Pastes the copied node next to where it was, with a new ID.
fn paste(ctx: &egui::Context, graph: &mut GameplayGraphData) -> Option<u32> {
    let copied = ctx.data(|data| data.get_temp::<GameplayNodeData>(clipboard_id()))?;
    if graph.nodes.len() >= MAX_GAMEPLAY_NODES {
        return None;
    }
    let id = graph.free_id();
    let mut node = copied;
    node.id = id;
    node.position = [node.position[0] + 30.0, node.position[1] + 30.0];
    ctx.data_mut(|data| data.insert_temp(clipboard_id(), node.clone()));
    graph.nodes.push(node);
    Some(id)
}

/// Draws the window while a graph is open. Returns whether the graph changed.
pub(crate) fn show(ctx: &egui::Context, state: &mut EditorState) -> bool {
    if let Some(path) = ctx.data_mut(|data| {
        let request =
            data.get_temp::<std::path::PathBuf>(crate::inspector::gameplay_graph_open_request());
        data.remove::<std::path::PathBuf>(crate::inspector::gameplay_graph_open_request());
        request
    }) && let Err(error) = state.open_gameplay_graph(&path)
    {
        state.asset_browser.operation_error = Some(error);
    }
    // Picking a graph file in Project Assets opens it.
    let picked_id = window_id().with("picked");
    let picked = state.selected_asset.clone().filter(|path| {
        crate::assets::AssetKind::from_path(path) == crate::assets::AssetKind::Graph
    });
    let last = ctx.data(|data| data.get_temp::<Option<std::path::PathBuf>>(picked_id));
    if last.as_ref() != Some(&picked) {
        ctx.data_mut(|data| data.insert_temp(picked_id, picked.clone()));
        if let Some(path) = picked
            && let Err(error) = state.open_gameplay_graph(&path)
        {
            state.asset_browser.operation_error = Some(error);
        }
    }
    if state.graph_editor.is_none() {
        return false;
    }
    let objects = scene_objects(state);
    let live = live_nodes(state);
    let Some(editing) = state.graph_editor.as_mut() else {
        return false;
    };
    let name = editing
        .path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("Graph")
        .to_owned();
    let error = editing.error.clone();
    let graph = &mut editing.data;
    let selection_id = window_id().with("selected");
    let mut selection = ctx
        .data(|data| data.get_temp::<Selection>(selection_id))
        .filter(|selection| match selection {
            Selection::Node(id) => graph.node(*id).is_some(),
            Selection::Comment(index) => *index < graph.comments.len(),
        });
    let mut changed = false;
    let mut open = true;
    let problems = graph.problems();

    egui::Window::new(format!("Gameplay graph — {name}"))
        .id(window_id())
        .open(&mut open)
        .default_size([1080.0, 780.0])
        .min_width(760.0)
        .frame(
            egui::Frame::NONE
                .fill(theme::NIGHT.panel)
                .stroke(egui::Stroke::new(1.0, theme::NIGHT.rule))
                .corner_radius(egui::CornerRadius::same(10))
                .inner_margin(egui::Margin::same(14)),
        )
        .show(ctx, |ui| {
            let palette = theme::NIGHT;
            ui.label("Events start white flow wires; follow them to see what happens next. Coloured wires carry values.");
            ui.weak("Drag from an output dot to an input dot to join them. Right-click empty space to add a node; ⌘C / ⌘V copy and paste it; Delete removes it.");
            ui.add_space(8.0);
            let at = node_canvas::centre(
                ui.ctx(),
                canvas_id(),
                egui::vec2(ui.available_width() - SIDE_WIDTH, CANVAS_HEIGHT),
            );
            ui.horizontal(|ui| {
                ui.add_enabled_ui(graph.nodes.len() < MAX_GAMEPLAY_NODES, |ui| {
                    ui.menu_button("+ Node", |ui| {
                        theme::style_context_menu(ui);
                        if let Some(id) = add_menu(ui, graph, at) {
                            selection = Some(Selection::Node(id));
                            changed = true;
                        }
                    });
                });
                if ui
                    .add_enabled(graph.comments.len() < MAX_GRAPH_COMMENTS, egui::Button::new("+ Comment"))
                    .on_hover_text("A note drawn behind nodes, to group them")
                    .clicked()
                {
                    graph.comments.push(GraphComment {
                        text: "Comment".into(),
                        position: [at[0] - 40.0, at[1] - 40.0],
                        size: [320.0, 200.0],
                    });
                    selection = Some(Selection::Comment(graph.comments.len() - 1));
                    changed = true;
                }
                let node = match selection {
                    Some(Selection::Node(id)) => Some(id),
                    _ => None,
                };
                if ui.add_enabled(node.is_some(), egui::Button::new("Copy")).clicked()
                    && let Some(copied) = node.and_then(|id| graph.node(id)).cloned()
                {
                    ui.ctx().data_mut(|data| data.insert_temp(clipboard_id(), copied));
                }
                let can_paste = ui
                    .ctx()
                    .data(|data| data.get_temp::<GameplayNodeData>(clipboard_id()).is_some());
                if ui.add_enabled(can_paste, egui::Button::new("Paste")).clicked()
                    && let Some(id) = paste(ui.ctx(), graph)
                {
                    selection = Some(Selection::Node(id));
                    changed = true;
                }
                if ui
                    .add_enabled(
                        selection.is_some(),
                        egui::Button::new(egui::RichText::new("Delete").color(palette.danger)),
                    )
                    .clicked()
                {
                    match selection {
                        Some(Selection::Node(id)) => changed |= graph.remove(id),
                        Some(Selection::Comment(index)) => {
                            graph.comments.remove(index);
                            changed = true;
                        }
                        None => {}
                    }
                    selection = None;
                }
                if ui.button("Reset view").clicked() {
                    node_canvas::reset_view(ui.ctx(), canvas_id());
                }
            });
            ui.add_space(6.0);
            if let Some(error) = &error {
                ui.colored_label(palette.danger, format!("Not saved: {error}"));
            }
            match problems.first() {
                Some(problem) => {
                    let more = problems.len() - 1;
                    ui.colored_label(
                        palette.danger,
                        if more == 0 {
                            format!("{problem}. The game skips that part.")
                        } else {
                            format!("{problem}, and {more} more. The game skips those parts.")
                        },
                    );
                }
                None if live.is_some() => {
                    ui.weak("Playing: nodes that ran in the last half second have a green edge.");
                }
                None => {
                    ui.weak("Saved. Objects with this graph run it when the game plays.");
                }
            }
            ui.add_space(6.0);

            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(SIDE_WIDTH - 12.0, CANVAS_HEIGHT),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_width(SIDE_WIDTH - 12.0);
                        egui::ScrollArea::vertical()
                            .id_salt("graph-variables")
                            .max_height(CANVAS_HEIGHT)
                            .show(ui, |ui| {
                                changed |= variables_panel(ui, graph, &objects);
                            });
                    },
                );
                let problem_nodes: Vec<u32> = problems.iter().filter_map(|p| p.node).collect();
                let nodes: Vec<CanvasNode> = graph
                    .nodes
                    .iter()
                    .map(|node| {
                        let inputs = graph.inputs_of(node);
                        let outputs = graph.outputs_of(node);
                        CanvasNode {
                            id: node.id,
                            position: node.position,
                            title: node.kind.label(),
                            detail: detail(&node.kind),
                            inputs: inputs.iter().map(|pin| pin.name.to_owned()).collect(),
                            outputs: outputs.iter().map(|pin| pin.name.to_owned()).collect(),
                            accent: accent(node.kind.group()),
                            warning: problem_nodes.contains(&node.id),
                            input_colors: inputs.iter().map(|pin| pin_color(pin.ty)).collect(),
                            output_colors: outputs.iter().map(|pin| pin_color(pin.ty)).collect(),
                            lit: live.as_ref().is_some_and(|live| live.contains(&node.id)),
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
                let comments: Vec<CanvasComment> = graph
                    .comments
                    .iter()
                    .map(|comment| CanvasComment {
                        position: comment.position,
                        size: comment.size,
                        text: comment.text.clone(),
                    })
                    .collect();
                let size = egui::vec2(ui.available_width().max(480.0), CANVAS_HEIGHT);
                let mut added = None;
                let selected_node = match selection {
                    Some(Selection::Node(id)) => Some(id),
                    _ => None,
                };
                let selected_comment = match selection {
                    Some(Selection::Comment(index)) => Some(index),
                    _ => None,
                };
                let events = node_canvas::show(
                    ui,
                    canvas_id(),
                    size,
                    &node_canvas::CanvasContent {
                        nodes: &nodes,
                        wires: &wires,
                        comments: &comments,
                        selected: selected_node,
                        selected_comment,
                    },
                    &mut |ui, at| {
                        let mut copy = graph.clone();
                        if let Some(id) = add_menu(ui, &mut copy, at) {
                            added = Some((copy, id));
                        }
                    },
                );
                if let Some((copy, id)) = added {
                    *graph = copy;
                    selection = Some(Selection::Node(id));
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
                        CanvasEvent::Selected(node) => selection = node.map(Selection::Node),
                        CanvasEvent::Connected(wire) => {
                            match graph.connect(GameplayWire {
                                from: wire.from,
                                from_pin: wire.from_pin,
                                to: wire.to,
                                to_pin: wire.to_pin,
                            }) {
                                Ok(made) => changed |= made,
                                Err(reason) => ui.ctx().data_mut(|data| {
                                    data.insert_temp(window_id().with("refused"), reason);
                                }),
                            }
                        }
                        CanvasEvent::Disconnected { to, to_pin } => {
                            let before = graph.wires.len();
                            // A flow input may have several wires: drop
                            // only the one picked up (the first).
                            if let Some(index) = graph
                                .wires
                                .iter()
                                .position(|wire| wire.to == to && wire.to_pin == to_pin)
                            {
                                graph.wires.remove(index);
                            }
                            changed |= graph.wires.len() != before;
                        }
                        CanvasEvent::Delete(node) => {
                            if graph.remove(node) {
                                selection = None;
                                changed = true;
                            }
                        }
                        CanvasEvent::CommentSelected(index) => {
                            selection = Some(Selection::Comment(index));
                        }
                        CanvasEvent::CommentMoved { index, position } => {
                            if let Some(comment) = graph.comments.get_mut(index) {
                                comment.position = position;
                                changed = true;
                            }
                        }
                        CanvasEvent::CommentResized { index, size } => {
                            if let Some(comment) = graph.comments.get_mut(index) {
                                comment.size = size;
                                changed = true;
                            }
                        }
                    }
                }
            });
            // Copy and paste with the keyboard while the pointer is over
            // the window and nothing is being typed.
            if !ui.ctx().egui_wants_keyboard_input() && ui.ui_contains_pointer() {
                let (copy, paste_pressed) = ui.input(|input| {
                    (
                        input.events.iter().any(|event| matches!(event, egui::Event::Copy)),
                        input.events.iter().any(|event| matches!(event, egui::Event::Paste(_))),
                    )
                });
                if copy
                    && let Some(Selection::Node(id)) = selection
                    && let Some(copied) = graph.node(id).cloned()
                {
                    ui.ctx().data_mut(|data| data.insert_temp(clipboard_id(), copied));
                }
                if paste_pressed && let Some(id) = paste(ui.ctx(), graph) {
                    selection = Some(Selection::Node(id));
                    changed = true;
                }
            }
            if let Some(reason) = ui
                .ctx()
                .data(|data| data.get_temp::<String>(window_id().with("refused")))
            {
                ui.colored_label(palette.danger, format!("Not joined: {reason}."));
            }

            ui.add_space(8.0);
            egui::ScrollArea::vertical()
                .id_salt("graph-settings")
                .max_height(200.0)
                .show(ui, |ui| match selection {
                    Some(Selection::Node(id)) => {
                        if let Some(node) = graph.node(id) {
                            ui.label(
                                egui::RichText::new(node.kind.label()).color(palette.text),
                            );
                        }
                        changed |= node_settings(ui, graph, id, &objects);
                    }
                    Some(Selection::Comment(index)) => {
                        if let Some(comment) = graph.comments.get_mut(index) {
                            changed |= text_setting(ui, "Comment", &mut comment.text, "What this part does");
                        }
                    }
                    None => {
                        ui.weak("Click a node to change its settings and the values of its unwired inputs.");
                    }
                });
        });
    ctx.data_mut(|data| match selection {
        Some(selection) => {
            data.insert_temp(selection_id, selection);
        }
        None => data.remove::<Selection>(selection_id),
    });
    if changed {
        ctx.data_mut(|data| data.remove::<String>(window_id().with("refused")));
        state.save_gameplay_graph();
    }
    if !open {
        state.settle_gameplay_graph();
        state.graph_editor = None;
    }
    changed
}

/// Nodes of the open graph that ran lately in a game playing in the Scene
/// View, when the watched object runs this graph.
fn live_nodes(state: &EditorState) -> Option<Vec<u32>> {
    let (_, nodes) = state.play.graph_trace()?;
    Some(nodes.to_vec())
}

/// Selects `node` in the window, for snapshots.
#[cfg(test)]
pub(crate) fn select_node(ctx: &egui::Context, node: u32) {
    ctx.data_mut(|data| data.insert_temp(window_id().with("selected"), Selection::Node(node)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pin_type_and_group_has_its_own_colour() {
        let colours: std::collections::HashSet<_> = [
            PinType::Flow,
            PinType::Bool,
            PinType::Number,
            PinType::Text,
            PinType::Vector,
            PinType::Object,
        ]
        .into_iter()
        .map(pin_color)
        .collect();
        assert_eq!(colours.len(), 6);
    }

    #[test]
    fn details_show_the_main_setting() {
        assert_eq!(
            detail(&GameplayNodeKind::KeyPressed {
                key: "Space".into()
            }),
            Some("Space".into())
        );
        assert_eq!(detail(&GameplayNodeKind::Add), None);
    }

    #[test]
    fn paste_adds_a_copy_beside_the_original() {
        let ctx = egui::Context::default();
        let mut graph = GameplayGraphData::new();
        let original = graph.nodes[0].clone();
        ctx.data_mut(|data| data.insert_temp(clipboard_id(), original.clone()));
        let id = paste(&ctx, &mut graph).expect("pasted");
        let copy = graph.node(id).expect("copy");
        assert_eq!(copy.kind, original.kind);
        assert_eq!(
            copy.position,
            [original.position[0] + 30.0, original.position[1] + 30.0]
        );
        assert_ne!(copy.id, original.id);
    }
}
