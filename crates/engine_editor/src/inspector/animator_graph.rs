//! The Animator window: an animated model's states as nodes, its
//! transitions as arrows, and the fields of whatever is selected below.
//!
//! The animator lives in the model's scene settings, so every change here
//! is an ordinary scene edit: it saves with the scene and ⌘Z undoes it.

use engine_ecs::prelude::Entity;
use engine_scene::{
    AnimatorData, AnimatorMotion, AnimatorStateData, AnimatorTransitionData, BlendClipData,
    MAX_ANIMATOR_STATES, MAX_ANIMATOR_TRANSITIONS, MAX_BLEND_CLIPS, MAX_TRANSITION_FADE,
};

use super::{choice, condition_fields, note, number, property_row};
use crate::theme;

/// Size of a state node on the canvas.
const NODE_SIZE: egui::Vec2 = egui::vec2(160.0, 44.0);
/// Height of the visible canvas.
const CANVAS_HEIGHT: f32 = 300.0;
/// Where the "Any state" node sits, in graph space.
const ANY_POSITION: egui::Pos2 = egui::pos2(24.0, 24.0);

/// What is selected in the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Selected {
    /// The "Any state" node: transitions that can leave every state.
    Any,
    /// A state, by index.
    State(usize),
}

fn window_id(entity: Entity) -> egui::Id {
    egui::Id::new(("animator_graph", entity))
}

/// Opens the window for `entity`'s animator.
pub(super) fn open(ctx: &egui::Context, entity: Entity) {
    ctx.data_mut(|data| data.insert_temp(window_id(entity), true));
}

/// Selects state `index` in `entity`'s window.
#[cfg(test)]
pub(super) fn select_state(ctx: &egui::Context, entity: Entity, index: usize) {
    ctx.data_mut(|data| {
        data.insert_temp(window_id(entity).with("selected"), Selected::State(index));
    });
}

/// Where state `index` is drawn, in graph space.
fn state_position(animator: &AnimatorData, index: usize) -> egui::Pos2 {
    let [x, y] = animator.states[index]
        .graph_position
        .unwrap_or_else(|| engine_scene::default_animator_position(index));
    egui::pos2(x, y)
}

/// A state name not used yet: `base`, `base 2`, `base 3`…
fn free_name(animator: &AnimatorData, base: &str) -> String {
    std::iter::once(base.to_owned())
        .chain((2..).map(|n| format!("{base} {n}")))
        .find(|name| animator.state_index(name).is_none())
        .unwrap_or_default()
}

/// The point where the segment from `from` toward `to` leaves `rect`.
fn edge_point(rect: egui::Rect, to: egui::Pos2) -> egui::Pos2 {
    let center = rect.center();
    let direction = to - center;
    if direction.length_sq() < 1.0 {
        return center;
    }
    let half = rect.size() / 2.0;
    let scale = (half.x / direction.x.abs().max(1e-3)).min(half.y / direction.y.abs().max(1e-3));
    center + direction * scale.min(1.0)
}

pub(super) fn arrow(
    painter: &egui::Painter,
    from: egui::Pos2,
    to: egui::Pos2,
    color: egui::Color32,
) {
    let stroke = egui::Stroke::new(2.0, color);
    painter.line_segment([from, to], stroke);
    let direction = (to - from).normalized();
    if !direction.x.is_finite() {
        return;
    }
    let side = egui::vec2(-direction.y, direction.x);
    let tip = to;
    let back = tip - direction * 9.0;
    painter.add(egui::Shape::convex_polygon(
        vec![tip, back + side * 5.0, back - side * 5.0],
        color,
        egui::Stroke::NONE,
    ));
}

