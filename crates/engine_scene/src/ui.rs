//! Converts authored scene UI into the engine's retained screen-space tree.

use std::collections::HashMap;

use engine_ui::{Anchor, Color, NodeId, Style, Ui, Widget};

use crate::{Scene, UiAnchor, UiElementData, UiKind};

impl UiElementData {
    /// A visible, editable label with a top-left screen anchor.
    pub fn label() -> Self {
        Self {
            kind: UiKind::Label,
            text: "Label".into(),
            anchor: UiAnchor::TopLeft,
            offset: [24.0, 24.0],
            size: [180.0, 32.0],
            color: [1.0; 4],
        }
    }

    /// A clickable button with a centered screen anchor.
    pub fn button() -> Self {
        Self {
            kind: UiKind::Button,
            text: "Button".into(),
            anchor: UiAnchor::Center,
            offset: [0.0, 0.0],
            size: [180.0, 44.0],
            color: [0.18, 0.35, 0.65, 1.0],
        }
    }
}

impl From<UiAnchor> for Anchor {
    fn from(anchor: UiAnchor) -> Self {
        match anchor {
            UiAnchor::TopLeft => Self::TopLeft,
            UiAnchor::Top => Self::Top,
            UiAnchor::TopRight => Self::TopRight,
            UiAnchor::Left => Self::Left,
            UiAnchor::Center => Self::Center,
            UiAnchor::Right => Self::Right,
            UiAnchor::BottomLeft => Self::BottomLeft,
            UiAnchor::Bottom => Self::Bottom,
            UiAnchor::BottomRight => Self::BottomRight,
        }
    }
}

impl Scene {
    /// Rebuilds authored labels and buttons for this scene. Node IDs map to
    /// stable scene object IDs for gameplay commands. Disabled objects are omitted.
    pub fn mount_ui(&self, ui: &mut Ui) -> HashMap<NodeId, String> {
        ui.clear();
        let mut nodes = HashMap::new();
        for entity in &self.entities {
            let Some(data) = &entity.ui else { continue };
            if entity.disabled {
                continue;
            }
            let color = Color {
                r: data.color[0],
                g: data.color[1],
                b: data.color[2],
                a: data.color[3],
            };
            let widget = match data.kind {
                UiKind::Label => Widget::Label {
                    text: data.text.clone(),
                    color,
                },
                UiKind::Button => Widget::Button {
                    text: data.text.clone(),
                    bg: color,
                    hot_bg: Color {
                        r: (color.r + 0.12).min(1.0),
                        g: (color.g + 0.12).min(1.0),
                        b: (color.b + 0.12).min(1.0),
                        a: color.a,
                    },
                },
            };
            let node = ui.add_screen(
                Style {
                    anchor: data.anchor.into(),
                    offset: data.offset,
                    size: data.size,
                },
                widget,
            );
            if let Some(id) = &entity.object_id {
                nodes.insert(node, id.clone());
            }
        }
        nodes
    }
}

