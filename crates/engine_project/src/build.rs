//! Build & export: stage a native game binary, packed assets, and a scene.
//!
//! Rust projects compile their game crate with Cargo. Lua projects copy the
//! bundled player without invoking Cargo. Both pack `assets/` into
//! `assets.pak` and stage the selected scene beside the executable.
//!
//! Cross-compilation is out of scope — you export for the OS you build
//! on. "Windows and macOS first-class" means the binary name and the
//! `cargo` invocation are correct on both.
//!
//! The result is one thing a player can start ([`ExportLayout`]): on macOS
//! a `<Game>.app` bundle (program in `Contents/MacOS`, data and icon in
//! `Contents/Resources`), elsewhere a `<Game>` folder holding the program
//! and its data. A new export replaces an older export of the same project
//! there, and refuses to touch anything else.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::{BuildConfiguration, GameplayLanguage, Project, ProjectError};

/// Versioned marker read by the bundled player for Lua exports.
pub const LUA_EXPORT_MARKER: &str = "lua.game";
/// Legacy v1 marker; current exports write `2\n<project-id>\n`.
pub const LUA_EXPORT_VERSION: &[u8] = b"1\n";
/// Every authored scene, packed so exported games (Lua or Rust) can change scene.
/// `main.ron` is still written beside it for older players.
pub const SCENE_BUNDLE_FILE: &str = "scenes.pak";
/// Written beside [`SCENE_BUNDLE_FILE`]: the scene the game starts on and the
/// scene order, as an [`ExportedSceneOrder`].
pub const SCENE_ORDER_FILE: &str = "scene_order.ron";

/// Written beside [`SCENE_ORDER_FILE`]: project settings an exported game
/// starts with, as an [`ExportedSettings`].
pub const EXPORT_SETTINGS_FILE: &str = "settings.ron";

/// Project settings an exported game (Lua or Rust) starts with.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExportedSettings {
    /// The graphics quality the game starts at.
    #[serde(default)]
    pub quality: crate::QualityLevel,
    /// The window title. Empty in exports from before game settings, which
    /// keep the title their program sets.
    #[serde(default)]
    pub title: String,
    /// How the window opens. `None` in older exports.
    #[serde(default)]
    pub window: Option<crate::WindowSettings>,
    /// The window icon's file beside these settings, if one was exported.
    #[serde(default)]
    pub icon: Option<String>,
}

/// Marks an export Studio made, holding the project's id, so a new export
/// replaces only its own earlier result.
pub const EXPORT_IDENTITY_FILE: &str = ".rustyengine-export";
/// The window icon written beside the game's data.
pub const WINDOW_ICON_FILE: &str = "icon.png";
/// The macOS bundle icon, in `Contents/Resources`.
pub const BUNDLE_ICON_FILE: &str = "AppIcon.icns";

/// Which kind of system an export is laid out for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetOs {
    /// A `.app` bundle.
    MacOs,
    /// A folder with `<Game>.exe`.
    Windows,
    /// A folder with the program.
    Linux,
}

impl TargetOs {
    /// The system Studio is running on (Linux for anything not macOS or
    /// Windows).
    #[must_use]
    pub fn host() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}

/// Where an export's pieces go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportLayout {
    /// What a player starts or copies: `<Game>.app` or the `<Game>` folder.
    pub root: PathBuf,
    /// The program.
    pub executable: PathBuf,
    /// The folder the game's data goes in (beside the program, or
    /// `Contents/Resources`).
    pub data: PathBuf,
}

impl ExportLayout {
    /// The layout of an export called `name` inside `dir` for `os`.
    #[must_use]
    pub fn new(dir: &Path, name: &str, os: TargetOs) -> Self {
        let folder = crate::file_safe_name(name);
        match os {
            TargetOs::MacOs => {
                let root = dir.join(format!("{folder}.app"));
                let contents = root.join("Contents");
                Self {
                    executable: contents.join("MacOS").join(crate::slug(name)),
                    data: contents.join("Resources"),
                    root,
                }
            }
            TargetOs::Windows => {
                let root = dir.join(&folder);
                Self {
                    executable: root.join(format!("{folder}.exe")),
                    data: root.clone(),
                    root,
                }
            }
            TargetOs::Linux => {
                let root = dir.join(&folder);
                Self {
                    executable: root.join(crate::slug(name)),
                    data: root.clone(),
                    root,
                }
            }
        }
    }

    /// The same layout moved to `root`, for staging beside the real one.
    fn rebased(&self, root: &Path) -> Self {
        let move_path = |path: &Path| {
            path.strip_prefix(&self.root)
                .map_or_else(|_| path.to_path_buf(), |rest| root.join(rest))
        };
        Self {
            root: root.to_path_buf(),
            executable: move_path(&self.executable),
            data: move_path(&self.data),
        }
    }
}

/// What an exported game knows about its scene order.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExportedSceneOrder {
    /// The scene the game starts on.
    pub start: String,
    /// Every scene, in order.
    pub order: Vec<String>,
}

/// Which cargo profile to compile the export with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BuildProfile {
    /// `cargo build` — unoptimised, fast to produce, for testing an
    /// export locally.
    Debug,
    /// `cargo build --release` — optimised, what you ship.
    #[default]
    Release,
}

impl BuildProfile {
    /// The `target/<dir>` subdirectory cargo writes this profile to.
    pub fn target_subdir(self) -> &'static str {
        match self {
            BuildProfile::Debug => "debug",
            BuildProfile::Release => "release",
        }
    }

    /// The extra `cargo` flag for this profile, if any.
    fn cargo_flag(self) -> Option<&'static str> {
        match self {
            BuildProfile::Debug => None,
            BuildProfile::Release => Some("--release"),
        }
    }

    /// Human label for a toolbar / log line.
    pub fn label(self) -> &'static str {
        match self {
            BuildProfile::Debug => "Debug",
            BuildProfile::Release => "Release",
        }
    }
}

