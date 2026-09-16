//! # engine_project
//!
//! The on-disk shape of a RustyEngine Studio project: a root directory
//! holding a versioned [`ProjectManifest`] (`project.ron`), gameplay
//! source (`src/`), an asset root (`assets/`), scene files (`scenes/`),
//! and an editor-only metadata directory (`.studio/`) that a shipped
//! runtime never reads.
//!
//! [`Project::create`] scaffolds that layout; [`Project::open`] parses
//! and validates an existing one; [`Project::open_or_create`] does
//! whichever fits. Path accessors ([`Project::assets_dir`],
//! [`Project::main_scene_path`], …) hand back resolved absolute paths so
//! callers never hand-join project-relative strings.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

pub mod build;

/// On-disk format version for `project.ron`. Bump on any breaking
/// manifest change; [`Project::open`] refuses a file newer than this.
pub const CURRENT_PROJECT_VERSION: u32 = 1;

/// Manifest file name at the project root.
pub const MANIFEST_FILE: &str = "project.ron";
/// Gameplay-source subdirectory.
pub const SRC_DIR: &str = "src";
/// Asset-root subdirectory.
pub const ASSETS_DIR: &str = "assets";
/// Scene-files subdirectory.
pub const SCENES_DIR: &str = "scenes";
/// Editor-only metadata subdirectory. Excluded from runtime / export
/// builds — nothing here is read by a shipped game.
pub const STUDIO_DIR: &str = ".studio";

/// Default relative path of the scene the editor opens for a new project.
const DEFAULT_MAIN_SCENE: &str = "scenes/main.ron";

/// The standard subdirectories every project root is expected to have.
const STANDARD_DIRS: [&str; 4] = [SRC_DIR, ASSETS_DIR, SCENES_DIR, STUDIO_DIR];

/// Anything that can go wrong creating, opening, or validating a project.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProjectError {
    /// A filesystem operation failed.
    #[error("project i/o at {path}: {source}")]
    Io {
        /// The path being read, written, or created.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },

    /// `project.ron` was not valid RON matching [`ProjectManifest`].
    #[error("could not parse project manifest at {path}: {message}")]
    Parse {
        /// The manifest path.
        path: PathBuf,
        /// The parser's message.
        message: String,
    },

    /// No `project.ron` at the given root.
    #[error("no project manifest at {0}")]
    NotFound(PathBuf),

    /// The manifest's version is newer than this build understands.
    #[error("project format version {found} is newer than supported ({max})")]
    UnsupportedVersion {
        /// Version found in the file.
        found: u32,
        /// Highest version this build supports.
        max: u32,
    },

    /// A project-relative path in the manifest points outside the root
    /// (absolute, or contains `..`).
    #[error("path {0} escapes the project root")]
    PathEscapesRoot(PathBuf),

    /// [`Project::create`] was asked to scaffold where a project already
    /// exists.
    #[error("a project already exists at {0}")]
    AlreadyExists(PathBuf),

    /// Writing the starter scene failed.
    #[error(transparent)]
    Scene(#[from] engine_scene::SceneError),

    /// A build/export step failed — cargo couldn't launch, the compile
    /// failed, or staging the output directory failed. See
    /// [`crate::build`].
    #[error("build/export failed: {0}")]
    Build(String),
}

/// One named way to build and run the project: which crate, which cargo
/// profile, which scene it starts on, which features are on.
///
/// A game has more than one of these before it ships — a fast debug
/// build to test with, an optimised one to hand out, often a third with
/// cheats or a profiler compiled in. They live in `project.ron` rather
/// than the editor session because they are the project's, not this
/// machine's: everyone who opens the project gets the same
/// configurations. Which one *you* have selected is per-user, and lives
/// in the session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildConfiguration {
    /// What the toolbar shows. Unique within a project by convention;
    /// nothing enforces it.
    pub name: String,
    /// The cargo profile to compile with.
    #[serde(default)]
    pub profile: build::BuildProfile,
    /// The cargo package to build. `None` derives it from the project
    /// name, which is right for a single-crate project.
    #[serde(default)]
    pub crate_name: Option<String>,
    /// The scene this configuration starts on, relative to the project
    /// root. `None` uses the manifest's `main_scene`.
    #[serde(default)]
    pub scene: Option<PathBuf>,
    /// Cargo features to enable for this configuration.
    #[serde(default)]
    pub features: Vec<String>,
}

