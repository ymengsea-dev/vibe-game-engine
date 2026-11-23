//! Animators: which clip an animated model plays, chosen by states and
//! transitions instead of by script calls.
//!
//! An animator is a small state graph saved on the model's object (in its
//! [`crate::ModelAnimationData`]), so it saves, undoes, copies and travels
//! in prefabs like every other setting. Each state plays one clip, or
//! blends clips by a number (walk to run by speed). Transitions move to
//! another state when their conditions on numbers hold, optionally only
//! once the current clip has played to its end.
//!
//! The numbers come from the object first (Lua
//! `engine.set_animator_value`, Rust `GameCommand::SetAnimatorValue`) and
//! then from the game's shared variables, the same ones rules use.
//!
//! This module is the format and the decisions; `engine::gameplay` applies
//! them to the live [`engine_ecs::AnimationPlayer`].

use serde::{Deserialize, Serialize};

use crate::format::{RuleCondition, RuleMatch};

/// Numbers one object's animator reads before the game's shared variables,
/// set with `engine.set_animator_value` or `GameCommand::SetAnimatorValue`.
#[derive(bevy_ecs::component::Component, Debug, Clone, Default, PartialEq)]
pub struct AnimatorValues(pub std::collections::HashMap<String, f64>);

/// Where a running animator is: which state, and its graph.
#[derive(bevy_ecs::component::Component, Debug, Clone)]
pub struct AnimatorState {
    /// The authored graph, shared so a frame does not copy it.
    pub data: std::sync::Arc<AnimatorData>,
    /// Index of the state it is in.
    pub current: usize,
}

impl AnimatorState {
    /// The name of the state it is in.
    pub fn current_name(&self) -> Option<&str> {
        self.data
            .states
            .get(self.current)
            .map(|state| state.name.as_str())
    }
}

/// The most states one animator may have.
pub const MAX_ANIMATOR_STATES: usize = 64;
/// The most transitions one animator may have.
pub const MAX_ANIMATOR_TRANSITIONS: usize = 256;
/// The most clips one blend state may mix.
pub const MAX_BLEND_CLIPS: usize = 8;
/// The longest crossfade a transition may take, in seconds.
pub const MAX_TRANSITION_FADE: f32 = 10.0;

/// A state graph choosing an animated model's clip. The first state is
/// where it starts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AnimatorData {
    /// Every state; the first is the start state.
    pub states: Vec<AnimatorStateData>,
    /// Every transition, checked in order: the first that can fire does.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transitions: Vec<AnimatorTransitionData>,
}

/// One state: what plays while the animator is in it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnimatorStateData {
    /// Unique name, 1 to 64 printable bytes.
    pub name: String,
    /// The clip, or clips, it plays.
    pub motion: AnimatorMotion,
    /// Playback rate; `1.0` is the authored speed.
    #[serde(default = "default_speed")]
    pub speed: f32,
    /// Whether the clip starts again after its last frame.
    #[serde(default = "default_true")]
    pub looping: bool,
    /// Where the editor's graph draws it; editor layout only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_position: Option<[f32; 2]>,
}

/// What a state plays.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AnimatorMotion {
    /// One clip, by its name in the model file.
    Clip(String),
    /// Several clips mixed by a number: at each clip's `at` value only that
    /// clip shows, and between two the pose is blended.
    Blend {
        /// The number that chooses the mix, such as `speed`.
        variable: String,
        /// The clips and where each sits on the number's scale.
        clips: Vec<BlendClipData>,
    },
}

/// One clip of a blend state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlendClipData {
    /// The clip's name in the model file.
    pub clip: String,
    /// The value of the blend number where this clip shows alone.
    pub at: f32,
}

/// A move from one state to another.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnimatorTransitionData {
    /// The state it leaves; `None` means from any state (but never into
    /// the state the animator is already in).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// The state it enters.
    pub to: String,
    /// Checks on numbers; an empty list always passes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<RuleCondition>,
    /// Whether all or any of the conditions must hold.
    #[serde(default, skip_serializing_if = "is_all")]
    pub match_mode: RuleMatch,
    /// Seconds to blend from the old clip into the new one.
    #[serde(default = "default_fade")]
    pub fade: f32,
    /// Whether it waits for the current clip to reach its end first
    /// (a looping clip: the end of one full play).
    #[serde(default, skip_serializing_if = "is_false")]
    pub at_end: bool,
}