/// The executable file-name suffix for the host OS (`.exe` on Windows,
/// empty elsewhere).
pub fn exe_suffix() -> &'static str {
    std::env::consts::EXE_SUFFIX
}

/// Everything needed to compile and stage one export — all derived, no
/// I/O. Build it with [`ExportPlan::new`], inspect [`ExportPlan::cargo_args`]
/// and the `*_dest` paths, then hand it to [`export`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportPlan {
    /// The cargo package name of the game crate to build (`-p <name>`).
    pub crate_name: String,
    /// Which profile to compile.
    pub profile: BuildProfile,
    /// Directory the staged output is written into.
    pub export_dir: PathBuf,
    /// `--target-dir` passed to cargo. `None` (the default) leaves the
    /// choice to Cargo: the enclosing workspace's `target/`,
    /// `CARGO_TARGET_DIR`, or `.cargo/config.toml`. A game inside a
    /// workspace then reuses the engine build already there instead of
    /// compiling it again.
    pub target_dir: Option<PathBuf>,
    /// Cargo features to enable, from the configuration. Empty passes no
    /// `--features` flag at all, which is not the same as passing an
    /// empty one.
    pub features: Vec<String>,
    /// Directory whose contents get packed into the asset bundle.
    pub assets_src: PathBuf,
    /// The scene file copied next to the bundle for the runtime to load.
    pub scene_src: PathBuf,
    /// The name players see; names the `.app` or folder.
    pub game_name: String,
    /// The project's game settings (version, bundle id, icon, window).
    pub game: crate::GameSettings,
    /// The project's name, for defaults the settings leave open.
    pub project_name: String,
    /// The system the export is laid out for.
    pub os: TargetOs,
    /// Image used when the project sets no icon (Studio passes its own).
    pub fallback_icon: Option<Vec<u8>>,
}

impl ExportPlan {
    /// A plan to export `project`'s `crate_name` crate at `profile` into
    /// `export_dir`. Cargo chooses the target directory.
    pub fn new(
        project: &Project,
        crate_name: impl Into<String>,
        profile: BuildProfile,
        export_dir: impl Into<PathBuf>,
    ) -> Self {
        let manifest = project.manifest();
        Self {
            crate_name: crate_name.into(),
            profile,
            export_dir: export_dir.into(),
            target_dir: None,
            features: Vec::new(),
            assets_src: project.assets_dir(),
            scene_src: project.main_scene_path(),
            game_name: manifest.game.display_name(&manifest.name),
            game: manifest.game.clone(),
            project_name: manifest.name.clone(),
            os: TargetOs::host(),
            fallback_icon: None,
        }
    }

    /// A plan for one of the project's named configurations (see
    /// [`BuildConfiguration`]): its profile, its features, the crate it
    /// builds and the scene it ships.
    ///
    /// A configuration whose `scene` no longer exists falls back to the
    /// manifest's `main_scene` rather than staging a missing file —
    /// [`export`] would otherwise produce a game that starts on nothing.
    pub fn for_configuration(
        project: &Project,
        configuration: &BuildConfiguration,
        export_dir: impl Into<PathBuf>,
    ) -> Self {
        let scene_src = configuration
            .scene
            .as_ref()
            .and_then(|relative| project.resolve(relative).ok())
            .filter(|absolute| absolute.is_file())
            .unwrap_or_else(|| project.main_scene_path());

        let manifest = project.manifest();
        Self {
            crate_name: configuration.crate_name(project),
            profile: configuration.profile,
            export_dir: export_dir.into(),
            target_dir: None,
            features: configuration.features.clone(),
            assets_src: project.assets_dir(),
            scene_src,
            game_name: manifest.game.display_name(&manifest.name),
            game: manifest.game.clone(),
            project_name: manifest.name.clone(),
            os: TargetOs::host(),
            fallback_icon: None,
        }
    }

    /// The exact `cargo` argument list this plan runs. Cargo reports what
    /// it built as JSON lines on stdout (read them with [`BuiltExecutables`]);
    /// progress and compiler errors stay readable text on stderr.
    pub fn cargo_args(&self) -> Vec<String> {
        let mut args = vec!["build".to_string()];
        if let Some(flag) = self.profile.cargo_flag() {
            args.push(flag.to_string());
        }
        args.push("-p".to_string());
        args.push(self.crate_name.clone());
        args.push(CARGO_MESSAGE_FORMAT.to_string());
        if let Some(target_dir) = &self.target_dir {
            args.push("--target-dir".to_string());
            args.push(target_dir.display().to_string());
        }
        if !self.features.is_empty() {
            args.push("--features".to_string());
            args.push(self.features.join(","));
        }
        args
    }

    /// Where the export's pieces go inside [`ExportPlan::export_dir`].
    pub fn layout(&self) -> ExportLayout {
        ExportLayout::new(&self.export_dir, &self.game_name, self.os)
    }

    /// Where [`export`] copies the program.
    pub fn binary_dest(&self) -> PathBuf {
        self.layout().executable
    }

    /// Where [`export`] writes the packed asset bundle.
    pub fn bundle_dest(&self) -> PathBuf {
        self.layout().data.join(engine_scene::BUNDLE_FILE)
    }

    /// Where [`export`] copies the main scene.
    pub fn scene_dest(&self) -> PathBuf {
        self.layout().data.join(engine_scene::EXPORT_SCENE_FILE)
    }
}

/// Cargo's machine-readable output mode. `-render-diagnostics` keeps
/// compiler errors as the usual readable text on stderr.
const CARGO_MESSAGE_FORMAT: &str = "--message-format=json-render-diagnostics";

/// The programs one `cargo build` produced, read from its JSON messages.
///
/// Cargo says where every executable it built (or found up to date) lives,
/// so nothing has to guess the target directory, the profile folder or the
/// binary's file name — that works for any game crate, wherever its
/// workspace or `CARGO_TARGET_DIR` puts the output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuiltExecutables {
    /// `(binary target name, executable path)`, in the order Cargo reported.
    found: Vec<(String, PathBuf)>,
}

