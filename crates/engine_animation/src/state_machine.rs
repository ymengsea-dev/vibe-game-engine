//! Animation state machines: named states (each playing one looping
//! clip), condition-gated transitions between them, and time-based
//! crossfading while a transition is in progress.
//!
//! Scope for this iteration: one clip per state (not a blend tree — see
//! [`crate::sample_blend_tree_1d`] for that separately; combining the
//! two is future work), boolean parameters only (no floats/triggers),
//! and condition-gated transitions only (no normalized-exit-time
//! auto-transitions). A transition also can't be interrupted by another
//! becoming eligible mid-crossfade — it always runs to completion first.

use std::collections::HashMap;

use engine_asset::{ImportedAnimation, ImportedSkeleton};
use engine_utils::Transform;
use thiserror::Error;

use crate::blend_tree::{blend_poses, sample_looping};

/// Errors that can occur building a [`StateMachine`].
#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum StateMachineError {
    /// [`StateMachine::new`]'s `start` state, or some transition's
    /// `target`, doesn't name any state in the graph.
    #[error("state machine has no state named {0:?}")]
    UnknownState(String),
}

/// One condition a [`Transition`] requires: a named boolean parameter
/// (set via [`StateMachine::set_parameter`]) must equal `required`. A
/// parameter that's never been set is treated as `false`.
#[derive(Debug, Clone, PartialEq)]
pub struct TransitionCondition {
    /// The parameter's name.
    pub parameter: String,
    /// The value it must hold for this condition to be satisfied.
    pub required: bool,
}

/// One directed edge out of a state: eligible once every one of its
/// `conditions` holds (an empty list is always eligible), crossfading
/// into `target` over `duration_seconds` once taken.
#[derive(Debug, Clone, PartialEq)]
pub struct Transition {
    /// The state this transition leads to — must name a real state in
    /// the same [`StateMachine`] (validated by [`StateMachine::new`]).
    pub target: String,
    /// Every condition that must hold for this transition to fire.
    pub conditions: Vec<TransitionCondition>,
    /// How long the crossfade from the outgoing state's pose to this
    /// one's takes once the transition starts.
    pub duration_seconds: f32,
}

/// One state: a name, the clip it plays (looping — see
/// [`crate::sample_looping`]), and the transitions that can fire out of
/// it, checked in order (the first whose conditions all hold wins).
#[derive(Debug)]
pub struct AnimationState<'a> {
    /// This state's name — what [`Transition::target`] and
    /// [`StateMachine::new`]'s `start` refer to it by.
    pub name: String,
    /// The clip this state plays.
    pub clip: &'a ImportedAnimation,
    /// Outgoing transitions, checked in order.
    pub transitions: Vec<Transition>,
}

/// An in-progress crossfade from one state to another.
#[derive(Debug)]
struct ActiveTransition {
    from: usize,
    from_time: f32,
    elapsed: f32,
    duration: f32,
}

/// A running instance of an authored state graph.
///
/// Owns its current/outgoing playhead times and its parameters; the
/// graph itself (`states`) is fixed for the machine's lifetime — see
/// [`StateMachine::new`].
#[derive(Debug)]
pub struct StateMachine<'a> {
    states: Vec<AnimationState<'a>>,
    current: usize,
    current_time: f32,
    transition: Option<ActiveTransition>,
    parameters: HashMap<String, bool>,
}

impl<'a> StateMachine<'a> {
    /// Builds a machine over `states`, starting in whichever one is
    /// named `start`.
    ///
    /// # Errors
    ///
    /// Returns [`StateMachineError::UnknownState`] if `start`, or any
    /// state's [`Transition::target`], doesn't name a state in `states`
    /// — validated up front so [`StateMachine::update`] never needs to
    /// handle a dangling reference.
    pub fn new(states: Vec<AnimationState<'a>>, start: &str) -> Result<Self, StateMachineError> {
        for state in &states {
            for transition in &state.transitions {
                if !states.iter().any(|s| s.name == transition.target) {
                    return Err(StateMachineError::UnknownState(transition.target.clone()));
                }
            }
        }
        let current = states
            .iter()
            .position(|s| s.name == start)
            .ok_or_else(|| StateMachineError::UnknownState(start.to_string()))?;

        Ok(Self {
            states,
            current,
            current_time: 0.0,
            transition: None,
            parameters: HashMap::new(),
        })
    }