fn default_speed() -> f32 {
    1.0
}

fn default_true() -> bool {
    true
}

fn default_fade() -> f32 {
    0.2
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_all(mode: &RuleMatch) -> bool {
    *mode == RuleMatch::All
}

impl AnimatorStateData {
    /// A looping state playing `clip` at authored speed.
    pub fn clip(name: impl Into<String>, clip: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            motion: AnimatorMotion::Clip(clip.into()),
            speed: 1.0,
            looping: true,
            graph_position: None,
        }
    }
}

impl AnimatorTransitionData {
    /// An unconditional move from `from` to `to` with the default fade.
    pub fn new(from: Option<String>, to: impl Into<String>) -> Self {
        Self {
            from,
            to: to.into(),
            conditions: Vec::new(),
            match_mode: RuleMatch::All,
            fade: default_fade(),
            at_end: false,
        }
    }
}

impl AnimatorData {
    /// One state per clip, starting with `start` (or the first clip), and
    /// no transitions: a starting point for the editor.
    pub fn from_clips(clips: &[String], start: Option<&str>) -> Self {
        let mut names: Vec<&String> = clips.iter().take(MAX_ANIMATOR_STATES).collect();
        if let Some(index) = start.and_then(|start| names.iter().position(|name| *name == start)) {
            let chosen = names.remove(index);
            names.insert(0, chosen);
        }
        Self {
            states: names
                .into_iter()
                .enumerate()
                .map(|(index, name)| AnimatorStateData {
                    graph_position: Some(default_position(index)),
                    ..AnimatorStateData::clip(name.clone(), name.clone())
                })
                .collect(),
            transitions: Vec::new(),
        }
    }

    /// The position of the state named `name`.
    pub fn state_index(&self, name: &str) -> Option<usize> {
        self.states.iter().position(|state| state.name == name)
    }

    /// The transition to take from state `current`, if any can fire now:
    /// `(target state, fade seconds)`. `value` reads a number by name;
    /// `clip_ended` says whether the current clip has reached its end.
    pub fn next_transition(
        &self,
        current: usize,
        value: impl Fn(&str) -> f64,
        clip_ended: bool,
    ) -> Option<(usize, f32)> {
        let current_name = &self.states.get(current)?.name;
        self.transitions.iter().find_map(|transition| {
            let leaves = match &transition.from {
                Some(from) => from == current_name,
                None => transition.to != *current_name,
            };
            if !leaves || (transition.at_end && !clip_ended) {
                return None;
            }
            let holds = |condition: &RuleCondition| {
                condition
                    .compare
                    .holds(value(&condition.variable), condition.value)
            };
            let passes = transition.conditions.is_empty()
                || match transition.match_mode {
                    RuleMatch::All => transition.conditions.iter().all(holds),
                    RuleMatch::Any => transition.conditions.iter().any(holds),
                };
            passes
                .then(|| self.state_index(&transition.to))
                .flatten()
                .map(|target| (target, transition.fade))
        })
    }

    /// Renames a state and every transition that names it. False when the
    /// new name is invalid or taken.
    pub fn rename_state(&mut self, index: usize, name: &str) -> bool {
        if !valid_name(name) || self.state_index(name).is_some_and(|other| other != index) {
            return false;
        }
        let Some(state) = self.states.get_mut(index) else {
            return false;
        };
        let old = std::mem::replace(&mut state.name, name.to_owned());
        for transition in &mut self.transitions {
            if transition.from.as_deref() == Some(old.as_str()) {
                transition.from = Some(name.to_owned());
            }
            if transition.to == old {
                transition.to = name.to_owned();
            }
        }
        true
    }

    /// Removes a state and every transition into or out of it.
    pub fn remove_state(&mut self, index: usize) {
        if index >= self.states.len() {
            return;
        }
        let name = self.states.remove(index).name;
        self.transitions.retain(|transition| {
            transition.to != name && transition.from.as_deref() != Some(name.as_str())
        });
    }