impl BuiltExecutables {
    /// Reads one line of Cargo's stdout, keeping any executable it reports.
    /// Lines that are not Cargo JSON are ignored.
    pub fn observe(&mut self, line: &str) {
        #[derive(Deserialize)]
        struct Message {
            reason: String,
            #[serde(default)]
            target: Option<Target>,
            #[serde(default)]
            executable: Option<PathBuf>,
        }
        #[derive(Deserialize)]
        struct Target {
            name: String,
            #[serde(default)]
            kind: Vec<String>,
        }
        let Ok(message) = serde_json::from_str::<Message>(line) else {
            return;
        };
        if message.reason != "compiler-artifact" {
            return;
        }
        if let (Some(target), Some(executable)) = (message.target, message.executable)
            && target.kind.iter().any(|kind| kind == "bin")
        {
            self.found.push((target.name, executable));
        }
    }

    /// The game program for `crate_name`: the binary named after the crate,
    /// or the crate's only binary.
    ///
    /// # Errors
    ///
    /// A readable message when the build produced no program, or several
    /// with none named after the crate.
    pub fn executable(&self, crate_name: &str) -> Result<PathBuf, String> {
        if let Some((_, path)) = self.found.iter().find(|(name, _)| name == crate_name) {
            return Ok(path.clone());
        }
        match self.found.as_slice() {
            [(_, path)] => Ok(path.clone()),
            [] => Err(format!(
                "cargo built no program for '{crate_name}'; does the crate have a src/main.rs?"
            )),
            several => Err(format!(
                "'{crate_name}' builds several programs ({}); name one of them '{crate_name}'",
                several
                    .iter()
                    .map(|(name, _)| name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }
}

/// What [`export`] produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportReport {
    /// What a player starts or copies: the `.app` bundle or the game folder.
    pub root: PathBuf,
    /// The staged game binary (run it to launch the game).
    pub binary: PathBuf,
    /// The staged `assets.pak`.
    pub bundle: PathBuf,
    /// How many files went into the bundle.
    pub bundle_files: usize,
    /// The staged scene file, if the source existed.
    pub scene: Option<PathBuf>,
}

fn io(path: &Path, err: std::io::Error) -> ProjectError {
    ProjectError::Io {
        path: path.to_path_buf(),
        source: err,
    }
}

/// Compiles `plan.crate_name` and stages the export folder.
///
/// Steps: `cargo build` (in the project root) → create `export_dir` →
/// copy the binary → [`engine_asset::pack_dir_with_metadata`] the assets into
/// `assets.pak` → copy the main scene (if present).
///
/// # Errors
///
/// [`ProjectError::Build`] if cargo can't be launched or the compile
/// fails; [`ProjectError::Io`] if staging a file fails; the packing
/// error is wrapped in [`ProjectError::Build`].
pub fn export(project: &Project, plan: &ExportPlan) -> Result<ExportReport, ProjectError> {
    export_logged(project, plan, &|line| tracing::info!("{line}"))
}

/// [`export`], reporting each step and Cargo's compiler output through
/// `log` as readable lines (called from more than one thread).
///
/// # Errors
///
/// As [`export`].
pub fn export_logged(
    project: &Project,
    plan: &ExportPlan,
    log: &(dyn Fn(&str) + Sync),
) -> Result<ExportReport, ProjectError> {
    let layout = plan.layout();
    log(&format!(
        "Exporting {} into {}",
        plan.game_name,
        layout.root.display()
    ));
    if project.language() == GameplayLanguage::Lua {
        let studio = std::env::current_exe()
            .map_err(|err| ProjectError::Build(format!("could not locate Studio: {err}")))?;
        let player = studio.with_file_name(format!("player{}", exe_suffix()));
        log("Packing the Lua game with the bundled player");
        return export_lua(project, plan, &player);
    }
    log(&format!(
        "Compiling {} ({})",
        plan.crate_name,
        plan.profile.label()
    ));

    let mut cargo = Command::new("cargo")
        .args(plan.cargo_args())
        .current_dir(project.root())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| ProjectError::Build(format!("could not launch cargo: {err}")))?;
    let mut built = BuiltExecutables::default();
    let stderr = cargo.stderr.take();
    let read = std::thread::scope(|scope| {
        // Compiler progress and errors, as they come.
        if let Some(stderr) = stderr {
            scope.spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    log(&line);
                }
            });
        }
        if let Some(stdout) = cargo.stdout.take() {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) => built.observe(&line),
                    Err(err) => {
                        return Err(ProjectError::Build(format!(
                            "could not read cargo output: {err}"
                        )));
                    }
                }
            }
        }
        Ok(())
    });
    read?;
    let status = cargo
        .wait()
        .map_err(|err| ProjectError::Build(format!("could not wait for cargo: {err}")))?;
    if !status.success() {
        return Err(ProjectError::Build(format!(
            "cargo build failed ({status})"
        )));
    }

    let compiled = built
        .executable(&plan.crate_name)
        .map_err(ProjectError::Build)?;
    if !compiled.exists() {
        return Err(ProjectError::Build(format!(
            "cargo reported success but {} is missing",
            compiled.display()
        )));
    }

    stage_export(project, plan, &compiled, false)
}

fn export_lua(
    project: &Project,
    plan: &ExportPlan,
    player: &Path,
) -> Result<ExportReport, ProjectError> {
    if !player.is_file() {
        return Err(ProjectError::Build(format!(
            "bundled player is missing at {}; install the complete Studio package",
            player.display()
        )));
    }
    let scene = engine_scene::Scene::load_from_file(&plan.scene_src)?;
    scene.validate()?;
    for script in std::iter::once("assets/scripts/main.lua").chain(
        scene
            .entities
            .iter()
            .filter_map(|entity| entity.script.as_deref()),
    ) {
        if !script.ends_with(".lua") {
            return Err(ProjectError::Build(format!(
                "Lua project cannot export Rust script {script}"
            )));
        }
        let path = project.resolve(Path::new(script))?;
        if !path.is_file() {
            return Err(ProjectError::Build(format!(
                "Lua script is missing: {script}"
            )));
        }
    }
    stage_export(project, plan, player, true)
}