impl BuildConfiguration {
    /// A configuration named `name` building at `profile`, with
    /// everything else defaulted.
    pub fn new(name: impl Into<String>, profile: build::BuildProfile) -> Self {
        Self {
            name: name.into(),
            profile,
            crate_name: None,
            scene: None,
            features: Vec::new(),
        }
    }

    /// The cargo package this configuration builds: its own
    /// `crate_name`, or one derived from the project's name.
    pub fn crate_name(&self, project: &Project) -> String {
        self.crate_name
            .clone()
            .unwrap_or_else(|| crate_name_from(project.name()))
    }
}

/// The configurations a project gets when its manifest names none —
/// exactly the two the studio's toolbar has always offered, so an older
/// project loses nothing by not having said so.
fn default_configurations() -> Vec<BuildConfiguration> {
    vec![
        BuildConfiguration::new("Debug", build::BuildProfile::Debug),
        BuildConfiguration::new("Release", build::BuildProfile::Release),
    ]
}

/// Turns a project name into a plausible cargo package name: ASCII
/// alphanumerics lowercased, everything else an underscore, and a
/// leading digit prefixed (cargo rejects a crate starting with one).
pub fn crate_name_from(project_name: &str) -> String {
    let mut name: String = project_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    if name.is_empty() {
        return "game".to_owned();
    }
    if name.starts_with(|c: char| c.is_ascii_digit()) {
        name.insert(0, '_');
    }
    name
}

/// Makes `root` absolute without resolving symlinks or touching the
/// disk beyond reading the working directory.
///
/// A project opened as `games/island` from the command line would
/// otherwise store a relative root, and every consumer that changes
/// directory — the export build runs `cargo` *in* the project root —
/// would then resolve `games/island/target` against the project itself.
/// A path that cannot be made absolute (no working directory) is kept
/// as given.
fn absolute_root(root: PathBuf) -> PathBuf {
    std::path::absolute(&root).unwrap_or(root)
}

/// The serialized contents of `project.ron`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectManifest {
    /// Format version — see [`CURRENT_PROJECT_VERSION`].
    pub version: u32,
    /// Human-readable project name (window title, `EngineConfig`).
    pub name: String,
    /// The scene the editor opens by default, as a path relative to the
    /// project root. Must stay inside the root.
    pub main_scene: PathBuf,
    /// The project's build/run configurations, in toolbar order. A
    /// manifest written before this field existed loads with an empty
    /// list, which [`Project::open`] fills with a default Debug/Release
    /// pair — an empty list would leave the studio with nothing to
    /// build.
    #[serde(default)]
    pub configurations: Vec<BuildConfiguration>,
}

impl ProjectManifest {
    /// Applies additive migrations in memory and stamps the current format.
    /// Returns whether the manifest changed and should be saved.
    pub fn migrate(&mut self) -> bool {
        let mut changed = false;
        if self.configurations.is_empty() {
            self.configurations = default_configurations();
            changed = true;
        }
        if self.version != CURRENT_PROJECT_VERSION {
            self.version = CURRENT_PROJECT_VERSION;
            changed = true;
        }
        changed
    }
}

/// An opened project: its root directory and parsed [`ProjectManifest`].
#[derive(Debug, Clone)]
pub struct Project {
    root: PathBuf,
    manifest: ProjectManifest,
}

