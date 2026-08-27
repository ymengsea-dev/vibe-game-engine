//! Mixer buses: named sub-tracks with independent volume control, routed
//! into the main output.

use kira::Tween;
use kira::sound::static_sound::StaticSoundHandle;
use kira::sound::streaming::StreamingSoundHandle;
use kira::track::TrackHandle;

use crate::error::AudioError;
use crate::sound::{StaticSound, StreamingSound};

/// A mixer bus: a named group of sounds with independent volume control,
/// routed into the main output.
///
/// Typical use: separate "Music" and "SFX" buses so a game can offer a
/// player-facing volume slider for each independently, without touching
/// individual sound handles one at a time. A thin wrapper over kira's own
/// [`TrackHandle`] — reach it via [`Bus::track_mut`] for anything this
/// wrapper doesn't add convenience for (effects, further sub-routing,
/// ...).
pub struct Bus {
    pub(crate) track: TrackHandle,
}

impl Bus {
    /// Sets this bus's volume, in decibels (`0.0` = unity gain/unchanged,
    /// negative attenuates, very negative effectively mutes — there's no
    /// hard mute switch, silence is just a large enough negative value).
    ///
    /// `tween` controls how the change is applied over time —
    /// [`Tween::default`] ramps linearly over 10ms (near-instant, avoids
    /// an audible click a truly instant jump can cause).
    pub fn set_volume(&mut self, volume_decibels: f32, tween: Tween) {
        self.track.set_volume(volume_decibels, tween);
    }

    /// Direct access to the underlying kira track handle, for anything
    /// this wrapper doesn't add convenience for.
    pub fn track_mut(&mut self) -> &mut TrackHandle {
        &mut self.track
    }
}

impl StaticSound {
    /// Starts playing this sound on `bus` — its volume follows `bus`'s
    /// (see [`Bus::set_volume`]) on top of its own, instead of routing
    /// straight to the main output like
    /// [`crate::AudioContext::play_static`] does.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::Playback`] if playback couldn't start (e.g.
    /// the maximum number of simultaneous sounds was reached).
    pub fn play_on_bus(self, bus: &mut Bus) -> Result<StaticSoundHandle, AudioError> {
        bus.track
            .play(self.data)
            .map_err(|err| AudioError::Playback(err.to_string()))
    }
}

impl StreamingSound {
    /// Starts playing this sound on `bus` — see
    /// [`StaticSound::play_on_bus`] for how bus playback differs from
    /// [`crate::AudioContext::play_streaming`]'s direct-to-main-output
    /// playback.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::Playback`] if playback couldn't start (e.g.
    /// the maximum number of simultaneous sounds was reached).
    pub fn play_on_bus(
        self,
        bus: &mut Bus,
    ) -> Result<StreamingSoundHandle<kira::sound::FromFileError>, AudioError> {
        bus.track
            .play(self.data)
            .map_err(|err| AudioError::Playback(err.to_string()))
    }
}
