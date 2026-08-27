//! # engine_audio
//!
//! Audio subsystem via kira: music, sound effects, spatial audio, and mixing.
//!
//! ## Status
//!
//! Milestone 8 complete: [`AudioContext`] owns kira's `AudioManager` (the
//! platform's default output device via `cpal`) for the engine's
//! lifetime, with a validated [`AudioContext::new`] entry point.
//! [`StaticSound`] (fully decoded, for short SFX) and [`StreamingSound`]
//! (decoded on demand, for music/long tracks) load audio data (WAV, OGG,
//! MP3, FLAC — kira's own `symphonia` decoder) from bytes or a file, and
//! [`AudioContext::play_static`]/[`AudioContext::play_streaming`] start
//! them playing, returning kira's own handle types for volume/pause/
//! stop/seek control. [`AudioContext::add_listener`]/
//! [`AudioContext::add_spatial_track`] plus [`StaticSound::play_on`]/
//! [`StreamingSound::play_on`] give sounds a 3D position, panned and
//! attenuated relative to a listener (`glam::Vec3`/`Quat` throughout —
//! kira's own `mint` interop makes this a direct call, no conversion
//! layer, the same story `engine_physics` has with rapier).
//! [`AudioContext::add_bus`] creates a [`Bus`] (independent volume
//! control, e.g. separate Music/SFX sliders) — [`StaticSound::play_on_bus`]/
//! [`StreamingSound::play_on_bus`] route to one; [`AudioContext::set_master_volume`]
//! controls everything at once. [`AudioContext::manager`]/
//! [`AudioContext::manager_mut`] still expose everything kira itself
//! offers beyond this — effects, clocks, modulators, and more.

mod context;
mod error;
mod mixer;
mod sound;

pub use context::AudioContext;
pub use error::AudioError;
pub use kira::Tween;
pub use kira::listener::ListenerHandle;
pub use kira::track::SpatialTrackHandle;
pub use mixer::Bus;
pub use sound::{StaticSound, StreamingSound};