impl Project {
    /// Scaffolds a fresh project at `root`: creates `src/`, `assets/`,
    /// `scenes/`, and `.studio/`, writes `project.ron`, drops a
    /// `src/.gitkeep`, and writes an empty `scenes/main.ron`.
    ///
    /// # Errors
    ///
    /// [`ProjectError::AlreadyExists`] if `root` already holds a
    /// `project.ron`; [`ProjectError::Io`] on any filesystem failure;
    /// [`ProjectError::Scene`] if the starter scene can't be written.
    pub fn create(root: impl Into<PathBuf>, name: impl Into<String>) -> Result<Self, ProjectError> {
        let root = absolute_root(root.into());
        let manifest_path = root.join(MANIFEST_FILE);
        if manifest_path.exists() {
            return Err(ProjectError::AlreadyExists(root));
        }

        for dir in STANDARD_DIRS {
            let path = root.join(dir);
            std::fs::create_dir_all(&path).map_err(|source| ProjectError::Io { path, source })?;
        }

        let gitkeep = root.join(SRC_DIR).join(".gitkeep");
        std::fs::write(&gitkeep, b"").map_err(|source| ProjectError::Io {
            path: gitkeep,
            source,
        })?;

        let manifest = ProjectManifest {
            version: CURRENT_PROJECT_VERSION,
            name: name.into(),
            main_scene: PathBuf::from(DEFAULT_MAIN_SCENE),
            configurations: default_configurations(),
        };
        let project = Self { root, manifest };
        project.save_manifest()?;

        engine_scene::Scene::default().save_to_file(&project.main_scene_path())?;

        tracing::info!(root = %project.root.display(), name = %project.manifest.name, "project created");
        Ok(project)
    }

    /// Opens the project rooted at `root` (a directory containing
    /// `project.ron`), parsing and validating the manifest. Any missing
    /// standard subdirectory is recreated (with a warning) so a
    /// half-deleted project still opens.
    ///
    /// # Errors
    ///
    /// [`ProjectError::NotFound`] if there's no `project.ron`;
    /// [`ProjectError::Parse`] if it isn't valid;
    /// [`ProjectError::UnsupportedVersion`] if it's from a newer build;
    /// [`ProjectError::PathEscapesRoot`] if `main_scene` leaves the root;
    /// [`ProjectError::Io`] on a filesystem failure.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, ProjectError> {
        let root = absolute_root(root.into());
        let manifest_path = root.join(MANIFEST_FILE);
        if !manifest_path.exists() {
            return Err(ProjectError::NotFound(manifest_path));
        }

        let text = std::fs::read_to_string(&manifest_path).map_err(|source| ProjectError::Io {
            path: manifest_path.clone(),
            source,
        })?;
        let mut manifest: ProjectManifest =
            ron::from_str(&text).map_err(|err| ProjectError::Parse {
                path: manifest_path.clone(),
                message: err.to_string(),
            })?;

        if manifest.version > CURRENT_PROJECT_VERSION {
            return Err(ProjectError::UnsupportedVersion {
                found: manifest.version,
                max: CURRENT_PROJECT_VERSION,
            });
        }
        if !is_contained(&manifest.main_scene) {
            return Err(ProjectError::PathEscapesRoot(manifest.main_scene.clone()));
        }
        // A configuration's scene is a path out of a file on disk, so it
        // gets the same treatment as `main_scene`.
        for configuration in &manifest.configurations {
            if let Some(scene) = &configuration.scene
                && !is_contained(scene)
            {
                return Err(ProjectError::PathEscapesRoot(scene.clone()));
            }
        }
        let migrated = manifest.migrate();

        for dir in STANDARD_DIRS {
            let path = root.join(dir);
            if !path.exists() {
                tracing::warn!(dir, "recreating missing project directory");
                std::fs::create_dir_all(&path)
                    .map_err(|source| ProjectError::Io { path, source })?;
            }
        }

