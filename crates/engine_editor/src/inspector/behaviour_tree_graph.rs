//! The Behaviour tree window: an AI brain's nodes drawn top-down from the
//! root, children left to right in the order they run, and the fields of
//! the selected node below.
//!
//! The tree lives in the object's AI settings, so every change here is an
//! ordinary scene edit: it saves with the scene and ⌘Z undoes it.

use engine_ecs::prelude::Entity;
use engine_scene::{
    BehaviourTreeData, BtNodeData, BtNodeKind, MAX_BT_NODES, MAX_PATROL_POINTS, RuleCompare,
};

use super::animator_graph::{arrow, node};
use super::{choice, note, number, property_row};
use crate::theme;

/// Size of a node on the canvas.
const NODE_SIZE: egui::Vec2 = egui::vec2(150.0, 44.0);
/// Horizontal room per leaf in the automatic layout.
const COLUMN: f32 = 170.0;
/// Vertical distance between tree levels in the automatic layout.
const ROW: f32 = 84.0;
/// Height of the visible canvas.
const CANVAS_HEIGHT: f32 = 320.0;

fn window_id(entity: Entity) -> egui::Id {
    egui::Id::new(("behaviour_tree_graph", entity))
}

/// Opens the window for `entity`'s behaviour tree.
pub(super) fn open(ctx: &egui::Context, entity: Entity) {
    ctx.data_mut(|data| data.insert_temp(window_id(entity), true));
}

/// Selects node `index` in `entity`'s window.
#[cfg(test)]
pub(super) fn select_node(ctx: &egui::Context, entity: Entity, index: usize) {
    ctx.data_mut(|data| data.insert_temp(window_id(entity).with("selected"), index));
}

/// Where every node goes when it has no saved position: each leaf in its own
/// column, each parent centred over its children, one row per level.
pub(super) fn auto_layout(tree: &BehaviourTreeData) -> Vec<egui::Pos2> {
    let mut positions = vec![egui::Pos2::ZERO; tree.nodes.len()];
    let mut next_leaf = 0.0;
    let mut visited = vec![false; tree.nodes.len()];
    place(tree, 0, 0, &mut next_leaf, &mut positions, &mut visited);
    positions
}

fn place(
    tree: &BehaviourTreeData,
    index: usize,
    depth: usize,
    next_leaf: &mut f32,
    positions: &mut [egui::Pos2],
    visited: &mut [bool],
) -> f32 {
    if index >= tree.nodes.len() || visited[index] {
        return *next_leaf * COLUMN;
    }
    visited[index] = true;
    let children: Vec<f32> = tree.nodes[index]
        .children
        .iter()
        .map(|&child| place(tree, child, depth + 1, next_leaf, positions, visited))
        .collect();
    let x = if children.is_empty() {
        let x = *next_leaf * COLUMN;
        *next_leaf += 1.0;
        x
    } else {
        children.iter().sum::<f32>() / children.len() as f32
    };
    positions[index] = egui::pos2(x + 16.0, depth as f32 * ROW + 16.0);
    x
}

/// Where node `index` is drawn, in graph space.
fn node_position(tree: &BehaviourTreeData, auto: &[egui::Pos2], index: usize) -> egui::Pos2 {
    tree.nodes[index].graph_position.map_or_else(
        || auto.get(index).copied().unwrap_or_default(),
        |[x, y]| egui::pos2(x, y),
    )
}

/// The text on a node: its kind and its main setting.
pub(super) fn node_label(kind: &BtNodeKind) -> String {
    let detail = match kind {
        BtNodeKind::TargetWithin { distance } => format!(" {distance} m"),
        BtNodeKind::Variable(condition) => format!(
            " {} {} {}",
            condition.variable,
            condition.compare.symbol(),
            engine_scene::format_variable(condition.value)
        ),
        BtNodeKind::Chance { percent } => format!(" {percent}%"),
        BtNodeKind::Patrol { points } => format!(" ({} points)", points.len()),
        BtNodeKind::Wait { seconds } => format!(" {seconds} s"),
        BtNodeKind::PlayAnimation { clip } if !clip.is_empty() => format!(" {clip}"),
        BtNodeKind::SendEvent { name } => format!(" {name}"),
        BtNodeKind::SetVariable { name, value } => {
            format!(" {name} = {}", engine_scene::format_variable(*value))
        }
        _ => String::new(),
    };
    format!("{}{detail}", kind.label())
}

