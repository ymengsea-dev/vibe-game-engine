//! # engine_core
//!
//! Engine foundation: startup, main loop, timing, configuration, and global services.
//!
//! ## Status
//!
//! Milestone 1 in progress. Implemented so far: [`App`] lifecycle skeleton
//! (config, error types, run/shutdown) and [`FixedTimestep`], the
//! fixed-step accumulator physics (and any other reproducible-rate
//! subsystem) drives its update loop from instead of the variable
//! per-frame delta. Logging and global services land in later Milestone 1
//! iterations.

mod app;
mod config;
mod error;
pub mod logging;
mod time;

pub use app::{App, AppState};
pub use config::EngineConfig;
pub use error::EngineError;
pub use time::FixedTimestep;