        let project = Self { root, manifest };
        if migrated {
            project.save_manifest()?;
            tracing::info!(path = %project.root.join(MANIFEST_FILE).display(), "migrated project manifest");
        }
        Ok(project)
    }

    /// [`Project::open`] if `root` holds a `project.ron`, else
    /// [`Project::create`].
    ///
    /// # Errors
    ///
    /// Whatever the chosen operation returns.
    pub fn open_or_create(
        root: impl Into<PathBuf>,
        name: impl Into<String>,
    ) -> Result<Self, ProjectError> {
        let root = root.into();
        if root.join(MANIFEST_FILE).exists() {
            Self::open(root)
        } else {
            Self::create(root, name)
        }
    }

    /// The project root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The project name from the manifest.
    pub fn name(&self) -> &str {
        &self.manifest.name
    }

    /// The project's build/run configurations, in toolbar order. Never
    /// empty for a project that came through [`Project::open`] or
    /// [`Project::create`].
    pub fn configurations(&self) -> &[BuildConfiguration] {
        &self.manifest.configurations
    }

    /// The configuration at `index`, or the first one if `index` is out
    /// of range — a stale selection (the session remembers an index, and
    /// someone may have deleted a configuration since) must not stop the
    /// studio building.
    pub fn configuration(&self, index: usize) -> Option<&BuildConfiguration> {
        self.manifest
            .configurations
            .get(index)
            .or_else(|| self.manifest.configurations.first())
    }

    /// The parsed manifest.
    pub fn manifest(&self) -> &ProjectManifest {
        &self.manifest
    }

    /// The manifest, mutably — to rename the project or point it at a
    /// different main scene. Call [`Project::save_manifest`] afterwards;
    /// this only changes the in-memory copy.
    pub fn manifest_mut(&mut self) -> &mut ProjectManifest {
        &mut self.manifest
    }

    /// Absolute path to `src/`.
    pub fn src_dir(&self) -> PathBuf {
        self.root.join(SRC_DIR)
    }

    /// Absolute path to `assets/`.
    pub fn assets_dir(&self) -> PathBuf {
        self.root.join(ASSETS_DIR)
    }

    /// Absolute path to `scenes/`.
    pub fn scenes_dir(&self) -> PathBuf {
        self.root.join(SCENES_DIR)
    }

    /// Absolute path to `.studio/` (editor-only metadata).
    pub fn studio_dir(&self) -> PathBuf {
        self.root.join(STUDIO_DIR)
    }

    /// Absolute path to the manifest's `main_scene`.
    pub fn main_scene_path(&self) -> PathBuf {
        self.root.join(&self.manifest.main_scene)
    }

    /// Joins `relative` onto the project root, rejecting anything that
    /// would escape it (absolute, or containing `..`).
    ///
    /// # Errors
    ///
    /// [`ProjectError::PathEscapesRoot`] if `relative` is not contained.
    pub fn resolve(&self, relative: &Path) -> Result<PathBuf, ProjectError> {
        if is_contained(relative) {
            Ok(self.root.join(relative))
        } else {
            Err(ProjectError::PathEscapesRoot(relative.to_path_buf()))
        }
    }

    /// Writes the current manifest back to `project.ron` (pretty RON).
    ///
    /// # Errors
    ///
    /// [`ProjectError::Parse`] if serialization fails (practically
    /// never), [`ProjectError::Io`] if the write fails.
    pub fn save_manifest(&self) -> Result<(), ProjectError> {
        let path = self.root.join(MANIFEST_FILE);
        let text = ron::ser::to_string_pretty(&self.manifest, ron::ser::PrettyConfig::default())
            .map_err(|err| ProjectError::Parse {
                path: path.clone(),
                message: err.to_string(),
            })?;
        std::fs::write(&path, text).map_err(|source| ProjectError::Io { path, source })
    }
}

