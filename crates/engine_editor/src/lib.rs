//! # engine_editor
//!
//! The RustyEngine Studio editor, built on egui: the single-window shell
//! (menu bar, toolbar, status bar, dockable panels) plus hierarchy,
//! inspector, asset browser, scene view, console, gizmos, undo/redo,
//! play-mode, and a CPU profiler.
//!
//! ## Status
//!
//! [`EditorShell`] owns the egui context, the `egui-winit` event bridge,
//! and the `egui-wgpu` renderer, and [`EditorShell::run_frame`] draws
//! the studio shell: a menu bar, a toolbar (play/pause/stop, [`Workspace`]
//! switcher, gizmo mode, [`TransformSpace`], [`BuildConfig`]), a status
//! bar, and the panels — each shown per [`PanelVisibility`]. [`Viewport`]
//! renders a placeholder 3D scene (one lit cube per entity — not the
//! game demo's full pipeline) off-screen into the central Scene panel.
//! [`hierarchy`] is the left entity tree; [`inspector`] is the right
//! component editor (Inspector / Lighting tabs); [`scan_assets`] feeds
//! the Asset Browser; [`ConsoleLayer`] feeds the Console / Problems /
//! Output panel. The Code and AI Assistant panels are placeholders — the
//! Monaco embed and the agent are separate tasks. A translate/rotate/
//! scale gizmo (see the `gizmo` module) draws over the selection.

// So the `Inspectable` derive's `::engine_editor::…` paths resolve inside
// this crate too (not just downstream ones).
extern crate self as engine_editor;

mod assets;
mod chrome;
mod console;
mod diagnostics;
mod dirty;
mod error;
pub mod gizmo;
pub mod hierarchy;
mod history;
mod import;
pub mod inspect;
pub mod inspector;
mod output;
pub mod play;
pub mod prefab;
mod preview;
pub mod profiler;
mod search;
mod session;
mod shell;
mod state;
#[cfg(feature = "terminal")]
mod terminal;
mod theme;
mod viewport;

pub use assets::{
    AssetEntry, AssetIndex, AssetKind, resolve_ids as resolve_asset_ids, scan as scan_assets,
};
pub use chrome::{
    BottomTab, BuildConfig, EditorDimension, InspectorTab, PanelVisibility, TransformSpace,
    Workspace,
};
pub use console::{ConsoleFilter, ConsoleLayer, ConsoleLine, ConsoleLog, LevelFilter};
pub use diagnostics::{Diagnostic, Diagnostics, Severity};
pub use dirty::{DirtyState, PendingAction};
pub use import::{AssetImporter, ImportOutcome, ImportRecord, ImportStats, ImportedAsset};
pub use output::OutputLog;
pub use prefab::{PREFAB_DIR, write_prefab};
pub use preview::{
    AudioSummary, MeshSummary, PreviewCache, TextureSummary, audio_summary, human_bytes,
    mesh_summary, texture_summary, waveform_bins,
};
pub use search::{
    Jump as SearchJump, MAX_TEXT_HITS, SearchMode, SearchQuery, SearchState, SymbolHit, TextHit,
    TextResults, run_text_search, symbol_kind_label,
};

pub use egui_wgpu::ScreenDescriptor;
pub use error::EditorError;
pub use gizmo::{Axis, GizmoMode};
pub use history::History;
pub use inspect::{InspectField, Inspectable};
pub use play::{EditorMode, PlayState};
pub use profiler::{FrameProfiler, FrameSample};
pub use session::{EditorSession, SESSION_FILE, SESSION_VERSION};
pub use shell::EditorShell;
pub use state::{AUTOSAVE_FILE, DEFAULT_SCENE_PATH, EditorState, recovery_candidate};
#[cfg(feature = "terminal")]
pub use terminal::{Terminal, TerminalError};
pub use viewport::Viewport;
