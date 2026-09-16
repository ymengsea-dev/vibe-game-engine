//! Studio chrome: the value types behind the menu bar, toolbar, and
//! status bar — workspaces and their panel-visibility presets, the
//! build-configuration and transform-space selectors, and the tab
//! selections for the multi-tab panels.
//!
//! Everything here is a small `Copy` enum or a plain struct of `bool`s
//! with pure mapping functions, so the shell's egui code stays free of
//! branching logic and this module can be unit-tested on its own.
//!
//! [`Workspace`] and [`PanelVisibility`] also derive `serde` so the
//! editor session (see [`crate::EditorSession`]) can persist them.

use serde::{Deserialize, Serialize};

/// A named layout of the studio's panels, switched from the toolbar.
///
/// Switching a workspace changes only [`PanelVisibility`] — never the
/// scene, the ECS world, or the undo history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Workspace {
    /// Full scene-authoring layout: Hierarchy, Scene, Asset Browser,
    /// Code, Console, and Inspector. The default, matching the UI
    /// reference.
    #[default]
    World,
    /// Code-focused: drops the Inspector, keeps Hierarchy, Asset
    /// Browser, Code, and Console.
    Code,
    /// AI-focused: `World` plus the AI Assistant dock.
    Ai,
    /// Diagnostics-focused: Scene view plus Console; Code and the Asset
    /// Browser are hidden.
    Debug,
}

impl Workspace {
    /// This workspace's position in [`Workspace::ALL`] — a stable index
    /// for `[T; 4]` per-workspace state.
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|w| *w == self).unwrap_or(0)
    }

    /// Every variant, in the order the toolbar switcher shows them.
    pub const ALL: [Workspace; 4] = [
        Workspace::World,
        Workspace::Code,
        Workspace::Ai,
        Workspace::Debug,
    ];

    /// Short label for the toolbar switcher.
    pub fn label(self) -> &'static str {
        match self {
            Workspace::World => "World",
            Workspace::Code => "Code",
            Workspace::Ai => "AI",
            Workspace::Debug => "Debug",
        }
    }

    /// The panel-visibility preset this workspace applies.
    pub fn visibility(self) -> PanelVisibility {
        match self {
            Workspace::World => PanelVisibility {
                hierarchy: true,
                inspector: true,
                asset_browser: true,
                code: true,
                ai: false,
                console: true,
                profiler: false,
            },
            // AI workspace: like World, plus the AI Assistant dock.
            Workspace::Ai => PanelVisibility {
                hierarchy: true,
                inspector: true,
                asset_browser: true,
                code: true,
                ai: true,
                console: true,
                profiler: false,
            },
            Workspace::Code => PanelVisibility {
                hierarchy: true,
                inspector: false,
                asset_browser: true,
                code: true,
                ai: false,
                console: true,
                profiler: false,
            },
            // Debug workspace: Scene + Console + the CPU profiler strip.
            Workspace::Debug => PanelVisibility {
                hierarchy: true,
                inspector: true,
                asset_browser: false,
                code: false,
                ai: false,
                console: true,
                profiler: true,
            },
        }
    }
}

/// Which of the studio's toggleable panels are currently shown.
///
/// The Scene/Game viewport and the menu/tool/status bars are always
/// present, so they are not tracked here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelVisibility {
    /// Left-side entity tree.
    pub hierarchy: bool,
    /// Right-side component editor (the Inspector / Lighting panel).
    pub inspector: bool,
    /// Bottom project-asset browser.
    pub asset_browser: bool,
    /// Bottom Rust code editor panel.
    pub code: bool,
    /// Right-side AI assistant panel (the `Ai` preset shows it).
    pub ai: bool,
    /// Bottom Console / Problems / Output panel.
    pub console: bool,
    /// Bottom CPU-frame profiler strip (the `Debug` preset shows it).
    pub profiler: bool,
}

impl Default for PanelVisibility {
    fn default() -> Self {
        Workspace::World.visibility()
    }
}

/// The gizmo coordinate space shown in the toolbar's transform-space
/// selector.
///
/// Display-only for now — the Scene View gizmo still operates on world
/// axes regardless of this value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TransformSpace {
    /// World axes.
    #[default]
    Global,
    /// The selected entity's local axes.
    Local,
}

impl TransformSpace {
    /// Both variants, in toolbar order.
    pub const ALL: [TransformSpace; 2] = [TransformSpace::Global, TransformSpace::Local];

    /// Label for the toolbar selector.
    pub fn label(self) -> &'static str {
        match self {
            TransformSpace::Global => "Global",
            TransformSpace::Local => "Local",
        }
    }
}

/// Whether the editor is authoring in 3D (perspective viewport, X/Y/Z
/// gizmo) or 2D (orthographic front view, gizmo constrained to the
/// screen plane — translate/scale in X-Y, rotate about Z).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EditorDimension {
    /// Perspective camera, all three gizmo axes.
    #[default]
    Mode3D,
    /// Orthographic front view, 2D gizmo conventions.
    Mode2D,
}