/// The colour bar of a node: composites, checks and actions differ.
fn accent(kind: &BtNodeKind) -> egui::Color32 {
    let palette = theme::NIGHT;
    match kind {
        BtNodeKind::Sequence | BtNodeKind::Selector | BtNodeKind::Inverter => palette.accent,
        BtNodeKind::CanSeeTarget
        | BtNodeKind::CanHearTarget
        | BtNodeKind::HasTarget
        | BtNodeKind::TargetWithin { .. }
        | BtNodeKind::Variable(_)
        | BtNodeKind::Chance { .. } => palette.purple,
        _ => palette.success,
    }
}

/// Adds a node of `kind` under `selected` (or next to it, when it takes no
/// children) and returns the new node's index.
pub(super) fn add_node(
    tree: &mut BehaviourTreeData,
    selected: usize,
    kind: BtNodeKind,
) -> Option<usize> {
    if tree.nodes.len() >= MAX_BT_NODES || selected >= tree.nodes.len() {
        return None;
    }
    let (parent, at) =
        if tree.nodes[selected].children.len() < tree.nodes[selected].kind.max_children() {
            (selected, tree.nodes[selected].children.len())
        } else {
            let parent = tree.parent_of(selected)?;
            let position = tree.nodes[parent]
                .children
                .iter()
                .position(|&child| child == selected)?;
            if tree.nodes[parent].children.len() >= tree.nodes[parent].kind.max_children() {
                return None;
            }
            (parent, position + 1)
        };
    let index = tree.nodes.len();
    tree.nodes.push(BtNodeData::new(kind));
    tree.nodes[parent].children.insert(at, index);
    Some(index)
}

/// Moves node `index` one place earlier (`-1`) or later (`1`) among its
/// siblings, which changes the order they run in.
pub(super) fn move_among_siblings(tree: &mut BehaviourTreeData, index: usize, step: isize) {
    let Some(parent) = tree.parent_of(index) else {
        return;
    };
    let siblings = &mut tree.nodes[parent].children;
    let Some(at) = siblings.iter().position(|&child| child == index) else {
        return;
    };
    let to = at as isize + step;
    if (0..siblings.len() as isize).contains(&to) {
        siblings.swap(at, to as usize);
    }
}

/// Draws the window while it is open. `clips` are the model's clip names;
/// `here` is the object's position, for adding patrol points.
pub(super) fn show(
    ctx: &egui::Context,
    entity: Entity,
    tree: &mut BehaviourTreeData,
    clips: &[String],
    here: [f32; 3],
) {
    let id = window_id(entity);
    let mut visible = ctx.data(|data| data.get_temp::<bool>(id).unwrap_or(false));
    if !visible || tree.nodes.is_empty() {
        return;
    }
    let selected_id = id.with("selected");
    let pan_id = id.with("pan");
    let mut selected = ctx
        .data(|data| data.get_temp::<usize>(selected_id))
        .filter(|index| *index < tree.nodes.len())
        .unwrap_or(0);
    let mut pan = ctx
        .data(|data| data.get_temp::<egui::Vec2>(pan_id))
        .unwrap_or(egui::Vec2::ZERO);

    egui::Window::new("Behaviour tree")
        .id(id)
        .open(&mut visible)
        .default_size([780.0, 660.0])
        .min_width(620.0)
        .frame(
            egui::Frame::NONE
                .fill(theme::NIGHT.panel)
                .stroke(egui::Stroke::new(1.0, theme::NIGHT.rule))
                .corner_radius(egui::CornerRadius::same(10))
                .inner_margin(egui::Margin::same(14)),
        )
        .show(ctx, |ui| {
            ui.label("Runs from the top every frame. A Selector tries its children left to right until one works; a Sequence runs them in order until one fails.");
            ui.weak("Drag nodes to arrange them; drag empty space to pan.");
            ui.add_space(10.0);
            toolbar(ui, tree, &mut selected, &mut pan);
            ui.add_space(10.0);
            canvas(ui, id, tree, &mut selected, &mut pan);
            ui.add_space(10.0);
            egui::ScrollArea::vertical()
                .id_salt(id.with("details"))
                .max_height(240.0)
                .show(ui, |ui| node_fields(ui, entity, tree, selected, clips, here));
        });
    ctx.data_mut(|data| {
        data.insert_temp(id, visible);
        data.insert_temp(pan_id, pan);
        data.insert_temp(selected_id, selected);
    });
}

