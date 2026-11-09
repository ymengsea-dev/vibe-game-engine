//! Visual rules that need the running game: key presses, timers, variable and
//! state changes, contacts, and the actions that change objects or scenes.
//!
//! The pure parts live here so they can be tested without a window:
//! [`RuleClock`] decides which time and change based triggers are due, and
//! [`rule_commands`] turns deferred rule actions into [`GameCommand`]s.

use std::collections::{BTreeMap, HashMap, HashSet};

use engine_ecs::components::{GlobalTransform, Transform};
use engine_ecs::prelude::World;
use engine_scene::{
    ContactRuleKind, GameStateName, RuleAction, RuleEvent, RuleTrigger, RuleVariables, Scene,
    SceneObjectId, SpawnAt,
};
use engine_scripting::{GameCommand, ScriptKey};

use super::CollisionEvent;
use super::collisions::ContactKind;

/// Deepest nesting of rule actions applying commands that emit events whose
/// rules apply more commands. Beyond it the chain stops.
pub(crate) const MAX_RULE_APPLY_DEPTH: u8 = 8;

/// A trigger that became due this frame.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DueEvent {
    Key(&'static str),
    Timer { owner: String, rule_index: usize },
    Variable(String),
    State(GameStateName),
}

impl DueEvent {
    pub(crate) fn as_event(&self) -> RuleEvent<'_> {
        match self {
            Self::Key(key) => RuleEvent::KeyPressed(key),
            Self::Timer { owner, rule_index } => RuleEvent::Timer {
                owner,
                rule_index: *rule_index,
            },
            Self::Variable(name) => RuleEvent::VariableChanged(name),
            Self::State(state) => RuleEvent::StateChanged(*state),
        }
    }
}

/// Timer progress plus the last seen variable values and game state.
#[derive(Debug, Default)]
pub(crate) struct RuleClock {
    elapsed: HashMap<(String, usize), f64>,
    finished: HashSet<(String, usize)>,
    variables: BTreeMap<String, f64>,
    state: Option<GameStateName>,
}

fn watched_variables(scene: &Scene) -> BTreeMap<String, f64> {
    scene
        .entities
        .iter()
        .flat_map(|entity| &entity.rules)
        .filter_map(|rule| match &rule.trigger {
            RuleTrigger::VariableChanged { name } => Some((name.clone(), 0.0)),
            _ => None,
        })
        .collect()
}

impl RuleClock {
    /// Starts over for a freshly loaded scene: every timer at zero, and the
    /// current variables and state taken as already seen.
    pub(crate) fn reset(
        &mut self,
        scene: &Scene,
        variables: Option<&RuleVariables>,
        state: GameStateName,
    ) {
        self.elapsed.clear();
        self.finished.clear();
        self.variables = watched_variables(scene);
        for (name, value) in &mut self.variables {
            *value = variables.map_or(0.0, |all| all.get(name));
        }
        self.state = Some(state);
    }

    /// Starts watching the variables `scene`'s rules name that aren't watched
    /// yet, from their current values. Timers need nothing: they start at zero
    /// the first time they are seen.
    pub(crate) fn watch(&mut self, scene: &Scene, variables: Option<&RuleVariables>) {
        for name in watched_variables(scene).into_keys() {
            let value = variables.map_or(0.0, |all| all.get(&name));
            self.variables.entry(name).or_insert(value);
        }
    }

    /// Takes the current variables and state as already seen, without touching
    /// the timers. Called after scene start so what `start` callbacks and
    /// scene-start rules set does not raise change rules on the first frame.
    pub(crate) fn rebase(&mut self, variables: Option<&RuleVariables>, state: GameStateName) {
        for (name, value) in &mut self.variables {
            *value = variables.map_or(0.0, |all| all.get(name));
        }
        self.state = Some(state);
    }