impl EditorDimension {
    /// Both variants, in toolbar order.
    pub const ALL: [EditorDimension; 2] = [EditorDimension::Mode3D, EditorDimension::Mode2D];

    /// Label for the toolbar selector.
    pub fn label(self) -> &'static str {
        match self {
            EditorDimension::Mode3D => "3D",
            EditorDimension::Mode2D => "2D",
        }
    }

    /// Whether this is the 2D authoring mode.
    pub fn is_2d(self) -> bool {
        matches!(self, EditorDimension::Mode2D)
    }
}

/// Which tab the bottom Console / Problems / Output / Terminal panel is
/// showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BottomTab {
    /// Captured engine/editor log lines.
    #[default]
    Console,
    /// Compiler and language-server diagnostics.
    Problems,
    /// Build and task output.
    Output,
    /// An interactive shell (only backed by a PTY in builds with the
    /// `terminal` feature — otherwise the tab shows a notice).
    Terminal,
    /// Project-wide text / symbol search.
    Search,
}

impl BottomTab {
    /// Every tab, left to right.
    pub const ALL: [BottomTab; 5] = [
        BottomTab::Console,
        BottomTab::Problems,
        BottomTab::Output,
        BottomTab::Terminal,
        BottomTab::Search,
    ];

    /// Tab label.
    pub fn label(self) -> &'static str {
        match self {
            BottomTab::Console => "Console",
            BottomTab::Problems => "Problems",
            BottomTab::Output => "Output",
            BottomTab::Terminal => "Terminal",
            BottomTab::Search => "Search",
        }
    }
}

/// Which tab the right-side Inspector / Lighting panel is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InspectorTab {
    /// The selected entity's components.
    #[default]
    Inspector,
    /// Scene lighting and environment settings. Stub — no controls yet.
    Lighting,
}

impl InspectorTab {
    /// Every tab, left to right.
    pub const ALL: [InspectorTab; 2] = [InspectorTab::Inspector, InspectorTab::Lighting];

    /// Tab label.
    pub fn label(self) -> &'static str {
        match self {
            InspectorTab::Inspector => "Inspector",
            InspectorTab::Lighting => "Lighting",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn world_workspace_shows_the_wired_panels() {
        let v = Workspace::World.visibility();
        assert!(v.hierarchy && v.inspector && v.asset_browser && v.code && v.console);
    }

    #[test]
    fn ai_and_profiler_panels_appear_only_in_their_own_presets() {
        assert!(
            Workspace::Ai.visibility().ai,
            "AI workspace shows the AI dock"
        );
        assert!(
            Workspace::Debug.visibility().profiler,
            "Debug workspace shows the profiler"
        );
        for workspace in [Workspace::World, Workspace::Code] {
            let v = workspace.visibility();
            assert!(!v.ai && !v.profiler, "{workspace:?} keeps them hidden");
        }
    }

    #[test]
    fn default_visibility_matches_the_world_workspace() {
        assert_eq!(PanelVisibility::default(), Workspace::World.visibility());
    }

    #[test]
    fn workspace_presets_match_their_documented_shape() {
        let code = Workspace::Code.visibility();
        assert!(!code.inspector);
        assert!(code.code && code.console && code.hierarchy);

        let ai = Workspace::Ai.visibility();
        assert!(ai.ai && ai.hierarchy && ai.inspector && ai.code);

        let debug = Workspace::Debug.visibility();
        assert!(!debug.code && !debug.asset_browser);
        assert!(debug.console && debug.profiler);
    }

    #[test]
    fn all_arrays_cover_every_variant_with_unique_non_empty_labels() {
        fn check<T: Copy>(all: &[T], label: impl Fn(T) -> &'static str) {
            let labels: Vec<&str> = all.iter().map(|&v| label(v)).collect();
            assert!(labels.iter().all(|l| !l.is_empty()), "no empty labels");
            for (i, a) in labels.iter().enumerate() {
                for b in &labels[i + 1..] {
                    assert_ne!(a, b, "labels must be unique");
                }
            }
        }
        check(&Workspace::ALL, Workspace::label);
        check(&TransformSpace::ALL, TransformSpace::label);
        check(&BottomTab::ALL, BottomTab::label);
        check(&InspectorTab::ALL, InspectorTab::label);
        check(&EditorDimension::ALL, EditorDimension::label);
    }

    #[test]
    fn selector_defaults_are_the_first_variant() {
        assert_eq!(Workspace::default(), Workspace::ALL[0]);
        assert_eq!(TransformSpace::default(), TransformSpace::ALL[0]);
        assert_eq!(BottomTab::default(), BottomTab::ALL[0]);
        assert_eq!(InspectorTab::default(), InspectorTab::ALL[0]);
        assert_eq!(EditorDimension::default(), EditorDimension::ALL[0]);
        assert!(!EditorDimension::Mode3D.is_2d());
        assert!(EditorDimension::Mode2D.is_2d());
    }
}
