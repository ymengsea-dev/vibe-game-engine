//! Node view over the same visual rules edited in the Inspector.

use engine_ecs::prelude::Entity;
use engine_scene::{RuleAction, RuleData, RuleTrigger, format_variable};

use super::{RuleActionKind, rule_fields};
use crate::theme;

pub(super) struct Controls<'a> {
    pub text_targets: &'a [(String, String)],
    pub audio_targets: &'a [(String, String)],
    pub owner_is_button: bool,
    pub can_add_rule: bool,
    pub add_rule: &'a mut Option<(RuleActionKind, Option<RuleTrigger>)>,
    pub remove_rule: &'a mut Option<usize>,
}

fn graph_id(entity: Entity) -> egui::Id {
    egui::Id::new(("gameplay_graph", entity))
}

pub(super) fn open(ctx: &egui::Context, entity: Entity) {
    ctx.data_mut(|data| data.insert_temp(graph_id(entity), true));
}

/// One event node per distinct trigger.
pub(super) fn event_nodes(rules: &[RuleData]) -> Vec<RuleTrigger> {
    let mut events = Vec::new();
    for rule in rules {
        if !events.contains(&rule.trigger) {
            events.push(rule.trigger.clone());
        }
    }
    events
}

/// Rewire one action to an existing event without changing its target or payload.
pub(super) fn connect(rules: &mut [RuleData], trigger: &RuleTrigger, target: usize) -> bool {
    if !rules.iter().any(|rule| &rule.trigger == trigger) {
        return false;
    }
    let Some(rule) = rules.get_mut(target) else {
        return false;
    };
    if &rule.trigger == trigger {
        return false;
    }
    rule.trigger = trigger.clone();
    true
}

fn action_label(
    rule: &RuleData,
    text_targets: &[(String, String)],
    audio_targets: &[(String, String)],
) -> String {
    let label = match &rule.action {
        RuleAction::SetUiText { target, .. } => {
            format!("Set UI text\n{}", target_name(target, text_targets))
        }
        RuleAction::PlayAudio { target } => {
            format!("Play sound\n{}", target_name(target, audio_targets))
        }
        RuleAction::EmitEvent { name } => format!("Emit event\n{name}"),
        RuleAction::SetVariable { name, value } => {
            format!("Set variable\n{name} = {}", format_variable(*value))
        }
        RuleAction::AddToVariable { name, amount } => {
            format!("Change variable\n{name} + {}", format_variable(*amount))
        }
    };
    match &rule.condition {
        Some(condition) => format!(
            "{label}\nif {} {} {}",
            condition.variable,
            condition.compare.symbol(),
            format_variable(condition.value)
        ),
        None => label,
    }
}

fn target_name<'a>(target: &str, candidates: &'a [(String, String)]) -> &'a str {
    candidates
        .iter()
        .find(|(id, _)| id == target)
        .map_or("Missing target", |(_, name)| name.as_str())
}

/// Graph-space size of every node.
const NODE_SIZE: egui::Vec2 = egui::vec2(220.0, 64.0);
/// Horizontal distance from an event node to its actions.
const COLUMN_GAP: f32 = 320.0;
/// Vertical spacing of actions in the automatic layout.
const ROW_GAP: f32 = 84.0;
/// Visible height of the graph canvas.
const CANVAS_HEIGHT: f32 = 320.0;

/// Where action `index` is drawn: its saved position, or the automatic column.
pub(super) fn action_position(rules: &[RuleData], index: usize) -> egui::Pos2 {
    rules
        .get(index)
        .and_then(|rule| rule.graph_position)
        .map_or_else(
            || egui::pos2(COLUMN_GAP, index as f32 * ROW_GAP),
            |[x, y]| egui::pos2(x, y),
        )
}

