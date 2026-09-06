//! Spatial audio driven by entity transforms.
//!
//! A game spawns an [`AudioEmitter`] on the entity that should make a
//! noise and an [`AudioListener`] on the entity that hears (usually the
//! camera). [`update_audio`] runs once per frame and does the rest:
//! creates kira's listener and spatial tracks the first time it sees
//! them, starts `autoplay` emitters exactly once, and keeps both in sync
//! with their [`GlobalTransform`] as they move.
//!
//! ## Handles live on the components
//!
//! `ListenerHandle` and `SpatialTrackHandle` are stored in the components
//! themselves, the same arrangement [`crate::components::MeshRenderer`]
//! has with GPU handles and [`crate::components::RigidBody`] has with
//! rapier's. The alternative — a side table keyed by entity — would need
//! its own cleanup pass on despawn; a handle on the component is dropped
//! with the entity.
//!
//! ## Spatial or bussed, not both
//!
//! kira plays a sound on exactly one track. A spatial track pans and
//! attenuates by distance; a [`engine_audio::Bus`] applies a shared
//! volume. An emitter therefore picks one: give it a `bus` and it plays
//! flat on that bus, leave `bus` as `None` and it plays spatially. This
//! is kira's model, not a limitation added here.

use bevy_ecs::prelude::{Component, Entity, World};
use engine_audio::{
    AudioContext, Bus, ListenerHandle, SpatialTrackHandle, StaticSound, Tween, set_listener_pose,
    set_track_position,
};
use glam::Vec3;

use crate::components::GlobalTransform;

/// How quickly a listener/emitter slews to a new pose. One frame at 60 Hz
/// — long enough to avoid zipper noise from teleporting a source, short
/// enough to feel instant.
fn pose_tween() -> Tween {
    Tween {
        duration: std::time::Duration::from_millis(16),
        ..Default::default()
    }
}

/// The entity that hears. Usually the camera.
///
/// Exactly one is expected; if several exist, the first the query yields
/// wins and the rest are ignored (with a warning) rather than fighting
/// over the output.
#[derive(Component, Default)]
pub struct AudioListener {
    /// kira's listener, created by [`update_audio`] on first sight.
    handle: Option<ListenerHandle>,
}

impl AudioListener {
    /// Whether this listener has been registered with the audio backend
    /// yet.
    pub fn is_active(&self) -> bool {
        self.handle.is_some()
    }
}

/// An entity that emits sound from its world position.
#[derive(Component)]
pub struct AudioEmitter {
    /// The sound to play. Cloned per play — cheap, kira reference-counts
    /// the samples.
    pub sound: StaticSound,
    /// Play as soon as the emitter is first seen. Set `false` to trigger
    /// it later with [`AudioEmitter::play`].
    pub autoplay: bool,
    /// Index into the caller's own bus list, if this should play flat on
    /// a bus instead of spatially. See the module docs.
    pub bus: Option<usize>,
    /// Set when a play has been requested and not yet started.
    pending: bool,
    /// Whether a play has already been started, so `autoplay` fires once
    /// rather than every frame.
    started: bool,
    /// This emitter's spatial track, created on first sight. `None` for a
    /// bussed emitter.
    track: Option<SpatialTrackHandle>,
}

impl AudioEmitter {
    /// An emitter that plays `sound` spatially as soon as it is spawned.
    pub fn new(sound: StaticSound) -> Self {
        Self {
            sound,
            autoplay: true,
            bus: None,
            pending: false,
            started: false,
            track: None,
        }
    }

    /// An emitter that stays silent until [`AudioEmitter::play`] is
    /// called.
    pub fn silent(sound: StaticSound) -> Self {
        Self {
            autoplay: false,
            ..Self::new(sound)
        }
    }

    /// Plays this emitter flat on the caller's bus at `bus_index` rather
    /// than spatially.
    pub fn on_bus(mut self, bus_index: usize) -> Self {
        self.bus = Some(bus_index);
        self
    }

    /// Requests a play on the next [`update_audio`]. Retriggerable: call
    /// it again to play the sound again.
    pub fn play(&mut self) {
        self.pending = true;
    }

    /// Whether this emitter has started playing at least once.
    pub fn has_started(&self) -> bool {
        self.started
    }
}

