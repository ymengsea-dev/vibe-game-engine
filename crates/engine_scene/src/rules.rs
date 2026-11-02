//! Engine-owned actions for authored scene events, shared by Lua and Rust games.

use std::collections::{BTreeMap, HashMap, VecDeque};

use engine_ecs::prelude::World;
use engine_ui::{NodeId, Ui};

use crate::{RuleAction, RuleTrigger, Scene, SceneObjectId};

const MAX_RULE_EVENT_DISPATCHES: usize = 64;
const MAX_RULE_ACTIONS_PER_DISPATCH: usize = 1024;

/// Whether `name` can name a gameplay variable: 1–64 bytes without control characters.
pub fn valid_variable_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && !name.chars().any(char::is_control)
}

/// Why a gameplay variable could not be written.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RuleVariableError {
    /// The name is empty, longer than 64 bytes, or has control characters.
    #[error("variable name must be 1–64 printable bytes")]
    InvalidName,
    /// NaN and infinities are rejected.
    #[error("variable value must be finite")]
    NotFinite,
    /// A new name would exceed [`RuleVariables::MAX_VARIABLES`].
    #[error(
        "at most {} gameplay variables may exist",
        RuleVariables::MAX_VARIABLES
    )]
    TooMany,
}

/// Numeric gameplay variables shared by visual rules, Lua, and Rust code.
///
/// Stored as a world resource, so values survive scene changes within one run
/// but are not saved to disk. Unset variables read as `0`.
#[derive(bevy_ecs::resource::Resource, Debug, Default, Clone, PartialEq)]
pub struct RuleVariables {
    values: BTreeMap<String, f64>,
}

impl RuleVariables {
    /// Upper bound on distinct variable names.
    pub const MAX_VARIABLES: usize = 256;

    /// The variable's value, or `0` when unset.
    pub fn get(&self, name: &str) -> f64 {
        self.values.get(name).copied().unwrap_or(0.0)
    }

    /// Sets a variable after validating its name, value, and the variable cap.
    pub fn set(&mut self, name: &str, value: f64) -> Result<(), RuleVariableError> {
        if !valid_variable_name(name) {
            return Err(RuleVariableError::InvalidName);
        }
        if !value.is_finite() {
            return Err(RuleVariableError::NotFinite);
        }
        if let Some(slot) = self.values.get_mut(name) {
            *slot = value;
        } else if self.values.len() >= Self::MAX_VARIABLES {
            return Err(RuleVariableError::TooMany);
        } else {
            self.values.insert(name.to_owned(), value);
        }
        Ok(())
    }

    /// Adds to a variable; the result must stay finite.
    pub fn add(&mut self, name: &str, amount: f64) -> Result<(), RuleVariableError> {
        self.set(name, self.get(name) + amount)
    }

    /// Every set variable in name order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, f64)> {
        self.values
            .iter()
            .map(|(name, value)| (name.as_str(), *value))
    }
}

/// Formats a variable for display: whole numbers without a decimal point.
pub fn format_variable(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}

/// Replaces each `{name}` holding a valid variable name with its value.
/// Other braces are kept, so ordinary text needs no escaping.
pub fn expand_variables(text: &str, variables: &RuleVariables) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close)
                if valid_variable_name(&after[..close]) && !after[..close].contains('{') =>
            {
                out.push_str(&format_variable(variables.get(&after[..close])));
                rest = &after[close + 1..];
            }
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

enum PendingEvent {
    UiClicked(String),
    Named(String),
}

