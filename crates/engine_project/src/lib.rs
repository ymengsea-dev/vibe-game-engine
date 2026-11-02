//! # engine_project
//!
//! The on-disk shape of a RustyEngine Studio project: a root directory
//! holding a versioned [`ProjectManifest`] (`project.ron`), gameplay
//! source (`src/`), an asset root (`assets/`), scene files (`scenes/`),
//! and a hidden `.studio/` directory. Its asset IDs are project data;
//! per-user editor session files stay local.
//!
//! [`Project::create`] scaffolds that layout; [`Project::open`] parses
//! and validates an existing one; [`Project::open_or_create`] does
//! whichever fits. Path accessors ([`Project::assets_dir`],
//! [`Project::main_scene_path`], …) hand back resolved absolute paths so
//! callers never hand-join project-relative strings.

use std::io::Write;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

pub mod build;
mod starter;
pub mod storage;

/// On-disk format version for `project.ron`. Bump on any breaking
/// manifest change; [`Project::open`] refuses a file newer than this.
pub const CURRENT_PROJECT_VERSION: u32 = 3;

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
const STARTER_LUA: &str = "-- The engine calls these functions while your game runs.\nreturn {\n    start = function()\n        engine.log(\"Game started\")\n    end,\n    update = function(dt)\n        -- Add your game rules here.\n    end,\n}\n";
/// Annotations exactly as first shipped, before files carried a version line.
/// Frozen: projects created then are recognised as untouched by this text.
const STUDIO_API_LUA_V0: &str = r#"---@meta
---Studio's Lua gameplay API. This file is for editor help; the player never executes it.
---@class StudioPosition
---@field x number Parent-relative X coordinate.
---@field y number Parent-relative Y coordinate.
---@field z number Parent-relative Z coordinate.

