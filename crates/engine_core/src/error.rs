//! Error types produced by `engine_core`.

use thiserror::Error;

/// Errors that can occur while configuring or driving the engine lifecycle.
///
/// Marked `#[non_exhaustive]` so later milestones can add variants (e.g.
/// subsystem initialization failures) without breaking downstream matches.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum EngineError {
    /// The supplied [`crate::config::EngineConfig`] failed validation.
    #[error("invalid engine config: {0}")]
    InvalidConfig(String),

    /// [`crate::app::App::run`] was called while the app was already
    /// running (or past running).
    #[error("app is already running")]
    AlreadyRunning,

    /// [`crate::app::App::shutdown`] was called after the app had already
    /// stopped.
    #[error("app is already stopped")]
    AlreadyStopped,

    /// [`crate::logging::init_default`] failed to install the global
    /// `tracing` subscriber (most commonly: one was already installed).
    #[error("failed to initialize logging: {0}")]
    LoggingInit(String),

    /// A [`crate::FixedTimestep`] constructor was given a step rate or
    /// step length that is not finite and positive.
    #[error("invalid fixed timestep: {0} (must be finite and positive)")]
    InvalidTimestep(f32),
}
