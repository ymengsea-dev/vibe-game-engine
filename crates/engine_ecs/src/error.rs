//! Error types produced by `engine_ecs`.

use thiserror::Error;

/// Errors that can occur while driving the ECS.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum EcsError {
    /// [`crate::Ecs::run_stage`] was called with a stage label that has no
    /// registered schedule. Shouldn't happen for [`crate::stages::Update`]
    /// or [`crate::stages::RenderExtract`] (both are registered by
    /// [`crate::Ecs::new`]) — only for a custom label the caller never
    /// registered via `world_mut().add_schedule(...)`.
    #[error("no schedule registered for stage {0}")]
    UnknownStage(String),
}
