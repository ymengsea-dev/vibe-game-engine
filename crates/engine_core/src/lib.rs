//! # engine_core
//!
//! Engine foundation: startup, main loop, timing, configuration, and global services.
//!
//! ## Status
//!
//! Milestone 1 in progress. Implemented so far: [`App`] lifecycle skeleton
//! (config, error types, run/shutdown). Timing, logging, and global
//! services land in later Milestone 1 iterations.

mod app;
mod config;
mod error;
pub mod logging;

pub use app::{App, AppState};
pub use config::EngineConfig;
pub use error::EngineError;