fn stage_export(
    project: &Project,
    plan: &ExportPlan,
    compiled: &Path,
    lua: bool,
) -> Result<ExportReport, ProjectError> {
    plan.game
        .validate()
        .map_err(|reason| ProjectError::Build(format!("game settings: {reason}")))?;
    let layout = plan.layout();
    let identity = project.manifest().project_id.to_string();
    if layout.root.exists() && !is_own_export(&layout, &identity) {
        return Err(ProjectError::Build(format!(
            "{} already holds something that isn't an export of this project; \
             move it away or choose another folder",
            layout.root.display()
        )));
    }
    let mut staging_name = layout.root.as_os_str().to_owned();
    staging_name.push(".partial");
    let staging = layout.rebased(Path::new(&staging_name));
    if staging.root.exists() {
        // Left by an export that stopped half way; only ever Studio's.
        std::fs::remove_dir_all(&staging.root).map_err(|err| io(&staging.root, err))?;
    }
    let staged = stage_files(project, plan, &staging, compiled, lua, &identity);
    let stats = match staged {
        Ok(stats) => stats,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&staging.root);
            return Err(error);
        }
    };
    if layout.root.exists() {
        std::fs::remove_dir_all(&layout.root).map_err(|err| io(&layout.root, err))?;
    }
    std::fs::rename(&staging.root, &layout.root).map_err(|err| io(&layout.root, err))?;
    if plan.os == TargetOs::MacOs {
        sign_bundle(&layout.root);
    }

    tracing::info!(
        game = %layout.root.display(),
        bundle_files = stats.files,
        "export complete"
    );
    let scene =
        Some(layout.data.join(engine_scene::EXPORT_SCENE_FILE)).filter(|path| path.is_file());
    Ok(ExportReport {
        root: layout.root.clone(),
        binary: layout.executable.clone(),
        bundle: layout.data.join(engine_scene::BUNDLE_FILE),
        bundle_files: stats.files,
        scene,
    })
}

/// Whether `layout.root` holds an earlier export of the project `identity`.
fn is_own_export(layout: &ExportLayout, identity: &str) -> bool {
    std::fs::read_to_string(layout.data.join(EXPORT_IDENTITY_FILE))
        .is_ok_and(|text| text.trim() == identity)
}

/// Writes every file of the export into `layout`.
fn stage_files(
    project: &Project,
    plan: &ExportPlan,
    layout: &ExportLayout,
    compiled: &Path,
    lua: bool,
    identity: &str,
) -> Result<engine_asset::BundleStats, ProjectError> {
    let write = |path: &Path, bytes: &[u8]| -> Result<(), ProjectError> {
        std::fs::write(path, bytes).map_err(|err| io(path, err))
    };
    for dir in [&layout.root, &layout.data] {
        std::fs::create_dir_all(dir).map_err(|err| io(dir, err))?;
    }
    if let Some(parent) = layout.executable.parent() {
        std::fs::create_dir_all(parent).map_err(|err| io(parent, err))?;
    }
    std::fs::copy(compiled, &layout.executable).map_err(|err| io(&layout.executable, err))?;
    write(
        &layout.data.join(EXPORT_IDENTITY_FILE),
        format!("{identity}\n").as_bytes(),
    )?;

    let stats = engine_asset::pack_dir_with_metadata(
        &plan.assets_src,
        &project.studio_dir().join("assets"),
        &layout.data.join(engine_scene::BUNDLE_FILE),
    )
    .map_err(|err| ProjectError::Build(format!("packing assets: {err}")))?;

    if plan.scene_src.exists() {
        let dest = layout.data.join(engine_scene::EXPORT_SCENE_FILE);
        std::fs::copy(&plan.scene_src, &dest).map_err(|err| io(&dest, err))?;
    }

    let scenes = project.resolve(Path::new(crate::SCENES_DIR))?;
    engine_asset::pack_dir(&scenes, &layout.data.join(SCENE_BUNDLE_FILE))
        .map_err(|err| ProjectError::Build(format!("packing scenes: {err}")))?;
    let order = ExportedSceneOrder {
        start: project
            .manifest()
            .main_scene
            .to_string_lossy()
            .replace('\\', "/"),
        order: project.ordered_scenes(),
    };
    let order_text = ron::ser::to_string_pretty(&order, ron::ser::PrettyConfig::default())
        .map_err(|err| ProjectError::Build(format!("writing the scene order: {err}")))?;
    write(&layout.data.join(SCENE_ORDER_FILE), order_text.as_bytes())?;

    let icon = export_icon(project, plan, layout)?;
    let settings = ExportedSettings {
        quality: project.manifest().quality,
        title: plan.game_name.clone(),
        window: Some(plan.game.window),
        icon: icon.then(|| WINDOW_ICON_FILE.to_owned()),
    };
    let settings_text = ron::ser::to_string_pretty(&settings, ron::ser::PrettyConfig::default())
        .map_err(|err| ProjectError::Build(format!("writing the project settings: {err}")))?;
    write(
        &layout.data.join(EXPORT_SETTINGS_FILE),
        settings_text.as_bytes(),
    )?;
    if lua {
        write(
            &layout.data.join(LUA_EXPORT_MARKER),
            format!("2\n{}\n", project.manifest().project_id).as_bytes(),
        )?;
    }
    if plan.os == TargetOs::MacOs {
        let plist = info_plist(
            &plan.game_name,
            &plan.game.bundle_id(&plan.project_name),
            &plan.game.version,
            &layout
                .executable
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            icon,
        );
        write(
            &layout.root.join("Contents").join("Info.plist"),
            plist.as_bytes(),
        )?;
    }
    Ok(stats)
}