/// Draws the window while it is open. `clips` are the model's clip names.
pub(super) fn show(
    ctx: &egui::Context,
    entity: Entity,
    animator: &mut AnimatorData,
    clips: &[String],
) {
    let id = window_id(entity);
    let mut visible = ctx.data(|data| data.get_temp::<bool>(id).unwrap_or(false));
    if !visible {
        return;
    }
    let selected_id = id.with("selected");
    let connect_id = id.with("connect_from");
    let pan_id = id.with("pan");
    let mut selected = ctx
        .data(|data| data.get_temp::<Selected>(selected_id))
        .filter(|selected| match selected {
            Selected::Any => true,
            Selected::State(index) => *index < animator.states.len(),
        });
    // A transition being drawn: its source, waiting for a target click.
    let mut connect_from = ctx
        .data(|data| data.get_temp::<Option<Selected>>(connect_id))
        .flatten();
    let mut pan = ctx
        .data(|data| data.get_temp::<egui::Vec2>(pan_id))
        .unwrap_or(egui::Vec2::ZERO);

    egui::Window::new("Animator")
        .id(id)
        .open(&mut visible)
        .default_size([760.0, 640.0])
        .min_width(600.0)
        .frame(
            egui::Frame::NONE
                .fill(theme::NIGHT.panel)
                .stroke(egui::Stroke::new(1.0, theme::NIGHT.rule))
                .corner_radius(egui::CornerRadius::same(10))
                .inner_margin(egui::Margin::same(14)),
        )
        .show(ctx, |ui| {
            ui.label("The first state plays when the game starts. Transitions move to another state when their conditions hold.");
            ui.weak("Drag states to arrange them; drag empty space to pan.");
            ui.add_space(10.0);
            toolbar(ui, animator, clips, &mut selected, &mut connect_from, &mut pan);
            ui.add_space(10.0);
            canvas(ui, id, animator, &mut selected, &mut connect_from, &mut pan);
            ui.add_space(10.0);
            egui::ScrollArea::vertical()
                .id_salt(id.with("details"))
                .max_height(260.0)
                .show(ui, |ui| match selected {
                    Some(Selected::State(index)) => {
                        state_fields(ui, entity, animator, index, clips, &mut selected);
                    }
                    Some(Selected::Any) => {
                        ui.label(theme::semibold("Any state"));
                        note(ui, "These transitions can leave whichever state is playing.");
                        transition_list(ui, entity, animator, None);
                    }
                    None => note(ui, "Select a state to edit it."),
                });
        });
    ctx.data_mut(|data| {
        data.insert_temp(id, visible);
        data.insert_temp(pan_id, pan);
        data.insert_temp(connect_id, connect_from);
        match selected {
            Some(selected) => {
                data.insert_temp(selected_id, selected);
            }
            None => data.remove::<Selected>(selected_id),
        }
    });
}

fn toolbar(
    ui: &mut egui::Ui,
    animator: &mut AnimatorData,
    clips: &[String],
    selected: &mut Option<Selected>,
    connect_from: &mut Option<Selected>,
    pan: &mut egui::Vec2,
) {
    ui.horizontal(|ui| {
        let can_add = animator.states.len() < MAX_ANIMATOR_STATES;
        if ui
            .add_enabled(can_add && !clips.is_empty(), egui::Button::new("+ State"))
            .clicked()
        {
            let clip = clips.first().cloned().unwrap_or_default();
            let name = free_name(animator, "state");
            let index = animator.states.len();
            animator.states.push(AnimatorStateData {
                graph_position: Some(engine_scene::default_animator_position(index)),
                ..AnimatorStateData::clip(name, clip)
            });
            *selected = Some(Selected::State(index));
        }
        let can_connect =
            selected.is_some() && animator.transitions.len() < MAX_ANIMATOR_TRANSITIONS;
        if ui
            .add_enabled(can_connect, egui::Button::new("+ Transition"))
            .on_hover_text("Then click the state it goes to")
            .clicked()
        {
            *connect_from = *selected;
        }
        ui.separator();
        if ui.button("Reset view").clicked() {
            *pan = egui::Vec2::ZERO;
        }
        if let Some(from) = connect_from {
            ui.label(
                egui::RichText::new(match from {
                    Selected::Any => "Click the state to go to from any state".to_owned(),
                    Selected::State(index) => format!(
                        "Click the state to go to from {}",
                        animator
                            .states
                            .get(*index)
                            .map_or("", |state| state.name.as_str())
                    ),
                })
                .color(theme::NIGHT.accent),
            );
            if ui.small_button("Cancel").clicked() {
                *connect_from = None;
            }
        }
    });
}