fn toolbar(
    ui: &mut egui::Ui,
    tree: &mut BehaviourTreeData,
    selected: &mut usize,
    pan: &mut egui::Vec2,
) {
    ui.horizontal(|ui| {
        let can_add = tree.nodes.len() < MAX_BT_NODES;
        ui.add_enabled_ui(can_add, |ui| {
            ui.menu_button("+ Node", |ui| {
                crate::theme::style_context_menu(ui);
                for kind in BtNodeKind::palette() {
                    let label = kind.label();
                    if ui.button(label).clicked() {
                        if let Some(index) = add_node(tree, *selected, kind) {
                            *selected = index;
                        }
                        ui.close();
                    }
                }
            });
        });
        let not_root = *selected != 0;
        if ui
            .add_enabled(not_root, egui::Button::new("◀"))
            .on_hover_text("Run earlier: move left among its siblings")
            .clicked()
        {
            move_among_siblings(tree, *selected, -1);
        }
        if ui
            .add_enabled(not_root, egui::Button::new("▶"))
            .on_hover_text("Run later: move right among its siblings")
            .clicked()
        {
            move_among_siblings(tree, *selected, 1);
        }
        if ui
            .add_enabled(
                not_root,
                egui::Button::new(egui::RichText::new("Delete").color(theme::NIGHT.danger)),
            )
            .on_hover_text("Delete this node and everything under it")
            .clicked()
        {
            let parent = tree.parent_of(*selected).unwrap_or(0);
            tree.remove_subtree(*selected);
            *selected = parent.min(tree.nodes.len().saturating_sub(1));
        }
        ui.separator();
        if ui
            .button("Tidy")
            .on_hover_text("Put every node back in the automatic layout")
            .clicked()
        {
            for node in &mut tree.nodes {
                node.graph_position = None;
            }
        }
        if ui.button("Reset view").clicked() {
            *pan = egui::Vec2::ZERO;
        }
    });
}

fn canvas(
    ui: &mut egui::Ui,
    id: egui::Id,
    tree: &mut BehaviourTreeData,
    selected: &mut usize,
    pan: &mut egui::Vec2,
) {
    let palette = theme::NIGHT;
    let (rect, background) = ui.allocate_exact_size(
        egui::vec2(ui.available_width().max(580.0), CANVAS_HEIGHT),
        egui::Sense::click_and_drag(),
    );
    if background.dragged() {
        *pan += background.drag_delta();
    }
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, egui::CornerRadius::same(8), palette.shell);
    let origin = rect.min + *pan;
    let auto = auto_layout(tree);
    let node_rect = |index: usize| {
        egui::Rect::from_min_size(
            origin + node_position(tree, &auto, index).to_vec2(),
            NODE_SIZE,
        )
    };

    let mut run_order = Vec::new();
    for (index, data) in tree.nodes.iter().enumerate() {
        let from = node_rect(index);
        for (order, &child) in data.children.iter().enumerate() {
            if child >= tree.nodes.len() {
                continue;
            }
            let to = node_rect(child);
            arrow(
                &painter,
                from.center_bottom(),
                to.center_top(),
                palette.subtle,
            );
            // The run order, in the child's corner, when there is more than one.
            if data.children.len() > 1 {
                run_order.push((to.right_top(), order + 1));
            }
        }
    }

    let mut moves = Vec::new();
    for index in 0..tree.nodes.len() {
        let label = node_label(&tree.nodes[index].kind);
        let Some(response) = node(
            ui,
            &painter,
            rect,
            node_rect(index),
            id.with(("node", index)),
            &label,
            *selected == index,
            accent(&tree.nodes[index].kind),
        ) else {
            continue;
        };
        if response.dragged() {
            moves.push((index, response.drag_delta()));
        }
        if response.clicked() || response.drag_started() {
            *selected = index;
        }
    }
    for (corner, order) in run_order {
        painter.text(
            corner + egui::vec2(-10.0, 12.0),
            egui::Align2::RIGHT_CENTER,
            format!("{order}"),
            egui::FontId::proportional(11.0),
            palette.subtle,
        );
    }
    for (index, delta) in moves {
        let at = node_position(tree, &auto, index) + delta;
        tree.nodes[index].graph_position = Some([at.x.round(), at.y.round()]);
    }
}

