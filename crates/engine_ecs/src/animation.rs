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
//! ## Switching clips
//!
//! A model placed in a scene also gets [`AnimationClips`], every clip its
//! file carries. [`AnimationPlayer::play`] switches to another one and can
//! crossfade: the old clip keeps playing underneath for the fade time and
//! [`update_animations`] blends the two poses.
//!
//! ## Not here: state machines
//!
//! [`engine_animation::StateMachine`] borrows its clips (`StateMachine<'a>`)
//! and so cannot live in a `'static` component. Drive one from game code
//! and point [`AnimationPlayer::clip`] at whichever clip it selects.

use std::sync::Arc;

use bevy_ecs::prelude::{Component, World};
use engine_animation::{blend_poses, compute_skinning_matrices, sample_looping, sample_pose};
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

    /// The events a clip carries in its file.
    pub fn from_clip(clip: &ImportedAnimation) -> Vec<Self> {
        clip.events
            .iter()
            .map(|event| Self::new(event.time, event.name.clone()))
            .collect()
    }
}

/// Every clip a model's file carries, in file order.
///
/// Lets a script or the Inspector switch clips by name without going back
/// to the file. Shared behind [`Arc`] like [`AnimationPlayer::clip`].
#[derive(Component, Clone, Default)]
pub struct AnimationClips(pub Vec<Arc<ImportedAnimation>>);

impl AnimationClips {
    /// The clip named `name`, if the file has one.
    pub fn get(&self, name: &str) -> Option<&Arc<ImportedAnimation>> {
        self.0
            .iter()
            .find(|clip| clip.name.as_deref() == Some(name))
    }

    /// Every clip's name; unnamed clips are skipped.
    pub fn names(&self) -> Vec<String> {
        self.0.iter().filter_map(|clip| clip.name.clone()).collect()
    }
}

/// Clips mixed by one number, as an animator's blend state sets them.
#[derive(Clone)]
pub struct Blend1d {
    /// Each clip and its place on the number's scale, in ascending order.
    pub clips: Vec<(f32, Arc<ImportedAnimation>)>,
    /// The number now.
    pub parameter: f32,
}

impl Blend1d {
    /// The pose at `phase` (0..1 through each clip's cycle): the two clips
    /// around `parameter` blended, or the end one alone outside the range.
    fn pose(&self, skeleton: &ImportedSkeleton, phase: f32) -> Vec<engine_utils::Transform> {
        let at = |clip: &ImportedAnimation| sample_looping(skeleton, clip, phase * clip.duration);
        let upper = self
            .clips
            .iter()
            .position(|(place, _)| *place >= self.parameter);
        match upper {
            Some(0) => at(&self.clips[0].1),
            Some(index) => {
                let (low_at, low) = &self.clips[index - 1];
                let (high_at, high) = &self.clips[index];
                let span = high_at - low_at;
                let weight = if span > f32::EPSILON {
                    (self.parameter - low_at) / span
                } else {
                    1.0
                };
                blend_poses(&at(low), &at(high), weight)
            }
            None => self
                .clips
                .last()
                .map_or_else(Vec::new, |(_, clip)| at(clip)),
        }
    }
}

/// How far `time` is through a clip `duration` long, `0.0..1.0`.
fn phase_of(time: f32, duration: f32) -> f32 {
    if duration > f32::EPSILON && time.is_finite() {
        (time / duration).rem_euclid(1.0)
    } else {
        0.0
    }
}

/// The clip being faded out while [`AnimationPlayer::clip`] fades in.
#[derive(Clone)]
pub struct Crossfade {
    /// The clip that was playing before the switch.
    pub clip: Arc<ImportedAnimation>,
    /// Its playback position, still advancing during the fade.
    pub time: f32,
    /// Whether it wraps while it fades.
    pub looping: bool,
    /// Seconds of fade done so far.
    pub elapsed: f32,
    /// Seconds the whole fade takes; always above zero.
    pub duration: f32,
}

