//! # engine
//!
//! Facade crate for the Vibe Game Engine (VGE).
//!
//! Game projects depend on this single crate and access every subsystem
//! through it, so internal crate boundaries can evolve without breaking
//! games:
//!
//! ```
//! use engine::prelude::*;
//! ```

pub use engine_animation as animation;
pub use engine_asset as asset;
pub use engine_audio as audio;
pub use engine_core as core;
pub use engine_ecs as ecs;
pub use engine_editor as editor;
pub use engine_network as network;
pub use engine_physics as physics;
pub use engine_platform as platform;
pub use engine_renderer as renderer;
pub use engine_scene as scene;
pub use engine_scripting as scripting;
pub use engine_ui as ui;
pub use engine_utils as utils;

pub use engine_ai as ai;

/// Commonly used engine types, re-exported for convenient glob imports.
///
/// Populated as subsystems are implemented milestone by milestone.
pub mod prelude {}
