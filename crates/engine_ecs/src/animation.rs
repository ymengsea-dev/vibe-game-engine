//! Skeletal animation driven from components.
//!
//! A game gives an entity an [`AnimationPlayer`] alongside its
//! [`crate::components::SkinnedMeshRenderer`]; [`update_animations`] runs
//! once per frame and does the rest — advances the clock, samples the
//! pose, computes skinning matrices, uploads them, and reports any
//! [`AnimationEvent`]s crossed this frame.
//!
//! Before this existed, `engine_animation` was a set of pure functions
//! and every game had to call `sample_pose` and
//! `compute_skinning_matrices` itself each frame, then find the right
//! GPU buffer to write them into.
//!
//! ## Clips are shared, clocks are not
//!
//! [`AnimationPlayer`] holds its skeleton and clip behind [`Arc`], so
//! fifty characters playing one walk cycle share a single copy of it
//! while each keeps its own `time`. `Arc` rather than `Rc` because
//! `bevy_ecs` requires components to be `Send + Sync`.
//!
//! ## Events are authored, not imported
//!
//! glTF has no concept of animation events, so
//! [`engine_asset::ImportedAnimation`] carries none. A game attaches them
//! to the player itself — the frame a foot plants, the frame a swing
//! connects.
//!
//! ## Not here: state machines
//!
//! [`engine_animation::StateMachine`] borrows its clips (`StateMachine<'a>`)
//! and so cannot live in a `'static` component. Drive one from game code
//! and point [`AnimationPlayer::clip`] at whichever clip it selects.

use std::sync::Arc;

use bevy_ecs::prelude::{Component, World};
use engine_animation::{compute_skinning_matrices, sample_looping, sample_pose};
use engine_asset::{ImportedAnimation, ImportedSkeleton};
use engine_renderer::{GpuContext, JointMatricesUniform};

use crate::components::SkinnedMeshRenderer;

/// Something that happens at a moment within a clip.
///
/// The frame a footstep lands, a hitbox opens, or a line of dialogue
/// starts. [`update_animations`] reports each one on the frame its time
/// is crossed.
#[derive(Debug, Clone, PartialEq)]
pub struct AnimationEvent {
    /// When in the clip this fires, in seconds from the clip's start.
    pub time: f32,
    /// What to call it. Game code matches on this.
    pub name: String,
}

impl AnimationEvent {
    /// An event named `name` at `time` seconds into the clip.
    pub fn new(time: f32, name: impl Into<String>) -> Self {
        Self {
            time,
            name: name.into(),
        }
    }
}

/// Plays one clip on one skinned entity.
#[derive(Component, Clone)]
pub struct AnimationPlayer {
    /// The skeleton the clip animates. Shared — see the module docs.
    pub skeleton: Arc<ImportedSkeleton>,
    /// The clip being played.
    pub clip: Arc<ImportedAnimation>,
    /// Playback position in seconds.
    pub time: f32,
    /// Playback rate. `1.0` is authored speed, `0.0` freezes, negative
    /// plays backwards.
    pub speed: f32,
    /// Whether to wrap back to the start at the end of the clip. A
    /// non-looping player stops at the last frame and clears
    /// [`AnimationPlayer::playing`].
    pub looping: bool,
    /// Whether the clock is advancing.
    pub playing: bool,
    /// Events to report as the clock crosses them.
    pub events: Vec<AnimationEvent>,
    /// Last frame's `time`, so event crossings can be detected over the
    /// interval actually travelled rather than tested against a single
    /// instant.
    previous_time: f32,
}

impl AnimationPlayer {
    /// A looping player, started at the beginning of `clip`.
    pub fn new(skeleton: Arc<ImportedSkeleton>, clip: Arc<ImportedAnimation>) -> Self {
        Self {
            skeleton,
            clip,
            time: 0.0,
            speed: 1.0,
            looping: true,
            playing: true,
            events: Vec::new(),
            previous_time: 0.0,
        }
    }

    /// The same player, playing once and stopping at the last frame.
    pub fn once(mut self) -> Self {
        self.looping = false;
        self
    }

    /// The same player, reporting `events`.
    pub fn with_events(mut self, events: Vec<AnimationEvent>) -> Self {
        self.events = events;
        self
    }

    /// Rewinds to the start without changing whether it is playing.
    ///
    /// Also resets the event cursor, so a replay fires every event again.
    pub fn restart(&mut self) {
        self.time = 0.0;
        self.previous_time = 0.0;
    }

    /// Advances the clock by `dt` seconds, returning the events crossed.
    ///
    /// Pure over `self` — no GPU, no world — so the whole timing and
    /// event contract is testable directly.
    pub fn advance(&mut self, dt: f32) -> Vec<String> {
        if !self.playing || !dt.is_finite() {
            return Vec::new();
        }
        let duration = self.clip.duration;
        if duration <= f32::EPSILON {
            return Vec::new();
        }

        self.previous_time = self.time;
        let advanced = self.time + dt * self.speed;

        let mut wrapped = false;
        if self.looping {
            // `rem_euclid` handles negative speed too: playing backwards
            // past zero wraps to the end rather than going negative.
            let wrapped_time = advanced.rem_euclid(duration);
            wrapped = advanced >= duration || advanced < 0.0;
            self.time = wrapped_time;
        } else if advanced >= duration {
            self.time = duration;
            self.playing = false;
        } else if advanced <= 0.0 {
            self.time = 0.0;
            self.playing = false;
        } else {
            self.time = advanced;
        }

        self.crossed_events(self.previous_time, self.time, wrapped, duration)
    }

