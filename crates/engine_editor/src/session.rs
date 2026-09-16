//! Editor session: per-project, editor-only state persisted under the
//! project's `.studio/` directory so a runtime or exported build never
//! reads it.
//!
//! Restored on project open: the active [`Workspace`], the current
//! [`PanelVisibility`], and the last scene that was open.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::assets::AssetView;
use crate::chrome::{PanelVisibility, Workspace};
use crate::error::EditorError;
use crate::overlays::OverlayToggles;

/// On-disk format version for `.studio/session.ron`.
pub const SESSION_VERSION: u32 = 1;

/// The session file's name inside a project's `.studio/` directory.
pub const SESSION_FILE: &str = "session.ron";

/// Editor-only state restored when a project reopens.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditorSession {
    /// Format version — see [`SESSION_VERSION`].
    pub version: u32,
    /// The workspace preset the editor was last in.
    pub workspace: Workspace,
    /// Panel visibility as the user last left it — may differ from the
    /// workspace's own preset.
    pub panels: PanelVisibility,
    /// Per-workspace panel layouts, indexed by [`Workspace::index`].
    /// Empty (from an older session) means "use each workspace's
    /// preset"; a short/long vector is padded/truncated on load.
    #[serde(default)]
    pub layouts: Vec<PanelVisibility>,
    /// The scene that was open, relative to the project root. `None`
    /// falls back to the project's `main_scene`.
    pub last_scene: Option<PathBuf>,
    /// How the Asset Browser was last grouping its rows. Additive, like
    /// `layouts`: a session written before this field existed loads with
    /// the default view rather than failing.
    #[serde(default)]
    pub asset_view: AssetView,
    /// Whether the Asset Browser preview strip is visible.
    #[serde(default = "default_preview_open")]
    pub preview_open: bool,
    /// Which build configuration the toolbar had selected, as an index
    /// into the project's list. Additive; clamped on load, since the
    /// project's configurations can change under it.
    #[serde(default)]
    pub configuration_index: usize,
    /// Which Scene-view debug overlays were on. Additive; an older
    /// session loads with them all off.
    #[serde(default)]
    pub overlays: OverlayToggles,
}

impl Default for EditorSession {
    fn default() -> Self {
        Self {
            version: SESSION_VERSION,
            workspace: Workspace::default(),
            panels: PanelVisibility::default(),
            layouts: Vec::new(),
            last_scene: None,
            asset_view: AssetView::default(),
            preview_open: true,
            configuration_index: 0,
            overlays: OverlayToggles::default(),
        }
    }
}

fn default_preview_open() -> bool {
    true
}

impl EditorSession {
    /// Reads a session from `path`. A missing file, an unreadable file,
    /// invalid RON, or a version newer than this build all fall back to
    /// [`EditorSession::default`] (with a warning for anything but a
    /// plain missing file) — a broken session must never stop a project
    /// from opening.
    pub fn load(path: &Path) -> Self {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) => {
                if err.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(
                        path = %path.display(),
                        error = %err,
                        "could not read editor session; using defaults"
                    );
                }
                return Self::default();
            }
        };

        match ron::from_str::<EditorSession>(&text) {
            Ok(session) if session.version <= SESSION_VERSION => session,
            Ok(session) => {
                tracing::warn!(
                    path = %path.display(),
                    found = session.version,
                    max = SESSION_VERSION,
                    "editor session is from a newer build; using defaults"
                );
                Self::default()
            }
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "invalid editor session; using defaults"
                );
                Self::default()
            }
        }
    }

    /// Writes this session to `path` as pretty RON, creating the parent
    /// directory if it doesn't exist.
    ///
    /// # Errors
    ///
    /// [`EditorError::Session`] if the parent can't be created, if
    /// serialization fails, or if the write fails.
    pub fn save(&self, path: &Path) -> Result<(), EditorError> {
        let fail = |message: String| EditorError::Session {
            path: path.to_path_buf(),
            message,
        };

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| fail(err.to_string()))?;
        }
        let text = ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
            .map_err(|err| fail(err.to_string()))?;
        std::fs::write(path, text).map_err(|err| fail(err.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_path(tag: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("vge_session_{tag}_{nanos}_{n}.ron"))
    }

    #[test]
    fn default_is_the_world_workspace_with_no_last_scene() {
        let session = EditorSession::default();
        assert_eq!(session.version, SESSION_VERSION);
        assert_eq!(session.workspace, Workspace::World);
        assert_eq!(session.panels, PanelVisibility::default());
        assert!(session.last_scene.is_none());
    }

    #[test]
    fn save_then_load_round_trips() {
        let path = temp_path("roundtrip");
        let session = EditorSession {
            workspace: Workspace::Debug,
            panels: PanelVisibility {
                ai: true,
                ..PanelVisibility::default()
            },
            last_scene: Some(PathBuf::from("scenes/level2.ron")),
            ..EditorSession::default()
        };

        session.save(&path).expect("save");
        let loaded = EditorSession::load(&path);
        assert_eq!(loaded, session);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_missing_file_is_default() {
        let path = temp_path("missing");
        assert_eq!(EditorSession::load(&path), EditorSession::default());
    }

    #[test]
    fn load_garbage_is_default() {
        let path = temp_path("garbage");
        std::fs::write(&path, "not ron at all {{{").unwrap();
        assert_eq!(EditorSession::load(&path), EditorSession::default());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_future_version_is_default() {
        let path = temp_path("future");
        let session = EditorSession {
            version: SESSION_VERSION + 1,
            ..EditorSession::default()
        };
        session.save(&path).expect("save");
        assert_eq!(EditorSession::load(&path), EditorSession::default());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_creates_a_missing_parent_directory() {
        let dir = temp_path("mkdir").with_extension("");
        let nested = dir.join("deep").join("session.ron");
        EditorSession::default()
            .save(&nested)
            .expect("save into new dirs");
        assert!(nested.is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
