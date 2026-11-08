//! Engine-owned actions for authored scene events, shared by Lua and Rust games.

use std::collections::{BTreeMap, HashMap, VecDeque};

use engine_ecs::prelude::World;
use engine_ui::{NodeId, Ui};

use crate::{GameStateName, RuleAction, RuleTrigger, Scene, SceneObjectId};

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
    SceneStart,
    KeyPressed(String),
    Contact {
        kind: ContactRuleKind,
        owner: String,
        other: String,
    },
    Timer {
        owner: String,
        rule_index: usize,
    },
    VariableChanged(String),
    StateChanged(GameStateName),
}

/// Which contact transition a [`RuleEvent::Contact`] reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContactRuleKind {
    /// Two solid objects began touching.
    CollisionEnter,
    /// Two solid objects stopped touching.
    CollisionExit,
    /// An overlap with a trigger volume began.
    TriggerEnter,
    /// An overlap with a trigger volume ended.
    TriggerExit,
}

/// Something that can run visual rules: a UI click, a named event, or one of
/// the runtime's own triggers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleEvent<'a> {
    /// Stable object ID of an authored UI Button.
    UiClicked(&'a str),
    /// Developer-defined event name.
    Named(&'a str),
    /// The scene finished starting.
    SceneStart,
    /// The named key went down this frame.
    KeyPressed(&'a str),
    /// A contact seen from `owner`'s side; `other` is the object it touched.
    Contact {
        /// Transition reported.
        kind: ContactRuleKind,
        /// Object whose rules are considered.
        owner: &'a str,
        /// The object on the other side.
        other: &'a str,
    },
    /// The timer rule `rule_index` of `owner` is due.
    Timer {
        /// Object that owns the rule.
        owner: &'a str,
        /// Position of the rule in the owner's rule list.
        rule_index: usize,
    },
    /// The named variable changed value.
    VariableChanged(&'a str),
    /// The game state changed to this state.
    StateChanged(GameStateName),
}

/// What dispatching one event did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RuleOutcome {
    /// Actions the dispatcher carried out itself.
    pub applied: usize,
    /// Actions that need the gameplay runtime (scene changes, object changes,
    /// spawns, game state), in rule order.
    pub deferred: Vec<RuleAction>,
}

impl PendingEvent {
    fn from_event(event: RuleEvent<'_>) -> Self {
        match event {
            RuleEvent::UiClicked(id) => Self::UiClicked(id.to_owned()),
            RuleEvent::Named(name) => Self::Named(name.to_owned()),
            RuleEvent::SceneStart => Self::SceneStart,
            RuleEvent::KeyPressed(key) => Self::KeyPressed(key.to_owned()),
            RuleEvent::Contact { kind, owner, other } => Self::Contact {
                kind,
                owner: owner.to_owned(),
                other: other.to_owned(),
            },
            RuleEvent::Timer { owner, rule_index } => Self::Timer {
                owner: owner.to_owned(),
                rule_index,
            },
            RuleEvent::VariableChanged(name) => Self::VariableChanged(name.to_owned()),
            RuleEvent::StateChanged(state) => Self::StateChanged(state),
        }
    }

    /// Whether the rule at `rule_index` of the object `owner_id` listens to this event.
    fn matches(&self, owner_id: Option<&str>, rule_index: usize, trigger: &RuleTrigger) -> bool {
        let with_ok = |with: &Option<String>, other: &String| {
            with.as_deref().is_none_or(|expected| expected == other)
        };
        match (trigger, self) {
            (RuleTrigger::UiClicked, Self::UiClicked(id)) => owner_id == Some(id.as_str()),
            (RuleTrigger::Named(name), Self::Named(event_name)) => name == event_name,
            (RuleTrigger::SceneStart, Self::SceneStart) => true,
            (RuleTrigger::KeyPressed { key }, Self::KeyPressed(pressed)) => key == pressed,
            (
                RuleTrigger::CollisionEnter { with },
                Self::Contact {
                    kind: ContactRuleKind::CollisionEnter,
                    owner,
                    other,
                },
            )
            | (
                RuleTrigger::CollisionExit { with },
                Self::Contact {
                    kind: ContactRuleKind::CollisionExit,
                    owner,
                    other,
                },
            )
            | (
                RuleTrigger::TriggerEnter { with },
                Self::Contact {
                    kind: ContactRuleKind::TriggerEnter,
                    owner,
                    other,
                },
            )
            | (
                RuleTrigger::TriggerExit { with },
                Self::Contact {
                    kind: ContactRuleKind::TriggerExit,
                    owner,
                    other,
                },
            ) => owner_id == Some(owner.as_str()) && with_ok(with, other),
            (
                RuleTrigger::Timer { .. },
                Self::Timer {
                    owner,
                    rule_index: due,
                },
            ) => owner_id == Some(owner.as_str()) && rule_index == *due,
            (RuleTrigger::VariableChanged { name }, Self::VariableChanged(changed)) => {
                name == changed
            }
            (RuleTrigger::StateChanged { state }, Self::StateChanged(entered)) => state == entered,
            _ => false,
        }
    }
}

impl Scene {
    /// Runs matching visual rules. Missing targets are reported and skipped
    /// without stopping other rules. Rules run in scene order, so a variable
    /// changed by one rule is seen by the conditions of later rules in the
    /// same dispatch.
    ///
    /// An owner with an object ID runs only while its live object exists and
    /// is not `Disabled`, so enabling a scene-disabled object turns its rules
    /// on. An owner without an ID does not run if disabled in the scene data.
    /// Rules of objects created at
    /// runtime (such as spawned prefabs) are not part of the scene and do not run.
    pub fn apply_rule_event(
        &self,
        event: RuleEvent<'_>,
        world: &mut World,
        ui: &mut Ui,
        ui_nodes: &HashMap<NodeId, String>,
    ) -> RuleOutcome {
        let mut variables = world.remove_resource::<RuleVariables>().unwrap_or_default();
        let outcome = self.dispatch_rules(event, world, ui, ui_nodes, &mut variables);
        world.insert_resource(variables);
        outcome
    }

    fn dispatch_rules(
        &self,
        event: RuleEvent<'_>,
        world: &mut World,
        ui: &mut Ui,
        ui_nodes: &HashMap<NodeId, String>,
        variables: &mut RuleVariables,
    ) -> RuleOutcome {
        // Which objects exist and are not disabled right now.
        let mut live: HashMap<String, bool> = HashMap::new();
        for entity in world.iter_entities() {
            if let Some(id) = entity.get::<SceneObjectId>() {
                live.insert(
                    id.0.clone(),
                    !entity.contains::<engine_ecs::components::Disabled>(),
                );
            }
        }
        let mut pending = VecDeque::from([PendingEvent::from_event(event)]);
        let mut outcome = RuleOutcome::default();
        let mut dispatched = 0;
        let mut attempted = 0;
        let mut warned_limit = false;
        while let Some(event) = pending.pop_front() {
            dispatched += 1;
            for owner in &self.entities {
                // An owner with an id is judged by the live `Disabled` marker,
                // so enabling a scene-disabled object turns its rules on. An
                // owner without one has only the scene data to go by.
                match &owner.object_id {
                    Some(id) if live.get(id) != Some(&true) => continue,
                    None if owner.disabled => continue,
                    _ => {}
                }
                for (rule_index, rule) in owner.rules.iter().enumerate() {
                    if !event.matches(owner.object_id.as_deref(), rule_index, &rule.trigger)
                        || !rule.conditions_hold(variables)
                    {
                        continue;
                    }
                    if attempted == MAX_RULE_ACTIONS_PER_DISPATCH {
                        tracing::warn!(
                            limit = MAX_RULE_ACTIONS_PER_DISPATCH,
                            "visual rule action chain stopped at its action limit"
                        );
                        return outcome;
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
                        // The gameplay runtime carries these out.
                        RuleAction::ChangeScene { .. }
                        | RuleAction::SetVisible { .. }
                        | RuleAction::SetEnabled { .. }
                        | RuleAction::SpawnPrefab { .. }
                        | RuleAction::Destroy { .. }
                        | RuleAction::SetGameState { .. } => {
                            outcome.deferred.push(rule.action.clone());
                            false
                        }
                    };
                    if success {
                        outcome.applied += 1;
                    } else if matches!(
                        rule.action,
                        RuleAction::SetUiText { .. } | RuleAction::PlayAudio { .. }
                    ) {
                        tracing::warn!(action = ?rule.action, "visual rule target is unavailable");
                    }
                }
            }
        }
        outcome
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
            conditions: condition.into_iter().collect(),
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
        scene.instantiate(&mut world);
        let coin = |world: &mut World, ui: &mut Ui| {
            scene
                .apply_rule_event(RuleEvent::Named("coin"), world, ui, &nodes)
                .applied
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
        scene.instantiate(&mut world);
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
        assert!(scene.entities[0].rules[0].conditions.is_empty());
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

    fn named_rule(trigger: RuleTrigger, action: RuleAction) -> RuleData {
        RuleData::new(trigger, action)
    }

    fn cond(variable: &str, value: f64) -> RuleCondition {
        RuleCondition {
            variable: variable.into(),
            compare: RuleCompare::GreaterOrEqual,
            value,
        }
    }

    fn scene_with(rules: Vec<RuleData>) -> Scene {
        Scene {
            entities: vec![SceneEntity {
                rules,
                ..SceneEntity::default()
            }],
            ..Scene::default()
        }
    }

    #[test]
    fn legacy_single_condition_round_trips_in_legacy_form() {
        let text = r#"(
            version: 5,
            entities: [(
                rules: [(trigger: Named("a"), action: EmitEvent(name: "b"),
                    condition: Some((variable: "coins", compare: GreaterOrEqual, value: 3.0)))],
            )],
        )"#;
        let scene = Scene::from_ron_str(text).expect("legacy condition");
        let rule = &scene.entities[0].rules[0];
        assert_eq!(rule.conditions.len(), 1);
        assert_eq!(rule.match_mode, crate::RuleMatch::All);
        let saved = scene.to_ron_string().expect("serialize");
        assert!(saved.contains("condition:"), "{saved}");
        assert!(!saved.contains("conditions:"), "{saved}");
    }

    #[test]
    fn two_conditions_write_the_list_form() {
        let mut rule = named_rule(
            RuleTrigger::Named("a".into()),
            RuleAction::EmitEvent { name: "b".into() },
        );
        rule.conditions = vec![cond("coins", 1.0), cond("lives", 1.0)];
        rule.match_mode = crate::RuleMatch::Any;
        let scene = scene_with(vec![rule]);
        let saved = scene.to_ron_string().expect("serialize");
        assert!(saved.contains("conditions:"), "{saved}");
        assert!(saved.contains("match_mode: Any"), "{saved}");
        assert_eq!(Scene::from_ron_str(&saved).expect("parse"), scene);
    }

    #[test]
    fn any_and_all_conditions_evaluate() {
        let mut variables = RuleVariables::default();
        variables.set("coins", 2.0).expect("set");
        variables.set("lives", 0.0).expect("set");
        let mut rule = named_rule(
            RuleTrigger::Named("a".into()),
            RuleAction::EmitEvent { name: "b".into() },
        );
        assert!(rule.conditions_hold(&variables), "empty is true");
        rule.conditions = vec![cond("coins", 1.0), cond("lives", 1.0)];
        assert!(!rule.conditions_hold(&variables));
        rule.match_mode = crate::RuleMatch::Any;
        assert!(rule.conditions_hold(&variables));
    }

    #[test]
    fn new_triggers_and_actions_round_trip() {
        use crate::{GameStateName, SpawnAt};
        let triggers = vec![
            RuleTrigger::SceneStart,
            RuleTrigger::KeyPressed {
                key: "Space".into(),
            },
            RuleTrigger::CollisionEnter { with: None },
            RuleTrigger::CollisionExit {
                with: Some("3fa85f64-5717-4562-b3fc-2c963f66afa6".into()),
            },
            RuleTrigger::TriggerEnter { with: None },
            RuleTrigger::TriggerExit { with: None },
            RuleTrigger::Timer {
                seconds: 1.5,
                repeat: true,
            },
            RuleTrigger::VariableChanged {
                name: "coins".into(),
            },
            RuleTrigger::StateChanged {
                state: GameStateName::Paused,
            },
        ];
        let actions = vec![
            RuleAction::ChangeScene {
                path: "scenes/b.ron".into(),
            },
            RuleAction::SetVisible {
                target: LABEL.into(),
                visible: false,
            },
            RuleAction::SetEnabled {
                target: LABEL.into(),
                enabled: true,
            },
            RuleAction::SpawnPrefab {
                path: "assets/prefabs/Crate.prefab".into(),
                at: SpawnAt::Object(LABEL.into()),
            },
            RuleAction::SpawnPrefab {
                path: "assets/prefabs/Crate.prefab".into(),
                at: SpawnAt::Position([1.0, 2.0, 3.0]),
            },
            RuleAction::Destroy {
                target: LABEL.into(),
            },
            RuleAction::SetGameState {
                state: GameStateName::GameOver,
            },
        ];
        let mut rules: Vec<RuleData> = triggers
            .into_iter()
            .map(|t| named_rule(t, RuleAction::EmitEvent { name: "x".into() }))
            .collect();
        rules.extend(
            actions
                .into_iter()
                .map(|a| named_rule(RuleTrigger::SceneStart, a)),
        );
        let scene = scene_with(rules);
        scene.validate().expect("valid");
        let text = scene.to_ron_string().expect("serialize");
        assert_eq!(Scene::from_ron_str(&text).expect("parse"), scene);
    }

    #[test]
    fn validate_rejects_bad_new_rules() {
        use crate::SpawnAt;
        let emit = || RuleAction::EmitEvent { name: "x".into() };
        let trig = |t: RuleTrigger| scene_with(vec![named_rule(t, emit())]);
        let act = |a: RuleAction| scene_with(vec![named_rule(RuleTrigger::SceneStart, a)]);

        let mut many = named_rule(RuleTrigger::SceneStart, emit());
        many.conditions = (0..9).map(|_| cond("a", 0.0)).collect();
        assert!(scene_with(vec![many.clone()]).validate().is_err(), "9");
        many.conditions.truncate(8);
        assert!(scene_with(vec![many]).validate().is_ok(), "8");

        for seconds in [0.0, 0.04, 3601.0, f32::NAN] {
            assert!(
                trig(RuleTrigger::Timer {
                    seconds,
                    repeat: false
                })
                .validate()
                .is_err(),
                "{seconds}"
            );
        }
        for seconds in [0.05, 3600.0] {
            assert!(
                trig(RuleTrigger::Timer {
                    seconds,
                    repeat: true
                })
                .validate()
                .is_ok(),
                "{seconds}"
            );
        }
        for key in [
            "",
            "a
b",
            &"k".repeat(33),
        ] {
            assert!(
                trig(RuleTrigger::KeyPressed { key: key.into() })
                    .validate()
                    .is_err()
            );
        }
        assert!(
            trig(RuleTrigger::VariableChanged {
                name: String::new()
            })
            .validate()
            .is_err()
        );
        assert!(
            act(RuleAction::SetVisible {
                target: String::new(),
                visible: true
            })
            .validate()
            .is_err()
        );
        assert!(
            act(RuleAction::SetEnabled {
                target: String::new(),
                enabled: true
            })
            .validate()
            .is_err()
        );
        assert!(
            act(RuleAction::Destroy {
                target: String::new()
            })
            .validate()
            .is_err()
        );
        for path in [
            "assets/a.ron",
            "scenes/a.txt",
            "../scenes/a.ron",
            "/scenes/a.ron",
        ] {
            assert!(
                act(RuleAction::ChangeScene { path: path.into() })
                    .validate()
                    .is_err(),
                "{path}"
            );
        }
        for path in [
            "scenes/a.prefab",
            "assets/other/a.prefab",
            "assets/prefabs/../a.prefab",
            "assets/prefabs/a.ron",
        ] {
            assert!(
                act(RuleAction::SpawnPrefab {
                    path: path.into(),
                    at: SpawnAt::Position([0.0; 3])
                })
                .validate()
                .is_err(),
                "{path}"
            );
        }
        assert!(
            act(RuleAction::SpawnPrefab {
                path: "assets/prefabs/a.prefab".into(),
                at: SpawnAt::Position([f32::INFINITY, 0.0, 0.0]),
            })
            .validate()
            .is_err()
        );
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

    const OWNER: &str = "00000000-0000-4000-8000-0000000000a1";
    const OTHER: &str = "00000000-0000-4000-8000-0000000000b2";

    fn owner_scene(triggers: Vec<RuleTrigger>) -> Scene {
        Scene {
            entities: vec![SceneEntity {
                object_id: Some(OWNER.into()),
                rules: triggers
                    .into_iter()
                    .map(|trigger| {
                        RuleData::new(
                            trigger,
                            RuleAction::AddToVariable {
                                name: "hits".into(),
                                amount: 1.0,
                            },
                        )
                    })
                    .collect(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn fire(scene: &Scene, world: &mut World, event: RuleEvent<'_>) -> RuleOutcome {
        let mut ui = Ui::new(800.0, 600.0);
        scene.apply_rule_event(event, world, &mut ui, &HashMap::new())
    }

    fn hits(world: &World) -> f64 {
        world
            .get_resource::<RuleVariables>()
            .map_or(0.0, |variables| variables.get("hits"))
    }

    #[test]
    fn contact_rules_match_the_owner_side_and_the_with_filter() {
        let scene = owner_scene(vec![
            RuleTrigger::CollisionEnter { with: None },
            RuleTrigger::CollisionEnter {
                with: Some(OTHER.into()),
            },
            RuleTrigger::TriggerExit { with: None },
        ]);
        let mut world = World::new();
        scene.instantiate(&mut world);
        let contact = |kind, owner, other| RuleEvent::Contact { kind, owner, other };
        let enter = ContactRuleKind::CollisionEnter;
        // Any object matches the first rule, OTHER also matches the second.
        assert_eq!(
            fire(&scene, &mut world, contact(enter, OWNER, OTHER)).applied,
            2
        );
        assert_eq!(
            fire(&scene, &mut world, contact(enter, OWNER, "stranger")).applied,
            1
        );
        // A contact seen from another object's side is not this owner's.
        assert_eq!(
            fire(&scene, &mut world, contact(enter, "stranger", OWNER)).applied,
            0
        );
        // The wrong kind does not match.
        let exit = ContactRuleKind::CollisionExit;
        assert_eq!(
            fire(&scene, &mut world, contact(exit, OWNER, OTHER)).applied,
            0
        );
        let trigger_exit = ContactRuleKind::TriggerExit;
        assert_eq!(
            fire(&scene, &mut world, contact(trigger_exit, OWNER, "any")).applied,
            1
        );
        assert_eq!(hits(&world), 4.0);
    }

    #[test]
    fn runtime_triggers_match_their_events() {
        let scene = owner_scene(vec![
            RuleTrigger::SceneStart,
            RuleTrigger::KeyPressed {
                key: "Space".into(),
            },
            RuleTrigger::Timer {
                seconds: 1.0,
                repeat: false,
            },
            RuleTrigger::VariableChanged {
                name: "score".into(),
            },
            RuleTrigger::StateChanged {
                state: GameStateName::Paused,
            },
        ]);
        let mut world = World::new();
        scene.instantiate(&mut world);
        let count = |world: &mut World, event| fire(&scene, world, event).applied;
        assert_eq!(count(&mut world, RuleEvent::SceneStart), 1);
        assert_eq!(count(&mut world, RuleEvent::KeyPressed("Space")), 1);
        assert_eq!(count(&mut world, RuleEvent::KeyPressed("W")), 0);
        let timer = |owner, rule_index| RuleEvent::Timer { owner, rule_index };
        assert_eq!(count(&mut world, timer(OWNER, 2)), 1);
        assert_eq!(count(&mut world, timer(OWNER, 1)), 0, "not a timer rule");
        assert_eq!(count(&mut world, timer("stranger", 2)), 0);
        assert_eq!(count(&mut world, RuleEvent::VariableChanged("score")), 1);
        assert_eq!(count(&mut world, RuleEvent::VariableChanged("other")), 0);
        assert_eq!(
            count(&mut world, RuleEvent::StateChanged(GameStateName::Paused)),
            1
        );
        assert_eq!(
            count(&mut world, RuleEvent::StateChanged(GameStateName::Menu)),
            0
        );
    }

    #[test]
    fn rules_of_destroyed_or_disabled_owners_do_not_fire() {
        let scene = owner_scene(vec![RuleTrigger::SceneStart]);
        let mut world = World::new();
        let spawned = scene.instantiate(&mut world);
        assert_eq!(fire(&scene, &mut world, RuleEvent::SceneStart).applied, 1);

        world
            .entity_mut(spawned[0])
            .insert(engine_ecs::components::Disabled);
        assert_eq!(fire(&scene, &mut world, RuleEvent::SceneStart).applied, 0);

        world
            .entity_mut(spawned[0])
            .remove::<engine_ecs::components::Disabled>();
        assert_eq!(fire(&scene, &mut world, RuleEvent::SceneStart).applied, 1);

        world.despawn(spawned[0]);
        assert_eq!(fire(&scene, &mut world, RuleEvent::SceneStart).applied, 0);

        let mut scene = scene;
        scene.entities[0].disabled = true;
        let mut world = World::new();
        scene.instantiate(&mut world);
        assert_eq!(fire(&scene, &mut world, RuleEvent::SceneStart).applied, 0);
    }

    #[test]
    fn a_scene_disabled_owner_fires_once_it_is_enabled() {
        let mut scene = owner_scene(vec![RuleTrigger::SceneStart]);
        scene.entities[0].disabled = true;
        let mut world = World::new();
        let spawned = scene.instantiate(&mut world);
        assert_eq!(fire(&scene, &mut world, RuleEvent::SceneStart).applied, 0);
        world
            .entity_mut(spawned[0])
            .remove::<engine_ecs::components::Disabled>();
        assert_eq!(fire(&scene, &mut world, RuleEvent::SceneStart).applied, 1);
    }

    #[test]
    fn runtime_actions_are_deferred_in_rule_order() {
        let actions = vec![
            RuleAction::ChangeScene {
                path: "scenes/b.ron".into(),
            },
            RuleAction::SetVisible {
                target: OTHER.into(),
                visible: false,
            },
            RuleAction::SetEnabled {
                target: OTHER.into(),
                enabled: false,
            },
            RuleAction::SpawnPrefab {
                path: "assets/prefabs/Crate.prefab".into(),
                at: crate::SpawnAt::Object(OTHER.into()),
            },
            RuleAction::Destroy {
                target: OTHER.into(),
            },
            RuleAction::SetGameState {
                state: GameStateName::GameOver,
            },
        ];
        let mut scene = owner_scene(vec![]);
        scene.entities[0].rules = actions
            .iter()
            .map(|action| RuleData::new(RuleTrigger::SceneStart, action.clone()))
            .collect();
        scene.entities[0].rules.insert(
            3,
            RuleData::new(
                RuleTrigger::SceneStart,
                RuleAction::AddToVariable {
                    name: "hits".into(),
                    amount: 1.0,
                },
            ),
        );
        let mut world = World::new();
        scene.instantiate(&mut world);
        let outcome = fire(&scene, &mut world, RuleEvent::SceneStart);
        assert_eq!(
            outcome.applied, 1,
            "the variable change ran in the dispatcher"
        );
        assert_eq!(outcome.deferred, actions);
    }
}