    /// The triggers due this frame, in order: pressed keys, timers, changed
    /// variables, then the state change. Time stands still while `paused`.
    /// Timers of disabled or despawned owners still run and are consumed
    /// without firing: the dispatcher skips those owners by their live state,
    /// so a scene-disabled owner that is enabled later has been counting.
    /// What a rule changes is compared at the next call, so it fires next frame.
    pub(crate) fn due(
        &mut self,
        scene: &Scene,
        variables: Option<&RuleVariables>,
        state: GameStateName,
        pressed: &[ScriptKey],
        dt: f32,
        paused: bool,
    ) -> Vec<DueEvent> {
        let mut due = Vec::new();
        let listens_to =
            |key: &str| {
                scene.entities.iter().flat_map(|entity| &entity.rules).any(
                    |rule| matches!(&rule.trigger, RuleTrigger::KeyPressed { key: k } if k == key),
                )
            };
        due.extend(
            pressed
                .iter()
                .map(|key| key.name())
                .filter(|name| listens_to(name))
                .map(DueEvent::Key),
        );

        let step = if paused { 0.0 } else { f64::from(dt.max(0.0)) };
        for owner in &scene.entities {
            let Some(id) = &owner.object_id else {
                continue;
            };
            for (rule_index, rule) in owner.rules.iter().enumerate() {
                let RuleTrigger::Timer { seconds, repeat } = rule.trigger else {
                    continue;
                };
                let key = (id.clone(), rule_index);
                if self.finished.contains(&key) {
                    continue;
                }
                let seconds = f64::from(seconds);
                let elapsed = self.elapsed.entry(key.clone()).or_insert(0.0);
                *elapsed += step;
                if *elapsed < seconds {
                    continue;
                }
                due.push(DueEvent::Timer {
                    owner: id.clone(),
                    rule_index,
                });
                if repeat {
                    *elapsed -= seconds;
                    if *elapsed >= seconds {
                        *elapsed = 0.0;
                    }
                } else {
                    self.elapsed.remove(&key);
                    self.finished.insert(key);
                }
            }
        }

        for (name, seen) in &mut self.variables {
            let now = variables.map_or(0.0, |all| all.get(name));
            if now != *seen {
                *seen = now;
                due.push(DueEvent::Variable(name.clone()));
            }
        }

        if self.state != Some(state) {
            self.state = Some(state);
            due.push(DueEvent::State(state));
        }
        due
    }
}

/// Counts how deeply rule actions are applying commands.
#[derive(Debug, Default)]
pub(crate) struct ApplyGuard {
    depth: u8,
    warned: bool,
}

impl ApplyGuard {
    /// Enters one level; `false` (with one warning per chain) when the chain is too deep.
    pub(crate) fn enter(&mut self) -> bool {
        if self.depth >= MAX_RULE_APPLY_DEPTH {
            if !self.warned {
                self.warned = true;
                tracing::warn!(
                    limit = MAX_RULE_APPLY_DEPTH,
                    "visual rule action chain stopped at its nesting limit"
                );
            }
            return false;
        }
        self.depth += 1;
        true
    }

    /// Leaves a level entered with [`Self::enter`].
    pub(crate) fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
        if self.depth == 0 {
            self.warned = false;
        }
    }
}

/// The rule events a contact raises: one per side, so each object's own rules
/// see the other object.
pub(crate) fn contact_events(event: &CollisionEvent) -> [RuleEvent<'_>; 2] {
    let kind = match event.kind {
        ContactKind::Enter => ContactRuleKind::CollisionEnter,
        ContactKind::Exit => ContactRuleKind::CollisionExit,
        ContactKind::TriggerEnter => ContactRuleKind::TriggerEnter,
        ContactKind::TriggerExit => ContactRuleKind::TriggerExit,
    };
    [
        RuleEvent::Contact {
            kind,
            owner: &event.first,
            other: &event.second,
        },
        RuleEvent::Contact {
            kind,
            owner: &event.second,
            other: &event.first,
        },
    ]
}

