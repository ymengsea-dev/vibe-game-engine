//! # engine_editor
//!
//! Visual editor built on egui: hierarchy, inspector, asset browser, scene view, console, and profiler.
//!
//! ## Status
//!
//! Milestone 9 in progress: [`EditorShell`] owns the egui context, the
//! `egui-winit` event bridge, and the `egui-wgpu` renderer — the
//! foundational "shell" every panel gets built inside. [`Viewport`]
//! renders a placeholder 3D scene (one lit cube — not the standalone
//! game demo's full pipeline) off-screen and registers it with egui,
//! proving the render-to-texture-then-display-in-a-panel mechanism
//! works; [`EditorShell::run_frame`] shows it in a "Scene View" panel.
//! [`hierarchy`] draws a left-side panel listing an ECS `World`'s
//! entities as a parent/child tree and tracks which one is selected.
//! [`inspector`] draws a right-side panel editing the selected entity's
//! `Name`/`Transform`. [`scan_assets`] lists files under a project's
//! assets directory, shown in a bottom panel. [`ConsoleLayer`] captures
//! `tracing` events into a [`ConsoleLog`], shown in another bottom
//! panel. A translate gizmo (see the `gizmo` module) draws over the
//! selected entity in the Scene View, draggable along an axis.

mod assets;
mod console;
mod error;
mod gizmo;
pub mod hierarchy;
pub mod inspector;
mod shell;
mod state;
mod viewport;

pub use assets::{AssetEntry, AssetKind, scan as scan_assets};
pub use console::{ConsoleLayer, ConsoleLine, ConsoleLog};

pub use egui_wgpu::ScreenDescriptor;
pub use error::EditorError;
pub use shell::EditorShell;
pub use state::EditorState;
pub use viewport::Viewport;
