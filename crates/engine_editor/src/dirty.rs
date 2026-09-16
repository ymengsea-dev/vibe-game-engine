//! Unsaved-change tracking.
//!
//! [`DirtyState`] records unsaved work per source — the scene, project
//! settings, and open text documents — so the status bar, the window
//! title, and the close guard can all ask one question: [`DirtyState::any`].
//!
//! Only the scene has a producer today; [`DirtyState::mark_settings`] and
//! [`DirtyState::mark_document`] are the slots the settings editor and the
//! code editor plug into later.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Which of the editor's savable surfaces have unsaved changes.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DirtyState {
    scene: bool,
    settings: bool,
    documents: BTreeSet<PathBuf>,
}

impl DirtyState {
    /// Whether anything at all is unsaved.
    pub fn any(&self) -> bool {
        self.scene || self.settings || !self.documents.is_empty()
    }

    /// Whether the scene has unsaved edits.
    pub fn scene(&self) -> bool {
        self.scene
    }

    /// Whether project settings have unsaved edits.
    pub fn settings(&self) -> bool {
        self.settings
    }

    /// Marks the scene as having unsaved edits.
    pub fn mark_scene(&mut self) {
        self.scene = true;
    }

    /// Clears the scene's unsaved flag — call after a successful save or
    /// load.
    pub fn clear_scene(&mut self) {
        self.scene = false;
    }

    /// Marks project settings as having unsaved edits.
    pub fn mark_settings(&mut self) {
        self.settings = true;
    }

    /// Clears the project-settings unsaved flag.
    pub fn clear_settings(&mut self) {
        self.settings = false;
    }

    /// Marks the document at `path` as having unsaved edits.
    pub fn mark_document(&mut self, path: impl Into<PathBuf>) {
        self.documents.insert(path.into());
    }

    /// Clears `path`'s unsaved flag — call after saving or closing it.
    pub fn clear_document(&mut self, path: &Path) {
        self.documents.remove(path);
    }

    /// The unsaved documents, in sorted path order.
    pub fn dirty_documents(&self) -> impl Iterator<Item = &Path> {
        self.documents.iter().map(PathBuf::as_path)
    }

    /// A short human summary of what's unsaved, for the status bar and
    /// the close-guard dialog — `None` when everything is saved.
    ///
    /// Examples: `"scene"`, `"scene + 2 files"`, `"settings + 1 file"`.
    pub fn summary(&self) -> Option<String> {
        if !self.any() {
            return None;
        }
        let mut parts: Vec<String> = Vec::new();
        if self.scene {
            parts.push("scene".to_string());
        }
        if self.settings {
            parts.push("settings".to_string());
        }
        let count = self.documents.len();
        if count > 0 {
            parts.push(format!("{count} file{}", if count == 1 { "" } else { "s" }));
        }
        Some(parts.join(" + "))
    }
}

/// A user action deferred until the "unsaved changes" dialog resolves.
///
/// `Clone` rather than `Copy` since the project actions carry a path:
/// the alternative was a payload-free enum plus a parallel "and here is
/// the path" field, which can go out of sync with the action it belongs
/// to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingAction {
    /// Close the editor.
    Quit,
    /// Replace the scene with a fresh empty one.
    NewScene,
    /// Reload the current scene from disk, discarding in-memory edits.
    RevertScene,
    /// Open an existing project at this directory.
    OpenProject(PathBuf),
    /// Create a project in this directory, then open it.
    NewProject(PathBuf),
}

impl PendingAction {
    /// Every payload-free variant, for exhaustiveness in tests. The
    /// project actions are excluded because they carry a path with no
    /// meaningful sample value.
    pub const ALL: [PendingAction; 3] = [
        PendingAction::Quit,
        PendingAction::NewScene,
        PendingAction::RevertScene,
    ];

    /// The action phrased for the dialog: "Save changes before {verb}?"
    pub fn verb(&self) -> &'static str {
        match self {
            PendingAction::Quit => "closing",
            PendingAction::NewScene => "starting a new scene",
            PendingAction::RevertScene => "reverting the scene",
            PendingAction::OpenProject(_) => "opening another project",
            PendingAction::NewProject(_) => "creating a new project",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_clean() {
        let dirty = DirtyState::default();
        assert!(!dirty.any());
        assert!(dirty.summary().is_none());
    }

    #[test]
    fn each_source_flips_any_independently() {
        let mut dirty = DirtyState::default();

        dirty.mark_scene();
        assert!(dirty.any() && dirty.scene());
        dirty.clear_scene();
        assert!(!dirty.any());

        dirty.mark_settings();
        assert!(dirty.any() && dirty.settings());
        dirty.clear_settings();
        assert!(!dirty.any());

        dirty.mark_document("src/player.rs");
        assert!(dirty.any());
        dirty.clear_document(Path::new("src/player.rs"));
        assert!(!dirty.any());
    }

    #[test]
    fn dirty_documents_are_sorted_and_deduplicated() {
        let mut dirty = DirtyState::default();
        dirty.mark_document("src/b.rs");
        dirty.mark_document("src/a.rs");
        dirty.mark_document("src/b.rs"); // duplicate

        let paths: Vec<&Path> = dirty.dirty_documents().collect();
        assert_eq!(paths, [Path::new("src/a.rs"), Path::new("src/b.rs")]);
    }

    #[test]
    fn summary_wording() {
        let mut dirty = DirtyState::default();
        assert_eq!(dirty.summary(), None);

        dirty.mark_scene();
        assert_eq!(dirty.summary().as_deref(), Some("scene"));

        dirty.mark_document("a.rs");
        assert_eq!(dirty.summary().as_deref(), Some("scene + 1 file"));

        dirty.mark_document("b.rs");
        dirty.mark_settings();
        assert_eq!(
            dirty.summary().as_deref(),
            Some("scene + settings + 2 files")
        );

        dirty.clear_scene();
        assert_eq!(dirty.summary().as_deref(), Some("settings + 2 files"));
    }

    #[test]
    fn pending_action_verbs_cover_every_variant() {
        for action in PendingAction::ALL {
            assert!(!action.verb().is_empty());
        }
    }
}
