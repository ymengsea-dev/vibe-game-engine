//! Errors produced by `engine_audio`.

use thiserror::Error;

/// Errors that can occur while driving the audio backend.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AudioError {
    /// [`crate::AudioContext::new`] failed to start the audio backend
    /// (e.g. no output device available, or the platform audio API
    /// rejected the requested configuration).
    #[error("failed to initialize audio backend: {0}")]
    Init(String),

    /// [`crate::StaticSound::from_bytes`]/`from_file` or
    /// [`crate::StreamingSound::from_file`] failed to decode audio data —
    /// treat all externally sourced audio bytes/files as untrusted.
    #[error("failed to decode audio: {0}")]
    Decode(String),

    /// [`crate::AudioContext::play_static`]/`play_streaming` failed to
    /// start playback (e.g. the maximum number of simultaneous sounds was
    /// reached).
    #[error("failed to play sound: {0}")]
    Playback(String),

    /// [`crate::AudioContext::add_listener`]/`add_spatial_track` failed
    /// because the maximum capacity for that resource has been reached.
    #[error("failed to create audio resource: {0}")]
    ResourceLimit(String),
}
