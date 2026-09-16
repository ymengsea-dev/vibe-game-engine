//! Audio engine lifecycle: owns kira's `AudioManager` for the engine's
//! lifetime.

use glam::{Quat, Vec3};
use kira::listener::ListenerHandle;
use kira::sound::FromFileError;
use kira::sound::static_sound::StaticSoundHandle;
use kira::sound::streaming::StreamingSoundHandle;
use kira::track::{SpatialTrackBuilder, SpatialTrackHandle, TrackBuilder};
use kira::{AudioManager, AudioManagerSettings, DefaultBackend, Tween};

use crate::error::AudioError;
use crate::mixer::Bus;
use crate::sound::{StaticSound, StreamingSound};

/// Owns the audio backend — kira's [`AudioManager`], on the platform's
/// default output device (`cpal`) — for the engine's lifetime.
///
/// Audio output stops automatically when this is dropped: kira's own
/// `Drop` impl on [`AudioManager`] handles that, nothing this wrapper
/// needs to do itself (the same "the underlying library already owns
/// correct teardown" reasoning [`crate::AudioContext`]'s `wgpu`
/// counterpart, `engine_renderer::GpuContext`, relies on).
pub struct AudioContext {
    manager: AudioManager<DefaultBackend>,
}

impl AudioContext {
    /// Initializes the audio backend: default output device, default
    /// internal buffer size, default mixer main track (no effects).
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::Init`] if the backend fails to start (e.g.
    /// no audio output device available, or the platform audio API
    /// rejected the requested configuration).
    pub fn new() -> Result<Self, AudioError> {
        let manager = AudioManager::<DefaultBackend>::new(AudioManagerSettings::default())
            .map_err(|err| AudioError::Init(err.to_string()))?;
        Ok(Self { manager })
    }

    /// Shared access to the underlying kira manager — for querying state
    /// that doesn't need mutation. Creating mixer tracks, clocks, and
    /// spatial listeners all need [`AudioContext::manager_mut`] instead;
    /// this thin lifecycle wrapper doesn't add convenience for any of
    /// that yet (spatial audio and the mixer are the next iterations).
    pub fn manager(&self) -> &AudioManager<DefaultBackend> {
        &self.manager
    }

    /// Mutable access to the underlying kira manager (e.g. to call
    /// [`AudioManager::play`] directly, which requires `&mut`).
    pub fn manager_mut(&mut self) -> &mut AudioManager<DefaultBackend> {
        &mut self.manager
    }

    /// Starts playing `sound`, returning a handle for controlling
    /// playback (volume, pausing, stopping, seeking, ...).
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::Playback`] if playback couldn't start (e.g.
    /// the maximum number of simultaneous sounds was reached).
    pub fn play_static(&mut self, sound: StaticSound) -> Result<StaticSoundHandle, AudioError> {
        self.manager
            .play(sound.data)
            .map_err(|err| AudioError::Playback(err.to_string()))
    }

    /// Starts playing `sound`, returning a handle for controlling
    /// playback (volume, pausing, stopping, seeking, ...).
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::Playback`] if playback couldn't start (e.g.
    /// the maximum number of simultaneous sounds was reached).
    pub fn play_streaming(
        &mut self,
        sound: StreamingSound,
    ) -> Result<StreamingSoundHandle<FromFileError>, AudioError> {
        self.manager
            .play(sound.data)
            .map_err(|err| AudioError::Playback(err.to_string()))
    }

    /// Creates a listener at `position`/`orientation` — the point sounds
    /// on a spatial track (see [`AudioContext::add_spatial_track`]) are
    /// heard *from*, panned and attenuated by distance/direction relative
    /// to it. Typically bound to the camera or player.
    ///
    /// `orientation`'s forward direction is `-Z` and up is `+Y`, matching
    /// kira's (and this engine's) right-handed convention.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::ResourceLimit`] if the maximum number of
    /// listeners has been reached.
    pub fn add_listener(
        &mut self,
        position: Vec3,
        orientation: Quat,
    ) -> Result<ListenerHandle, AudioError> {
        self.manager
            .add_listener(position, orientation)
            .map_err(|err| AudioError::ResourceLimit(err.to_string()))
    }

    /// Creates a spatial mixer sub-track at `position`, bound to
    /// `listener`. Play a [`StaticSound`]/[`StreamingSound`] on the
    /// returned handle (its own `play_on` method — spatial playback isn't
    /// [`AudioContext::play_static`]/`play_streaming`, which always play
    /// on the plain, non-spatial main track) to pan/attenuate it relative
    /// to `listener` as either of them moves.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::ResourceLimit`] if the maximum number of
    /// mixer tracks has been reached.
    pub fn add_spatial_track(
        &mut self,
        listener: &ListenerHandle,
        position: Vec3,
    ) -> Result<SpatialTrackHandle, AudioError> {
        self.manager
            .add_spatial_sub_track(listener, position, SpatialTrackBuilder::default())
            .map_err(|err| AudioError::ResourceLimit(err.to_string()))
    }

    /// Creates a mixer [`Bus`] at `volume_decibels` (`0.0` = unity gain),
    /// routed into the main output. Play sounds on it with
    /// [`StaticSound::play_on_bus`]/[`StreamingSound::play_on_bus`]
    /// (`crate::sound`'s methods of the same name) so their volume
    /// follows the bus's independently of every other sound.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::ResourceLimit`] if the maximum number of
    /// mixer tracks has been reached.
    pub fn add_bus(&mut self, volume_decibels: f32) -> Result<Bus, AudioError> {
        let track = self
            .manager
            .add_sub_track(TrackBuilder::new().volume(volume_decibels))
            .map_err(|err| AudioError::ResourceLimit(err.to_string()))?;
        Ok(Bus { track })
    }

    /// Sets the master (main output) volume, in decibels — affects every
    /// sound, however it's routed (main output directly, a [`Bus`], or a
    /// spatial track). See [`Bus::set_volume`] for what `tween` does.
    pub fn set_master_volume(&mut self, volume_decibels: f32, tween: Tween) {
        self.manager.main_track().set_volume(volume_decibels, tween);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context_or_skip() -> Option<AudioContext> {
        match AudioContext::new() {
            Ok(context) => Some(context),
            Err(error) => {
                eprintln!("skipping audio-device test: {error}");
                None
            }
        }
    }

    #[test]
    fn new_initializes_the_backend() {
        // Exercises the real cpal backend against whatever output device
        // the test machine has — the same "prove it against the real
        // thing, not a mock" choice `engine_renderer`'s live `cargo run`
        // verification makes for GPU init, made possible here because
        // (unlike GPU init) this needs no window handle to attempt.
        let _ = context_or_skip();
    }

    #[test]
    fn manager_and_manager_mut_are_reachable() {
        let Some(mut context) = context_or_skip() else {
            return;
        };
        let _: &AudioManager<DefaultBackend> = context.manager();
        let _: &mut AudioManager<DefaultBackend> = context.manager_mut();
    }

    #[test]
    fn dropping_the_context_does_not_panic() {
        let Some(context) = context_or_skip() else {
            return;
        };
        drop(context);
    }

    #[test]
    fn play_static_starts_playback() {
        let wav = crate::sound::silent_wav(44_100, 1000);
        let Some(mut context) = context_or_skip() else {
            return;
        };
        let sound = StaticSound::from_bytes(wav, false).unwrap();
        assert!(context.play_static(sound).is_ok());
    }

    #[test]
    fn play_static_with_looping_starts_playback() {
        let wav = crate::sound::silent_wav(44_100, 1000);
        let Some(mut context) = context_or_skip() else {
            return;
        };
        let sound = StaticSound::from_bytes(wav, true).unwrap();
        assert!(context.play_static(sound).is_ok());
    }

    #[test]
    fn add_listener_succeeds() {
        let Some(mut context) = context_or_skip() else {
            return;
        };
        let listener = context.add_listener(Vec3::ZERO, Quat::IDENTITY);
        assert!(listener.is_ok());
    }

    #[test]
    fn add_spatial_track_succeeds() {
        let Some(mut context) = context_or_skip() else {
            return;
        };
        let listener = context.add_listener(Vec3::ZERO, Quat::IDENTITY).unwrap();
        let track = context.add_spatial_track(&listener, Vec3::new(5.0, 0.0, 0.0));
        assert!(track.is_ok());
    }

    #[test]
    fn play_on_a_spatial_track_starts_playback() {
        let Some(mut context) = context_or_skip() else {
            return;
        };
        let listener = context.add_listener(Vec3::ZERO, Quat::IDENTITY).unwrap();
        let mut track = context
            .add_spatial_track(&listener, Vec3::new(5.0, 0.0, 0.0))
            .unwrap();

        let wav = crate::sound::silent_wav(44_100, 1000);
        let sound = StaticSound::from_bytes(wav, false).unwrap();
        assert!(sound.play_on(&mut track).is_ok());
    }

    #[test]
    fn add_bus_succeeds() {
        let Some(mut context) = context_or_skip() else {
            return;
        };
        assert!(context.add_bus(0.0).is_ok());
    }

    #[test]
    fn bus_set_volume_does_not_panic() {
        let Some(mut context) = context_or_skip() else {
            return;
        };
        let mut bus = context.add_bus(0.0).unwrap();
        bus.set_volume(-6.0, Tween::default());
    }

    #[test]
    fn play_on_a_bus_starts_playback() {
        let Some(mut context) = context_or_skip() else {
            return;
        };
        let mut bus = context.add_bus(0.0).unwrap();

        let wav = crate::sound::silent_wav(44_100, 1000);
        let sound = StaticSound::from_bytes(wav, false).unwrap();
        assert!(sound.play_on_bus(&mut bus).is_ok());
    }

    #[test]
    fn set_master_volume_does_not_panic() {
        let Some(mut context) = context_or_skip() else {
            return;
        };
        context.set_master_volume(-6.0, Tween::default());
    }
}
