//! Switching the studio from one project to another.
//!
//! ## Why this is not one function
//!
//! Opening a project touches things the editor state does not own. The
//! world, the asset index and the import cache live in
//! [`crate::EditorState`]; the GPU resources, the language server and the
//! code editor live in the shell binary. A swap has to reset all of
//! them, in that order, or the new project renders the old project's
//! meshes.
//!
//! So a menu item does not swap anything. It records a
//! [`ProjectRequest`], and the binary — which can see both halves —
//! carries it out on the next frame. The alternative was passing the GPU
//! context into the menu code, which would put rendering resources in
//! reach of every panel that draws a button.
//!
//! ## Recent projects
//!
//! [`RecentProjects`] is a small most-recently-used list, kept in the
//! user's config directory rather than in the project (a list of
//! projects cannot live inside one of them).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::EditorError;

/// How many projects the recent list keeps.
///
/// Ten is enough to cover what a person switches between and short
/// enough to stay a menu rather than a browser.
pub const MAX_RECENT_PROJECTS: usize = 10;

/// A pending switch to another project, recorded by the File menu and
/// carried out by the shell binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRequest {
    /// The project directory.
    pub root: PathBuf,
    /// Whether to scaffold it first. `false` requires it to exist
    /// already.
    pub create: bool,
}

impl ProjectRequest {
    /// Open an existing project at `root`.
    pub fn open(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            create: false,
        }
    }

    /// Create a project at `root`, then open it.
    pub fn create(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            create: true,
        }
    }
}

/// The most recently opened projects, newest first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentProjects {
    /// Project roots, newest first, at most [`MAX_RECENT_PROJECTS`].
    pub paths: Vec<PathBuf>,
}

impl RecentProjects {
    /// An empty list.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records `root` as the most recently opened project.
    ///
    /// Re-opening a project already in the list moves it to the front
    /// rather than adding a second entry, and the list is capped — a
    /// menu that grows without limit is a worse menu.
    pub fn push(&mut self, root: impl AsRef<Path>) {
        let root = root.as_ref();
        // Canonicalize so `./island` and an absolute path to the same
        // directory are one entry, not two. A path that cannot be
        // canonicalized (it may have just been created) is kept as
        // given rather than dropped.
        let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        self.paths.retain(|existing| existing != &root);
        self.paths.insert(0, root);
        self.paths.truncate(MAX_RECENT_PROJECTS);
    }

    /// Forgets `root`, if it is listed — for an entry whose directory
    /// has since been deleted or moved.
    pub fn remove(&mut self, root: impl AsRef<Path>) {
        let root = root.as_ref();
        self.paths.retain(|existing| existing != root);
    }

    /// Reads the list from `path`.
    ///
    /// A missing file is an empty list, not an error: nobody has opened
    /// a second project yet. A corrupt file is also an empty list, with
    /// a warning — losing a menu's history must never stop the studio
    /// starting.
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::new();
        };
        match ron::from_str(&text) {
            Ok(recent) => recent,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    path = %path.display(),
                    "recent-projects list is unreadable; starting empty"
                );
                Self::new()
            }
        }
    }

    /// Writes the list to `path`, creating parent directories.
    ///
    /// # Errors
    ///
    /// [`EditorError::Session`] if the file cannot be written — it is
    /// per-user editor state, the same category as `session.ron`.
    pub fn save(&self, path: &Path) -> Result<(), EditorError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| EditorError::Session {
                path: parent.to_path_buf(),
                message: err.to_string(),
            })?;
        }
        let text =
            ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default()).map_err(|err| {
                EditorError::Session {
                    path: path.to_path_buf(),
                    message: err.to_string(),
                }
            })?;
        std::fs::write(path, text).map_err(|err| EditorError::Session {
            path: path.to_path_buf(),
            message: err.to_string(),
        })
    }

    /// Where the list lives for this user.
    ///
    /// Per-user config, not per-project: a list of projects cannot live
    /// inside one of them. Returns `None` when there is no home
    /// directory to write into, in which case the studio simply runs
    /// without a recent list.
    pub fn default_path() -> Option<PathBuf> {
        let home = PathBuf::from(std::env::var_os("HOME")?);
        // macOS keeps app data here; the XDG-ish fallback covers the
        // platforms this has not been run on yet.
        let dir = if cfg!(target_os = "macos") {
            home.join("Library/Application Support/RustyEngine")
        } else {
            home.join(".config/rustyengine")
        };
        Some(dir.join("recent-projects.ron"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_moves_a_repeat_to_the_front_without_duplicating() {
        let mut recent = RecentProjects::new();
        recent.push("/tmp/a");
        recent.push("/tmp/b");
        recent.push("/tmp/a");

        assert_eq!(
            recent.paths.len(),
            2,
            "re-opening must not add a second row"
        );
        assert_eq!(recent.paths[0], PathBuf::from("/tmp/a"));
        assert_eq!(recent.paths[1], PathBuf::from("/tmp/b"));
    }

    #[test]
    fn the_list_is_capped() {
        let mut recent = RecentProjects::new();
        for index in 0..MAX_RECENT_PROJECTS + 5 {
            recent.push(format!("/tmp/project-{index}"));
        }
        assert_eq!(recent.paths.len(), MAX_RECENT_PROJECTS);
        assert_eq!(
            recent.paths[0],
            PathBuf::from(format!("/tmp/project-{}", MAX_RECENT_PROJECTS + 4)),
            "newest first",
        );
    }

    #[test]
    fn remove_forgets_an_entry() {
        let mut recent = RecentProjects::new();
        recent.push("/tmp/a");
        recent.push("/tmp/b");
        recent.remove(PathBuf::from("/tmp/a"));
        assert_eq!(recent.paths, vec![PathBuf::from("/tmp/b")]);
    }

    #[test]
    fn the_list_survives_a_round_trip() {
        let dir = std::env::temp_dir().join("vge-recent-projects-roundtrip");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested/recent.ron");

        let mut recent = RecentProjects::new();
        recent.push("/tmp/one");
        recent.push("/tmp/two");
        recent.save(&path).expect("save creates parent directories");

        assert_eq!(RecentProjects::load(&path), recent);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_or_corrupt_list_loads_empty_rather_than_failing() {
        let missing = std::env::temp_dir().join("vge-recent-projects-missing.ron");
        let _ = std::fs::remove_file(&missing);
        assert!(RecentProjects::load(&missing).paths.is_empty());

        let corrupt = std::env::temp_dir().join("vge-recent-projects-corrupt.ron");
        std::fs::write(&corrupt, "not ron at all {{{").expect("write");
        assert!(RecentProjects::load(&corrupt).paths.is_empty());
        let _ = std::fs::remove_file(&corrupt);
    }

    #[test]
    fn a_request_records_what_it_is_for() {
        assert!(!ProjectRequest::open("/tmp/a").create);
        assert!(ProjectRequest::create("/tmp/b").create);
    }
}