/// Where an event is drawn: one column left of its actions, centred on them.
/// Events store no position of their own; they follow their actions.
pub(super) fn event_position(rules: &[RuleData], trigger: &RuleTrigger) -> egui::Pos2 {
    let positions: Vec<_> = (0..rules.len())
        .filter(|index| &rules[*index].trigger == trigger)
        .map(|index| action_position(rules, index))
        .collect();
    let Some(first) = positions.first() else {
        return egui::Pos2::ZERO;
    };
    let left = positions
        .iter()
        .fold(first.x, |left, position| left.min(position.x));
    let middle = positions.iter().map(|position| position.y).sum::<f32>() / positions.len() as f32;
    egui::pos2(left - COLUMN_GAP, middle)
}

/// Moves one action node, saving its position on the rule.
pub(super) fn move_action(rules: &mut [RuleData], index: usize, delta: egui::Vec2) {
    let moved = action_position(rules, index) + delta;
    if let Some(rule) = rules.get_mut(index)
        && moved.x.is_finite()
        && moved.y.is_finite()
    {
        let clamp = |value: f32| value.clamp(-1_000_000.0, 1_000_000.0);
        rule.graph_position = Some([clamp(moved.x), clamp(moved.y)]);
    }
}

/// Moves an event node together with every action it runs.
pub(super) fn move_event(rules: &mut [RuleData], trigger: &RuleTrigger, delta: egui::Vec2) {
    for index in 0..rules.len() {
        if &rules[index].trigger == trigger {
            move_action(rules, index, delta);
        }
    }
}

/// Forgets saved positions so the graph returns to its automatic layout.
pub(super) fn reset_layout(rules: &mut [RuleData]) {
    for rule in rules {
        rule.graph_position = None;
    }
}

fn event_label(trigger: &RuleTrigger) -> String {
    match trigger {
        RuleTrigger::UiClicked => "When\nbutton clicked".to_owned(),
        RuleTrigger::Named(name) => format!("When event\n{name}"),
    }
}

enum NodeMove {
    Event(RuleTrigger, egui::Vec2),
    Action(usize, egui::Vec2),
}

/// The visible graph area and the painter clipped to it.
struct Canvas {
    rect: egui::Rect,
    painter: egui::Painter,
}

/// How a node is drawn.
struct NodeLook<'a> {
    label: &'a str,
    selected: bool,
    accent: egui::Color32,
}

/// Paints one node and returns its click/drag response; nothing outside the
/// visible canvas is interactive.
fn node(
    ui: &mut egui::Ui,
    canvas: &Canvas,
    rect: egui::Rect,
    id: egui::Id,
    look: NodeLook<'_>,
) -> Option<egui::Response> {
    let visible = rect.intersect(canvas.rect);
    if !visible.is_positive() {
        return None;
    }
    let response = ui.interact(visible, id, egui::Sense::click_and_drag());
    let fill = if look.selected {
        theme::NIGHT.accent_fill
    } else if response.hovered() || response.dragged() {
        theme::NIGHT.hover
    } else {
        theme::NIGHT.panel
    };
    let stroke = if look.selected {
        egui::Stroke::new(1.5, theme::NIGHT.accent)
    } else {
        egui::Stroke::new(1.0, theme::NIGHT.border)
    };
    let painter = &canvas.painter;
    painter.rect(
        rect,
        egui::CornerRadius::same(8),
        fill,
        stroke,
        egui::StrokeKind::Inside,
    );
    painter.rect_filled(
        egui::Rect::from_min_size(rect.min, egui::vec2(4.0, rect.height())),
        egui::CornerRadius {
            nw: 8,
            sw: 8,
            ne: 0,
            se: 0,
        },
        look.accent,
    );
    painter.text(
        rect.left_center() + egui::vec2(16.0, 0.0),
        egui::Align2::LEFT_CENTER,
        look.label,
        egui::FontId::proportional(13.0),
        theme::NIGHT.text,
    );
    Some(response)
}