    /// Sets (or overwrites) a named boolean parameter, checked against
    /// by [`Transition::conditions`].
    pub fn set_parameter(&mut self, name: impl Into<String>, value: bool) {
        self.parameters.insert(name.into(), value);
    }

    /// The current state's name — the target state's, while
    /// mid-transition (the one [`StateMachine::sample`] is fading
    /// *towards*).
    pub fn current_state_name(&self) -> &str {
        &self.states[self.current].name
    }

    /// Whether a crossfade is currently in progress.
    pub fn is_transitioning(&self) -> bool {
        self.transition.is_some()
    }

    /// Advances every active playhead (the current state's, the
    /// outgoing one's if mid-transition, and the transition's own
    /// elapsed time) by `dt_seconds`, then — only when *not*
    /// mid-transition — checks the current state's transitions in order
    /// and starts the first one whose conditions all hold.
    ///
    /// Starting a transition resets the new current state's playhead to
    /// `0.0` (each state starts its clip fresh on entry, rather than
    /// phase-matching the outgoing one — a simpler, if less seamless,
    /// choice than real engines' optional phase sync).
    pub fn update(&mut self, dt_seconds: f32) {
        self.current_time += dt_seconds;

        if let Some(active) = &mut self.transition {
            active.from_time += dt_seconds;
            active.elapsed += dt_seconds;
            if active.elapsed >= active.duration {
                self.transition = None;
            }
            // Deliberately no transition-checking while one's already
            // in progress — see this module's docs.
            return;
        }

        let Some(transition) = self.states[self.current]
            .transitions
            .iter()
            .find(|transition| self.conditions_hold(&transition.conditions))
        else {
            return;
        };

        // Already validated by `StateMachine::new`.
        let Some(target_index) = self.states.iter().position(|s| s.name == transition.target)
        else {
            return;
        };

        self.transition = Some(ActiveTransition {
            from: self.current,
            from_time: self.current_time,
            elapsed: 0.0,
            duration: transition.duration_seconds,
        });
        self.current = target_index;
        self.current_time = 0.0;
    }

    fn conditions_hold(&self, conditions: &[TransitionCondition]) -> bool {
        conditions.iter().all(|condition| {
            self.parameters
                .get(&condition.parameter)
                .copied()
                .unwrap_or(false)
                == condition.required
        })
    }