    /// Checks names, references and numbers; scene files are untrusted.
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.states.is_empty() {
            return Err("an animator needs at least one state".into());
        }
        if self.states.len() > MAX_ANIMATOR_STATES {
            return Err(format!(
                "an animator may have at most {MAX_ANIMATOR_STATES} states"
            ));
        }
        if self.transitions.len() > MAX_ANIMATOR_TRANSITIONS {
            return Err(format!(
                "an animator may have at most {MAX_ANIMATOR_TRANSITIONS} transitions"
            ));
        }
        for (index, state) in self.states.iter().enumerate() {
            if !valid_name(&state.name) {
                return Err("animator state names must be 1 to 64 printable bytes".into());
            }
            if self.states[..index]
                .iter()
                .any(|other| other.name == state.name)
            {
                return Err(format!("two animator states are named {:?}", state.name));
            }
            if !state.speed.is_finite() || state.speed.abs() > 100.0 {
                return Err(format!(
                    "state {:?} speed must be from -100 to 100",
                    state.name
                ));
            }
            if state.graph_position.is_some_and(|at| {
                !at.iter()
                    .all(|value| value.is_finite() && value.abs() <= 1_000_000.0)
            }) {
                return Err("graph position must be finite and within ±1,000,000".into());
            }
            match &state.motion {
                AnimatorMotion::Clip(clip) => valid_clip(clip)?,
                AnimatorMotion::Blend { variable, clips } => {
                    if !crate::rules::valid_variable_name(variable) {
                        return Err("blend variable name must be 1 to 64 printable bytes".into());
                    }
                    if clips.is_empty() || clips.len() > MAX_BLEND_CLIPS {
                        return Err(format!("a blend mixes 1 to {MAX_BLEND_CLIPS} clips"));
                    }
                    for clip in clips {
                        valid_clip(&clip.clip)?;
                        if !clip.at.is_finite() {
                            return Err("blend positions must be finite".into());
                        }
                    }
                }
            }
        }
        for transition in &self.transitions {
            if self.state_index(&transition.to).is_none()
                || transition
                    .from
                    .as_ref()
                    .is_some_and(|from| self.state_index(from).is_none())
            {
                return Err("an animator transition names a state that does not exist".into());
            }
            if !transition.fade.is_finite()
                || !(0.0..=MAX_TRANSITION_FADE).contains(&transition.fade)
            {
                return Err(format!(
                    "transition fade must be 0 to {MAX_TRANSITION_FADE} seconds"
                ));
            }
            if transition.conditions.len() > crate::MAX_RULE_CONDITIONS {
                return Err(format!(
                    "a transition may have at most {} conditions",
                    crate::MAX_RULE_CONDITIONS
                ));
            }
            for condition in &transition.conditions {
                if !crate::rules::valid_variable_name(&condition.variable)
                    || !condition.value.is_finite()
                {
                    return Err(
                        "transition conditions need a variable name and a finite value".into(),
                    );
                }
            }
        }
        Ok(())
    }
}

/// Where the editor's graph puts the `index`th state that has no saved
/// position: rows of four, below the "Any state" node.
pub fn default_position(index: usize) -> [f32; 2] {
    [
        24.0 + 200.0 * (index % 4) as f32,
        110.0 + 90.0 * (index / 4) as f32,
    ]
}

/// Whether `name` is a usable state name.
pub fn valid_name(name: &str) -> bool {
    !name.trim().is_empty() && name.len() <= 64 && !name.chars().any(char::is_control)
}