fn seconds_field(ui: &mut egui::Ui, label: &str, value: &mut f32) {
    property_row(ui, label, |ui| {
        number(
            ui,
            egui::DragValue::new(value)
                .speed(0.05)
                .range(0.0..=3_600.0)
                .suffix(" s"),
        );
    });
}

fn node_fields(
    ui: &mut egui::Ui,
    entity: Entity,
    tree: &mut BehaviourTreeData,
    index: usize,
    clips: &[String],
    here: [f32; 3],
) {
    let children = tree.nodes[index].children.len();
    let kind = &mut tree.nodes[index].kind;
    ui.label(theme::semibold(if index == 0 {
        format!("{} (start)", kind.label())
    } else {
        kind.label().to_owned()
    }));
    match kind {
        BtNodeKind::Sequence | BtNodeKind::Selector | BtNodeKind::Inverter => {
            property_row(ui, "Kind", |ui| {
                choice(("bt_composite", entity, index), ui)
                    .selected_text(kind.label())
                    .show_ui(ui, |ui| {
                        for candidate in [
                            BtNodeKind::Sequence,
                            BtNodeKind::Selector,
                            BtNodeKind::Inverter,
                        ] {
                            let fits = children <= candidate.max_children();
                            let label = candidate.label();
                            if ui
                                .add_enabled(
                                    fits,
                                    egui::Button::selectable(*kind == candidate, label),
                                )
                                .clicked()
                            {
                                *kind = candidate;
                            }
                        }
                    });
            });
            note(
                ui,
                match kind {
                    BtNodeKind::Sequence => {
                        "Runs its children in order and stops at the first that fails."
                    }
                    BtNodeKind::Selector => {
                        "Tries its children in order and stops at the first that works."
                    }
                    _ => "Runs its one child and turns works into fails and fails into works.",
                },
            );
            if children == 0 {
                note(ui, "Select it and use + Node to give it children.");
            }
        }
        BtNodeKind::CanSeeTarget => note(ui, "Works while its senses see the target."),
        BtNodeKind::CanHearTarget => note(ui, "Works while its senses hear the target."),
        BtNodeKind::HasTarget => note(ui, "Works from noticing a target until it is lost."),
        BtNodeKind::TargetWithin { distance } => {
            property_row(ui, "Within", |ui| {
                number(
                    ui,
                    egui::DragValue::new(distance)
                        .speed(0.05)
                        .range(0.0..=engine_scene::MAX_AI_DISTANCE),
                );
            });
        }
        BtNodeKind::Variable(condition) => {
            property_row(ui, "Variable", |ui| {
                crate::widgets::text_field(ui, &mut condition.variable, "health");
            });
            property_row(ui, "Is", |ui| {
                choice(("bt_compare", entity, index), ui)
                    .selected_text(condition.compare.symbol())
                    .show_ui(ui, |ui| {
                        for compare in RuleCompare::ALL {
                            ui.selectable_value(&mut condition.compare, compare, compare.symbol());
                        }
                    });
            });
            property_row(ui, "Value", |ui| {
                number(ui, egui::DragValue::new(&mut condition.value).speed(0.1));
            });
        }
        BtNodeKind::Chance { percent } => {
            property_row(ui, "Chance", |ui| {
                number(
                    ui,
                    egui::DragValue::new(percent)
                        .speed(1.0)
                        .range(0.0..=100.0)
                        .suffix("%"),
                );
            });
        }
        BtNodeKind::MoveToTarget { speed } => {
            let mut own = speed.is_some();
            property_row(ui, "Own speed", |ui| {
                crate::widgets::checkbox(ui, &theme::NIGHT, &mut own, "")
                    .on_hover_text("Off uses the Walking speed");
            });
            if own != speed.is_some() {
                *speed = own.then_some(5.0);
            }
            if let Some(speed) = speed {
                property_row(ui, "Speed", |ui| {
                    number(
                        ui,
                        egui::DragValue::new(speed)
                            .speed(0.05)
                            .range(0.0..=engine_scene::MAX_AI_DISTANCE),
                    );
                });
            }
            note(ui, "Keeps running until it reaches the target.");
        }
        BtNodeKind::Patrol { points } => {
            super::ai::patrol_points(ui, entity, points, here);
            if points.len() >= MAX_PATROL_POINTS {
                note(ui, "That is the most points one patrol can have.");
            }
            note(
                ui,
                "Walks to the next point each time it runs, and works on arrival.",
            );
        }
        BtNodeKind::MoveTo { position } => {
            property_row(ui, "Position", |ui| {
                for (axis, component) in ["X", "Y", "Z"].into_iter().zip(position.iter_mut()) {
                    number(
                        ui,
                        egui::DragValue::new(component)
                            .speed(0.1)
                            .prefix(format!("{axis} ")),
                    );
                }
            });
        }
        BtNodeKind::Wait { seconds } => seconds_field(ui, "Wait", seconds),
        BtNodeKind::PlayAnimation { clip } => {
            property_row(ui, "Clip", |ui| {
                if clips.is_empty() {
                    crate::widgets::text_field(ui, clip, "Clip name");
                } else {
                    choice(("bt_clip", entity, index), ui)
                        .selected_text(clip.as_str())
                        .show_ui(ui, |ui| {
                            for name in clips {
                                ui.selectable_value(clip, name.clone(), name);
                            }
                        });
                }
            });
        }
        BtNodeKind::SendEvent { name } => {
            property_row(ui, "Event", |ui| {
                crate::widgets::text_field(ui, name, "Event name");
            });
            note(
                ui,
                "Named event rules and scripts' on_event can react to it.",
            );
        }
        BtNodeKind::SetVariable { name, value } => {
            property_row(ui, "Variable", |ui| {
                crate::widgets::text_field(ui, name, "Variable name");
            });
            property_row(ui, "To", |ui| {
                number(ui, egui::DragValue::new(value).speed(0.1));
            });
        }
        BtNodeKind::FaceTarget => note(ui, "Turns toward the target; fails without one."),
        BtNodeKind::Stop => note(ui, "Stops walking."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_automatic_layout_puts_children_below_and_parents_over_them() {
        let tree = BehaviourTreeData::default();
        let at = auto_layout(&tree);
        assert!(at[1].y > at[0].y && at[2].y > at[1].y);
        assert!(at[2].x < at[3].x, "children run left to right");
        assert!((at[1].x - (at[2].x + at[3].x) / 2.0).abs() < 1e-3);
    }

    #[test]
    fn adding_goes_under_composites_and_beside_actions() {
        let mut tree = BehaviourTreeData::default();
        let child = add_node(&mut tree, 1, BtNodeKind::Stop).expect("under the sequence");
        assert_eq!(tree.nodes[1].children.last(), Some(&child));
        let sibling = add_node(&mut tree, 4, BtNodeKind::FaceTarget).expect("beside wait");
        assert_eq!(tree.nodes[0].children, vec![1, 4, sibling]);
        tree.validate().expect("still a tree");
    }

    #[test]
    fn moving_among_siblings_changes_run_order() {
        let mut tree = BehaviourTreeData::default();
        move_among_siblings(&mut tree, 4, -1);
        assert_eq!(tree.nodes[0].children, vec![4, 1]);
        move_among_siblings(&mut tree, 4, -1);
        assert_eq!(tree.nodes[0].children, vec![4, 1]);
        move_among_siblings(&mut tree, 0, 1);
        assert_eq!(tree.nodes[0].children, vec![4, 1]);
    }
}
