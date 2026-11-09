//! Build & export: stage a native game binary, packed assets, and a scene.
//!
//! Rust projects compile their game crate with Cargo. Lua projects copy the
//! bundled player without invoking Cargo. Both pack `assets/` into
//! `assets.pak` and stage the selected scene beside the executable.
//!
//! Cross-compilation is out of scope — you export for the OS you build
//! on. "Windows and macOS first-class" means the binary name and the
//! `cargo` invocation are correct on both.

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
        Self {
            crate_name: crate_name.into(),
            profile,
            export_dir: export_dir.into(),
            target_dir: None,
            features: Vec::new(),
            assets_src: project.assets_dir(),
            scene_src: project.main_scene_path(),
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

        Self {
            crate_name: configuration.crate_name(project),
            profile: configuration.profile,
            export_dir: export_dir.into(),
            target_dir: None,
            features: configuration.features.clone(),
            assets_src: project.assets_dir(),
            scene_src,
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

    /// The binary file name (`<crate>` plus the host `.exe` suffix).
    pub fn binary_name(&self) -> String {
        format!("{}{}", self.crate_name, exe_suffix())
    }

    /// Where [`export`] copies the binary in the staged folder.
    pub fn binary_dest(&self) -> PathBuf {
        self.export_dir.join(self.binary_name())
    }

    /// Where [`export`] writes the packed asset bundle.
    pub fn bundle_dest(&self) -> PathBuf {
        self.export_dir.join(engine_scene::BUNDLE_FILE)
    }

    /// Where [`export`] copies the main scene.
    pub fn scene_dest(&self) -> PathBuf {
        self.export_dir.join(engine_scene::EXPORT_SCENE_FILE)
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
    /// The staged game binary.
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
    if project.language() == GameplayLanguage::Lua {
        let studio = std::env::current_exe()
            .map_err(|err| ProjectError::Build(format!("could not locate Studio: {err}")))?;
        let player = studio.with_file_name(format!("player{}", exe_suffix()));
        return export_lua(project, plan, &player);
    }
    tracing::info!(
        crate_name = %plan.crate_name,
        profile = plan.profile.label(),
        out = %plan.export_dir.display(),
        "starting export build"
    );

    let mut cargo = Command::new("cargo")
        .args(plan.cargo_args())
        .current_dir(project.root())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|err| ProjectError::Build(format!("could not launch cargo: {err}")))?;
    let mut built = BuiltExecutables::default();
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
    std::fs::create_dir_all(&plan.export_dir).map_err(|err| io(&plan.export_dir, err))?;
    // Invalidate an old export identity before replacing any game content.
    let marker = plan.export_dir.join(LUA_EXPORT_MARKER);
    if marker.exists() {
        std::fs::remove_file(&marker).map_err(|err| io(&marker, err))?;
    }

    let binary_dest = plan.binary_dest();
    std::fs::copy(compiled, &binary_dest).map_err(|err| io(&binary_dest, err))?;

    let bundle_dest = plan.bundle_dest();
    let stats = engine_asset::pack_dir_with_metadata(
        &plan.assets_src,
        &project.studio_dir().join("assets"),
        &bundle_dest,
    )
    .map_err(|err| ProjectError::Build(format!("packing assets: {err}")))?;

    let scene = if plan.scene_src.exists() {
        let dest = plan.scene_dest();
        std::fs::copy(&plan.scene_src, &dest).map_err(|err| io(&dest, err))?;
        Some(dest)
    } else {
        None
    };

    let scenes = project.resolve(Path::new(crate::SCENES_DIR))?;
    engine_asset::pack_dir(&scenes, &plan.export_dir.join(SCENE_BUNDLE_FILE))
        .map_err(|err| ProjectError::Build(format!("packing scenes: {err}")))?;
    let order = ExportedSceneOrder {
        start: project
            .manifest()
            .main_scene
            .to_string_lossy()
            .replace('\\', "/"),
        order: project.ordered_scenes(),
    };
    let order_path = plan.export_dir.join(SCENE_ORDER_FILE);
    let order_text = ron::ser::to_string_pretty(&order, ron::ser::PrettyConfig::default())
        .map_err(|err| ProjectError::Build(format!("writing the scene order: {err}")))?;
    std::fs::write(&order_path, order_text).map_err(|err| io(&order_path, err))?;
    if lua {
        std::fs::write(&marker, format!("2\n{}\n", project.manifest().project_id))
            .map_err(|err| io(&marker, err))?;
    }

    tracing::info!(
        binary = %binary_dest.display(),
        bundle_files = stats.files,
        "export complete"
    );

    Ok(ExportReport {
        binary: binary_dest,
        bundle: bundle_dest,
        bundle_files: stats.files,
        scene,
    })
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
        assert!(plan.export_dir.join(LUA_EXPORT_MARKER).is_file());
        assert_eq!(
            std::fs::read_to_string(plan.export_dir.join(LUA_EXPORT_MARKER)).expect("marker"),
            format!("2\n{}\n", project.manifest().project_id)
        );
        let content = engine_scene::open_export(&plan.export_dir)
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
        let packed =
            engine_asset::Bundle::open(&plan.export_dir.join("scenes.pak")).expect("packed scenes");
        assert!(packed.get("main.ron").is_some());
        assert!(packed.get("level2.ron").is_some());

        std::fs::remove_file(&second).expect("remove second source");
        export_lua(&project, &plan, &player).expect("re-export");
        let packed = engine_asset::Bundle::open(&plan.export_dir.join("scenes.pak"))
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
        let text = std::fs::read_to_string(plan.export_dir.join(SCENE_ORDER_FILE)).expect("file");
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

        let packed = engine_asset::Bundle::open(&plan.export_dir.join(SCENE_BUNDLE_FILE))
            .expect("packed scenes");
        assert!(packed.get("level2.ron").is_some());
        assert!(!plan.export_dir.join(LUA_EXPORT_MARKER).exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn failed_reexport_never_keeps_the_previous_games_identity() {
        let root = std::env::temp_dir().join(format!(
            "vge-lua-export-marker-failure-{}",
            std::process::id()
        ));
        let project = Project::create_with_language(&root, "New Game", GameplayLanguage::Lua)
            .expect("project");
        let binary = root.join("bundled-player");
        std::fs::write(&binary, b"player").expect("binary");
        let mut plan = ExportPlan::for_configuration(
            &project,
            &BuildConfiguration::new("Debug", BuildProfile::Debug),
            root.join("out"),
        );
        std::fs::create_dir_all(&plan.export_dir).expect("export dir");
        let marker = plan.export_dir.join(LUA_EXPORT_MARKER);
        std::fs::write(&marker, b"2\n00000000-0000-4000-8000-000000000001\n").expect("old marker");
        plan.assets_src = root.join("missing-assets");

        assert!(stage_export(&project, &plan, &binary, true).is_err());
        assert!(
            !marker.exists(),
            "old save identity must be invalidated first"
        );
        let _ = std::fs::remove_dir_all(root);
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

        assert_eq!(
            plan.binary_dest(),
            out.join(format!("game{}", exe_suffix()))
        );
        assert_eq!(plan.bundle_dest(), out.join("assets.pak"));
        assert_eq!(plan.scene_dest(), out.join("main.ron"));

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
        assert_eq!(
            report.scene.as_deref(),
            Some(project.root().join("export/main.ron").as_path())
        );
        assert!(report.scene.as_ref().is_some_and(|path| path.is_file()));
        let exported = engine_scene::open_export(&out)
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