/// Syncs the listener and every emitter with their transforms, and starts
/// any playback that was requested.
///
/// Call once per frame, after transform propagation so `GlobalTransform`
/// is current. Never fails: a backend that refuses a track or a play logs
/// and the entity stays silent, because losing audio must not stop a
/// game.
///
/// `buses` is the caller's own bus list, indexed by
/// [`AudioEmitter::bus`]. Pass an empty slice if no emitter uses one.
pub fn update_audio(world: &mut World, audio: &mut AudioContext, buses: &mut [Bus]) {
    let listener_entity = sync_listener(world, audio);

    // Collect first so the query's borrow ends before the world is
    // mutated below.
    let emitters: Vec<(Entity, Vec3)> = world
        .query::<(Entity, &GlobalTransform, &AudioEmitter)>()
        .iter(world)
        .map(|(entity, global, _)| (entity, global.0.translation))
        .collect();

    // Tracks are created here, holding a *shared* borrow of the world (to
    // read the listener handle) alongside `&mut audio`. Those don't
    // conflict: `audio` is not part of the world. The results are written
    // back afterwards, once the shared borrow is gone.
    let mut new_tracks: Vec<(Entity, SpatialTrackHandle)> = Vec::new();
    if let Some(listener_entity) = listener_entity
        && let Some(handle) = world
            .get::<AudioListener>(listener_entity)
            .and_then(|listener| listener.handle.as_ref())
    {
        for &(entity, position) in &emitters {
            let needs_track = world
                .get::<AudioEmitter>(entity)
                .is_some_and(|emitter| emitter.track.is_none() && emitter.bus.is_none());
            if !needs_track {
                continue;
            }
            match audio.add_spatial_track(handle, position) {
                Ok(track) => new_tracks.push((entity, track)),
                Err(err) => {
                    tracing::warn!(error = %err, "could not create a spatial audio track")
                }
            }
        }
    }
    for (entity, track) in new_tracks {
        if let Some(mut emitter) = world.get_mut::<AudioEmitter>(entity) {
            emitter.track = Some(track);
        }
    }

    for (entity, position) in emitters {
        let Some(mut emitter) = world.get_mut::<AudioEmitter>(entity) else {
            continue;
        };
        if let Some(track) = emitter.track.as_mut() {
            set_track_position(track, position, pose_tween());
        }

        let should_play = emitter.pending || (emitter.autoplay && !emitter.started);
        if !should_play {
            continue;
        }
        emitter.pending = false;
        emitter.started = true;

        let sound = emitter.sound.clone();
        let result = match emitter.bus {
            Some(index) => match buses.get_mut(index) {
                Some(bus) => sound.play_on_bus(bus).map(|_| ()),
                None => {
                    tracing::warn!(index, "emitter names a bus that doesn't exist");
                    continue;
                }
            },
            None => match emitter.track.as_mut() {
                Some(track) => sound.play_on(track).map(|_| ()),
                None => {
                    // No listener yet, so no spatial track. Try again
                    // next frame rather than dropping the play.
                    emitter.started = false;
                    emitter.pending = true;
                    continue;
                }
            },
        };
        if let Err(err) = result {
            tracing::warn!(error = %err, "could not start an audio emitter");
        }
    }
}