fn canvas(
    ui: &mut egui::Ui,
    id: egui::Id,
    animator: &mut AnimatorData,
    selected: &mut Option<Selected>,
    connect_from: &mut Option<Selected>,
    pan: &mut egui::Vec2,
) {
    let palette = theme::NIGHT;
    let (rect, background) = ui.allocate_exact_size(
        egui::vec2(ui.available_width().max(560.0), CANVAS_HEIGHT),
        egui::Sense::click_and_drag(),
    );
    if background.dragged() {
        *pan += background.drag_delta();
    }
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, egui::CornerRadius::same(8), palette.shell);
    let origin = rect.min + *pan;
    let node_rect =
        |position: egui::Pos2| egui::Rect::from_min_size(origin + position.to_vec2(), NODE_SIZE);
    let any_rect = node_rect(ANY_POSITION);
    let state_rect =
        |animator: &AnimatorData, index: usize| node_rect(state_position(animator, index));

    for transition in &animator.transitions {
        let Some(to) = animator.state_index(&transition.to) else {
            continue;
        };
        let from_rect = match &transition.from {
            Some(from) => match animator.state_index(from) {
                Some(index) => state_rect(animator, index),
                None => continue,
            },
            None => any_rect,
        };
        let to_rect = state_rect(animator, to);
        // Two-way pairs get a small sideways offset so both arrows show.
        let direction = (to_rect.center() - from_rect.center()).normalized();
        let offset = egui::vec2(-direction.y, direction.x) * 6.0;
        let start = edge_point(from_rect, to_rect.center()) + offset;
        let end = edge_point(to_rect, from_rect.center()) + offset;
        arrow(&painter, start, end, palette.accent);
    }

    let mut clicked = None;
    let any_used = animator
        .transitions
        .iter()
        .any(|transition| transition.from.is_none());
    let any_selected = *selected == Some(Selected::Any);
    if (any_used || any_selected || connect_from.is_some())
        && let Some(response) = node(
            ui,
            &painter,
            rect,
            any_rect,
            id.with("any"),
            "Any state",
            any_selected,
            palette.subtle,
        )
        && response.clicked()
    {
        clicked = Some(Selected::Any);
    }
    let mut moves = Vec::new();
    for index in 0..animator.states.len() {
        let label = if index == 0 {
            format!("▶ {}", animator.states[index].name)
        } else {
            animator.states[index].name.clone()
        };
        let accent = if index == 0 {
            palette.accent
        } else {
            palette.subtle
        };
        let Some(response) = node(
            ui,
            &painter,
            rect,
            state_rect(animator, index),
            id.with(("state", index)),
            &label,
            *selected == Some(Selected::State(index)),
            accent,
        ) else {
            continue;
        };
        if response.dragged() {
            moves.push((index, response.drag_delta()));
        }
        if response.clicked() {
            clicked = Some(Selected::State(index));
        }
    }
    for (index, delta) in moves {
        let at = state_position(animator, index) + delta;
        animator.states[index].graph_position = Some([at.x.round(), at.y.round()]);
    }
    if let Some(target) = clicked {
        if let (Some(from), Selected::State(to)) = (connect_from.take(), target) {
            let from = match from {
                Selected::Any => None,
                Selected::State(index) => {
                    animator.states.get(index).map(|state| state.name.clone())
                }
            };
            let to_name = animator.states[to].name.clone();
            if from.as_deref() != Some(to_name.as_str()) {
                animator
                    .transitions
                    .push(AnimatorTransitionData::new(from, to_name));
            }
        }
        *selected = Some(target);
    }
}

