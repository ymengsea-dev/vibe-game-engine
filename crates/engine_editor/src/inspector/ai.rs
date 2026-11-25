//! The Inspector's AI section: how an object walks, what it notices, and
//! the brain that decides what it does.
//!
//! Everything is the object's [`SceneAi`], so edits save with the scene and
//! ⌘Z undoes them like any other field.

use engine_ecs::prelude::{Entity, World};
use engine_scene::{
    AiBrainData, AiData, AiState, BehaviourTreeData, MAX_AI_DISTANCE, MAX_PATROL_POINTS, SceneAi,
    SensesData, SimpleBrainData,
};

use super::{behaviour_tree_graph, choice, note, number, property_row, subsection};
use crate::theme;

/// Present while a game is playing: behaviour tree windows then show what
/// the game reports (`trace`: the watched object's ID and its tree's last
/// frame) and can't be edited.
#[derive(bevy_ecs::resource::Resource, Debug, Default)]
pub struct PlayingTrees {
    /// The watched object and its tree's answers, once the game sent them.
    pub trace: Option<(String, engine_scene::BtTrace)>,
}

/// The section's body. Returns whether anything changed.
pub(super) fn inspect(world: &mut World, entity: Entity, ui: &mut egui::Ui) -> bool {
    let Some(before) = world.get::<SceneAi>(entity).map(|ai| ai.0.clone()) else {
        return false;
    };
    let clips = world
        .get::<engine_ecs::AnimationClips>(entity)
        .map(engine_ecs::AnimationClips::names)
        .unwrap_or_default();
    let here = crate::hierarchy::world_transform(world, entity).translation;
    let mut value = before.clone();

    subsection(ui, "Walking", |ui| walking(ui, &mut value));
    subsection(ui, "Senses", |ui| senses(ui, &mut value));
    subsection(ui, "Brain", |ui| {
        brain(ui, entity, &mut value, &clips, here.to_array())
    });

    if let Some(AiBrainData::Tree(tree)) = value.brain.as_mut() {
        let playing = world.get_resource::<PlayingTrees>();
        let live = playing
            .and_then(|playing| playing.trace.as_ref())
            .filter(|(watched, _)| {
                world
                    .get::<engine_scene::SceneObjectId>(entity)
                    .is_some_and(|id| id.0 == *watched)
            })
            .map(|(_, trace)| behaviour_tree_graph::live_answers(tree, trace));
        let playing = playing.is_some();
        behaviour_tree_graph::show(
            ui.ctx(),
            entity,
            tree,
            &clips,
            here.to_array(),
            playing,
            live.as_deref(),
        );
    }
    let changed = value != before;
    if changed && let Ok(mut entity_mut) = world.get_entity_mut(entity) {
        entity_mut.insert(SceneAi(value));
    }
    changed
}

fn distance_field(ui: &mut egui::Ui, label: &str, value: &mut f32, hint: &str) {
    property_row(ui, label, |ui| {
        number(
            ui,
            egui::DragValue::new(value)
                .speed(0.05)
                .range(0.0..=MAX_AI_DISTANCE),
        )
        .on_hover_text(hint);
    });
}

fn walking(ui: &mut egui::Ui, value: &mut AiData) {
    let agent = &mut value.agent;
    distance_field(ui, "Speed", &mut agent.speed, "World units per second");
    distance_field(
        ui,
        "Stop within",
        &mut agent.stopping_distance,
        "How close to where it is going counts as arrived",
    );
    property_row(ui, "Turn speed", |ui| {
        number(
            ui,
            egui::DragValue::new(&mut agent.turn_speed)
                .speed(5.0)
                .range(0.0..=10_000.0)
                .suffix("°/s"),
        )
        .on_hover_text("How fast it turns to face where it walks; 0 never turns it");
    });
    property_row(ui, "Radius", |ui| {
        number(
            ui,
            egui::DragValue::new(&mut agent.radius)
                .speed(0.01)
                .range(0.0..=engine_scene::MAX_AGENT_RADIUS),
        )
        .on_hover_text("Its size on the ground, for keeping clear of other characters");
    });
    let palette = theme::NIGHT;
    property_row(ui, "Avoid others", |ui| {
        crate::widgets::checkbox(ui, &palette, &mut agent.avoid_others, "")
            .on_hover_text("Steer round other AI characters while walking");
    });
}