/// The command a deferred rule action stands for. `None` for actions the
/// dispatcher handles itself, and for a spawn whose anchor object is missing
/// (warned once per object).
pub(crate) fn rule_commands(
    action: &RuleAction,
    world: &World,
    warned: &mut HashSet<(String, &'static str)>,
) -> Option<GameCommand> {
    Some(match action {
        RuleAction::ChangeScene { path } => GameCommand::ChangeScene { path: path.clone() },
        RuleAction::LoadScene { path } => GameCommand::LoadScene { path: path.clone() },
        RuleAction::UnloadScene { path } => GameCommand::UnloadScene { path: path.clone() },
        RuleAction::NextScene => GameCommand::NextScene,
        RuleAction::SetVisible { target, visible } => GameCommand::SetVisible {
            object_id: target.clone(),
            visible: *visible,
        },
        RuleAction::SetEnabled { target, enabled } => GameCommand::SetEnabled {
            object_id: target.clone(),
            enabled: *enabled,
        },
        RuleAction::Destroy { target } => GameCommand::Destroy {
            object_id: target.clone(),
        },
        RuleAction::SetGameState { state } => GameCommand::SetGameState {
            state: state.name().to_owned(),
        },
        RuleAction::SpawnPrefab { path, at } => {
            let position = match at {
                SpawnAt::Position(position) => *position,
                SpawnAt::Object(id) => match object_position(world, id) {
                    Some(position) => position,
                    None => {
                        if warned.insert((id.clone(), "spawn_at")) {
                            tracing::warn!(object_id = %id, "spawn rule's anchor object is missing");
                        }
                        return None;
                    }
                },
            };
            GameCommand::SpawnPrefab {
                object_id: uuid::Uuid::new_v4().to_string(),
                path: path.clone(),
                position,
            }
        }
        RuleAction::SetUiText { .. }
        | RuleAction::PlayAudio { .. }
        | RuleAction::EmitEvent { .. }
        | RuleAction::SetVariable { .. }
        | RuleAction::AddToVariable { .. } => return None,
    })
}

fn object_position(world: &World, id: &str) -> Option<[f32; 3]> {
    let entity = world
        .iter_entities()
        .find(|entity| entity.get::<SceneObjectId>().is_some_and(|o| o.0 == id))?;
    let transform = entity
        .get::<GlobalTransform>()
        .map(|global| global.0)
        .or_else(|| entity.get::<Transform>().map(|local| local.0))?;
    Some(transform.translation.to_array())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use engine_ecs::components::{Disabled, Hidden};
    use engine_physics::PhysicsWorld;
    use engine_scene::{GameState, RuleData, SceneEntity};
    use engine_ui::Ui;

    use super::super::{GameplayRuntime, apply_world_commands, find_object, state};
    use super::*;

    const OWNER: &str = "00000000-0000-4000-8000-0000000000a1";
    const OTHER: &str = "00000000-0000-4000-8000-0000000000b2";
    const FRAME: f32 = 1.0 / 60.0;

    fn rule(trigger: RuleTrigger, action: RuleAction) -> RuleData {
        RuleData::new(trigger, action)
    }

    fn bump(name: &str) -> RuleAction {
        RuleAction::AddToVariable {
            name: name.into(),
            amount: 1.0,
        }
    }

    fn scene_with(rules: Vec<RuleData>) -> Scene {
        Scene {
            entities: vec![
                SceneEntity {
                    object_id: Some(OWNER.into()),
                    rules,
                    ..Default::default()
                },
                SceneEntity {
                    object_id: Some(OTHER.into()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    /// A scene, its world and clock, stepping like the host does each frame.
    struct Harness {
        scene: Scene,
        world: World,
        ui: Ui,
        clock: RuleClock,
    }

    impl Harness {
        fn new(rules: Vec<RuleData>) -> Self {
            let scene = scene_with(rules);
            let mut world = World::new();
            scene.instantiate(&mut world);
            let mut clock = RuleClock::default();
            clock.reset(&scene, world.get_resource(), GameStateName::Playing);
            Self {
                scene,
                world,
                ui: Ui::new(800.0, 600.0),
                clock,
            }
        }

        fn dispatch(&mut self, event: RuleEvent<'_>) -> usize {
            self.scene
                .apply_rule_event(event, &mut self.world, &mut self.ui, &HashMap::new())
                .applied
        }

        /// One frame: keys, timers and changes as `update_rules` runs them.
        fn frame(&mut self, pressed: &[ScriptKey], dt: f32, paused: bool) -> usize {
            let state = state::current(&self.world);
            let due = self.clock.due(
                &self.scene,
                self.world.get_resource(),
                state,
                pressed,
                dt,
                paused,
            );
            due.iter()
                .map(|event| self.dispatch(event.as_event()))
                .sum()
        }

        fn count(&self, name: &str) -> f64 {
            self.world
                .get_resource::<RuleVariables>()
                .map_or(0.0, |variables| variables.get(name))
        }
    }

    #[test]
    fn scene_start_rule_fires_once_per_scene_load() {
        let mut h = Harness::new(vec![rule(RuleTrigger::SceneStart, bump("started"))]);
        // The host fires SceneStart once, after the scene's scripts' start.
        assert_eq!(h.dispatch(RuleEvent::SceneStart), 1);
        // Frames never raise it again.
        for _ in 0..10 {
            h.frame(&[], FRAME, false);
        }
        assert_eq!(h.count("started"), 1.0);
        // A second scene load starts fresh and fires once more.
        h.clock
            .reset(&h.scene, h.world.get_resource(), GameStateName::Playing);
        assert_eq!(h.dispatch(RuleEvent::SceneStart), 1);
        assert_eq!(h.count("started"), 2.0);
    }

    #[test]
    fn key_pressed_rule_fires_on_press_not_hold() {
        let mut h = Harness::new(vec![rule(
            RuleTrigger::KeyPressed {
                key: "Space".into(),
            },
            bump("jumps"),
        )]);
        // The host passes only keys that went down this frame.
        h.frame(&[ScriptKey::Space], FRAME, false);
        h.frame(&[], FRAME, false);
        h.frame(&[], FRAME, false);
        assert_eq!(h.count("jumps"), 1.0, "holding does not repeat");
        h.frame(&[ScriptKey::W], FRAME, false);
        assert_eq!(h.count("jumps"), 1.0, "another key");
        // Key rules still run while paused, so a menu can unpause.
        h.frame(&[ScriptKey::Space], FRAME, true);
        assert_eq!(h.count("jumps"), 2.0);
    }

    #[test]
    fn timer_rule_fires_after_seconds_and_repeats() {
        let once = RuleTrigger::Timer {
            seconds: 1.0,
            repeat: false,
        };
        let repeating = RuleTrigger::Timer {
            seconds: 1.0,
            repeat: true,
        };
        let mut h = Harness::new(vec![
            rule(once, bump("once")),
            rule(repeating, bump("tick")),
        ]);
        let mut once_at = Vec::new();
        let mut tick_at = Vec::new();
        let (mut once_seen, mut tick_seen) = (0.0, 0.0);
        for frame in 1..=183 {
            h.frame(&[], FRAME, false);
            let seconds = f64::from(frame) / 60.0;
            if h.count("once") != once_seen {
                once_seen = h.count("once");
                once_at.push(seconds);
            }
            if h.count("tick") != tick_seen {
                tick_seen = h.count("tick");
                tick_at.push(seconds);
            }
        }
        assert_eq!(once_at.len(), 1);
        assert!((1.0..1.0 + 2.0 * f64::from(FRAME)).contains(&once_at[0]));
        assert_eq!(tick_at.len(), 3, "fires at 1, 2 and 3 s: {tick_at:?}");
        for (n, at) in tick_at.iter().enumerate() {
            let due = (n + 1) as f64;
            assert!((due..due + 2.0 / 60.0).contains(at), "{at} vs {due}");
        }
    }

    #[test]
    fn a_scene_disabled_owner_counts_its_timer_and_fires_once_enabled() {
        let mut h = Harness::new(vec![rule(
            RuleTrigger::Timer {
                seconds: 1.0,
                repeat: true,
            },
            bump("tick"),
        )]);
        h.scene.entities[0].disabled = true;
        let owner = find_object(&h.world, OWNER).expect("owner");
        h.world.entity_mut(owner).insert(Disabled);
        for _ in 0..61 {
            h.frame(&[], FRAME, false);
        }
        assert_eq!(h.count("tick"), 0.0, "disabled owners do not fire");
        h.world.entity_mut(owner).remove::<Disabled>();
        for _ in 0..61 {
            h.frame(&[], FRAME, false);
        }
        assert_eq!(h.count("tick"), 1.0, "fires once enabled");
    }

    #[test]
    fn scene_rule_actions_become_commands_and_new_rules_get_watched() {
        let world = crate::prelude::World::new();
        let mut warned = HashSet::new();
        let path = "scenes/extra.ron".to_owned();
        assert_eq!(
            rule_commands(
                &RuleAction::LoadScene { path: path.clone() },
                &world,
                &mut warned
            ),
            Some(GameCommand::LoadScene { path: path.clone() })
        );
        assert_eq!(
            rule_commands(
                &RuleAction::UnloadScene { path: path.clone() },
                &world,
                &mut warned
            ),
            Some(GameCommand::UnloadScene { path })
        );
        assert_eq!(
            rule_commands(&RuleAction::NextScene, &world, &mut warned),
            Some(GameCommand::NextScene)
        );

        let mut clock = RuleClock::default();
        let mut variables = RuleVariables::default();
        let _ = variables.set("score", 5.0);
        let scene = Scene {
            entities: vec![engine_scene::SceneEntity {
                rules: vec![rule(
                    RuleTrigger::VariableChanged {
                        name: "score".into(),
                    },
                    bump("changes"),
                )],
                ..Default::default()
            }],
            ..Default::default()
        };
        clock.watch(&scene, Some(&variables));
        assert_eq!(clock.variables.get("score"), Some(&5.0));
    }

    #[test]
    fn rebase_keeps_start_time_values_from_raising_change_rules() {
        let mut h = Harness::new(vec![rule(
            RuleTrigger::VariableChanged {
                name: "score".into(),
            },
            bump("changes"),
        )]);
        let mut variables = RuleVariables::default();
        let _ = variables.set("score", 5.0);
        h.world.insert_resource(variables);
        let mut fresh = Harness::new(vec![rule(
            RuleTrigger::VariableChanged {
                name: "score".into(),
            },
            bump("changes"),
        )]);
        let mut again = RuleVariables::default();
        let _ = again.set("score", 5.0);
        fresh.world.insert_resource(again);
        // Without a rebase the value set after reset looks like a change.
        h.frame(&[], FRAME, false);
        assert_eq!(h.count("changes"), 1.0);
        // With one, the first frame is quiet.
        fresh
            .clock
            .rebase(fresh.world.get_resource(), GameStateName::Playing);
        fresh.frame(&[], FRAME, false);
        assert_eq!(fresh.count("changes"), 0.0);
    }

    #[test]
    fn no_timer_time_passes_while_paused() {
        let mut h = Harness::new(vec![rule(
            RuleTrigger::Timer {
                seconds: 1.0,
                repeat: true,
            },
            bump("tick"),
        )]);
        for _ in 0..600 {
            h.frame(&[], FRAME, true);
        }
        assert_eq!(h.count("tick"), 0.0);
        for _ in 0..61 {
            h.frame(&[], FRAME, false);
        }
        assert_eq!(h.count("tick"), 1.0);
    }

    #[test]
    fn variable_changed_rule_that_changes_its_variable_fires_once_per_frame() {
        let mut h = Harness::new(vec![rule(
            RuleTrigger::VariableChanged { name: "x".into() },
            bump("x"),
        )]);
        assert_eq!(h.frame(&[], FRAME, false), 0, "nothing changed yet");
        h.world
            .get_resource_or_insert_with(RuleVariables::default)
            .set("x", 5.0)
            .expect("set");
        // Each frame the rule runs exactly once; its own change waits a frame.
        for expected in [6.0, 7.0, 8.0] {
            assert_eq!(h.frame(&[], FRAME, false), 1);
            assert_eq!(h.count("x"), expected);
        }
    }

    #[test]
    fn state_changed_rule_fires_on_change_only() {
        let mut h = Harness::new(vec![rule(
            RuleTrigger::StateChanged {
                state: GameStateName::Paused,
            },
            bump("pauses"),
        )]);
        state::init_state(&mut h.world, None);
        h.frame(&[], FRAME, false);
        assert_eq!(h.count("pauses"), 0.0);
        state::apply_state_commands(
            &mut h.world,
            &[GameCommand::SetGameState {
                state: "paused".into(),
            }],
        );
        h.frame(&[], FRAME, true);
        h.frame(&[], FRAME, true);
        assert_eq!(
            h.count("pauses"),
            1.0,
            "fires on the change, not while it holds"
        );
        // Entering a different state does not match; coming back re-fires.
        state::apply_state_commands(
            &mut h.world,
            &[GameCommand::SetGameState {
                state: "menu".into(),
            }],
        );
        h.frame(&[], FRAME, false);
        state::apply_state_commands(
            &mut h.world,
            &[GameCommand::SetGameState {
                state: "paused".into(),
            }],
        );
        h.frame(&[], FRAME, true);
        assert_eq!(h.count("pauses"), 2.0);
    }

    #[test]
    fn collision_events_raise_a_rule_event_for_each_side() {
        let event = CollisionEvent {
            kind: ContactKind::TriggerEnter,
            first: "a".into(),
            second: "b".into(),
        };
        let [one, two] = contact_events(&event);
        assert_eq!(
            one,
            RuleEvent::Contact {
                kind: ContactRuleKind::TriggerEnter,
                owner: "a",
                other: "b"
            }
        );
        assert_eq!(
            two,
            RuleEvent::Contact {
                kind: ContactRuleKind::TriggerEnter,
                owner: "b",
                other: "a"
            }
        );
        for (kind, rule_kind) in [
            (ContactKind::Enter, ContactRuleKind::CollisionEnter),
            (ContactKind::Exit, ContactRuleKind::CollisionExit),
            (ContactKind::TriggerExit, ContactRuleKind::TriggerExit),
        ] {
            let event = CollisionEvent {
                kind,
                first: "a".into(),
                second: "b".into(),
            };
            assert!(matches!(
                contact_events(&event)[0],
                RuleEvent::Contact { kind, .. } if kind == rule_kind
            ));
        }
    }

    #[test]
    fn collision_rules_match_owner_and_with() {
        let mut h = Harness::new(vec![
            rule(RuleTrigger::CollisionEnter { with: None }, bump("any")),
            rule(
                RuleTrigger::CollisionEnter {
                    with: Some(OTHER.into()),
                },
                bump("other"),
            ),
            rule(RuleTrigger::CollisionExit { with: None }, bump("exit")),
        ]);
        let contact = |kind, first: &str, second: &str| CollisionEvent {
            kind,
            first: first.into(),
            second: second.into(),
        };
        let raise = |h: &mut Harness, event: &CollisionEvent| {
            for e in contact_events(event) {
                h.dispatch(e);
            }
        };
        raise(&mut h, &contact(ContactKind::Enter, OWNER, OTHER));
        assert_eq!((h.count("any"), h.count("other")), (1.0, 1.0));
        // Owner on the second side works the same; a stranger does not match `with`.
        raise(&mut h, &contact(ContactKind::Enter, "stranger", OWNER));
        assert_eq!((h.count("any"), h.count("other")), (2.0, 1.0));
        // A contact between two other objects is nobody's.
        raise(&mut h, &contact(ContactKind::Enter, "x", "y"));
        assert_eq!(h.count("any"), 2.0);
        raise(&mut h, &contact(ContactKind::Exit, OWNER, OTHER));
        assert_eq!(h.count("exit"), 1.0);
        assert_eq!(h.count("any"), 2.0, "an exit is not an enter");
    }

    #[test]
    fn trigger_rules_fire_for_either_side() {
        let mut h = Harness::new(vec![
            rule(RuleTrigger::TriggerEnter { with: None }, bump("enter")),
            rule(
                RuleTrigger::TriggerExit {
                    with: Some(OTHER.into()),
                },
                bump("exit"),
            ),
        ]);
        // OWNER is the trigger volume in one overlap and the visitor in the other.
        for (first, second) in [(OWNER, OTHER), (OTHER, OWNER)] {
            let event = CollisionEvent {
                kind: ContactKind::TriggerEnter,
                first: first.into(),
                second: second.into(),
            };
            for e in contact_events(&event) {
                h.dispatch(e);
            }
        }
        assert_eq!(h.count("enter"), 2.0);
        let event = CollisionEvent {
            kind: ContactKind::TriggerExit,
            first: OTHER.into(),
            second: OWNER.into(),
        };
        for e in contact_events(&event) {
            h.dispatch(e);
        }
        assert_eq!(h.count("exit"), 1.0);
    }

    #[test]
    fn rules_of_destroyed_or_disabled_owners_do_not_fire() {
        let mut h = Harness::new(vec![rule(RuleTrigger::SceneStart, bump("n"))]);
        let owner = find_object(&h.world, OWNER).expect("owner");
        h.world.entity_mut(owner).insert(Disabled);
        assert_eq!(h.dispatch(RuleEvent::SceneStart), 0);
        h.world.entity_mut(owner).remove::<Disabled>();
        assert_eq!(h.dispatch(RuleEvent::SceneStart), 1);
        h.world.despawn(owner);
        assert_eq!(h.dispatch(RuleEvent::SceneStart), 0);
        assert_eq!(h.count("n"), 1.0);
    }

    #[test]
    fn deferred_actions_become_commands() {
        let mut world = World::new();
        let mut warned = HashSet::new();
        let mut physics = PhysicsWorld::default();
        let mut ui = Ui::new(800.0, 600.0);
        let mut runtime = GameplayRuntime::new(Scene::default(), None);
        state::init_state(&mut world, None);
        let anchor = world
            .spawn((
                SceneObjectId(OTHER.into()),
                Transform::from(engine_utils::Transform::from_translation(glam::Vec3::new(
                    4.0, 5.0, 6.0,
                ))),
            ))
            .id();
        world.spawn((SceneObjectId(OWNER.into()), Transform::default()));

        let actions = [
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
                at: SpawnAt::Object(OTHER.into()),
            },
            RuleAction::SpawnPrefab {
                path: "assets/prefabs/Crate.prefab".into(),
                at: SpawnAt::Position([1.0, 2.0, 3.0]),
            },
            RuleAction::SetGameState {
                state: GameStateName::Paused,
            },
            RuleAction::ChangeScene {
                path: "scenes/b.ron".into(),
            },
            RuleAction::Destroy {
                target: OWNER.into(),
            },
        ];
        let commands: Vec<GameCommand> = actions
            .iter()
            .filter_map(|action| rule_commands(action, &world, &mut warned))
            .collect();
        assert_eq!(commands.len(), actions.len());
        assert!(matches!(
            &commands[2],
            GameCommand::SpawnPrefab { position: [4.0, 5.0, 6.0], object_id, .. }
                if uuid::Uuid::parse_str(object_id).is_ok()
        ));
        assert!(matches!(
            &commands[3],
            GameCommand::SpawnPrefab {
                position: [1.0, 2.0, 3.0],
                ..
            }
        ));
        assert!(matches!(&commands[4], GameCommand::SetGameState { state } if state == "paused"));
        for command in &commands {
            if let GameCommand::ChangeScene { path } = command {
                runtime.request_scene_change(path);
            }
        }

        // Apply them the way the runtime does and look at the world.
        let mut spawned_paths = Vec::new();
        apply_world_commands(
            &mut world,
            &commands,
            &mut |world, path, object_id, position| {
                spawned_paths.push((path.to_owned(), position));
                let entity = world
                    .spawn((
                        SceneObjectId(object_id.to_owned()),
                        Transform::from(engine_utils::Transform::from_translation(
                            glam::Vec3::from_array(position),
                        )),
                    ))
                    .id();
                Ok((vec![entity], Scene::default()))
            },
        )
        .expect("commands apply");
        let mut contacts = super::super::ContactTracker::default();
        let (mut ui_nodes, mut grounded, mut scene_order, mut active_camera) =
            (HashMap::new(), HashMap::new(), Vec::new(), None);
        let mut destroyed = Vec::new();
        super::super::objects::apply_object_commands(
            super::super::objects::ObjectTargets {
                world: &mut world,
                physics: &mut physics,
                ui: &mut ui,
            },
            super::super::objects::ObjectState {
                ui_nodes: &mut ui_nodes,
                contacts: &mut contacts,
                warned: &mut warned,
                grounded: &mut grounded,
                scene_order: &mut scene_order,
                active_camera: &mut active_camera,
                destroyed: &mut destroyed,
            },
            &mut |_, _| {},
            &commands,
        );
        runtime.record_state_changes(&mut world, &commands);

        assert!(world.get::<Hidden>(anchor).is_some(), "hidden");
        assert!(world.get::<Disabled>(anchor).is_some(), "disabled");
        assert!(find_object(&world, OWNER).is_none(), "destroyed");
        assert_eq!(spawned_paths.len(), 2);
        assert_eq!(spawned_paths[0].1, [4.0, 5.0, 6.0], "spawned at the object");
        assert_eq!(world.resource::<GameState>().current, GameStateName::Paused);
        assert_eq!(
            runtime.take_scene_request().as_deref(),
            Some("scenes/b.ron")
        );
    }

    #[test]
    fn spawn_at_a_missing_object_is_skipped_with_one_warning() {
        let world = World::new();
        let mut warned = HashSet::new();
        let action = RuleAction::SpawnPrefab {
            path: "assets/prefabs/Crate.prefab".into(),
            at: SpawnAt::Object(OTHER.into()),
        };
        assert!(rule_commands(&action, &world, &mut warned).is_none());
        assert!(rule_commands(&action, &world, &mut warned).is_none());
        assert_eq!(warned.len(), 1);
        assert!(warned.contains(&(OTHER.to_owned(), "spawn_at")));
    }

    #[test]
    fn deferred_rule_commands_stop_at_depth_limit() {
        // Each level stands for a rule action whose applied commands emit an
        // event whose rules defer another apply.
        fn nest(guard: &mut ApplyGuard, reached: &mut u32) {
            if !guard.enter() {
                return;
            }
            *reached += 1;
            nest(guard, reached);
            guard.leave();
        }
        let mut guard = ApplyGuard::default();
        let mut reached = 0;
        nest(&mut guard, &mut reached);
        assert_eq!(reached, u32::from(MAX_RULE_APPLY_DEPTH));
        assert!(guard.warned || guard.depth == 0);
        // After the chain unwinds, a new chain starts fresh.
        assert_eq!((guard.depth, guard.warned), (0, false));
        assert!(guard.enter());
        guard.leave();
    }

    #[test]
    fn rule_spawns_are_taken_once() {
        let mut runtime = GameplayRuntime::new(Scene::default(), None);
        assert!(runtime.take_rule_spawns().is_empty());
        runtime.rule_spawns.push(super::super::SpawnedObject {
            object_id: OWNER.into(),
            script: Some("assets/scripts/coin.lua".into()),
        });
        let taken = runtime.take_rule_spawns();
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].script.as_deref(), Some("assets/scripts/coin.lua"));
        assert!(runtime.take_rule_spawns().is_empty());
    }
}