/// Paints a node and returns its response; nothing off the canvas reacts.
#[allow(
    clippy::too_many_arguments,
    reason = "a node's place, identity and look, kept flat like graph::node"
)]
pub(super) fn node(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    canvas: egui::Rect,
    rect: egui::Rect,
    id: egui::Id,
    label: &str,
    selected: bool,
    accent: egui::Color32,
) -> Option<egui::Response> {
    let palette = theme::NIGHT;
    let visible = rect.intersect(canvas);
    if !visible.is_positive() {
        return None;
    }
    let response = ui.interact(visible, id, egui::Sense::click_and_drag());
    let fill = if selected {
        palette.accent_fill
    } else if response.hovered() || response.dragged() {
        palette.hover
    } else {
        palette.panel
    };
    painter.rect_filled(rect, egui::CornerRadius::same(8), fill);
    painter.rect_filled(
        egui::Rect::from_min_size(rect.min, egui::vec2(4.0, rect.height())),
        egui::CornerRadius {
            nw: 8,
            sw: 8,
            ne: 0,
            se: 0,
        },
        accent,
    );
    painter.text(
        rect.left_center() + egui::vec2(16.0, 0.0),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(13.0),
        palette.text,
    );
    Some(response)
}

fn clip_choice(
    ui: &mut egui::Ui,
    salt: impl std::hash::Hash + std::fmt::Debug,
    clip: &mut String,
    clips: &[String],
    width: Option<f32>,
) {
    let mut combo = choice(salt, ui);
    if let Some(width) = width {
        combo = combo.width(width);
    }
    combo.selected_text(clip.as_str()).show_ui(ui, |ui| {
        for name in clips {
            ui.selectable_value(clip, name.clone(), name);
        }
    });
}