/// Writes the window icon (and on macOS the bundle icon) from the project's
/// icon image, or the plan's fallback. Returns whether an icon was written.
fn export_icon(
    project: &Project,
    plan: &ExportPlan,
    layout: &ExportLayout,
) -> Result<bool, ProjectError> {
    let source = match &plan.game.icon {
        Some(icon) => {
            let path = project.resolve(&Path::new(crate::ASSETS_DIR).join(icon))?;
            Some(std::fs::read(&path).map_err(|err| io(&path, err))?)
        }
        None => plan.fallback_icon.clone(),
    };
    let Some(source) = source else {
        return Ok(false);
    };
    let icon =
        crate::icon::square_icon(&source).map_err(|err| ProjectError::Build(err.to_string()))?;
    let png = crate::icon::png_at(&icon, crate::icon::WINDOW_ICON_SIZE)
        .map_err(|err| ProjectError::Build(err.to_string()))?;
    let path = layout.data.join(WINDOW_ICON_FILE);
    std::fs::write(&path, png).map_err(|err| io(&path, err))?;
    if plan.os == TargetOs::MacOs {
        let icns = crate::icon::icns(&icon).map_err(|err| ProjectError::Build(err.to_string()))?;
        let path = layout.data.join(BUNDLE_ICON_FILE);
        std::fs::write(&path, icns).map_err(|err| io(&path, err))?;
    }
    Ok(true)
}

/// `text` safe inside an XML element.
fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// A macOS bundle's `Info.plist` for the game.
fn info_plist(name: &str, bundle_id: &str, version: &str, executable: &str, icon: bool) -> String {
    let entry = |key: &str, value: &str| {
        format!(
            "\t<key>{}</key>\n\t<string>{}</string>\n",
            xml_escape(key),
            xml_escape(value)
        )
    };
    let mut body = String::new();
    body.push_str(&entry("CFBundleDevelopmentRegion", "en"));
    body.push_str(&entry("CFBundleDisplayName", name));
    body.push_str(&entry("CFBundleExecutable", executable));
    if icon {
        body.push_str(&entry("CFBundleIconFile", "AppIcon"));
    }
    body.push_str(&entry("CFBundleIdentifier", bundle_id));
    body.push_str(&entry("CFBundleInfoDictionaryVersion", "6.0"));
    body.push_str(&entry("CFBundleName", name));
    body.push_str(&entry("CFBundlePackageType", "APPL"));
    body.push_str(&entry("CFBundleShortVersionString", version));
    body.push_str(&entry("CFBundleVersion", version));
    body.push_str(&entry(
        "LSApplicationCategoryType",
        "public.app-category.games",
    ));
    body.push_str(&entry("LSMinimumSystemVersion", "11.0"));
    body.push_str("\t<key>NSHighResolutionCapable</key>\n\t<true/>\n");
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n{body}</dict>\n</plist>\n"
    )
}