/// Changes a mounted scene UI caption by stable object ID.
pub fn set_authored_ui_text(
    ui: &mut Ui,
    nodes: &HashMap<NodeId, String>,
    object_id: &str,
    text: &str,
) -> bool {
    let node = nodes
        .iter()
        .find_map(|(node, id)| (id == object_id).then_some(*node));
    match node.and_then(|node| ui.node_mut(node)) {
        Some(engine_ui::Node {
            widget: Widget::Label { text: caption, .. } | Widget::Button { text: caption, .. },
            ..
        }) => {
            *caption = text.to_owned();
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod rule_tests {
    use super::*;
    use crate::{RuleAction, RuleData, RuleEvent, RuleTrigger, SceneEntity};
    use engine_ecs::prelude::World;

    #[test]
    fn a_button_rule_can_emit_an_event_that_runs_followup_actions() {
        let button_id = "67d09e47-5520-4b5a-a132-a68a4a60f452";
        let label_id = "3fa85f64-5717-4562-b3fc-2c963f66afa6";
        let scene = Scene {
            entities: vec![
                SceneEntity {
                    object_id: Some(button_id.into()),
                    ui: Some(UiElementData::button()),
                    rules: vec![
                        RuleData {
                            trigger: RuleTrigger::UiClicked,
                            action: RuleAction::SetUiText {
                                target: label_id.into(),
                                text: "First".into(),
                            },
                            condition: None,
                            graph_position: None,
                        },
                        RuleData {
                            trigger: RuleTrigger::UiClicked,
                            action: RuleAction::EmitEvent {
                                name: "stage_two".into(),
                            },
                            condition: None,
                            graph_position: None,
                        },
                    ],
                    ..Default::default()
                },
                SceneEntity {
                    object_id: Some(label_id.into()),
                    ui: Some(UiElementData::label()),
                    rules: vec![RuleData {
                        trigger: RuleTrigger::Named("stage_two".into()),
                        action: RuleAction::SetUiText {
                            target: label_id.into(),
                            text: "Second".into(),
                        },
                        condition: None,
                        graph_position: None,
                    }],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        scene.validate().expect("valid event chain");
        let scene = Scene::from_ron_str(&scene.to_ron_string().expect("serialize chain"))
            .expect("reopen event chain");
        let mut ui = Ui::new(800.0, 600.0);
        let nodes = scene.mount_ui(&mut ui);
        let mut world = World::new();
        assert_eq!(
            scene.apply_rule_event(RuleEvent::UiClicked(button_id), &mut world, &mut ui, &nodes),
            3
        );
        let label_node = nodes
            .iter()
            .find_map(|(node, id)| (id == label_id).then_some(*node))
            .expect("label node");
        assert!(matches!(
            &ui.node(label_node).expect("label").widget,
            Widget::Label { text, .. } if text == "Second"
        ));
    }

    #[test]
    fn a_visual_event_cycle_stops_at_a_bounded_number_of_actions() {
        let scene = Scene {
            entities: vec![SceneEntity {
                rules: vec![RuleData {
                    trigger: RuleTrigger::Named("again".into()),
                    action: RuleAction::EmitEvent {
                        name: "again".into(),
                    },
                    condition: None,
                    graph_position: None,
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        scene.validate().expect("valid cyclic graph");
        let mut ui = Ui::new(800.0, 600.0);
        let mut world = World::new();
        let count = scene.apply_rule_event(
            RuleEvent::Named("again"),
            &mut world,
            &mut ui,
            &HashMap::new(),
        );
        assert!((1..=64).contains(&count), "a cycle must terminate promptly");
    }

    #[test]
    fn branching_event_chains_bound_total_actions_not_only_event_count() {
        let label_id = "3fa85f64-5717-4562-b3fc-2c963f66afa6";
        let mut scene = Scene::default();
        scene.entities.push(SceneEntity {
            object_id: Some(label_id.into()),
            ui: Some(UiElementData::label()),
            ..Default::default()
        });
        for _ in 0..4 {
            let mut rules = Vec::new();
            for _ in 0..8 {
                rules.push(RuleData {
                    trigger: RuleTrigger::Named("loop".into()),
                    action: RuleAction::SetUiText {
                        target: label_id.into(),
                        text: "Still running".into(),
                    },
                    condition: None,
                    graph_position: None,
                });
                rules.push(RuleData {
                    trigger: RuleTrigger::Named("loop".into()),
                    action: RuleAction::EmitEvent {
                        name: "loop".into(),
                    },
                    condition: None,
                    graph_position: None,
                });
            }
            scene.entities.push(SceneEntity {
                rules,
                ..Default::default()
            });
        }
        scene.validate().expect("valid branching graph");
        let mut ui = Ui::new(800.0, 600.0);
        let nodes = scene.mount_ui(&mut ui);
        let mut world = World::new();
        let count = scene.apply_rule_event(RuleEvent::Named("loop"), &mut world, &mut ui, &nodes);
        assert!((1..=1024).contains(&count), "actions must stay bounded");
    }

    #[test]
    fn clicking_a_button_runs_its_text_rule_once() {
        let button_id = "67d09e47-5520-4b5a-a132-a68a4a60f452";
        let label_id = "3fa85f64-5717-4562-b3fc-2c963f66afa6";
        let scene = Scene {
            entities: vec![
                SceneEntity {
                    object_id: Some(button_id.into()),
                    ui: Some(UiElementData::button()),
                    rules: vec![RuleData {
                        trigger: RuleTrigger::UiClicked,
                        action: RuleAction::SetUiText {
                            target: label_id.into(),
                            text: "Ready".into(),
                        },
                        condition: None,
                        graph_position: None,
                    }],
                    ..Default::default()
                },
                SceneEntity {
                    object_id: Some(label_id.into()),
                    ui: Some(UiElementData::label()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let mut ui = Ui::new(800.0, 600.0);
        let nodes = scene.mount_ui(&mut ui);
        let mut world = World::new();
        assert_eq!(
            scene.apply_rule_event(RuleEvent::UiClicked(button_id), &mut world, &mut ui, &nodes),
            1
        );
        let label_node = nodes
            .iter()
            .find_map(|(node, id)| (id == label_id).then_some(*node))
            .unwrap();
        assert!(matches!(
            &ui.node(label_node).unwrap().widget,
            Widget::Label { text, .. } if text == "Ready"
        ));
        assert_eq!(
            scene.apply_rule_event(RuleEvent::UiClicked(label_id), &mut world, &mut ui, &nodes),
            0
        );
    }

    #[test]
    fn named_event_runs_audio_action_and_skips_missing_target() {
        let audio_id = "72ec89b0-b7e5-45b1-87d9-71bd110a3f47";
        let missing_id = "52265ff6-dafa-44a3-8d90-6a4d09e13d4d";
        let scene = Scene {
            entities: vec![SceneEntity {
                rules: vec![
                    RuleData {
                        trigger: RuleTrigger::Named("game_start".into()),
                        action: RuleAction::PlayAudio {
                            target: audio_id.into(),
                        },
                        condition: None,
                        graph_position: None,
                    },
                    RuleData {
                        trigger: RuleTrigger::Named("game_start".into()),
                        action: RuleAction::PlayAudio {
                            target: missing_id.into(),
                        },
                        condition: None,
                        graph_position: None,
                    },
                ],
                ..Default::default()
            }],
            ..Default::default()
        };
        let wav = Vec::from(&b"RIFF\x26\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x40\x1f\0\0\x80\x3e\0\0\x02\0\x10\0data\x02\0\0\0\0\0"[..]);
        let sound = engine_audio::StaticSound::from_bytes(wav, false).unwrap();
        let mut world = World::new();
        world.spawn((
            crate::SceneObjectId(audio_id.into()),
            engine_ecs::AudioEmitter::silent(sound),
        ));
        let mut ui = Ui::new(800.0, 600.0);
        let nodes = HashMap::new();

        assert_eq!(
            scene.apply_rule_event(RuleEvent::Named("other"), &mut world, &mut ui, &nodes),
            0
        );
        assert_eq!(
            scene.apply_rule_event(RuleEvent::Named("game_start"), &mut world, &mut ui, &nodes),
            1
        );
    }
}