pub(super) fn show(
    ctx: &egui::Context,
    entity: Entity,
    rules: &mut [RuleData],
    controls: Controls<'_>,
) {
    let Controls {
        text_targets,
        audio_targets,
        owner_is_button,
        can_add_rule,
        add_rule,
        remove_rule,
    } = controls;
    let id = graph_id(entity);
    let mut visible = ctx.data(|data| data.get_temp::<bool>(id).unwrap_or(false));
    if !visible {
        return;
    }
    let selected_id = id.with("selected_event");
    let mut selected_event = ctx.data(|data| data.get_temp::<RuleTrigger>(selected_id));
    let selected_action_id = id.with("selected_action");
    let mut selected_action = ctx
        .data(|data| data.get_temp::<usize>(selected_action_id))
        .filter(|index| *index < rules.len());
    if !selected_event
        .as_ref()
        .is_some_and(|event| event_nodes(rules).contains(event))
    {
        selected_event = None;
    }
    let pan_id = id.with("pan");
    let mut pan = ctx
        .data(|data| data.get_temp::<egui::Vec2>(pan_id))
        .unwrap_or_else(|| egui::vec2(24.0, 24.0));
    let mut pending_connection = None;
    let mut moves = Vec::new();
    let mut relayout = false;
    egui::Window::new("Gameplay Graph")
        .id(id)
        .open(&mut visible)
        .default_size([760.0, 520.0])
        .min_width(600.0)
        .frame(
            egui::Frame::NONE
                .fill(theme::NIGHT.panel)
                .stroke(egui::Stroke::new(1.0, theme::NIGHT.rule))
                .corner_radius(egui::CornerRadius::same(10))
                .inner_margin(egui::Margin::same(14)),
        )
        .show(ctx, |ui| {
            ui.label("Click an event, then an action to connect them.");
            ui.weak(
                "Drag nodes to arrange them; drag empty space to pan. Layout saves with the scene.",
            );
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        can_add_rule && !text_targets.is_empty(),
                        egui::Button::new("+ Text action"),
                    )
                    .clicked()
                {
                    *add_rule = Some((RuleActionKind::Text, selected_event.clone()));
                }
                if ui
                    .add_enabled(
                        can_add_rule && !audio_targets.is_empty(),
                        egui::Button::new("+ Play sound"),
                    )
                    .clicked()
                {
                    *add_rule = Some((RuleActionKind::Audio, selected_event.clone()));
                }
                if ui
                    .add_enabled(can_add_rule, egui::Button::new("+ Emit event"))
                    .clicked()
                {
                    *add_rule = Some((RuleActionKind::Event, selected_event.clone()));
                }
                if ui
                    .add_enabled(can_add_rule, egui::Button::new("+ Variable"))
                    .clicked()
                {
                    *add_rule = Some((RuleActionKind::Variable, selected_event.clone()));
                }
                ui.separator();
                if ui.button("Reset view").clicked() {
                    pan = egui::vec2(24.0, 24.0);
                }
                if ui
                    .add_enabled(!rules.is_empty(), egui::Button::new("Auto layout"))
                    .clicked()
                {
                    relayout = true;
                }
            });
            ui.add_space(12.0);
            if rules.is_empty() {
                ui.weak("No actions yet. Add one above to begin.");
                return;
            }

            let (canvas, background) = ui.allocate_exact_size(
                egui::vec2(ui.available_width().max(560.0), CANVAS_HEIGHT),
                egui::Sense::click_and_drag(),
            );
            if background.dragged() {
                pan += background.drag_delta();
            }
            let canvas = Canvas {
                rect: canvas,
                painter: ui.painter_at(canvas),
            };
            let painter = &canvas.painter;
            painter.rect_filled(canvas.rect, egui::CornerRadius::same(8), theme::NIGHT.shell);
            let spacing = 24.0;
            let offset = egui::vec2(pan.x.rem_euclid(spacing), pan.y.rem_euclid(spacing));
            let mut y = canvas.rect.top() + offset.y;
            while y < canvas.rect.bottom() {
                let mut x = canvas.rect.left() + offset.x;
                while x < canvas.rect.right() {
                    painter.circle_filled(egui::pos2(x, y), 1.0, theme::NIGHT.rule);
                    x += spacing;
                }
                y += spacing;
            }

            let origin = canvas.rect.min + pan;
            let events = event_nodes(rules);
            let event_rect = |trigger: &RuleTrigger| {
                egui::Rect::from_min_size(
                    origin + event_position(rules, trigger).to_vec2(),
                    NODE_SIZE,
                )
            };
            let action_rect = |index: usize| {
                egui::Rect::from_min_size(
                    origin + action_position(rules, index).to_vec2(),
                    NODE_SIZE,
                )
            };
            for (index, rule) in rules.iter().enumerate() {
                let from = event_rect(&rule.trigger).right_center();
                let to = action_rect(index).left_center();
                let bend = ((to.x - from.x).abs() * 0.5).max(40.0);
                painter.add(egui::epaint::CubicBezierShape::from_points_stroke(
                    [
                        from,
                        from + egui::vec2(bend, 0.0),
                        to - egui::vec2(bend, 0.0),
                        to,
                    ],
                    false,
                    egui::Color32::TRANSPARENT,
                    egui::Stroke::new(2.0, theme::NIGHT.accent),
                ));
            }
            for (index, trigger) in events.iter().enumerate() {
                let selected = selected_event.as_ref() == Some(trigger);
                let label = event_label(trigger);
                let look = NodeLook {
                    label: &label,
                    selected,
                    accent: theme::NIGHT.accent,
                };
                let Some(response) = node(
                    ui,
                    &canvas,
                    event_rect(trigger),
                    id.with(("event_node", index)),
                    look,
                ) else {
                    continue;
                };
                if response.dragged() {
                    moves.push(NodeMove::Event(trigger.clone(), response.drag_delta()));
                }
                if response.clicked() {
                    selected_event = Some(trigger.clone());
                }
            }
            for (index, rule) in rules.iter().enumerate() {
                let label = action_label(rule, text_targets, audio_targets);
                let look = NodeLook {
                    label: &label,
                    selected: selected_action == Some(index),
                    accent: theme::NIGHT.subtle,
                };
                let Some(response) = node(
                    ui,
                    &canvas,
                    action_rect(index),
                    id.with(("action_node", index)),
                    look,
                ) else {
                    continue;
                };
                if response.dragged() {
                    moves.push(NodeMove::Action(index, response.drag_delta()));
                }
                if response.clicked() {
                    if let Some(trigger) = selected_event.take() {
                        pending_connection = Some((trigger, index));
                    }
                    selected_action = Some(index);
                }
            }

            if let Some(index) = selected_action
                && let Some(rule) = rules.get_mut(index)
            {
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.label(format!("Action {}", index + 1));
                    if ui.small_button("Remove action").clicked() {
                        *remove_rule = Some(index);
                        selected_action = None;
                    }
                });
                rule_fields(
                    ui,
                    entity,
                    index,
                    rule,
                    owner_is_button,
                    text_targets,
                    audio_targets,
                );
            }
        });
    for node_move in moves {
        match node_move {
            NodeMove::Event(trigger, delta) => move_event(rules, &trigger, delta),
            NodeMove::Action(index, delta) => move_action(rules, index, delta),
        }
    }
    if relayout {
        reset_layout(rules);
    }
    if let Some((trigger, target)) = pending_connection {
        connect(rules, &trigger, target);
    }
    ctx.data_mut(|data| {
        data.insert_temp(id, visible);
        data.insert_temp(pan_id, pan);
        if let Some(trigger) = selected_event {
            data.insert_temp(selected_id, trigger);
        } else {
            data.remove::<RuleTrigger>(selected_id);
        }
        if let Some(index) = selected_action {
            data.insert_temp(selected_action_id, index);
        } else {
            data.remove::<usize>(selected_action_id);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variable_action_nodes_show_their_change_and_condition() {
        let add = RuleData::new(
            RuleTrigger::Named("coin".into()),
            RuleAction::AddToVariable {
                name: "coins".into(),
                amount: 1.0,
            },
        );
        assert_eq!(action_label(&add, &[], &[]), "Change variable\ncoins + 1");
        let win = RuleData {
            condition: Some(engine_scene::RuleCondition {
                variable: "coins".into(),
                compare: engine_scene::RuleCompare::GreaterOrEqual,
                value: 3.0,
            }),
            ..RuleData::new(
                RuleTrigger::Named("coin".into()),
                RuleAction::SetVariable {
                    name: "level".into(),
                    value: 2.0,
                },
            )
        };
        assert_eq!(
            action_label(&win, &[], &[]),
            "Set variable\nlevel = 2\nif coins ≥ 3"
        );
    }

    fn named(event: &str, emits: &str) -> RuleData {
        RuleData::new(
            RuleTrigger::Named(event.into()),
            RuleAction::EmitEvent { name: emits.into() },
        )
    }

    #[test]
    fn automatic_layout_puts_events_left_of_their_centred_actions() {
        let rules = vec![named("a", "x"), named("b", "y"), named("a", "z")];
        assert_eq!(action_position(&rules, 1), egui::pos2(COLUMN_GAP, ROW_GAP));
        let a = event_position(&rules, &RuleTrigger::Named("a".into()));
        assert_eq!(a, egui::pos2(0.0, ROW_GAP), "centred between rows 0 and 2");
    }

    #[test]
    fn moving_nodes_saves_positions_and_events_carry_their_actions() {
        let mut rules = vec![named("a", "x"), named("b", "y"), named("a", "z")];
        move_action(&mut rules, 1, egui::vec2(10.0, -5.0));
        assert_eq!(
            rules[1].graph_position,
            Some([COLUMN_GAP + 10.0, ROW_GAP - 5.0])
        );
        assert_eq!(
            rules[0].graph_position, None,
            "others keep the automatic layout"
        );

        let a = RuleTrigger::Named("a".into());
        let before = event_position(&rules, &a);
        move_event(&mut rules, &a, egui::vec2(0.0, 100.0));
        assert_eq!(event_position(&rules, &a), before + egui::vec2(0.0, 100.0));
        assert_eq!(rules[0].graph_position, Some([COLUMN_GAP, 100.0]));
        assert_eq!(
            rules[2].graph_position,
            Some([COLUMN_GAP, 2.0 * ROW_GAP + 100.0])
        );

        move_action(&mut rules, 0, egui::vec2(f32::NAN, 0.0));
        assert_eq!(
            rules[0].graph_position,
            Some([COLUMN_GAP, 100.0]),
            "NaN ignored"
        );
        move_action(&mut rules, 0, egui::vec2(1.0e9, 0.0));
        assert_eq!(rules[0].graph_position.unwrap()[0], 1_000_000.0, "clamped");

        reset_layout(&mut rules);
        assert!(rules.iter().all(|rule| rule.graph_position.is_none()));
    }

    #[test]
    fn dragging_an_action_node_moves_it_on_the_canvas() {
        let ctx = egui::Context::default();
        let mut world = engine_ecs::prelude::World::new();
        let entity = world.spawn_empty().id();
        let mut rules = vec![named("start", "next_event")];
        let mut draw = |time: f64, events: Vec<egui::Event>| {
            let mut add = None;
            let mut remove = None;
            let mut label_at = None;
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1200.0, 900.0),
                    )),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| {
                    open(ui.ctx(), entity);
                    show(
                        ui.ctx(),
                        entity,
                        &mut rules,
                        Controls {
                            text_targets: &[],
                            audio_targets: &[],
                            owner_is_button: false,
                            can_add_rule: true,
                            add_rule: &mut add,
                            remove_rule: &mut remove,
                        },
                    );
                },
            );
            for shape in &output.shapes {
                if let egui::epaint::Shape::Text(text) = &shape.shape
                    && text.galley.text() == "Emit event\nnext_event"
                {
                    label_at = Some(text.visual_bounding_rect().center());
                }
            }
            output.textures_delta.clear();
            label_at
        };
        draw(0.0, vec![]);
        let start = draw(0.1, vec![]).expect("action node is drawn");
        let press = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let end = start + egui::vec2(60.0, 40.0);
        draw(
            0.2,
            vec![egui::Event::PointerMoved(start), press(start, true)],
        );
        draw(
            0.3,
            vec![egui::Event::PointerMoved(start + egui::vec2(30.0, 20.0))],
        );
        draw(0.4, vec![egui::Event::PointerMoved(end)]);
        draw(0.5, vec![press(end, false)]);

        let [x, y] = rules[0]
            .graph_position
            .expect("dragged node saves its position");
        assert!((x - (COLUMN_GAP + 60.0)).abs() < 0.5, "x = {x}");
        assert!((y - 40.0).abs() < 0.5, "y = {y}");
    }

    #[test]
    fn empty_graph_can_request_an_event_action() {
        let ctx = egui::Context::default();
        let mut world = engine_ecs::prelude::World::new();
        let entity = world.spawn_empty().id();
        let mut rules = Vec::new();
        let mut draw = |time, events| {
            let mut add = None;
            let mut remove = None;
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(900.0, 600.0),
                    )),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| {
                    open(ui.ctx(), entity);
                    show(
                        ui.ctx(),
                        entity,
                        &mut rules,
                        Controls {
                            text_targets: &[],
                            audio_targets: &[],
                            owner_is_button: false,
                            can_add_rule: true,
                            add_rule: &mut add,
                            remove_rule: &mut remove,
                        },
                    );
                },
            );
            let button = output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == "+ Emit event" => {
                    Some(text.visual_bounding_rect().center())
                }
                _ => None,
            });
            output.textures_delta.clear();
            (button, add, remove)
        };
        draw(0.0, vec![]);
        let (button, _, _) = draw(0.1, vec![]);
        let button = button.expect("event action is available in an empty graph");
        let click = |pressed| egui::Event::PointerButton {
            pos: button,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        draw(0.2, vec![click(true)]);
        let (_, add, remove) = draw(0.3, vec![click(false)]);
        assert!(matches!(
            add,
            Some((super::super::RuleActionKind::Event, None))
        ));
        assert_eq!(remove, None);
    }

    #[test]
    fn selected_graph_action_can_be_removed() {
        let ctx = egui::Context::default();
        let mut world = engine_ecs::prelude::World::new();
        let entity = world.spawn_empty().id();
        let mut rules = vec![RuleData {
            trigger: RuleTrigger::Named("game_start".into()),
            action: RuleAction::EmitEvent {
                name: "next_event".into(),
            },
            condition: None,
            graph_position: None,
        }];
        let mut draw = |time, events, label: &str| {
            let mut add = None;
            let mut remove = None;
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(900.0, 600.0),
                    )),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| {
                    open(ui.ctx(), entity);
                    show(
                        ui.ctx(),
                        entity,
                        &mut rules,
                        Controls {
                            text_targets: &[],
                            audio_targets: &[],
                            owner_is_button: false,
                            can_add_rule: true,
                            add_rule: &mut add,
                            remove_rule: &mut remove,
                        },
                    );
                },
            );
            let position = output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == label => {
                    Some(text.visual_bounding_rect().center())
                }
                _ => None,
            });
            output.textures_delta.clear();
            (position, remove)
        };
        draw(0.0, vec![], "Emit event\nnext_event");
        let (action, _) = draw(0.1, vec![], "Emit event\nnext_event");
        let action = action.expect("action node is visible");
        let click = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        draw(0.2, vec![click(action, true)], "Remove action");
        let (remove_button, _) = draw(0.3, vec![click(action, false)], "Remove action");
        let remove_button = remove_button.expect("selected action has a remove control");
        draw(0.4, vec![click(remove_button, true)], "Remove action");
        let (_, remove) = draw(0.5, vec![click(remove_button, false)], "Remove action");
        assert_eq!(remove, Some(0));
    }
}