fn senses(ui: &mut egui::Ui, value: &mut AiData) {
    let palette = theme::NIGHT;
    let mut on = value.senses.is_some();
    property_row(ui, "Notices", |ui| {
        crate::widgets::checkbox(ui, &palette, &mut on, "")
            .on_hover_text("Look and listen for objects with a tag, such as the player");
    });
    if on != value.senses.is_some() {
        value.senses = on.then(SensesData::default);
    }
    let Some(senses) = value.senses.as_mut() else {
        return;
    };
    property_row(ui, "Looks for tag", |ui| {
        crate::widgets::text_field(ui, &mut senses.target_tag, "player");
    });
    distance_field(ui, "Sees up to", &mut senses.sight_range, "How far it sees");
    property_row(ui, "View angle", |ui| {
        number(
            ui,
            egui::DragValue::new(&mut senses.sight_angle)
                .speed(1.0)
                .range(0.0..=360.0)
                .suffix("°"),
        )
        .on_hover_text("How wide it sees; 360 sees all round");
    });
    property_row(ui, "Eye height", |ui| {
        number(
            ui,
            egui::DragValue::new(&mut senses.eye_height)
                .speed(0.05)
                .range(-MAX_AI_DISTANCE..=MAX_AI_DISTANCE),
        )
        .on_hover_text("Walls between its eyes and a target block the view");
    });
    distance_field(
        ui,
        "Hears within",
        &mut senses.hearing_radius,
        "Targets this close are noticed through walls",
    );
    property_row(ui, "Forget after", |ui| {
        number(
            ui,
            egui::DragValue::new(&mut senses.forget_after)
                .speed(0.1)
                .range(0.0..=3_600.0)
                .suffix(" s"),
        )
        .on_hover_text("Seconds without seeing or hearing a target before it is lost");
    });
    note(
        ui,
        "The Scene View shows the view and hearing range while it is selected.",
    );
}

/// The brain choices, in picker order.
const BRAINS: [&str; 3] = ["None", "Simple", "Behaviour tree"];

fn brain_label(brain: Option<&AiBrainData>) -> &'static str {
    match brain {
        None => BRAINS[0],
        Some(AiBrainData::Simple(_)) => BRAINS[1],
        Some(AiBrainData::Tree(_)) => BRAINS[2],
    }
}

fn brain(ui: &mut egui::Ui, entity: Entity, value: &mut AiData, clips: &[String], here: [f32; 3]) {
    property_row(ui, "Brain", |ui| {
        choice(("ai_brain", entity), ui)
            .selected_text(brain_label(value.brain.as_ref()))
            .show_ui(ui, |ui| {
                for label in BRAINS {
                    let current = brain_label(value.brain.as_ref()) == label;
                    if ui.selectable_label(current, label).clicked() && !current {
                        value.brain = match label {
                            "Simple" => Some(AiBrainData::Simple(SimpleBrainData::default())),
                            "Behaviour tree" => {
                                Some(AiBrainData::Tree(BehaviourTreeData::default()))
                            }
                            _ => None,
                        };
                    }
                }
            });
    });
    let needs_senses = value.senses.is_none();
    match value.brain.as_mut() {
        None => note(ui, "It walks only where scripts and rules send it."),
        Some(AiBrainData::Simple(simple)) => {
            if needs_senses {
                note(ui, "Tick Notices under Senses so it can chase anything.");
            }
            simple_fields(ui, entity, simple, clips, here);
        }
        Some(AiBrainData::Tree(tree)) => {
            if needs_senses {
                note(ui, "Tick Notices under Senses so target checks can work.");
            }
            property_row(ui, "Tree", |ui| {
                if ui.button("Edit behaviour tree").clicked() {
                    behaviour_tree_graph::open(ui.ctx(), entity);
                }
                ui.label(
                    egui::RichText::new(format!("{} nodes", tree.nodes.len()))
                        .color(theme::NIGHT.subtle),
                );
            });
        }
    }
}