/// A UI click or a named gameplay event sent by Lua or Rust code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleEvent<'a> {
    /// Stable object ID of an authored UI Button.
    UiClicked(&'a str),
    /// Developer-defined event name.
    Named(&'a str),
}

impl Scene {
    /// Runs matching visual rules and returns the number of actions applied.
    /// Missing targets are reported and skipped without stopping other rules.
    /// Rules run in scene order, so a variable changed by one rule is seen by
    /// the conditions of later rules in the same dispatch.
    pub fn apply_rule_event(
        &self,
        event: RuleEvent<'_>,
        world: &mut World,
        ui: &mut Ui,
        ui_nodes: &HashMap<NodeId, String>,
    ) -> usize {
        let mut variables = world.remove_resource::<RuleVariables>().unwrap_or_default();
        let applied = self.dispatch_rules(event, world, ui, ui_nodes, &mut variables);
        world.insert_resource(variables);
        applied
    }

    fn dispatch_rules(
        &self,
        event: RuleEvent<'_>,
        world: &mut World,
        ui: &mut Ui,
        ui_nodes: &HashMap<NodeId, String>,
        variables: &mut RuleVariables,
    ) -> usize {
        let initial = match event {
            RuleEvent::UiClicked(id) => PendingEvent::UiClicked(id.to_owned()),
            RuleEvent::Named(name) => PendingEvent::Named(name.to_owned()),
        };
        let mut pending = VecDeque::from([initial]);
        let mut dispatched = 0;
        let mut applied = 0;
        let mut attempted = 0;
        let mut warned_limit = false;
        while let Some(event) = pending.pop_front() {
            dispatched += 1;
            for owner in &self.entities {
                if owner.disabled {
                    continue;
                }
                for rule in &owner.rules {
                    let matches = match (&rule.trigger, &event) {
                        (RuleTrigger::UiClicked, PendingEvent::UiClicked(id)) => {
                            owner.object_id.as_deref() == Some(id.as_str())
                        }
                        (RuleTrigger::Named(name), PendingEvent::Named(event_name)) => {
                            name == event_name
                        }
                        _ => false,
                    };
                    if !matches
                        || rule.condition.as_ref().is_some_and(|condition| {
                            !condition
                                .compare
                                .holds(variables.get(&condition.variable), condition.value)
                        })
                    {
                        continue;
                    }
                    if attempted == MAX_RULE_ACTIONS_PER_DISPATCH {
                        tracing::warn!(
                            limit = MAX_RULE_ACTIONS_PER_DISPATCH,
                            "visual rule action chain stopped at its action limit"
                        );
                        return applied;
                    }
                    attempted += 1;
                    let success = match &rule.action {
                        RuleAction::SetUiText { target, text } => crate::ui::set_authored_ui_text(
                            ui,
                            ui_nodes,
                            target,
                            &expand_variables(text, variables),
                        ),
                        RuleAction::PlayAudio { target } => {
                            let entity = world
                                .iter_entities()
                                .find(|entity| {
                                    entity
                                        .get::<SceneObjectId>()
                                        .is_some_and(|id| id.0 == *target)
                                })
                                .map(|entity| entity.id());
                            entity
                                .and_then(|entity| {
                                    world.get_mut::<engine_ecs::AudioEmitter>(entity)
                                })
                                .is_some_and(|mut emitter| {
                                    emitter.play();
                                    true
                                })
                        }
                        RuleAction::SetVariable { name, value } => {
                            variable_written(variables.set(name, *value), name)
                        }
                        RuleAction::AddToVariable { name, amount } => {
                            variable_written(variables.add(name, *amount), name)
                        }
                        RuleAction::EmitEvent { name } => {
                            if dispatched + pending.len() < MAX_RULE_EVENT_DISPATCHES {
                                pending.push_back(PendingEvent::Named(name.clone()));
                                true
                            } else {
                                if !warned_limit {
                                    tracing::warn!(
                                        limit = MAX_RULE_EVENT_DISPATCHES,
                                        "visual rule event chain stopped at its dispatch limit"
                                    );
                                    warned_limit = true;
                                }
                                false
                            }
                        }
                    };
                    if success {
                        applied += 1;
                    } else if matches!(
                        rule.action,
                        RuleAction::SetUiText { .. } | RuleAction::PlayAudio { .. }
                    ) {
                        tracing::warn!(action = ?rule.action, "visual rule target is unavailable");
                    }
                }
            }
        }
        applied
    }
}

fn variable_written(result: Result<(), RuleVariableError>, name: &str) -> bool {
    match result {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(variable = name, %error, "visual rule could not change a variable");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RuleCompare, RuleCondition, RuleData, SceneEntity, UiElementData};
    use engine_ui::Widget;

    const LABEL: &str = "3fa85f64-5717-4562-b3fc-2c963f66afa6";

    fn rule(trigger: &str, action: RuleAction, condition: Option<RuleCondition>) -> RuleData {
        RuleData {
            condition,
            ..RuleData::new(RuleTrigger::Named(trigger.into()), action)
        }
    }

    fn at_least(variable: &str, value: f64) -> Option<RuleCondition> {
        Some(RuleCondition {
            variable: variable.into(),
            compare: RuleCompare::GreaterOrEqual,
            value,
        })
    }

    fn label_text(ui: &Ui, nodes: &HashMap<NodeId, String>) -> String {
        let node = nodes
            .iter()
            .find_map(|(node, id)| (id == LABEL).then_some(*node))
            .expect("label node");
        match &ui.node(node).expect("label").widget {
            Widget::Label { text, .. } => text.clone(),
            other => panic!("unexpected widget {other:?}"),
        }
    }

    fn coin_scene() -> Scene {
        Scene {
            entities: vec![SceneEntity {
                object_id: Some(LABEL.into()),
                ui: Some(UiElementData::label()),
                rules: vec![
                    rule(
                        "coin",
                        RuleAction::AddToVariable {
                            name: "coins".into(),
                            amount: 1.0,
                        },
                        None,
                    ),
                    rule(
                        "coin",
                        RuleAction::SetUiText {
                            target: LABEL.into(),
                            text: "Coins: {coins}".into(),
                        },
                        None,
                    ),
                    rule(
                        "coin",
                        RuleAction::SetUiText {
                            target: LABEL.into(),
                            text: "You win with {coins} coins!".into(),
                        },
                        at_least("coins", 3.0),
                    ),
                ],
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn conditions_see_variables_changed_by_earlier_rules_and_persist_between_events() {
        let scene = coin_scene();
        scene.validate().expect("valid rules");
        let scene = Scene::from_ron_str(&scene.to_ron_string().expect("serialize"))
            .expect("conditions and variable actions round-trip");
        let mut ui = Ui::new(800.0, 600.0);
        let nodes = scene.mount_ui(&mut ui);
        let mut world = World::new();
        let coin = |world: &mut World, ui: &mut Ui| {
            scene.apply_rule_event(RuleEvent::Named("coin"), world, ui, &nodes)
        };

        assert_eq!(coin(&mut world, &mut ui), 2, "win rule is skipped below 3");
        assert_eq!(label_text(&ui, &nodes), "Coins: 1");
        coin(&mut world, &mut ui);
        assert_eq!(label_text(&ui, &nodes), "Coins: 2");
        assert_eq!(
            coin(&mut world, &mut ui),
            3,
            "third coin meets the condition"
        );
        assert_eq!(label_text(&ui, &nodes), "You win with 3 coins!");
        assert_eq!(world.resource::<RuleVariables>().get("coins"), 3.0);
    }

    #[test]
    fn variables_set_by_game_code_drive_rule_conditions() {
        let scene = coin_scene();
        let mut ui = Ui::new(800.0, 600.0);
        let nodes = scene.mount_ui(&mut ui);
        let mut world = World::new();
        let mut variables = RuleVariables::default();
        variables.set("coins", 10.0).expect("set");
        world.insert_resource(variables);
        scene.apply_rule_event(RuleEvent::Named("coin"), &mut world, &mut ui, &nodes);
        assert_eq!(label_text(&ui, &nodes), "You win with 11 coins!");
    }

    #[test]
    fn older_rules_without_a_condition_still_parse() {
        let text = r#"(
            version: 5,
            entities: [(
                rules: [(trigger: Named("start"), action: EmitEvent(name: "next"))],
            )],
        )"#;
        let scene = Scene::from_ron_str(text).expect("old rule format");
        assert_eq!(scene.entities[0].rules[0].condition, None);
        let saved = scene.to_ron_string().expect("serialize");
        assert!(
            !saved.contains("condition"),
            "absent conditions stay out of files"
        );
    }

    #[test]
    fn invalid_variable_rules_are_rejected_by_validation() {
        let mut scene = coin_scene();
        scene.entities[0].rules[0].action = RuleAction::SetVariable {
            name: String::new(),
            value: 1.0,
        };
        assert!(scene.validate().is_err(), "empty variable name");
        scene.entities[0].rules[0].action = RuleAction::AddToVariable {
            name: "coins".into(),
            amount: f64::NAN,
        };
        assert!(scene.validate().is_err(), "non-finite amount");
        scene.entities[0].rules[0] = rule(
            "coin",
            RuleAction::EmitEvent { name: "x".into() },
            Some(RuleCondition {
                variable: "bad\nname".into(),
                compare: RuleCompare::Equal,
                value: 0.0,
            }),
        );
        assert!(scene.validate().is_err(), "condition name");
    }

    #[test]
    fn variable_store_rejects_bad_names_values_and_overflow() {
        let mut variables = RuleVariables::default();
        assert_eq!(variables.get("missing"), 0.0);
        assert_eq!(variables.set("", 1.0), Err(RuleVariableError::InvalidName));
        assert_eq!(
            variables.set(&"x".repeat(65), 1.0),
            Err(RuleVariableError::InvalidName)
        );
        assert_eq!(
            variables.set("v", f64::INFINITY),
            Err(RuleVariableError::NotFinite)
        );
        variables.set("big", f64::MAX).expect("finite");
        assert_eq!(
            variables.add("big", f64::MAX),
            Err(RuleVariableError::NotFinite)
        );
        for index in 1..RuleVariables::MAX_VARIABLES {
            variables.set(&format!("v{index}"), 1.0).expect("under cap");
        }
        assert_eq!(
            variables.set("one_more", 1.0),
            Err(RuleVariableError::TooMany)
        );
        variables
            .set("v1", 2.0)
            .expect("existing names can still change");
    }

    #[test]
    fn text_expansion_only_replaces_valid_variable_placeholders() {
        let mut variables = RuleVariables::default();
        variables.set("score", 12.0).expect("set");
        variables.set("speed", 1.5).expect("set");
        assert_eq!(
            expand_variables("Score {score}, speed {speed}, lives {lives}", &variables),
            "Score 12, speed 1.5, lives 0"
        );
        assert_eq!(expand_variables("{} { {score", &variables), "{} { {score");
        assert_eq!(expand_variables("{{score}}", &variables), "{12}");
        assert_eq!(
            expand_variables("a {bad\nname} b", &variables),
            "a {bad\nname} b"
        );
        assert_eq!(format_variable(-3.0), "-3");
        assert_eq!(format_variable(0.25), "0.25");
    }

    #[test]
    fn graph_positions_round_trip_and_reject_invalid_values() {
        let mut scene = coin_scene();
        scene.entities[0].rules[1].graph_position = Some([320.0, -84.5]);
        scene.validate().expect("valid layout");
        let reopened =
            Scene::from_ron_str(&scene.to_ron_string().expect("serialize")).expect("reopen");
        assert_eq!(
            reopened.entities[0].rules[1].graph_position,
            Some([320.0, -84.5])
        );
        assert_eq!(reopened.entities[0].rules[0].graph_position, None);

        scene.entities[0].rules[1].graph_position = Some([f32::NAN, 0.0]);
        assert!(scene.validate().is_err());
        scene.entities[0].rules[1].graph_position = Some([0.0, 2.0e6]);
        assert!(scene.validate().is_err());
    }
}