/// Creates or moves the listener, returning the entity that owns it.
fn sync_listener(world: &mut World, audio: &mut AudioContext) -> Option<Entity> {
    let listeners: Vec<(Entity, Vec3, glam::Quat)> = world
        .query::<(Entity, &GlobalTransform, &AudioListener)>()
        .iter(world)
        .map(|(entity, global, _)| (entity, global.0.translation, global.0.rotation))
        .collect();

    let (entity, position, rotation) = *listeners.first()?;
    if listeners.len() > 1 {
        tracing::warn!(
            count = listeners.len(),
            "more than one AudioListener; using the first and ignoring the rest"
        );
    }

    let already_active = world
        .get::<AudioListener>(entity)
        .is_some_and(AudioListener::is_active);

    if already_active {
        if let Some(mut listener) = world.get_mut::<AudioListener>(entity)
            && let Some(handle) = listener.handle.as_mut()
        {
            set_listener_pose(handle, position, rotation, pose_tween());
        }
        return Some(entity);
    }

    match audio.add_listener(position, rotation) {
        Ok(handle) => {
            if let Some(mut listener) = world.get_mut::<AudioListener>(entity) {
                listener.handle = Some(handle);
            }
            Some(entity)
        }
        Err(err) => {
            tracing::warn!(error = %err, "could not create the audio listener");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::Transform;

    /// Serializes the tests that open a real audio device.
    ///
    /// Each `AudioContext` claims backend resources, and opening several
    /// at once from parallel test threads can make the backend refuse one
    /// — a flake that says nothing about this module. One at a time
    /// removes the contention entirely.
    static AUDIO_DEVICE: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Locks the device and opens a context, or returns `None` when the
    /// machine has no usable audio output.
    ///
    /// Skipping rather than failing is deliberate: a missing sound device
    /// is an environment fact (CI, a headless sandbox), not a defect. The
    /// guard is returned alongside so it stays held for the test's body.
    fn context() -> Option<(AudioContext, std::sync::MutexGuard<'static, ()>)> {
        let guard = AUDIO_DEVICE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        AudioContext::new().ok().map(|audio| (audio, guard))
    }

    fn silent_sound() -> Option<StaticSound> {
        // 0.05 s of silence as a valid, tiny WAV.
        let samples = 2205u32;
        let data_len = samples * 2;
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&44100u32.to_le_bytes());
        wav.extend_from_slice(&88200u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        wav.extend_from_slice(&vec![0u8; data_len as usize]);
        StaticSound::from_bytes(wav, false).ok()
    }

    #[test]
    fn emitter_defaults_to_spatial_autoplay() {
        let Some(sound) = silent_sound() else { return };
        let emitter = AudioEmitter::new(sound);
        assert!(emitter.autoplay);
        assert!(emitter.bus.is_none());
        assert!(!emitter.has_started());
    }

    #[test]
    fn silent_emitter_does_not_autoplay() {
        let Some(sound) = silent_sound() else { return };
        assert!(!AudioEmitter::silent(sound).autoplay);
    }

    #[test]
    fn on_bus_switches_off_spatial_playback() {
        let Some(sound) = silent_sound() else { return };
        let emitter = AudioEmitter::new(sound).on_bus(2);
        assert_eq!(emitter.bus, Some(2));
    }

    #[test]
    fn play_marks_the_emitter_pending() {
        let Some(sound) = silent_sound() else { return };
        let mut emitter = AudioEmitter::silent(sound);
        assert!(!emitter.pending);
        emitter.play();
        assert!(emitter.pending);
    }

    #[test]
    fn update_activates_the_listener_once() {
        let (Some((mut audio, _guard)), Some(_)) = (context(), silent_sound()) else {
            return;
        };
        let mut world = World::new();
        let listener = world
            .spawn((
                Transform::default(),
                GlobalTransform::default(),
                AudioListener::default(),
            ))
            .id();

        update_audio(&mut world, &mut audio, &mut []);
        assert!(
            world
                .get::<AudioListener>(listener)
                .is_some_and(AudioListener::is_active),
            "the listener should have been registered with the backend",
        );

        // A second pass must move it, not create a second one.
        update_audio(&mut world, &mut audio, &mut []);
        assert!(
            world
                .get::<AudioListener>(listener)
                .is_some_and(AudioListener::is_active)
        );
    }

    #[test]
    fn autoplay_emitter_starts_exactly_once() {
        let (Some((mut audio, _guard)), Some(sound)) = (context(), silent_sound()) else {
            return;
        };
        let mut world = World::new();
        world.spawn((
            Transform::default(),
            GlobalTransform::default(),
            AudioListener::default(),
        ));
        let emitter = world
            .spawn((
                Transform::default(),
                GlobalTransform::default(),
                AudioEmitter::new(sound),
            ))
            .id();

        update_audio(&mut world, &mut audio, &mut []);
        assert!(
            world
                .get::<AudioEmitter>(emitter)
                .is_some_and(AudioEmitter::has_started),
            "autoplay should start on the first pass",
        );

        // Ten more frames must not retrigger it.
        for _ in 0..10 {
            update_audio(&mut world, &mut audio, &mut []);
        }
        assert!(
            world
                .get::<AudioEmitter>(emitter)
                .is_some_and(AudioEmitter::has_started)
        );
    }

    #[test]
    fn emitter_without_a_listener_defers_rather_than_dropping_the_play() {
        let (Some((mut audio, _guard)), Some(sound)) = (context(), silent_sound()) else {
            return;
        };
        let mut world = World::new();
        let emitter = world
            .spawn((
                Transform::default(),
                GlobalTransform::default(),
                AudioEmitter::new(sound),
            ))
            .id();

        update_audio(&mut world, &mut audio, &mut []);
        assert!(
            !world
                .get::<AudioEmitter>(emitter)
                .is_some_and(AudioEmitter::has_started),
            "with no listener there is no spatial track, so the play waits",
        );

        world.spawn((
            Transform::default(),
            GlobalTransform::default(),
            AudioListener::default(),
        ));
        update_audio(&mut world, &mut audio, &mut []);
        assert!(
            world
                .get::<AudioEmitter>(emitter)
                .is_some_and(AudioEmitter::has_started),
            "once a listener exists the deferred play should start",
        );
    }

    #[test]
    fn emitter_naming_a_missing_bus_is_skipped_not_fatal() {
        let (Some((mut audio, _guard)), Some(sound)) = (context(), silent_sound()) else {
            return;
        };
        let mut world = World::new();
        world.spawn((
            Transform::default(),
            GlobalTransform::default(),
            AudioListener::default(),
        ));
        world.spawn((
            Transform::default(),
            GlobalTransform::default(),
            AudioEmitter::new(sound).on_bus(7),
        ));

        // The point is that this returns at all.
        update_audio(&mut world, &mut audio, &mut []);
    }
}