fn simple_fields(
    ui: &mut egui::Ui,
    entity: Entity,
    brain: &mut SimpleBrainData,
    clips: &[String],
    here: [f32; 3],
) {
    let palette = theme::NIGHT;
    property_row(ui, "Starts by", |ui| {
        choice(("ai_start", entity), ui)
            .selected_text(if brain.start == AiState::Patrol {
                "Patrolling"
            } else {
                "Standing still"
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut brain.start, AiState::Idle, "Standing still");
                ui.selectable_value(&mut brain.start, AiState::Patrol, "Patrolling");
            });
    });
    if brain.start == AiState::Patrol {
        patrol_points(ui, entity, &mut brain.patrol_points, here);
        property_row(ui, "Wait at each", |ui| {
            number(
                ui,
                egui::DragValue::new(&mut brain.patrol_wait)
                    .speed(0.1)
                    .range(0.0..=3_600.0)
                    .suffix(" s"),
            );
        });
        property_row(ui, "Walk back", |ui| {
            crate::widgets::checkbox(ui, &palette, &mut brain.patrol_back_and_forth, "")
                .on_hover_text("After the last point, walk back through them in reverse");
        });
    }
    distance_field(
        ui,
        "Chase speed",
        &mut brain.chase_speed,
        "Running speed while chasing",
    );
    distance_field(
        ui,
        "Attack within",
        &mut brain.attack_range,
        "How close the target must be to attack it",
    );
    property_row(ui, "Attack every", |ui| {
        number(
            ui,
            egui::DragValue::new(&mut brain.attack_cooldown)
                .speed(0.05)
                .range(0.0..=3_600.0)
                .suffix(" s"),
        );
    });
    property_row(ui, "Attack event", |ui| {
        crate::widgets::text_field(ui, &mut brain.attack_event, "attack");
    });
    note(
        ui,
        "Each attack sends this event; a Named event rule can take health away.",
    );
    if clips.is_empty() {
        return;
    }
    let clips_row = |ui: &mut egui::Ui, label: &str, clip: &mut String, salt: &str| {
        property_row(ui, label, |ui| {
            choice(("ai_clip", entity, salt), ui)
                .selected_text(if clip.is_empty() {
                    "None"
                } else {
                    clip.as_str()
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(clip, String::new(), "None");
                    for name in clips {
                        ui.selectable_value(clip, name.clone(), name);
                    }
                });
        });
    };
    clips_row(ui, "Idle clip", &mut brain.clips.idle, "idle");
    clips_row(ui, "Walk clip", &mut brain.clips.walk, "walk");
    clips_row(ui, "Run clip", &mut brain.clips.run, "run");
    clips_row(ui, "Attack clip", &mut brain.clips.attack, "attack");
}

/// The patrol point list: one row of X, Y, Z per point, and a button that
/// adds the object's own position.
pub(super) fn patrol_points(
    ui: &mut egui::Ui,
    entity: Entity,
    points: &mut Vec<[f32; 3]>,
    here: [f32; 3],
) {
    let palette = theme::NIGHT;
    property_row(ui, "Patrol points", |ui| {
        if ui
            .add_enabled(
                points.len() < MAX_PATROL_POINTS,
                egui::Button::new("Add point here").fill(palette.hover),
            )
            .on_hover_text("Adds where the object is now; move it, then add the next")
            .clicked()
        {
            points.push(here);
        }
    });
    if points.is_empty() {
        note(ui, "Add points for it to walk between.");
    }
    let mut remove = None;
    for (index, point) in points.iter_mut().enumerate() {
        ui.push_id(("ai_patrol_point", entity, index), |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(format!("{}", index + 1)).color(palette.subtle));
                for (axis, component) in ["X", "Y", "Z"].into_iter().zip(point.iter_mut()) {
                    number(
                        ui,
                        egui::DragValue::new(component)
                            .speed(0.1)
                            .prefix(format!("{axis} ")),
                    );
                }
                if ui
                    .button(egui::RichText::new("Remove").color(palette.danger))
                    .clicked()
                {
                    remove = Some(index);
                }
            });
        });
    }
    if let Some(index) = remove {
        points.remove(index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brain_labels_follow_the_choice_order() {
        assert_eq!(brain_label(None), "None");
        assert_eq!(
            brain_label(Some(&AiBrainData::Simple(SimpleBrainData::default()))),
            "Simple"
        );
        assert_eq!(
            brain_label(Some(&AiBrainData::Tree(BehaviourTreeData::default()))),
            "Behaviour tree"
        );
    }
}