---@class StudioEngine
---@field log fun(message: string)
---@field object_id fun(): string? Current attached scene object; nil in the global script.
---@field set_position fun(object_id: string, x: number, y: number, z: number) Set a parent-relative position.
---@field translate fun(object_id: string, dx: number, dy: number, dz: number) Move by a parent-relative delta.
---@field find_object fun(name: string): string? Find the first scene object with this name.
---@field get_position fun(object_id: string): StudioPosition? Read a parent-relative position; nil if missing.
---@field spawn_empty fun(name: string, x: number, y: number, z: number): string Create a named runtime object without a mesh and return its UUID.
---@field spawn_prefab fun(path: string, x: number, y: number, z: number): string Spawn a packaged assets/prefabs/*.prefab object and return its UUID.
---@field key_down fun(key: string): boolean Whether a supported key is held.
---@field key_pressed fun(key: string): boolean Whether a supported key went down this frame.
---@field play_audio fun(object_id: string) Trigger an authored audio emitter on a scene object (up to 32 times per callback dispatch).
---@field set_animation_playing fun(object_id: string, playing: boolean) Pause or resume an authored sprite-sheet animation.
---@field ui_set_text fun(object_id: string, text: string) Change an authored label or button caption (at most 1024 bytes).
---@field emit_event fun(name: string) Send a named event to authored visual rules (1–64 printable bytes).
---@field change_scene fun(path: string) Load a packaged scenes/*.ron scene after this frame; the new scene runs start before its first update.
---@field save_get fun(key: string): string|number|boolean|nil Read a restored gameplay value.
---@field save_set fun(key: string, value: string|number|boolean|nil) Set a gameplay value; nil removes it.
---@field save fun() Request an atomic save at the end of the frame.
---@type StudioEngine
engine = {}

---@class StudioScript
---@field start? fun()
---@field update? fun(dt: number)
---@field fixed_update? fun(dt: number)
---@field on_event? fun(kind: string, value: string) Keyboard events use key_pressed/key_released.
---@field on_collision? fun(kind: string, self_id: string, other_id: string) Contact enter/exit after a physics step; global script receives the sorted pair.
"#;
/// Current annotations. Never edit in place without bumping
/// [`STUDIO_API_VERSION`] and freezing the previous text; see `refresh_lua_annotations`.
const STUDIO_API_LUA: &str = r#"---@meta
---Studio's Lua gameplay API. This file is for editor help; the player never executes it.
---@class StudioPosition
---@field x number Parent-relative X coordinate.
---@field y number Parent-relative Y coordinate.
---@field z number Parent-relative Z coordinate.

---@class StudioEngine
---@field log fun(message: string)
---@field object_id fun(): string? Current attached scene object; nil in the global script.
---@field set_position fun(object_id: string, x: number, y: number, z: number) Set a parent-relative position.
---@field translate fun(object_id: string, dx: number, dy: number, dz: number) Move by a parent-relative delta.
---@field find_object fun(name: string): string? Find the first scene object with this name.
---@field get_position fun(object_id: string): StudioPosition? Read a parent-relative position; nil if missing.
---@field spawn_empty fun(name: string, x: number, y: number, z: number): string Create a named runtime object without a mesh and return its UUID.
---@field spawn_prefab fun(path: string, x: number, y: number, z: number): string Spawn a packaged assets/prefabs/*.prefab object and return its UUID.
---@field key_down fun(key: string): boolean Whether a supported key is held.
---@field key_pressed fun(key: string): boolean Whether a supported key went down this frame.
---@field play_audio fun(object_id: string) Trigger an authored audio emitter on a scene object (up to 32 times per callback dispatch).
---@field set_animation_playing fun(object_id: string, playing: boolean) Pause or resume an authored sprite-sheet animation.
---@field ui_set_text fun(object_id: string, text: string) Change an authored label or button caption (at most 1024 bytes).
---@field emit_event fun(name: string) Send a named event to authored visual rules (1–64 printable bytes).
---@field change_scene fun(path: string) Load a packaged scenes/*.ron scene after this frame; the new scene runs start before its first update.
---@field save_get fun(key: string): string|number|boolean|nil Read a restored gameplay value.
---@field save_set fun(key: string, value: string|number|boolean|nil) Set a gameplay value; nil removes it.
---@field save fun() Request an atomic save at the end of the frame.
---@field get_var fun(name: string): number Read a gameplay variable shared with visual rules; unset variables are 0.
---@field set_var fun(name: string, value: number) Set a gameplay variable shared with visual rules (finite number; name 1–64 printable bytes).
---@type StudioEngine
engine = {}

---@class StudioScript
---@field start? fun()
---@field update? fun(dt: number)
---@field fixed_update? fun(dt: number)
---@field on_event? fun(kind: string, value: string) Keyboard events use key_pressed/key_released.
---@field on_collision? fun(kind: string, self_id: string, other_id: string) Contact enter/exit after a physics step; global script receives the sorted pair.
"#;
const STUDIO_API_VERSION: u32 = 2;

fn versioned_studio_api_lua() -> String {
    with_version_line(STUDIO_API_LUA, STUDIO_API_VERSION)
}

fn with_version_line(text: &str, version: u32) -> String {
    text.replacen(
        "---@meta\n",
        &format!("---@meta\n-- Studio API annotations v{version}\n"),
        1,
    )
}

/// The backup name for a generated annotation file Studio may replace, or
/// `None` when the file is not byte-identical to any earlier generated version.
fn generated_annotation_backup(existing: &str) -> Option<&'static str> {
    if existing == STUDIO_API_LUA_V0 {
        Some("src/studio_api.lua.legacy.bak")
    } else if existing == with_version_line(STUDIO_API_LUA_V0, 1) {
        Some("src/studio_api.lua.v1.bak")
    } else {
        None
    }
}

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

    /// A Lua API annotation file or migration backup has user changes.
    #[error("Lua API annotations were customized; refusing to replace {0}")]
    AnnotationsModified(PathBuf),

    /// [`Project::create`] was asked to scaffold where a project already
    /// exists.
    #[error("a project already exists at {0}")]
    AlreadyExists(PathBuf),

    /// Writing the starter scene failed.
    #[error(transparent)]
    Scene(#[from] engine_scene::SceneError),

    /// Generating a starter image or model failed.
    #[error(transparent)]
    Asset(#[from] engine_asset::AssetError),

    /// A build/export step failed — the player or Cargo was unavailable,
    /// compilation failed, or staging the output directory failed. See
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

/// Gameplay authoring path selected for a project. The engine itself remains Rust.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum GameplayLanguage {
    /// Existing projects without a language field keep their Cargo workflow.
    #[default]
    Rust,
    /// Gameplay is loaded by the bundled Lua-capable player.
    Lua,
}

/// Creation-only scene choice; it is not stored in the manifest after creation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ProjectTemplate {
    /// An empty scene, matching previous project creation behavior.
    #[default]
    Blank,
    /// A sprite, orthographic camera, and clickable game UI.
    Starter2D,
    /// A model, perspective camera, lighting, and clickable game UI.
    Starter3D,
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
    /// Stable identity for save files across renamed or moved exports.
    #[serde(default)]
    pub project_id: uuid::Uuid,
    /// The scene the editor opens by default, as a path relative to the
    /// project root. Must stay inside the root.
    pub main_scene: PathBuf,
    /// Missing in older manifests, which were always Rust projects.
    #[serde(default)]
    pub gameplay_language: GameplayLanguage,
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
        if self.project_id.is_nil() {
            self.project_id = uuid::Uuid::new_v4();
            changed = true;
        }
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
    /// Scaffolds a fresh project in a directory that does not yet exist.
    ///
    /// Unlike [`Project::create`], this reserves the project root before
    /// writing any project files. It is for user-selected destinations,
    /// where replacing an intervening directory would risk data loss.
    pub fn create_new(
        root: impl Into<PathBuf>,
        name: impl Into<String>,
    ) -> Result<Self, ProjectError> {
        Self::create_new_with_language(root, name, GameplayLanguage::Lua)
    }

    /// Reserves a fresh project root with the requested gameplay language.
    pub fn create_new_with_language(
        root: impl Into<PathBuf>,
        name: impl Into<String>,
        language: GameplayLanguage,
    ) -> Result<Self, ProjectError> {
        Self::create_new_with_template(root, name, language, ProjectTemplate::Blank)
    }

    /// Reserves a fresh project root and creates its selected scene template.
    pub fn create_new_with_template(
        root: impl Into<PathBuf>,
        name: impl Into<String>,
        language: GameplayLanguage,
        template: ProjectTemplate,
    ) -> Result<Self, ProjectError> {
        let root = absolute_root(root.into());
        match std::fs::create_dir(&root) {
            Ok(()) => Self::create_with_template(root, name, language, template),
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(ProjectError::AlreadyExists(root))
            }
            Err(source) => Err(ProjectError::Io { path: root, source }),
        }
    }

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
        Self::create_with_language(root, name, GameplayLanguage::Lua)
    }

    /// Scaffolds a project using the selected gameplay language.
    pub fn create_with_language(
        root: impl Into<PathBuf>,
        name: impl Into<String>,
        language: GameplayLanguage,
    ) -> Result<Self, ProjectError> {
        Self::create_with_template(root, name, language, ProjectTemplate::Blank)
    }

    /// Scaffolds a project with a blank or guided starter scene.
    pub fn create_with_template(
        root: impl Into<PathBuf>,
        name: impl Into<String>,
        language: GameplayLanguage,
        template: ProjectTemplate,
    ) -> Result<Self, ProjectError> {
        let root = absolute_root(root.into());
        let manifest_path = root.join(MANIFEST_FILE);
        if manifest_path.exists() {
            return Err(ProjectError::AlreadyExists(root));
        }

        for dir in STANDARD_DIRS {
            let path = root.join(dir);
            std::fs::create_dir_all(&path).map_err(|source| ProjectError::Io { path, source })?;
        }

        let manifest = ProjectManifest {
            version: CURRENT_PROJECT_VERSION,
            name: name.into(),
            project_id: uuid::Uuid::new_v4(),
            main_scene: PathBuf::from(DEFAULT_MAIN_SCENE),
            gameplay_language: language,
            configurations: default_configurations(),
        };
        validate_project_paths(&root, &manifest)?;

        let gitkeep = root.join(SRC_DIR).join(".gitkeep");
        std::fs::write(&gitkeep, b"").map_err(|source| ProjectError::Io {
            path: gitkeep,
            source,
        })?;

        if language == GameplayLanguage::Lua {
            let scripts = root.join(ASSETS_DIR).join("scripts");
            std::fs::create_dir_all(&scripts).map_err(|source| ProjectError::Io {
                path: scripts.clone(),
                source,
            })?;
            let main_script = scripts.join("main.lua");
            std::fs::write(&main_script, STARTER_LUA).map_err(|source| ProjectError::Io {
                path: main_script,
                source,
            })?;
            let annotations = root.join(SRC_DIR).join("studio_api.lua");
            std::fs::write(&annotations, versioned_studio_api_lua()).map_err(|source| {
                ProjectError::Io {
                    path: annotations,
                    source,
                }
            })?;
        }

        let project = Self { root, manifest };
        project.save_manifest()?;

        let scene = starter::create_scene(&project.root, language, template)?;
        scene.save_to_file(&project.main_scene_path())?;

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
        validate_project_paths(&root, &manifest)?;
        let previous_version = manifest.version;
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
            if previous_version < CURRENT_PROJECT_VERSION {
                let backup = project
                    .root
                    .join(format!("{MANIFEST_FILE}.v{previous_version}.bak"));
                match std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&backup)
                {
                    Ok(mut file) => {
                        file.write_all(text.as_bytes())
                            .and_then(|()| file.sync_all())
                            .map_err(|source| ProjectError::Io {
                                path: backup.clone(),
                                source,
                            })?;
                    }
                    Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
                        let existing = std::fs::read_to_string(&backup).map_err(|source| {
                            ProjectError::Io {
                                path: backup.clone(),
                                source,
                            }
                        })?;
                        if existing != text {
                            return Err(ProjectError::Io {
                                path: backup,
                                source: std::io::Error::new(
                                    std::io::ErrorKind::InvalidData,
                                    "existing migration backup differs from the project manifest",
                                ),
                            });
                        }
                    }
                    Err(source) => {
                        return Err(ProjectError::Io {
                            path: backup,
                            source,
                        });
                    }
                }
            }
            project.save_manifest()?;
            tracing::info!(path = %project.root.join(MANIFEST_FILE).display(), "migrated project manifest");
        }
        Ok(project)
    }

    /// Refreshes an unmodified legacy annotation file without losing its old version.
    /// Customized files are kept intact so users can merge the new API by hand.
    /// This editor-only operation is explicit: opening a project for Play must not write it.
    ///
    /// # Errors
    ///
    /// Returns a path or I/O error if the annotation or backup cannot be read or written.
    pub fn refresh_lua_annotations(&self) -> Result<(), ProjectError> {
        if self.language() != GameplayLanguage::Lua {
            return Ok(());
        }
        let relative = Path::new("src/studio_api.lua");
        let path = self.resolve(relative)?;
        let current = versioned_studio_api_lua();
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if !metadata.file_type().is_file() => {
                return Err(ProjectError::AnnotationsModified(path));
            }
            Err(source) if source.kind() != std::io::ErrorKind::NotFound => {
                return Err(ProjectError::Io { path, source });
            }
            _ => {}
        }
        let existing = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                    .and_then(|mut file| file.write_all(current.as_bytes()))
                    .map_err(|source| ProjectError::Io { path, source })?;
                return Ok(());
            }
            Err(source) => return Err(ProjectError::Io { path, source }),
        };
        if existing == current {
            return Ok(());
        }
        let Some(backup) = generated_annotation_backup(&existing) else {
            return Err(ProjectError::AnnotationsModified(path));
        };
        let backup = self.resolve(Path::new(backup))?;
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&backup)
        {
            Ok(mut file) => {
                file.write_all(existing.as_bytes())
                    .and_then(|()| file.sync_all())
                    .map_err(|source| ProjectError::Io {
                        path: backup.clone(),
                        source,
                    })?;
            }
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
                let metadata =
                    std::fs::symlink_metadata(&backup).map_err(|source| ProjectError::Io {
                        path: backup.clone(),
                        source,
                    })?;
                if !metadata.file_type().is_file() {
                    return Err(ProjectError::AnnotationsModified(backup));
                }
                if std::fs::read_to_string(&backup).map_err(|source| ProjectError::Io {
                    path: backup.clone(),
                    source,
                })? != existing
                {
                    return Err(ProjectError::AnnotationsModified(backup));
                }
            }
            Err(source) => {
                return Err(ProjectError::Io {
                    path: backup,
                    source,
                });
            }
        }
        engine_scene::write_atomic(&path, current.as_bytes())
            .map_err(|source| ProjectError::Io { path, source })
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

    /// The project's gameplay language; legacy projects return Rust.
    pub fn language(&self) -> GameplayLanguage {
        self.manifest.gameplay_language
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

    /// Absolute path to `.studio/` (asset IDs and local editor state).
    pub fn studio_dir(&self) -> PathBuf {
        self.root.join(STUDIO_DIR)
    }

    /// Absolute path to the manifest's `main_scene`.
    pub fn main_scene_path(&self) -> PathBuf {
        self.root.join(&self.manifest.main_scene)
    }

    /// Joins `relative` onto the project root, rejecting anything that
    /// would escape it (absolute, containing `..`, or following an
    /// existing symlink outside the root).
    ///
    /// # Errors
    ///
    /// [`ProjectError::PathEscapesRoot`] if `relative` is not contained.
    pub fn resolve(&self, relative: &Path) -> Result<PathBuf, ProjectError> {
        resolve_contained(&self.root, relative)
    }

    /// Writes the current manifest back to `project.ron` (pretty RON).
    ///
    /// # Errors
    ///
    /// [`ProjectError::Parse`] if serialization fails (practically
    /// never), [`ProjectError::Io`] if the write fails.
    pub fn save_manifest(&self) -> Result<(), ProjectError> {
        validate_project_paths(&self.root, &self.manifest)?;
        let path = self.root.join(MANIFEST_FILE);
        let text = ron::ser::to_string_pretty(&self.manifest, ron::ser::PrettyConfig::default())
            .map_err(|err| ProjectError::Parse {
                path: path.clone(),
                message: err.to_string(),
            })?;
        std::fs::write(&path, text).map_err(|source| ProjectError::Io { path, source })
    }
}

fn validate_project_paths(root: &Path, manifest: &ProjectManifest) -> Result<(), ProjectError> {
    for dir in STANDARD_DIRS {
        resolve_contained(root, Path::new(dir))?;
    }
    resolve_contained(root, &manifest.main_scene)?;
    for configuration in &manifest.configurations {
        if let Some(scene) = &configuration.scene {
            resolve_contained(root, scene)?;
        }
    }
    Ok(())
}

fn resolve_contained(root: &Path, relative: &Path) -> Result<PathBuf, ProjectError> {
    if !is_contained(relative) {
        return Err(ProjectError::PathEscapesRoot(relative.to_path_buf()));
    }

    let root_canonical = root.canonicalize().map_err(|source| ProjectError::Io {
        path: root.to_path_buf(),
        source,
    })?;
    let path = root.join(relative);
    let existing = path
        .ancestors()
        .find(|ancestor| ancestor.exists())
        .unwrap_or(root);
    let canonical = existing.canonicalize().map_err(|source| ProjectError::Io {
        path: existing.to_path_buf(),
        source,
    })?;
    if !canonical.starts_with(&root_canonical) {
        return Err(ProjectError::PathEscapesRoot(relative.to_path_buf()));
    }
    Ok(path)
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
    fn new_project_defaults_to_lua_and_reopens_as_lua() {
        let root = temp_root("lua-default");
        let created = Project::create(&root, "Lua Game").expect("create");
        assert_eq!(created.language(), GameplayLanguage::Lua);
        assert_eq!(
            Project::open(&root).expect("reopen").language(),
            GameplayLanguage::Lua
        );
        assert!(root.join("assets/scripts/main.lua").is_file());
        assert!(
            std::fs::read_to_string(root.join("assets/scripts/main.lua"))
                .expect("starter script")
                .contains("return")
        );
        let annotations = std::fs::read_to_string(root.join("src/studio_api.lua"))
            .expect("Lua editor API annotations");
        assert!(annotations.contains("set_position"));
        assert!(annotations.contains("translate"));
        assert!(annotations.contains("key_pressed"));
        assert!(annotations.contains("play_audio"));
        assert!(annotations.contains("change_scene"));
        assert!(!root.join("Cargo.toml").exists());
        cleanup(&root);
    }

    #[test]
    fn opening_legacy_lua_project_upgrades_pristine_annotations_with_backup() {
        let root = temp_root("lua-api-upgrade");
        Project::create(&root, "Lua API Upgrade").expect("create");
        let annotations = root.join("src/studio_api.lua");
        std::fs::write(&annotations, STUDIO_API_LUA_V0).expect("legacy annotations");

        let project = Project::open(&root).expect("open");
        project.refresh_lua_annotations().expect("upgrade");

        let updated = std::fs::read_to_string(&annotations).expect("updated annotations");
        assert!(updated.starts_with("---@meta\n-- Studio API annotations v2\n"));
        assert!(updated.contains("get_var"));
        assert!(updated.contains("---@field change_scene"));
        assert_eq!(
            std::fs::read_to_string(root.join("src/studio_api.lua.legacy.bak"))
                .expect("legacy backup"),
            STUDIO_API_LUA_V0
        );
        Project::open(&root)
            .expect("reopen")
            .refresh_lua_annotations()
            .expect("already current");
        assert_eq!(
            std::fs::read_to_string(&annotations).expect("same annotations"),
            updated
        );
        cleanup(&root);
    }

    #[test]
    fn v1_annotations_upgrade_even_after_an_earlier_legacy_backup() {
        let root = temp_root("lua-api-v1-upgrade");
        Project::create(&root, "Lua API v1").expect("create");
        let annotations = root.join("src/studio_api.lua");
        let v1 = with_version_line(STUDIO_API_LUA_V0, 1);
        std::fs::write(&annotations, &v1).expect("v1 annotations");
        // Left by the earlier v0 → v1 refresh; it must not block this one.
        std::fs::write(
            root.join("src/studio_api.lua.legacy.bak"),
            STUDIO_API_LUA_V0,
        )
        .expect("old backup");

        Project::open(&root)
            .expect("open")
            .refresh_lua_annotations()
            .expect("upgrade");

        assert_eq!(
            std::fs::read_to_string(&annotations).expect("updated"),
            versioned_studio_api_lua()
        );
        assert_eq!(
            std::fs::read_to_string(root.join("src/studio_api.lua.v1.bak")).expect("v1 backup"),
            v1
        );
        cleanup(&root);
    }

    #[test]
    fn edited_v1_annotations_are_not_replaced() {
        let root = temp_root("lua-api-v1-custom");
        Project::create(&root, "Lua API v1 custom").expect("create");
        let annotations = root.join("src/studio_api.lua");
        let edited = format!("{}-- my note\n", with_version_line(STUDIO_API_LUA_V0, 1));
        std::fs::write(&annotations, &edited).expect("edited annotations");

        let project = Project::open(&root).expect("open");
        assert!(project.refresh_lua_annotations().is_err());
        assert_eq!(
            std::fs::read_to_string(&annotations).expect("untouched"),
            edited
        );
        assert!(!root.join("src/studio_api.lua.v1.bak").exists());
        cleanup(&root);
    }

    #[test]
    fn opening_lua_project_keeps_custom_annotations_untouched() {
        let root = temp_root("lua-api-custom");
        Project::create(&root, "Custom Lua API").expect("create");
        let annotations = root.join("src/studio_api.lua");
        let custom = "---@meta\n-- My own API annotations\n";
        std::fs::write(&annotations, custom).expect("custom annotations");

        let project = Project::open(&root).expect("custom annotations do not block opening");
        assert!(project.refresh_lua_annotations().is_err());

        assert_eq!(
            std::fs::read_to_string(&annotations).expect("untouched"),
            custom
        );
        assert!(!root.join("src/studio_api.lua.legacy.bak").exists());
        cleanup(&root);
    }

    #[test]
    fn opening_project_for_play_does_not_write_editor_annotations() {
        let root = temp_root("lua-api-readonly-open");
        Project::create(&root, "Play Without Editor Writes").expect("create");
        let annotations = root.join("src/studio_api.lua");
        std::fs::write(&annotations, STUDIO_API_LUA_V0).expect("legacy annotations");

        Project::open(&root).expect("open for player");

        assert_eq!(
            std::fs::read_to_string(&annotations).expect("unchanged annotations"),
            STUDIO_API_LUA_V0
        );
        assert!(!root.join("src/studio_api.lua.legacy.bak").exists());
        cleanup(&root);
    }

    #[cfg(unix)]
    #[test]
    fn annotation_backup_symlink_cannot_masquerade_as_a_safe_backup() {
        use std::os::unix::fs::symlink;

        let root = temp_root("lua-api-backup-link");
        let project = Project::create(&root, "Lua API Backup Link").expect("create");
        let annotations = root.join("src/studio_api.lua");
        let backup = root.join("src/studio_api.lua.legacy.bak");
        std::fs::write(&annotations, STUDIO_API_LUA_V0).expect("legacy annotations");
        symlink(&annotations, &backup).expect("link backup to source");

        assert!(project.refresh_lua_annotations().is_err());
        assert_eq!(
            std::fs::read_to_string(&annotations).expect("source unchanged"),
            STUDIO_API_LUA_V0
        );
        cleanup(&root);
    }

    #[cfg(unix)]
    #[test]
    fn annotation_source_symlink_is_not_replaced_during_refresh() {
        use std::os::unix::fs::symlink;

        let root = temp_root("lua-api-source-link");
        let project = Project::create(&root, "Lua API Source Link").expect("create");
        let annotations = root.join("src/studio_api.lua");
        let custom = root.join("src/custom.lua");
        std::fs::write(&custom, STUDIO_API_LUA_V0).expect("custom source");
        std::fs::remove_file(&annotations).expect("remove generated annotations");
        symlink(&custom, &annotations).expect("link annotations");

        assert!(project.refresh_lua_annotations().is_err());
        assert!(
            std::fs::symlink_metadata(&annotations)
                .expect("link remains")
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(&custom).expect("custom remains"),
            STUDIO_API_LUA_V0
        );
        cleanup(&root);
    }

    #[test]
    fn project_identity_survives_reopen_and_v2_migration_keeps_backup() {
        let root = temp_root("save-identity");
        let created = Project::create(&root, "Save Game").expect("create");
        let id = created.manifest().project_id;
        assert!(!id.is_nil());
        assert_eq!(
            Project::open(&root).expect("reopen").manifest().project_id,
            id
        );
        cleanup(&root);

        std::fs::create_dir_all(&root).expect("root");
        let old = "(version: 2, name: \"Old Lua\", main_scene: \"scenes/main.ron\", gameplay_language: Lua)";
        std::fs::write(root.join(MANIFEST_FILE), old).expect("old manifest");
        let migrated = Project::open(&root).expect("migrate");
        assert!(!migrated.manifest().project_id.is_nil());
        assert_eq!(
            std::fs::read_to_string(root.join("project.ron.v2.bak")).expect("backup"),
            old
        );
        assert_eq!(
            Project::open(&root)
                .expect("reopen migrated")
                .manifest()
                .project_id,
            migrated.manifest().project_id
        );
        cleanup(&root);
    }

    #[test]
    fn explicit_rust_project_reopens_as_rust() {
        let root = temp_root("rust-option");
        Project::create_with_language(&root, "Rust Game", GameplayLanguage::Rust).expect("create");
        assert_eq!(
            Project::open(&root).expect("reopen").language(),
            GameplayLanguage::Rust
        );
        cleanup(&root);
    }

    #[test]
    fn guided_starters_reopen_with_importable_scene_assets_in_both_languages() {
        for language in [GameplayLanguage::Lua, GameplayLanguage::Rust] {
            for template in [ProjectTemplate::Starter2D, ProjectTemplate::Starter3D] {
                let root = temp_root(&format!("starter-{language:?}-{template:?}"));
                let project =
                    Project::create_with_template(&root, "First Game", language, template)
                        .expect("create guided project");
                let reopened = Project::open(&root).expect("reopen guided project");
                assert_eq!(reopened.language(), language);
                let scene = engine_scene::Scene::load_from_file(&project.main_scene_path())
                    .expect("load generated scene");
                assert!(scene.entities.iter().any(|entity| entity.camera.is_some()));
                assert!(scene.entities.iter().any(|entity| !entity.rules.is_empty()));
                let assets = engine_scene::AssetLibrary::index(&project.assets_dir())
                    .expect("index generated assets");
                match template {
                    ProjectTemplate::Starter2D => {
                        let sprite = scene
                            .entities
                            .iter()
                            .find_map(|entity| entity.sprite.as_ref())
                            .expect("2D sprite");
                        let bytes = assets
                            .read(&sprite.atlas.id)
                            .expect("read sprite")
                            .expect("sprite asset exists");
                        engine_asset::import_texture_bytes(&bytes).expect("import starter PNG");
                    }
                    ProjectTemplate::Starter3D => {
                        let model = scene
                            .entities
                            .iter()
                            .find_map(|entity| entity.mesh_renderer.as_ref())
                            .expect("3D model");
                        assets
                            .import_mesh(&model.mesh.id)
                            .expect("import starter model")
                            .expect("model asset exists");
                    }
                    ProjectTemplate::Blank => unreachable!(),
                }
                assert!(root.join("START_HERE.md").is_file());
                cleanup(&root);
            }
        }
    }

    #[test]
    fn legacy_manifest_without_language_remains_rust() {
        let root = temp_root("legacy-language");
        std::fs::create_dir_all(&root).expect("root");
        let original = "(version: 1, name: \"Old\", main_scene: \"scenes/main.ron\")";
        std::fs::write(root.join(MANIFEST_FILE), original).expect("legacy manifest");
        let project = Project::open(&root).expect("open legacy");
        assert_eq!(project.language(), GameplayLanguage::Rust);
        assert_eq!(project.manifest().version, CURRENT_PROJECT_VERSION);
        assert_eq!(
            std::fs::read_to_string(root.join("project.ron.v1.bak")).expect("rollback copy"),
            original
        );
        assert_eq!(
            Project::open(&root).expect("reopen migrated").language(),
            GameplayLanguage::Rust
        );
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
    fn create_new_refuses_an_existing_directory_without_touching_it() {
        let root = temp_root("existing-directory");
        std::fs::create_dir_all(&root).expect("existing directory");
        let sentinel = root.join("keep.txt");
        std::fs::write(&sentinel, "keep").expect("sentinel");

        let err = Project::create_new(&root, "Demo").expect_err("existing root is rejected");
        assert!(matches!(err, ProjectError::AlreadyExists(_)));
        assert_eq!(
            std::fs::read_to_string(&sentinel).expect("sentinel survives"),
            "keep"
        );
        assert!(!root.join(MANIFEST_FILE).exists());

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
    fn save_manifest_rejects_an_escaping_main_scene() {
        let root = temp_root("save-escape");
        let mut project = Project::create(&root, "Save Escape").expect("create");
        project.manifest_mut().main_scene = PathBuf::from("../outside.ron");

        assert!(matches!(
            project.save_manifest(),
            Err(ProjectError::PathEscapesRoot(_))
        ));
        cleanup(&root);
    }

    #[cfg(unix)]
    #[test]
    fn resolve_rejects_a_symlink_that_leaves_the_project() {
        use std::os::unix::fs::symlink;

        let root = temp_root("symlink-escape");
        let outside = temp_root("symlink-target");
        let project = Project::create(&root, "Symlink Escape").expect("create");
        std::fs::create_dir_all(&outside).expect("outside directory");
        symlink(&outside, project.scenes_dir().join("outside")).expect("scene symlink");

        assert!(matches!(
            project.resolve(Path::new("scenes/outside/scene.ron")),
            Err(ProjectError::PathEscapesRoot(_))
        ));
        cleanup(&root);
        cleanup(&outside);
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