    /// Which events lie in the interval actually travelled this frame.
    ///
    /// Testing `time == event.time` would miss almost everything: a frame
    /// steps over several milliseconds at once, so an event is nearly
    /// never landed on exactly. This tests the half-open interval
    /// `(from, to]` instead, and splits it in two when the clock wrapped
    /// so neither the tail of the clip nor the head is skipped.
    fn crossed_events(&self, from: f32, to: f32, wrapped: bool, duration: f32) -> Vec<String> {
        let forwards = self.speed >= 0.0;
        self.events
            .iter()
            .filter(|event| {
                if wrapped {
                    if forwards {
                        // ...from → end of clip, then start → to.
                        event.time > from || event.time <= to
                    } else {
                        event.time < from || event.time >= to
                    }
                } else if forwards {
                    event.time > from && event.time <= to
                } else {
                    event.time < from && event.time >= to
                }
            })
            .filter(|event| event.time >= 0.0 && event.time <= duration)
            .map(|event| event.name.clone())
            .collect()
    }
}

/// The events an entity's animation crossed this frame.
///
/// Rewritten every frame by [`update_animations`], so game code reads it
/// without having to clear it. Empty on a frame where nothing fired.
#[derive(Component, Debug, Clone, Default, PartialEq, Eq)]
pub struct AnimationEvents(pub Vec<String>);

impl AnimationEvents {
    /// Whether `name` fired this frame.
    pub fn fired(&self, name: &str) -> bool {
        self.0.iter().any(|fired| fired == name)
    }
}