fn state_fields(
    ui: &mut egui::Ui,
    entity: Entity,
    animator: &mut AnimatorData,
    index: usize,
    clips: &[String],
    selected: &mut Option<Selected>,
) {
    let palette = theme::NIGHT;
    let mut remove = false;
    let mut make_start = false;
    ui.horizontal(|ui| {
        ui.label(theme::semibold(if index == 0 {
            "Start state"
        } else {
            "State"
        }));
        if index != 0 && ui.small_button("Make start state").clicked() {
            make_start = true;
        }
        if ui
            .add(
                egui::Button::new(egui::RichText::new("Remove state").color(palette.danger))
                    .frame(false),
            )
            .clicked()
        {
            remove = true;
        }
    });
    let mut name = animator.states[index].name.clone();
    property_row(ui, "Name", |ui| {
        crate::widgets::text_field(ui, &mut name, "State name");
    });
    if name != animator.states[index].name {
        // Rejected names (blank, taken) simply don't apply.
        animator.rename_state(index, &name);
    }
    let state = &mut animator.states[index];
    let mut blend = matches!(state.motion, AnimatorMotion::Blend { .. });
    property_row(ui, "Plays", |ui| {
        super::on_off_choice(
            ui,
            (entity, 20_000 + index),
            &mut blend,
            "Blend by a number",
            "One clip",
        );
    });
    match (&mut state.motion, blend) {
        (AnimatorMotion::Clip(clip), true) => {
            state.motion = AnimatorMotion::Blend {
                variable: "speed".into(),
                clips: vec![BlendClipData {
                    clip: clip.clone(),
                    at: 0.0,
                }],
            };
        }
        (AnimatorMotion::Blend { clips: mixed, .. }, false) => {
            let first = mixed
                .first()
                .map(|blend| blend.clip.clone())
                .unwrap_or_default();
            state.motion = AnimatorMotion::Clip(first);
        }
        _ => {}
    }
    match &mut state.motion {
        AnimatorMotion::Clip(clip) => {
            property_row(ui, "Clip", |ui| {
                clip_choice(ui, ("animator_clip", entity, index), clip, clips, None)
            });
        }
        AnimatorMotion::Blend {
            variable,
            clips: mixed,
        } => {
            property_row(ui, "Number", |ui| {
                crate::widgets::text_field(ui, variable, "Variable, e.g. speed").on_hover_text(
                    "Set per object with engine.set_animator_value, or a shared game variable",
                );
            });
            let mut remove_clip = None;
            for (position, entry) in mixed.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    clip_choice(
                        ui,
                        ("animator_blend", entity, index, position),
                        &mut entry.clip,
                        clips,
                        Some(160.0),
                    );
                    ui.label("at");
                    number(ui, egui::DragValue::new(&mut entry.at).speed(0.05));
                    if ui
                        .button(egui::RichText::new("Remove").color(palette.danger))
                        .clicked()
                    {
                        remove_clip = Some(position);
                    }
                });
            }
            if let Some(position) = remove_clip.filter(|_| mixed.len() > 1) {
                mixed.remove(position);
            }
            if mixed.len() < MAX_BLEND_CLIPS && ui.button("Add clip to blend").clicked() {
                let at = mixed.last().map_or(0.0, |last| last.at + 1.0);
                let clip = clips.first().cloned().unwrap_or_default();
                mixed.push(BlendClipData { clip, at });
            }
        }
    }
    property_row(ui, "Speed", |ui| {
        number(
            ui,
            egui::DragValue::new(&mut state.speed)
                .speed(0.05)
                .range(-10.0..=10.0)
                .suffix("×"),
        );
    });
    property_row(ui, "Loop", |ui| {
        crate::widgets::checkbox(ui, &palette, &mut state.looping, "");
    });
    let from = Some(animator.states[index].name.clone());
    ui.add_space(6.0);
    ui.label(theme::semibold("Transitions from here"));
    transition_list(ui, entity, animator, from);

    if make_start && index < animator.states.len() {
        let state = animator.states.remove(index);
        animator.states.insert(0, state);
        *selected = Some(Selected::State(0));
    }
    if remove {
        animator.remove_state(index);
        *selected = None;
        if animator.states.is_empty() {
            let clip = clips.first().cloned().unwrap_or_default();
            animator
                .states
                .push(AnimatorStateData::clip(clip.clone(), clip));
        }
    }
}

/// The transitions leaving `from` (`None`: the "Any state" ones), each with
/// its fields.
fn transition_list(
    ui: &mut egui::Ui,
    entity: Entity,
    animator: &mut AnimatorData,
    from: Option<String>,
) {
    let palette = theme::NIGHT;
    let mut remove = None;
    let mut any = false;
    for (index, transition) in animator.transitions.iter_mut().enumerate() {
        if transition.from != from {
            continue;
        }
        any = true;
        ui.push_id(("animator_transition", index), |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(format!("→ {}", transition.to));
                if ui
                    .add(
                        egui::Button::new(egui::RichText::new("Remove").color(palette.danger))
                            .frame(false),
                    )
                    .clicked()
                {
                    remove = Some(index);
                }
            });
            property_row(ui, "Blend time", |ui| {
                number(
                    ui,
                    egui::DragValue::new(&mut transition.fade)
                        .speed(0.01)
                        .range(0.0..=MAX_TRANSITION_FADE)
                        .suffix(" s"),
                );
            });
            property_row(ui, "Wait for end", |ui| {
                crate::widgets::checkbox(ui, &palette, &mut transition.at_end, "")
                    .on_hover_text("Go only after the clip playing now reaches its end");
            });
            condition_fields(
                ui,
                (entity, 10_000 + index),
                &mut transition.conditions,
                &mut transition.match_mode,
                "Go only when a number matches",
                "speed",
            );
        });
    }
    if !any {
        note(
            ui,
            "None yet. Use + Transition, then click the state to go to.",
        );
    }
    if let Some(index) = remove {
        animator.transitions.remove(index);
    }
}