impl Crossfade {
    /// How much of the new clip shows, from `0.0` (just switched) to `1.0`.
    pub fn weight(&self) -> f32 {
        (self.elapsed / self.duration).clamp(0.0, 1.0)
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
    /// The previous clip, blended out after a [`AnimationPlayer::play`]
    /// with a fade time.
    pub fade: Option<Crossfade>,
    /// Whether the clip has reached its end since it started: a one-shot
    /// stopped, or a looping clip wrapped at least once.
    pub ended: bool,
    /// Clips mixed by a number instead of `clip` alone (an animator's
    /// blend state); `clip` is then the one keeping time.
    pub blend: Option<Blend1d>,
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
            fade: None,
            ended: false,
            blend: None,
            previous_time: 0.0,
        }
    }

    /// Switches to `clip` from its start, with the clip's own events.
    ///
    /// With `fade_seconds` above zero the current clip keeps playing and
    /// is blended out over that time; at zero (or not finite) the switch
    /// is instant. Playing the clip already playing restarts it.
    pub fn play(
        &mut self,
        clip: Arc<ImportedAnimation>,
        looping: bool,
        speed: f32,
        fade_seconds: f32,
    ) {
        self.fade = (fade_seconds.is_finite() && fade_seconds > 0.0).then(|| Crossfade {
            clip: Arc::clone(&self.clip),
            time: self.time,
            looping: self.looping,
            elapsed: 0.0,
            duration: fade_seconds,
        });
        self.events = AnimationEvent::from_clip(&clip);
        self.clip = clip;
        self.blend = None;
        self.looping = looping;
        if speed.is_finite() {
            self.speed = speed;
        }
        self.playing = true;
        self.restart();
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
        self.ended = false;
    }

    /// Mixes `clips` (each with its place on the blend number's scale,
    /// sorted by the caller or not) at `parameter`.
    ///
    /// The clip nearest `parameter` keeps time, and switching which one is
    /// nearest keeps the place in the cycle, so a walk turning into a run
    /// stays in step. Its events are the ones reported.
    pub fn set_blend(&mut self, clips: Vec<(f32, Arc<ImportedAnimation>)>, parameter: f32) {
        let mut clips = clips;
        clips.retain(|(at, _)| at.is_finite());
        clips.sort_by(|a, b| a.0.total_cmp(&b.0));
        let parameter = if parameter.is_finite() {
            parameter
        } else {
            0.0
        };
        let Some((_, nearest)) = clips
            .iter()
            .min_by(|a, b| (a.0 - parameter).abs().total_cmp(&(b.0 - parameter).abs()))
            .cloned()
        else {
            self.blend = None;
            return;
        };
        if !Arc::ptr_eq(&nearest, &self.clip) {
            let phase = phase_of(self.time, self.clip.duration);
            self.time = phase * nearest.duration.max(0.0);
            self.previous_time = self.time;
            self.events = AnimationEvent::from_clip(&nearest);
            self.clip = nearest;
        }
        self.blend = Some(Blend1d { clips, parameter });
    }

    /// Advances the clock by `dt` seconds, returning the events crossed.
    ///
    /// Pure over `self` — no GPU, no world — so the whole timing and
    /// event contract is testable directly.
    pub fn advance(&mut self, dt: f32) -> Vec<String> {
        if !self.playing || !dt.is_finite() {
            return Vec::new();
        }
        self.advance_fade(dt);
        let duration = self.clip.duration;
        if duration <= f32::EPSILON {
            return Vec::new();
        }

        self.previous_time = self.time;
        let advanced = self.time + dt * self.speed;

        if self.looping {
            // `rem_euclid` handles negative speed too: playing backwards
            // past zero wraps to the end rather than going negative.
            let wrapped_time = advanced.rem_euclid(duration);
            self.time = wrapped_time;
            if (advanced - wrapped_time).abs() > f32::EPSILON {
                self.ended = true;
            }
            return self.crossed_looping_events(self.previous_time, dt * self.speed, duration);
        } else if advanced >= duration {
            self.time = duration;
            self.playing = false;
            self.ended = true;
        } else if advanced <= 0.0 {
            self.time = 0.0;
            self.playing = false;
            self.ended = true;
        } else {
            self.time = advanced;
        }

        self.crossed_events(self.previous_time, self.time, false, duration)
    }

    /// Moves the faded-out clip on and drops it once the fade is done.
    fn advance_fade(&mut self, dt: f32) {
        let speed = self.speed;
        let Some(fade) = &mut self.fade else {
            return;
        };
        fade.elapsed += dt.abs();
        if fade.elapsed >= fade.duration {
            self.fade = None;
            return;
        }
        let duration = fade.clip.duration;
        let advanced = fade.time + dt * speed;
        fade.time = if fade.looping && duration > f32::EPSILON {
            advanced.rem_euclid(duration)
        } else {
            advanced.clamp(0.0, duration.max(0.0))
        };
    }

    /// The pose this player shows now: its clip, blended with the clip
    /// fading out if there is one.
    pub fn pose(&self) -> Vec<engine_utils::Transform> {
        let pose = match &self.blend {
            Some(blend) => blend.pose(&self.skeleton, phase_of(self.time, self.clip.duration)),
            None => sample_clip(&self.skeleton, &self.clip, self.time, self.looping),
        };
        match &self.fade {
            Some(fade) => {
                let old = sample_clip(&self.skeleton, &fade.clip, fade.time, fade.looping);
                blend_poses(&old, &pose, fade.weight())
            }
            None => pose,
        }
    }

    fn crossed_looping_events(&self, from: f32, travelled: f32, duration: f32) -> Vec<String> {
        let forwards = travelled >= 0.0;
        let mut position = from;
        let mut remaining = travelled.abs();
        let mut fired = Vec::new();

        while remaining > f32::EPSILON {
            let to_boundary = if forwards {
                duration - position
            } else {
                position
            };
            if to_boundary <= f32::EPSILON {
                position = if forwards { 0.0 } else { duration };
                continue;
            }
            let step = remaining.min(to_boundary);
            let next = if forwards {
                position + step
            } else {
                position - step
            };
            fired.extend(
                self.events
                    .iter()
                    .filter(|event| {
                        event.time >= 0.0
                            && event.time <= duration
                            && if forwards {
                                event.time > position && event.time <= next
                            } else {
                                event.time < position && event.time >= next
                            }
                    })
                    .map(|event| event.name.clone()),
            );

            remaining -= step;
            if remaining <= f32::EPSILON {
                break;
            }
            position = if forwards { 0.0 } else { duration };
        }
        fired
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
/// something other than a mesh. So does a skinned mesh that was past its
/// draw distance last frame, without uploading a pose.
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

        // Past its draw distance last frame: keep time (and events), but
        // don't work out a pose nobody sees.
        if let Some(skinned) = skinned
            && !skinned
                .beyond_draw_distance
                .load(std::sync::atomic::Ordering::Relaxed)
        {
            upload_pose(gpu, &player, skinned);
        }
    }
}