/// Whether `relative` stays inside a root when joined to it — no root/
/// prefix component, no `..`.
fn is_contained(relative: &Path) -> bool {
    !relative.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// A unique, non-existent temp directory path for one test.
    fn temp_root(tag: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("engine_project_{tag}_{nanos}_{n}"))
    }

    fn cleanup(root: &Path) {
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn create_scaffolds_the_full_layout() {
        let root = temp_root("create");
        let project = Project::create(&root, "Demo").expect("create");

        assert!(root.join("project.ron").is_file());
        assert!(project.src_dir().is_dir());
        assert!(project.assets_dir().is_dir());
        assert!(project.scenes_dir().is_dir());
        assert!(project.studio_dir().is_dir());
        assert!(project.main_scene_path().is_file());
        assert_eq!(project.name(), "Demo");
        assert_eq!(project.manifest().version, CURRENT_PROJECT_VERSION);

        cleanup(&root);
    }

    #[test]
    fn create_refuses_an_existing_project() {
        let root = temp_root("exists");
        Project::create(&root, "One").expect("first create");
        let err = Project::create(&root, "Two").expect_err("second create must fail");
        assert!(matches!(err, ProjectError::AlreadyExists(_)));
        cleanup(&root);
    }

    #[test]
    fn open_round_trips_a_created_project() {
        let root = temp_root("roundtrip");
        let created = Project::create(&root, "Round").expect("create");
        let opened = Project::open(&root).expect("open");
        assert_eq!(opened.name(), created.name());
        assert_eq!(opened.manifest(), created.manifest());
        assert_eq!(opened.main_scene_path(), created.main_scene_path());
        cleanup(&root);
    }

    #[test]
    fn open_without_a_manifest_is_not_found() {
        let root = temp_root("missing");
        std::fs::create_dir_all(&root).unwrap();
        let err = Project::open(&root).expect_err("no manifest");
        assert!(matches!(err, ProjectError::NotFound(_)));
        cleanup(&root);
    }

    #[test]
    fn open_rejects_a_future_version() {
        let root = temp_root("future");
        std::fs::create_dir_all(&root).unwrap();
        let manifest = format!(
            "(version: {}, name: \"X\", main_scene: \"scenes/main.ron\")",
            CURRENT_PROJECT_VERSION + 1
        );
        std::fs::write(root.join("project.ron"), manifest).unwrap();
        let err = Project::open(&root).expect_err("future version");
        assert!(matches!(
            err,
            ProjectError::UnsupportedVersion { found, max }
                if found == CURRENT_PROJECT_VERSION + 1 && max == CURRENT_PROJECT_VERSION
        ));
        cleanup(&root);
    }

    #[test]
    fn open_rejects_a_main_scene_escaping_the_root() {
        let root = temp_root("escape");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("project.ron"),
            "(version: 1, name: \"X\", main_scene: \"../evil.ron\")",
        )
        .unwrap();
        let err = Project::open(&root).expect_err("escaping path");
        assert!(matches!(err, ProjectError::PathEscapesRoot(_)));
        cleanup(&root);
    }

    #[test]
    fn open_recreates_a_deleted_standard_directory() {
        let root = temp_root("heal");
        let project = Project::create(&root, "Heal").expect("create");
        std::fs::remove_dir_all(project.assets_dir()).unwrap();
        assert!(!project.assets_dir().exists());

        let reopened = Project::open(&root).expect("open still succeeds");
        assert!(reopened.assets_dir().is_dir(), "assets dir recreated");
        cleanup(&root);
    }

    #[test]
    fn manifest_without_configurations_gets_a_default_one() {
        let root = temp_root("old-manifest");
        std::fs::create_dir_all(&root).unwrap();
        // A manifest written before configurations existed.
        std::fs::write(
            root.join(MANIFEST_FILE),
            "(version: 1, name: \"Old\", main_scene: \"scenes/main.ron\")",
        )
        .unwrap();

        let project = Project::open(&root).expect("an older manifest still opens");
        assert!(
            !project.configurations().is_empty(),
            "a project with no configurations still has to be buildable"
        );
        assert_eq!(project.configurations()[0].name, "Debug");
        assert_eq!(
            project.configuration(0).map(|c| c.profile),
            Some(build::BuildProfile::Debug)
        );
        // A stale selection falls back rather than vanishing.
        assert_eq!(
            project.configuration(99).map(|c| c.name.as_str()),
            Some("Debug")
        );
        cleanup(&root);
    }

    #[test]
    fn configurations_survive_a_manifest_round_trip() {
        let root = temp_root("config-round-trip");
        let mut project = Project::create(&root, "Round Trip").expect("create");
        project
            .manifest_mut()
            .configurations
            .push(BuildConfiguration {
                name: "Profiling".to_owned(),
                profile: build::BuildProfile::Release,
                crate_name: Some("island".to_owned()),
                scene: Some(PathBuf::from("scenes/bench.ron")),
                features: vec!["profiler".to_owned(), "overlays".to_owned()],
            });
        project.save_manifest().expect("save");

        let reopened = Project::open(&root).expect("reopen");
        assert_eq!(reopened.configurations().len(), 3);
        let profiling = &reopened.configurations()[2];
        assert_eq!(profiling.features, vec!["profiler", "overlays"]);
        assert_eq!(profiling.crate_name(&reopened), "island");
        cleanup(&root);
    }

    #[test]
    fn a_configuration_scene_cannot_escape_the_project() {
        let root = temp_root("config-escape");
        let project = Project::create(&root, "Escape").expect("create");
        let manifest = "(version: 1, name: \"Escape\", main_scene: \"scenes/main.ron\", configurations: [(name: \"Bad\", scene: Some(\"../../etc/passwd\"))])";
        std::fs::write(project.root().join(MANIFEST_FILE), manifest).unwrap();

        assert!(matches!(
            Project::open(&root),
            Err(ProjectError::PathEscapesRoot(_))
        ));
        cleanup(&root);
    }

    #[test]
    fn a_project_opened_by_a_relative_path_still_has_an_absolute_root() {
        // Cargo runs tests with the package directory as the working
        // directory, so a path under `target/` is a genuine relative
        // path without this test touching global state (changing the
        // process directory would race the other tests).
        let relative = PathBuf::from("target").join("relative-root-test");
        let _ = std::fs::remove_dir_all(&relative);
        Project::create(&relative, "Relative").expect("create");

        // The export build runs cargo *inside* the project root, so a
        // relative root would resolve `<root>/target` against the
        // project itself and never find the compiled binary.
        let project = Project::open(&relative).expect("opens by relative path");
        assert!(
            project.root().is_absolute(),
            "root stayed relative: {}",
            project.root().display()
        );
        assert!(project.main_scene_path().is_absolute());
        let _ = std::fs::remove_dir_all(&relative);
    }

    #[test]
    fn crate_names_are_derived_from_the_project_name() {
        assert_eq!(crate_name_from("Island"), "island");
        assert_eq!(crate_name_from("My First Game!"), "my_first_game_");
        assert_eq!(crate_name_from("2048"), "_2048");
        assert_eq!(crate_name_from(""), "game");
    }

    #[test]
    fn resolve_rejects_traversal_and_absolute_paths() {
        let root = temp_root("resolve");
        let project = Project::create(&root, "R").expect("create");

        assert!(project.resolve(Path::new("scenes/main.ron")).is_ok());
        assert!(matches!(
            project.resolve(Path::new("../x")),
            Err(ProjectError::PathEscapesRoot(_))
        ));
        assert!(matches!(
            project.resolve(Path::new("/etc/passwd")),
            Err(ProjectError::PathEscapesRoot(_))
        ));
        cleanup(&root);
    }

    #[test]
    fn open_or_create_creates_then_opens() {
        let root = temp_root("ooc");
        let first = Project::open_or_create(&root, "OOC").expect("create path");
        let second = Project::open_or_create(&root, "ignored").expect("open path");
        assert_eq!(first.manifest(), second.manifest());
        assert_eq!(second.name(), "OOC");
        cleanup(&root);
    }
}
