//! Error types produced by `engine_platform`.

use thiserror::Error;

/// Errors that can occur while creating or running a platform window.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PlatformError {
    /// Failed to create the OS event loop.
    #[error("failed to create event loop: {0}")]
    EventLoop(String),

    /// Failed to create the OS window.
    #[error("failed to create window: {0}")]
    WindowCreation(String),
}