/// Writes `player`'s current pose into `skinned`'s joint buffer without
/// moving its clock. The editor uses this to show a paused or scrubbed
/// pose.
pub fn upload_pose(gpu: &GpuContext, player: &AnimationPlayer, skinned: &SkinnedMeshRenderer) {
    let matrices = compute_skinning_matrices(&player.skeleton, &player.pose());
    gpu.write_uniform_buffer(
        &skinned.skin.joints_buffer,
        &JointMatricesUniform::from_matrices(&matrices),
    );
}

/// Looping clips sample with wrap-around interpolation so the last frame
/// blends back into the first; one-shots clamp.
fn sample_clip(
    skeleton: &ImportedSkeleton,
    clip: &ImportedAnimation,
    time: f32,
    looping: bool,
) -> Vec<engine_utils::Transform> {
    if looping {
        sample_looping(skeleton, clip, time)
    } else {
        sample_pose(skeleton, clip, time)
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
            events: Vec::new(),
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
    fn negative_speed_wraps_backwards_from_the_exact_start() {
        let mut player = AnimationPlayer::new(skeleton(), clip(1.0));
        player.speed = -1.0;
        player.advance(0.2);

        assert!((player.time - 0.8).abs() < 1e-5);
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
    fn events_fire_for_each_loop_crossed_in_one_update() {
        let mut player = player_with_events(vec![AnimationEvent::new(0.25, "footstep")]);
        let fired = player.advance(2.3);

        assert_eq!(fired, vec!["footstep", "footstep", "footstep"]);
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

    fn named_clip(name: &str, duration: f32, events: &[(f32, &str)]) -> Arc<ImportedAnimation> {
        Arc::new(ImportedAnimation {
            name: Some(name.into()),
            duration,
            channels: HashMap::new(),
            events: events
                .iter()
                .map(|(time, name)| engine_asset::ImportedAnimationEvent {
                    time: *time,
                    name: (*name).into(),
                })
                .collect(),
        })
    }

    #[test]
    fn play_switches_clip_restarts_and_takes_the_clips_events() {
        let mut player = AnimationPlayer::new(skeleton(), named_clip("idle", 1.0, &[]));
        player.advance(0.4);
        player.play(named_clip("walk", 2.0, &[(0.5, "step")]), false, 2.0, 0.0);

        assert_eq!(player.clip.name.as_deref(), Some("walk"));
        assert_eq!(player.time, 0.0);
        assert!(!player.looping);
        assert_eq!(player.speed, 2.0);
        assert!(player.fade.is_none(), "a zero fade switches at once");
        assert_eq!(player.advance(0.3), vec!["step".to_string()]);
    }

    #[test]
    fn crossfade_blends_out_the_old_clip_then_drops_it() {
        let mut player = AnimationPlayer::new(skeleton(), named_clip("idle", 1.0, &[]));
        player.advance(0.4);
        player.play(named_clip("run", 1.0, &[]), true, 1.0, 0.5);

        let fade = player.fade.as_ref().expect("fading");
        assert_eq!(fade.clip.name.as_deref(), Some("idle"));
        assert!(
            (fade.time - 0.4).abs() < 1e-5,
            "the old clip keeps its place"
        );
        assert_eq!(fade.weight(), 0.0);

        player.advance(0.25);
        let fade = player.fade.as_ref().expect("still fading");
        assert!((fade.weight() - 0.5).abs() < 1e-5);
        assert!((fade.time - 0.65).abs() < 1e-5, "the old clip keeps moving");

        player.advance(0.3);
        assert!(player.fade.is_none(), "the fade ends after its time");
    }

    #[test]
    fn a_paused_crossfade_does_not_move() {
        let mut player = AnimationPlayer::new(skeleton(), named_clip("idle", 1.0, &[]));
        player.play(named_clip("run", 1.0, &[]), true, 1.0, 0.5);
        player.playing = false;
        player.advance(1.0);
        assert_eq!(player.fade.as_ref().map(Crossfade::weight), Some(0.0));
    }

    #[test]
    fn non_finite_speed_or_fade_is_ignored() {
        let mut player = AnimationPlayer::new(skeleton(), named_clip("idle", 1.0, &[]));
        player.play(named_clip("run", 1.0, &[]), true, f32::NAN, f32::INFINITY);
        assert_eq!(player.speed, 1.0);
        assert!(player.fade.is_none());
    }

    #[test]
    fn clips_find_by_name_and_list_named_ones() {
        let mut unnamed = (*named_clip("x", 1.0, &[])).clone();
        unnamed.name = None;
        let clips = AnimationClips(vec![
            named_clip("idle", 1.0, &[]),
            Arc::new(unnamed),
            named_clip("walk", 1.0, &[]),
        ]);
        assert_eq!(clips.names(), vec!["idle".to_string(), "walk".to_string()]);
        assert!(clips.get("walk").is_some());
        assert!(clips.get("jump").is_none());
    }

    #[test]
    fn pose_blends_with_the_fading_clip() {
        let mut player = AnimationPlayer::new(skeleton(), named_clip("idle", 1.0, &[]));
        player.play(named_clip("run", 1.0, &[]), true, 1.0, 0.5);
        assert_eq!(player.pose().len(), 1, "one pose entry per joint");
    }

    #[test]
    fn ended_marks_a_finished_one_shot_and_a_wrapped_loop() {
        let mut once = AnimationPlayer::new(skeleton(), clip(1.0)).once();
        once.advance(0.5);
        assert!(!once.ended);
        once.advance(0.6);
        assert!(once.ended);

        let mut looping = AnimationPlayer::new(skeleton(), clip(1.0));
        looping.advance(0.9);
        assert!(!looping.ended);
        looping.advance(0.2);
        assert!(looping.ended, "wrapped once");
        looping.restart();
        assert!(!looping.ended);
    }

    #[test]
    fn blend_keeps_the_place_in_the_cycle_when_the_nearest_clip_changes() {
        let walk = named_clip("walk", 1.0, &[(0.5, "step")]);
        let run = named_clip("run", 0.5, &[(0.25, "run_step")]);
        let mut player = AnimationPlayer::new(skeleton(), Arc::clone(&walk));
        player.set_blend(vec![(0.0, Arc::clone(&walk)), (4.0, Arc::clone(&run))], 1.0);
        assert_eq!(player.clip.name.as_deref(), Some("walk"));
        player.time = 0.4;

        player.set_blend(vec![(0.0, walk), (4.0, run)], 3.5);
        assert_eq!(
            player.clip.name.as_deref(),
            Some("run"),
            "run is nearest now"
        );
        assert!((player.time - 0.2).abs() < 1e-5, "40% through either cycle");
        assert_eq!(player.events[0].name, "run_step");
        assert_eq!(player.pose().len(), 1);
    }

    #[test]
    fn play_drops_a_blend() {
        let walk = named_clip("walk", 1.0, &[]);
        let mut player = AnimationPlayer::new(skeleton(), Arc::clone(&walk));
        player.set_blend(vec![(0.0, Arc::clone(&walk))], 0.0);
        player.play(named_clip("jump", 1.0, &[]), false, 1.0, 0.0);
        assert!(player.blend.is_none());
    }
}