    /// Samples the current pose: just the current state's clip, or —
    /// mid-transition — that blended with the outgoing state's clip by
    /// how far the crossfade has progressed.
    pub fn sample(&self, skeleton: &ImportedSkeleton) -> Vec<Transform> {
        let current_pose =
            sample_looping(skeleton, self.states[self.current].clip, self.current_time);

        let Some(active) = &self.transition else {
            return current_pose;
        };

        let from_pose = sample_looping(skeleton, self.states[active.from].clip, active.from_time);
        let t = if active.duration > f32::EPSILON {
            (active.elapsed / active.duration).clamp(0.0, 1.0)
        } else {
            1.0
        };
        blend_poses(&from_pose, &current_pose, t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_asset::{
        ImportedAnimationChannels, ImportedInterpolation, ImportedJoint, ImportedKeyframes,
    };
    use glam::Vec3;

    fn single_joint_skeleton() -> ImportedSkeleton {
        ImportedSkeleton {
            name: None,
            joints: vec![ImportedJoint {
                name: None,
                node_index: 0,
                parent: None,
                local_bind_transform: Transform::IDENTITY,
                inverse_bind_matrix: glam::Mat4::IDENTITY,
            }],
        }
    }

    fn constant_translation_clip(x: f32) -> ImportedAnimation {
        let mut channels = HashMap::new();
        channels.insert(
            0,
            ImportedAnimationChannels {
                translation: Some(ImportedKeyframes {
                    interpolation: ImportedInterpolation::Step,
                    times: vec![0.0],
                    values: vec![Vec3::new(x, 0.0, 0.0)],
                }),
                rotation: None,
                scale: None,
            },
        );
        ImportedAnimation {
            name: None,
            duration: 1.0,
            channels,
        }
    }

    #[test]
    fn new_rejects_an_unknown_start_state() {
        let idle = constant_translation_clip(0.0);
        let states = vec![AnimationState {
            name: "Idle".to_string(),
            clip: &idle,
            transitions: vec![],
        }];
        let err = StateMachine::new(states, "Nope").unwrap_err();
        assert_eq!(err, StateMachineError::UnknownState("Nope".to_string()));
    }

    #[test]
    fn new_rejects_a_transition_to_an_unknown_state() {
        let idle = constant_translation_clip(0.0);
        let states = vec![AnimationState {
            name: "Idle".to_string(),
            clip: &idle,
            transitions: vec![Transition {
                target: "Ghost".to_string(),
                conditions: vec![],
                duration_seconds: 0.2,
            }],
        }];
        let err = StateMachine::new(states, "Idle").unwrap_err();
        assert_eq!(err, StateMachineError::UnknownState("Ghost".to_string()));
    }

    #[test]
    fn starts_in_the_named_start_state() {
        let idle = constant_translation_clip(0.0);
        let walk = constant_translation_clip(1.0);
        let states = vec![
            AnimationState {
                name: "Idle".to_string(),
                clip: &idle,
                transitions: vec![],
            },
            AnimationState {
                name: "Walk".to_string(),
                clip: &walk,
                transitions: vec![],
            },
        ];
        let machine = StateMachine::new(states, "Walk").unwrap();
        assert_eq!(machine.current_state_name(), "Walk");
    }

    #[test]
    fn stays_put_without_an_eligible_transition() {
        let idle = constant_translation_clip(0.0);
        let walk = constant_translation_clip(1.0);
        let states = vec![
            AnimationState {
                name: "Idle".to_string(),
                clip: &idle,
                transitions: vec![Transition {
                    target: "Walk".to_string(),
                    conditions: vec![TransitionCondition {
                        parameter: "IsMoving".to_string(),
                        required: true,
                    }],
                    duration_seconds: 0.2,
                }],
            },
            AnimationState {
                name: "Walk".to_string(),
                clip: &walk,
                transitions: vec![],
            },
        ];
        let mut machine = StateMachine::new(states, "Idle").unwrap();
        machine.update(0.5); // "IsMoving" never set — defaults to false
        assert_eq!(machine.current_state_name(), "Idle");
        assert!(!machine.is_transitioning());
    }

    #[test]
    fn transitions_once_its_condition_holds() {
        let idle = constant_translation_clip(0.0);
        let walk = constant_translation_clip(1.0);
        let states = vec![
            AnimationState {
                name: "Idle".to_string(),
                clip: &idle,
                transitions: vec![Transition {
                    target: "Walk".to_string(),
                    conditions: vec![TransitionCondition {
                        parameter: "IsMoving".to_string(),
                        required: true,
                    }],
                    duration_seconds: 0.2,
                }],
            },
            AnimationState {
                name: "Walk".to_string(),
                clip: &walk,
                transitions: vec![],
            },
        ];
        let mut machine = StateMachine::new(states, "Idle").unwrap();
        machine.set_parameter("IsMoving", true);
        machine.update(0.01);
        assert_eq!(machine.current_state_name(), "Walk");
        assert!(machine.is_transitioning());
    }

    #[test]
    fn crossfade_blends_from_the_outgoing_state_towards_the_new_one() {
        let idle = constant_translation_clip(0.0);
        let walk = constant_translation_clip(10.0);
        let states = vec![
            AnimationState {
                name: "Idle".to_string(),
                clip: &idle,
                transitions: vec![Transition {
                    target: "Walk".to_string(),
                    conditions: vec![TransitionCondition {
                        parameter: "IsMoving".to_string(),
                        required: true,
                    }],
                    duration_seconds: 1.0,
                }],
            },
            AnimationState {
                name: "Walk".to_string(),
                clip: &walk,
                transitions: vec![],
            },
        ];
        let skeleton = single_joint_skeleton();
        let mut machine = StateMachine::new(states, "Idle").unwrap();
        machine.set_parameter("IsMoving", true);

        machine.update(0.0); // starts the transition, elapsed = 0.0
        let just_started = machine.sample(&skeleton);
        assert_eq!(just_started[0].translation, Vec3::new(0.0, 0.0, 0.0));

        machine.update(0.5); // halfway through a 1.0s crossfade
        let halfway = machine.sample(&skeleton);
        assert_eq!(halfway[0].translation, Vec3::new(5.0, 0.0, 0.0));

        machine.update(0.5); // finishes the crossfade
        assert!(!machine.is_transitioning());
        let finished = machine.sample(&skeleton);
        assert_eq!(finished[0].translation, Vec3::new(10.0, 0.0, 0.0));
    }

    #[test]
    fn a_transition_in_progress_is_not_interrupted() {
        let idle = constant_translation_clip(0.0);
        let walk = constant_translation_clip(1.0);
        let run = constant_translation_clip(2.0);
        let states = vec![
            AnimationState {
                name: "Idle".to_string(),
                clip: &idle,
                transitions: vec![Transition {
                    target: "Walk".to_string(),
                    conditions: vec![TransitionCondition {
                        parameter: "IsMoving".to_string(),
                        required: true,
                    }],
                    duration_seconds: 1.0,
                }],
            },
            AnimationState {
                name: "Walk".to_string(),
                clip: &walk,
                transitions: vec![Transition {
                    target: "Run".to_string(),
                    conditions: vec![TransitionCondition {
                        parameter: "IsRunning".to_string(),
                        required: true,
                    }],
                    duration_seconds: 1.0,
                }],
            },
            AnimationState {
                name: "Run".to_string(),
                clip: &run,
                transitions: vec![],
            },
        ];
        let mut machine = StateMachine::new(states, "Idle").unwrap();
        machine.set_parameter("IsMoving", true);
        machine.set_parameter("IsRunning", true);

        // Even though "Run"'s condition is already satisfied on "Walk",
        // the Idle -> Walk transition must run to completion first.
        machine.update(0.1);
        assert_eq!(machine.current_state_name(), "Walk");
        assert!(machine.is_transitioning());
    }

    #[test]
    fn first_matching_transition_wins() {
        let idle = constant_translation_clip(0.0);
        let walk = constant_translation_clip(1.0);
        let run = constant_translation_clip(2.0);
        let states = vec![
            AnimationState {
                name: "Idle".to_string(),
                clip: &idle,
                transitions: vec![
                    Transition {
                        target: "Walk".to_string(),
                        conditions: vec![],
                        duration_seconds: 0.1,
                    },
                    Transition {
                        target: "Run".to_string(),
                        conditions: vec![],
                        duration_seconds: 0.1,
                    },
                ],
            },
            AnimationState {
                name: "Walk".to_string(),
                clip: &walk,
                transitions: vec![],
            },
            AnimationState {
                name: "Run".to_string(),
                clip: &run,
                transitions: vec![],
            },
        ];
        let mut machine = StateMachine::new(states, "Idle").unwrap();
        machine.update(0.01);
        assert_eq!(machine.current_state_name(), "Walk");
    }
}