fn valid_clip(clip: &str) -> Result<(), String> {
    if clip.is_empty() || clip.len() > 256 {
        return Err("animator clip names must be 1 to 256 bytes".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::RuleCompare;

    fn condition(variable: &str, compare: RuleCompare, value: f64) -> RuleCondition {
        RuleCondition {
            variable: variable.into(),
            compare,
            value,
        }
    }

    fn walker() -> AnimatorData {
        let mut idle_to_walk = AnimatorTransitionData::new(Some("idle".into()), "walk");
        idle_to_walk.conditions = vec![condition("speed", RuleCompare::Greater, 0.1)];
        let mut walk_to_idle = AnimatorTransitionData::new(Some("walk".into()), "idle");
        walk_to_idle.conditions = vec![condition("speed", RuleCompare::LessOrEqual, 0.1)];
        let mut any_to_jump = AnimatorTransitionData::new(None, "jump");
        any_to_jump.conditions = vec![condition("jumping", RuleCompare::Equal, 1.0)];
        let mut jump_back = AnimatorTransitionData::new(Some("jump".into()), "idle");
        jump_back.at_end = true;
        AnimatorData {
            states: vec![
                AnimatorStateData::clip("idle", "idle"),
                AnimatorStateData::clip("walk", "walk"),
                AnimatorStateData::clip("jump", "jump"),
            ],
            transitions: vec![idle_to_walk, walk_to_idle, any_to_jump, jump_back],
        }
    }

    #[test]
    fn transitions_follow_conditions_order_and_clip_end() {
        let animator = walker();
        let numbers = |speed: f64, jumping: f64| {
            move |name: &str| match name {
                "speed" => speed,
                "jumping" => jumping,
                _ => 0.0,
            }
        };
        assert_eq!(animator.next_transition(0, numbers(0.0, 0.0), false), None);
        assert_eq!(
            animator.next_transition(0, numbers(2.0, 0.0), false),
            Some((1, 0.2))
        );
        assert_eq!(
            animator.next_transition(1, numbers(0.0, 0.0), false),
            Some((0, 0.2))
        );
        assert_eq!(
            animator.next_transition(1, numbers(2.0, 1.0), false),
            Some((2, 0.2)),
            "from any state"
        );
        assert_eq!(
            animator.next_transition(2, numbers(0.0, 1.0), false),
            None,
            "any-state never re-enters itself; jump waits for its end"
        );
        assert_eq!(
            animator.next_transition(2, numbers(0.0, 0.0), true),
            Some((0, 0.2))
        );
    }

    #[test]
    fn any_mode_needs_one_condition() {
        let mut animator = walker();
        animator.transitions[0]
            .conditions
            .push(condition("x", RuleCompare::Equal, 5.0));
        animator.transitions[0].match_mode = RuleMatch::Any;
        let only_x = |name: &str| if name == "x" { 5.0 } else { 0.0 };
        assert_eq!(animator.next_transition(0, only_x, false), Some((1, 0.2)));
    }

    #[test]
    fn rename_and_remove_keep_transitions_consistent() {
        let mut animator = walker();
        assert!(animator.rename_state(1, "stroll"));
        assert_eq!(animator.transitions[0].to, "stroll");
        assert_eq!(animator.transitions[1].from.as_deref(), Some("stroll"));
        assert!(!animator.rename_state(1, "idle"), "taken");
        assert!(!animator.rename_state(1, " "), "blank");
        animator.remove_state(2);
        assert_eq!(animator.transitions.len(), 2, "jump's transitions went too");
        assert!(animator.validate().is_ok());
    }

    #[test]
    fn from_clips_puts_the_chosen_clip_first() {
        let clips = vec!["idle".to_owned(), "walk".to_owned(), "run".to_owned()];
        let animator = AnimatorData::from_clips(&clips, Some("walk"));
        let names: Vec<_> = animator
            .states
            .iter()
            .map(|state| state.name.as_str())
            .collect();
        assert_eq!(names, ["walk", "idle", "run"]);
        assert!(animator.validate().is_ok());
    }

    #[test]
    fn validation_rejects_broken_graphs() {
        let mut animator = walker();
        assert!(animator.validate().is_ok());
        animator.transitions[0].to = "swim".into();
        assert!(animator.validate().is_err(), "unknown state");
        let mut animator = walker();
        animator.states[1].name = "idle".into();
        assert!(animator.validate().is_err(), "duplicate");
        let mut animator = walker();
        animator.transitions[0].fade = f32::NAN;
        assert!(animator.validate().is_err(), "fade");
        let mut animator = walker();
        animator.states[0].motion = AnimatorMotion::Blend {
            variable: "speed".into(),
            clips: Vec::new(),
        };
        assert!(animator.validate().is_err(), "empty blend");
        assert!(AnimatorData::default().validate().is_err(), "no states");
    }

    #[test]
    fn round_trips_through_ron_with_defaults_left_out() {
        let animator = walker();
        let text = ron::to_string(&animator).expect("serializes");
        assert!(!text.contains("match_mode"), "{text}");
        let back: AnimatorData = ron::from_str(&text).expect("parses");
        assert_eq!(back, animator);
    }
}