/// Advances every [`AnimationPlayer`], uploads the resulting pose, and
/// records the frame's events.
///
/// Call once per frame, before rendering. An entity with an
/// [`AnimationPlayer`] but no [`crate::components::SkinnedMeshRenderer`]
/// still ticks its clock and fires its events — useful for driving
/// something other than a mesh.
///
/// Never fails: a clip with no duration is skipped, and an oversized
/// skeleton is truncated to what the shader can hold rather than
/// refusing to draw.
pub fn update_animations(world: &mut World, gpu: &GpuContext, dt: f32) {
    let mut query = world.query::<(
        &mut AnimationPlayer,
        Option<&SkinnedMeshRenderer>,
        Option<&mut AnimationEvents>,
    )>();

    for (mut player, skinned, events) in query.iter_mut(world) {
        let fired = player.advance(dt);

        if let Some(mut events) = events {
            // Assign rather than extend: the component is this frame's
            // events, not a growing log.
            events.0 = fired;
        }

        let Some(skinned) = skinned else {
            continue;
        };

        // Looping clips sample with wrap-around interpolation so the last
        // frame blends back into the first; one-shots clamp.
        let pose = if player.looping {
            sample_looping(&player.skeleton, &player.clip, player.time)
        } else {
            sample_pose(&player.skeleton, &player.clip, player.time)
        };
        let matrices = compute_skinning_matrices(&player.skeleton, &pose);

        gpu.write_uniform_buffer(
            &skinned.skin.joints_buffer,
            &JointMatricesUniform::from_matrices(&matrices),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_asset::{ImportedAnimationChannels, ImportedJoint};
    use std::collections::HashMap;

    /// A one-joint skeleton, enough to exercise sampling.
    fn skeleton() -> Arc<ImportedSkeleton> {
        Arc::new(ImportedSkeleton {
            name: Some("test".into()),
            joints: vec![ImportedJoint {
                name: Some("root".into()),
                node_index: 0,
                parent: None,
                local_bind_transform: engine_utils::Transform::IDENTITY,
                inverse_bind_matrix: glam::Mat4::IDENTITY,
            }],
        })
    }

    /// A clip `duration` seconds long with no channels — long enough to
    /// have a clock, simple enough to reason about.
    fn clip(duration: f32) -> Arc<ImportedAnimation> {
        Arc::new(ImportedAnimation {
            name: Some("test".into()),
            duration,
            channels: HashMap::<usize, ImportedAnimationChannels>::new(),
        })
    }

    fn player_with_events(events: Vec<AnimationEvent>) -> AnimationPlayer {
        AnimationPlayer::new(skeleton(), clip(1.0)).with_events(events)
    }

    #[test]
    fn player_advances_and_wraps_time() {
        let mut player = AnimationPlayer::new(skeleton(), clip(1.0));
        player.advance(0.4);
        assert!((player.time - 0.4).abs() < 1e-5);
        player.advance(0.8);
        assert!(
            (player.time - 0.2).abs() < 1e-5,
            "1.2s into a 1s looping clip is 0.2s, got {}",
            player.time,
        );
        assert!(player.playing);
    }

    #[test]
    fn non_looping_player_clamps_and_stops() {
        let mut player = AnimationPlayer::new(skeleton(), clip(1.0)).once();
        player.advance(5.0);
        assert!(
            (player.time - 1.0).abs() < 1e-5,
            "should hold the last frame"
        );
        assert!(!player.playing, "a one-shot must stop when it ends");

        // Further advances do nothing.
        player.advance(1.0);
        assert!((player.time - 1.0).abs() < 1e-5);
    }

    #[test]
    fn zero_speed_freezes_and_negative_plays_backwards() {
        let mut player = AnimationPlayer::new(skeleton(), clip(1.0));
        player.time = 0.5;
        player.speed = 0.0;
        player.advance(0.25);
        assert!((player.time - 0.5).abs() < 1e-5, "speed 0 must freeze");

        player.speed = -1.0;
        player.advance(0.2);
        assert!(
            (player.time - 0.3).abs() < 1e-5,
            "negative speed should rewind, got {}",
            player.time,
        );
    }

    #[test]
    fn negative_speed_wraps_backwards_past_zero() {
        let mut player = AnimationPlayer::new(skeleton(), clip(1.0));
        player.time = 0.1;
        player.speed = -1.0;
        player.advance(0.3);
        assert!(
            (player.time - 0.8).abs() < 1e-5,
            "rewinding past 0 should wrap to the end, got {}",
            player.time,
        );
    }

    #[test]
    fn event_fires_once_when_dt_overshoots_it() {
        let mut player = player_with_events(vec![AnimationEvent::new(0.5, "footstep")]);
        // One big step straight past the event.
        let fired = player.advance(0.9);
        assert_eq!(fired, vec!["footstep".to_string()]);

        // And not again while the clock stays past it.
        assert!(player.advance(0.05).is_empty());
    }

    #[test]
    fn event_does_not_fire_before_it_is_reached() {
        let mut player = player_with_events(vec![AnimationEvent::new(0.5, "footstep")]);
        assert!(player.advance(0.2).is_empty());
        assert!(player.advance(0.2).is_empty());
    }

    #[test]
    fn event_refires_after_loop_wrap() {
        let mut player = player_with_events(vec![AnimationEvent::new(0.5, "footstep")]);
        assert_eq!(player.advance(0.6), vec!["footstep".to_string()]);
        // Wrap around and pass it again.
        assert_eq!(player.advance(1.0), vec!["footstep".to_string()]);
    }

    #[test]
    fn an_event_near_the_clip_end_survives_the_wrap() {
        // The case a naive `from < t <= to` misses entirely: the interval
        // straddles the loop point.
        let mut player = player_with_events(vec![AnimationEvent::new(0.95, "late")]);
        player.advance(0.9);
        let fired = player.advance(0.2); // 0.9 -> 1.1 -> wraps to 0.1
        assert_eq!(
            fired,
            vec!["late".to_string()],
            "an event between the old time and the clip end must still fire",
        );
    }

    #[test]
    fn multiple_events_in_one_step_all_fire() {
        let mut player = player_with_events(vec![
            AnimationEvent::new(0.2, "a"),
            AnimationEvent::new(0.4, "b"),
            AnimationEvent::new(0.6, "c"),
        ]);
        let fired = player.advance(0.7);
        assert_eq!(fired.len(), 3, "got {fired:?}");
    }

    #[test]
    fn a_paused_player_fires_nothing() {
        let mut player = player_with_events(vec![AnimationEvent::new(0.5, "footstep")]);
        player.playing = false;
        assert!(player.advance(1.0).is_empty());
        assert_eq!(player.time, 0.0);
    }

    #[test]
    fn a_zero_length_clip_is_skipped_rather_than_dividing_by_zero() {
        let mut player = AnimationPlayer::new(skeleton(), clip(0.0));
        assert!(player.advance(0.5).is_empty());
        assert!(player.time.is_finite());
    }

    #[test]
    fn a_non_finite_dt_is_ignored() {
        let mut player = AnimationPlayer::new(skeleton(), clip(1.0));
        player.time = 0.25;
        player.advance(f32::NAN);
        player.advance(f32::INFINITY);
        assert!((player.time - 0.25).abs() < 1e-5, "time must stay finite");
    }

    #[test]
    fn restart_rewinds_and_lets_events_fire_again() {
        let mut player = player_with_events(vec![AnimationEvent::new(0.5, "footstep")]);
        assert_eq!(player.advance(0.6).len(), 1);
        player.restart();
        assert_eq!(player.time, 0.0);
        assert_eq!(player.advance(0.6).len(), 1, "a replay must fire it again");
    }

    #[test]
    fn events_component_reports_by_name() {
        let events = AnimationEvents(vec!["footstep".into(), "swing".into()]);
        assert!(events.fired("footstep"));
        assert!(!events.fired("jump"));
    }
}