/// Seals a macOS bundle with an ad-hoc signature, so macOS on Apple silicon
/// accepts the copied program inside it. A failure is logged, not fatal: the
/// game still runs on this Mac.
fn sign_bundle(bundle: &Path) {
    let codesign = Path::new("/usr/bin/codesign");
    if !codesign.is_file() {
        return;
    }
    match Command::new(codesign)
        .args(["--force", "--deep", "--sign", "-"])
        .arg(bundle)
        .output()
    {
        Ok(output) if output.status.success() => {}
        Ok(output) => tracing::warn!(
            stderr = %String::from_utf8_lossy(&output.stderr),
            "could not sign the game bundle"
        ),
        Err(error) => tracing::warn!(%error, "could not run codesign"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GameplayLanguage;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_project() -> Project {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "vge-engine_project-build-{}-{n}",
            std::process::id()
        ));
        Project::create_with_language(&root, "Demo", GameplayLanguage::Rust)
            .expect("scaffold Rust project")
    }

    #[test]
    fn lua_export_stages_player_scripts_and_scene_without_cargo() {
        let root = std::env::temp_dir().join(format!("vge-lua-export-{}", std::process::id()));
        let project = Project::create_with_language(&root, "Lua Export", GameplayLanguage::Lua)
            .expect("project");
        let player = root.join("bundled-player");
        std::fs::write(&player, b"player binary").expect("fake player");
        let plan = ExportPlan::for_configuration(
            &project,
            &BuildConfiguration::new("Debug", BuildProfile::Debug),
            root.join("out"),
        );
        let report = export_lua(&project, &plan, &player).expect("export Lua project");
        assert_eq!(
            std::fs::read(&report.binary).expect("binary"),
            b"player binary"
        );
        assert!(report.scene.as_ref().is_some_and(|path| path.is_file()));
        assert!(plan.layout().data.join(LUA_EXPORT_MARKER).is_file());
        assert_eq!(
            std::fs::read_to_string(plan.layout().data.join(LUA_EXPORT_MARKER)).expect("marker"),
            format!("2\n{}\n", project.manifest().project_id)
        );
        let content = engine_scene::open_export(&plan.layout().data)
            .expect("packed bundle")
            .expect("open export");
        assert!(
            content
                .library
                .read_path("scripts/main.lua")
                .expect("read")
                .is_some()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn lua_export_packs_additional_scenes_and_removes_stale_entries_on_reexport() {
        let root =
            std::env::temp_dir().join(format!("vge-lua-export-scenes-{}", std::process::id()));
        let project =
            Project::create_with_language(&root, "Scenes", GameplayLanguage::Lua).expect("project");
        let second = project.scenes_dir().join("level2.ron");
        engine_scene::Scene::default()
            .save_to_file(&second)
            .expect("second scene");
        let player = root.join("bundled-player");
        std::fs::write(&player, b"player binary").expect("fake player");
        let plan = ExportPlan::for_configuration(
            &project,
            &BuildConfiguration::new("Debug", BuildProfile::Debug),
            root.join("out"),
        );
        export_lua(&project, &plan, &player).expect("first export");
        let packed = engine_asset::Bundle::open(&plan.layout().data.join("scenes.pak"))
            .expect("packed scenes");
        assert!(packed.get("main.ron").is_some());
        assert!(packed.get("level2.ron").is_some());

        std::fs::remove_file(&second).expect("remove second source");
        export_lua(&project, &plan, &player).expect("re-export");
        let packed = engine_asset::Bundle::open(&plan.layout().data.join("scenes.pak"))
            .expect("repacked scenes");
        assert!(packed.get("level2.ron").is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_export_records_the_start_scene_and_the_scene_order() {
        let root = std::env::temp_dir().join(format!("vge-export-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut project =
            Project::create_with_language(&root, "Order", GameplayLanguage::Lua).expect("project");
        engine_scene::Scene::default()
            .save_to_file(&project.scenes_dir().join("level2.ron"))
            .expect("second scene");
        project.manifest_mut().scene_order = vec![
            PathBuf::from("scenes/level2.ron"),
            PathBuf::from("scenes/main.ron"),
        ];
        let player = root.join("bundled-player");
        std::fs::write(&player, b"player binary").expect("fake player");
        let plan = ExportPlan::for_configuration(
            &project,
            &BuildConfiguration::new("Debug", BuildProfile::Debug),
            root.join("out"),
        );
        export_lua(&project, &plan, &player).expect("export");
        let text =
            std::fs::read_to_string(plan.layout().data.join(SCENE_ORDER_FILE)).expect("file");
        let order: ExportedSceneOrder = ron::from_str(&text).expect("parse");
        assert_eq!(order.start, "scenes/main.ron");
        assert_eq!(order.order, ["scenes/level2.ron", "scenes/main.ron"]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rust_export_packs_scenes_for_scene_changes_without_a_lua_marker() {
        let root =
            std::env::temp_dir().join(format!("vge-rust-export-scenes-{}", std::process::id()));
        let project = Project::create_with_language(&root, "Rust Game", GameplayLanguage::Rust)
            .expect("project");
        engine_scene::Scene::default()
            .save_to_file(&project.scenes_dir().join("level2.ron"))
            .expect("second scene");
        let binary = root.join("compiled-game");
        std::fs::write(&binary, b"game").expect("binary");
        let plan = ExportPlan::for_configuration(
            &project,
            &BuildConfiguration::new("Debug", BuildProfile::Debug),
            root.join("out"),
        );

        stage_export(&project, &plan, &binary, false).expect("export");

        let packed = engine_asset::Bundle::open(&plan.layout().data.join(SCENE_BUNDLE_FILE))
            .expect("packed scenes");
        assert!(packed.get("level2.ron").is_some());
        assert!(!plan.layout().data.join(LUA_EXPORT_MARKER).exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_failed_reexport_keeps_the_previous_export_whole() {
        let root =
            std::env::temp_dir().join(format!("vge-lua-export-failure-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let project = Project::create_with_language(&root, "New Game", GameplayLanguage::Lua)
            .expect("project");
        let binary = root.join("bundled-player");
        std::fs::write(&binary, b"player").expect("binary");
        let mut plan = ExportPlan::for_configuration(
            &project,
            &BuildConfiguration::new("Debug", BuildProfile::Debug),
            root.join("out"),
        );
        let first = stage_export(&project, &plan, &binary, true).expect("first export");
        plan.assets_src = root.join("missing-assets");

        assert!(stage_export(&project, &plan, &binary, true).is_err());
        assert!(first.bundle.is_file(), "the earlier export is untouched");
        assert!(plan.layout().data.join(LUA_EXPORT_MARKER).is_file());
        let partial = std::fs::read_dir(&plan.export_dir)
            .expect("out")
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().ends_with(".partial"));
        assert!(!partial, "the half-made export is cleaned up");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_export_never_replaces_what_it_did_not_make() {
        let root = std::env::temp_dir().join(format!("vge-export-foreign-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let project =
            Project::create_with_language(&root, "Taken", GameplayLanguage::Lua).expect("project");
        let binary = root.join("bundled-player");
        std::fs::write(&binary, b"player").expect("binary");
        let plan = ExportPlan::for_configuration(
            &project,
            &BuildConfiguration::new("Debug", BuildProfile::Debug),
            root.join("out"),
        );
        let layout = plan.layout();
        std::fs::create_dir_all(&layout.root).expect("someone else's folder");
        std::fs::write(layout.root.join("notes.txt"), b"mine").expect("their file");
        let error = stage_export(&project, &plan, &binary, true).expect_err("refused");
        assert!(
            error
                .to_string()
                .contains("isn't an export of this project"),
            "{error}"
        );
        assert!(layout.root.join("notes.txt").is_file());

        // Another project's export there is refused too.
        std::fs::remove_file(layout.root.join("notes.txt")).expect("clear");
        std::fs::create_dir_all(&layout.data).expect("data");
        std::fs::write(layout.data.join(EXPORT_IDENTITY_FILE), "another-project\n").expect("id");
        assert!(stage_export(&project, &plan, &binary, true).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn exports_carry_the_game_name_window_and_icon() {
        let root = std::env::temp_dir().join(format!("vge-export-game-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut project =
            Project::create_with_language(&root, "Plain", GameplayLanguage::Lua).expect("project");
        let image = image::RgbaImage::from_pixel(8, 8, image::Rgba([10, 200, 10, 255]));
        image
            .save(project.assets_dir().join("icon.png"))
            .expect("icon");
        let game = &mut project.manifest_mut().game;
        game.name = Some("Moon & Co".into());
        game.icon = Some("icon.png".into());
        game.window.fullscreen = true;
        let binary = root.join("bundled-player");
        std::fs::write(&binary, b"player").expect("binary");
        let plan = ExportPlan::for_configuration(
            &project,
            &BuildConfiguration::new("Debug", BuildProfile::Debug),
            root.join("out"),
        );
        let report = stage_export(&project, &plan, &binary, true).expect("export");
        let layout = plan.layout();
        assert_eq!(report.root, layout.root);
        assert!(layout.root.ends_with(match TargetOs::host() {
            TargetOs::MacOs => "Moon Co.app",
            _ => "Moon Co",
        }));
        let settings: ExportedSettings = ron::from_str(
            &std::fs::read_to_string(layout.data.join(EXPORT_SETTINGS_FILE)).expect("settings"),
        )
        .expect("parse");
        assert_eq!(settings.title, "Moon & Co");
        assert!(settings.window.is_some_and(|window| window.fullscreen));
        assert_eq!(settings.icon.as_deref(), Some(WINDOW_ICON_FILE));
        assert!(layout.data.join(WINDOW_ICON_FILE).is_file());
        if TargetOs::host() == TargetOs::MacOs {
            assert!(layout.data.join(BUNDLE_ICON_FILE).is_file());
            let plist =
                std::fs::read_to_string(layout.root.join("Contents/Info.plist")).expect("plist");
            assert!(plist.contains("<string>Moon &amp; Co</string>"), "{plist}");
            assert!(
                plist.contains("<string>com.rustyengine.moon-co</string>"),
                "{plist}"
            );
            assert!(
                plist.contains("<string>Moon-Co</string>"),
                "executable: {plist}"
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn each_system_gets_its_own_layout() {
        let dir = Path::new("/out");
        let mac = ExportLayout::new(dir, "Space Cats", TargetOs::MacOs);
        assert_eq!(mac.root, dir.join("Space Cats.app"));
        assert_eq!(
            mac.executable,
            dir.join("Space Cats.app/Contents/MacOS/Space-Cats")
        );
        assert_eq!(mac.data, dir.join("Space Cats.app/Contents/Resources"));
        let windows = ExportLayout::new(dir, "Space Cats", TargetOs::Windows);
        assert_eq!(windows.executable, dir.join("Space Cats/Space Cats.exe"));
        assert_eq!(windows.data, dir.join("Space Cats"));
        let linux = ExportLayout::new(dir, "Space Cats", TargetOs::Linux);
        assert_eq!(linux.executable, dir.join("Space Cats/Space-Cats"));
    }

    #[test]
    fn lua_export_rejects_missing_starter_script() {
        let root =
            std::env::temp_dir().join(format!("vge-lua-export-missing-{}", std::process::id()));
        let project = Project::create_with_language(&root, "Lua Export", GameplayLanguage::Lua)
            .expect("project");
        std::fs::remove_file(root.join("assets/scripts/main.lua")).expect("remove starter");
        let player = root.join("bundled-player");
        std::fs::write(&player, b"player binary").expect("fake player");
        let plan = ExportPlan::for_configuration(
            &project,
            &BuildConfiguration::new("Debug", BuildProfile::Debug),
            root.join("out"),
        );
        assert!(matches!(
            export_lua(&project, &plan, &player),
            Err(ProjectError::Build(_))
        ));
        assert!(!plan.export_dir.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn lua_export_rejects_missing_attached_script() {
        let root = std::env::temp_dir().join(format!(
            "vge-lua-export-missing-attached-{}",
            std::process::id()
        ));
        let project = Project::create_with_language(&root, "Lua Export", GameplayLanguage::Lua)
            .expect("project");
        let mut scene = engine_scene::Scene::default();
        scene.entities.push(engine_scene::SceneEntity {
            script: Some("assets/scripts/missing.lua".into()),
            ..Default::default()
        });
        scene
            .save_to_file(&project.main_scene_path())
            .expect("scene");
        let player = root.join("bundled-player");
        std::fs::write(&player, b"player binary").expect("fake player");
        let plan = ExportPlan::for_configuration(
            &project,
            &BuildConfiguration::new("Debug", BuildProfile::Debug),
            root.join("out"),
        );
        assert!(
            matches!(export_lua(&project, &plan, &player), Err(ProjectError::Build(message)) if message.contains("missing.lua"))
        );
        assert!(!plan.export_dir.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn selected_configuration_drives_the_build_arguments() {
        let project = temp_project();
        let configuration = BuildConfiguration {
            name: "Shipping".to_owned(),
            profile: BuildProfile::Release,
            crate_name: Some("island".to_owned()),
            scene: None,
            features: vec!["steam".to_owned(), "no_console".to_owned()],
        };
        let plan =
            ExportPlan::for_configuration(&project, &configuration, project.root().join("out"));

        let args = plan.cargo_args();
        assert!(args.contains(&"--release".to_string()), "{args:?}");
        assert!(args.contains(&"island".to_string()), "{args:?}");
        let features = args
            .iter()
            .position(|a| a == "--features")
            .map(|i| args[i + 1].clone());
        assert_eq!(features, Some("steam,no_console".to_owned()));

        // A configuration that names no crate builds the project's own.
        let defaulted = BuildConfiguration::new("Debug", BuildProfile::Debug);
        let plan = ExportPlan::for_configuration(&project, &defaulted, project.root().join("out"));
        assert_eq!(plan.crate_name, "demo");
        assert!(
            !plan.cargo_args().contains(&"--features".to_string()),
            "no features means no flag, not an empty one"
        );
    }

    #[test]
    fn a_configuration_with_a_missing_scene_falls_back_to_the_main_scene() {
        let project = temp_project();
        let mut configuration = BuildConfiguration::new("Bench", BuildProfile::Debug);
        configuration.scene = Some(PathBuf::from("scenes/deleted.ron"));

        let plan =
            ExportPlan::for_configuration(&project, &configuration, project.root().join("out"));
        assert_eq!(
            plan.scene_src,
            project.main_scene_path(),
            "shipping a game that starts on a missing file helps nobody"
        );
    }

    fn artifact(name: &str, kind: &str, executable: Option<&str>) -> String {
        serde_json::json!({
            "reason": "compiler-artifact",
            "package_id": format!("path+file:///games/{name}#0.1.0"),
            "target": { "name": name, "kind": [kind] },
            "executable": executable,
            "fresh": true,
        })
        .to_string()
    }

    #[test]
    fn the_built_program_comes_from_cargo_not_a_guessed_path() {
        let mut built = BuiltExecutables::default();
        built.observe(&artifact("engine", "lib", None));
        built.observe(r#"{"reason":"build-finished","success":true}"#);
        built.observe("Compiling engine v0.1.0"); // Not JSON: ignored.
        assert!(built.executable("my-game").is_err(), "no program yet");

        built.observe(&artifact(
            "my-game",
            "bin",
            Some("/work/target/debug/my-game"),
        ));
        assert_eq!(
            built.executable("my-game"),
            Ok(PathBuf::from("/work/target/debug/my-game"))
        );

        // A crate whose only binary has another name still runs.
        let mut renamed = BuiltExecutables::default();
        renamed.observe(&artifact("launcher", "bin", Some("/t/debug/launcher")));
        assert_eq!(
            renamed.executable("my-game"),
            Ok(PathBuf::from("/t/debug/launcher"))
        );

        // Several binaries and none named after the crate: ask, don't guess.
        renamed.observe(&artifact("tool", "bin", Some("/t/debug/tool")));
        let message = renamed.executable("my-game").expect_err("ambiguous");
        assert!(message.contains("launcher, tool"), "{message}");
    }

    #[test]
    fn cargo_args_differ_by_profile() {
        let project = temp_project();
        let plan_dbg = ExportPlan::new(
            &project,
            "game",
            BuildProfile::Debug,
            project.root().join("out"),
        );
        let plan_rel = ExportPlan::new(
            &project,
            "game",
            BuildProfile::Release,
            project.root().join("out"),
        );

        assert_eq!(
            &plan_dbg.cargo_args()[..3],
            &["build".to_string(), "-p".to_string(), "game".to_string()]
        );
        assert!(plan_rel.cargo_args().contains(&"--release".to_string()));
        assert!(
            !plan_rel.cargo_args().contains(&"--target-dir".to_string()),
            "Cargo picks the target dir, so a workspace member reuses its build"
        );
        assert!(
            plan_rel
                .cargo_args()
                .contains(&CARGO_MESSAGE_FORMAT.to_string())
        );
        let mut forced = plan_rel.clone();
        forced.target_dir = Some(PathBuf::from("/tmp/shared-target"));
        let args = forced.cargo_args();
        let at = args.iter().position(|arg| arg == "--target-dir");
        assert_eq!(at.map(|i| args[i + 1].as_str()), Some("/tmp/shared-target"));

        std::fs::remove_dir_all(project.root()).ok();
    }

    #[test]
    fn staged_paths_are_under_the_export_dir() {
        let project = temp_project();
        let out = project.root().join("export");
        let plan = ExportPlan::new(&project, "game", BuildProfile::Release, &out);
        let layout = plan.layout();

        assert!(layout.root.starts_with(&out));
        assert_eq!(plan.binary_dest(), layout.executable);
        assert_eq!(plan.bundle_dest(), layout.data.join("assets.pak"));
        assert_eq!(plan.scene_dest(), layout.data.join("main.ron"));

        std::fs::remove_dir_all(project.root()).ok();
    }

    #[test]
    fn export_reports_a_missing_binary_rather_than_panicking() {
        // No cargo run here; call `export` against a plan whose crate
        // doesn't exist so cargo fails fast, and assert we get a
        // `Build` error, not a panic.
        let project = temp_project();
        let plan = ExportPlan::new(
            &project,
            "definitely-not-a-real-crate",
            BuildProfile::Debug,
            project.root().join("out"),
        );
        let result = export(&project, &plan);
        assert!(matches!(result, Err(ProjectError::Build(_))));
        std::fs::remove_dir_all(project.root()).ok();
    }

    #[test]
    fn export_builds_and_stages_a_standalone_binary() {
        let project = temp_project();
        std::fs::write(
            project.root().join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("write game manifest");
        std::fs::write(
            project.src_dir().join("main.rs"),
            "fn main() { println!(\"standalone game\"); }\n",
        )
        .expect("write game entry point");
        std::fs::create_dir_all(project.assets_dir().join("models")).expect("models directory");
        std::fs::write(project.assets_dir().join("models/character.gltf"), b"model")
            .expect("model");
        let metadata = project
            .studio_dir()
            .join("assets/models/character.gltf.meta");
        std::fs::create_dir_all(metadata.parent().expect("metadata parent"))
            .expect("metadata directory");
        std::fs::write(
            metadata,
            "(id: \"f2672902-8436-4652-a937-b8eaf99f3091\", source_hash: 0)",
        )
        .expect("metadata");

        let out = project.root().join("export");
        let plan = ExportPlan::new(&project, "demo", BuildProfile::Debug, &out);
        let report = export(&project, &plan).expect("export game");

        assert!(report.binary.is_file(), "staged binary must exist");
        assert!(report.bundle.is_file(), "asset bundle must exist");
        assert_eq!(report.scene, Some(plan.scene_dest()));
        assert!(report.scene.as_ref().is_some_and(|path| path.is_file()));
        let exported = engine_scene::open_export(&plan.layout().data)
            .expect("bundle exists")
            .expect("export opens");
        assert_eq!(
            exported.library.id_for_path("models/character.gltf"),
            Some("f2672902-8436-4652-a937-b8eaf99f3091")
        );

        // Execute from the staged folder, with no editor or source tree on
        // the process' working path. A real exported game must stand alone.
        let run = std::process::Command::new(&report.binary)
            .current_dir(&out)
            .output()
            .expect("run staged game");
        assert!(run.status.success(), "staged game failed: {run:?}");
        assert_eq!(
            String::from_utf8_lossy(&run.stdout).trim(),
            "standalone game"
        );
        std::fs::remove_dir_all(project.root()).ok();
    }
}
