//! # engine_platform
//!
//! Platform layer: window creation, keyboard/mouse input, monitors, and OS integration via winit.
//!
//! ## Status
//!
//! Milestone 1 in progress. Implemented so far: single-window creation and
//! event-loop ownership ([`run_windowed`]), keyboard/mouse input state
//! ([`InputState`]). Stage 2 "Input action-mapping layer": [`ActionMap`]
//! binds raw [`Binding`]s (keys, mouse buttons) to caller-defined named
//! actions, queried against an [`InputState`] — gameplay code depends on
//! action names, not physical keys, so rebinding never touches it.

mod actions;
mod app;
mod config;
mod error;
mod event;
mod input;

pub use actions::{ActionMap, Binding};
pub use app::{PlatformHandler, Window, run_windowed};
pub use config::WindowConfig;
pub use error::PlatformError;
pub use event::{KeyCode, MouseButton, PlatformEvent};
pub use input::InputState;
/// Winit's raw window event type, re-exported for
/// [`PlatformHandler::on_raw_window_event`] implementors that need it
/// directly (e.g. `engine_editor`'s egui integration) — everyone else
/// should prefer [`PlatformEvent`].
pub use winit::event::WindowEvent;
