//! Build & export: compile a project's game crate and stage a shippable
//! folder — the game binary, a packed asset bundle, and the main scene.
//!
//! [`ExportPlan`] is pure (all the paths and the exact `cargo` argument
//! list, testable without touching the toolchain); [`export`] runs it:
//! `cargo build` in the project root, then copy the binary, pack
//! `assets/` into `assets.pak` (see [`engine_asset::pack_dir`]), and
//! copy the main scene next to it.
//!
//! Cross-compilation is out of scope — you export for the OS you build
//! on. "Windows and macOS first-class" means the binary name and the
//! `cargo` invocation are correct on both.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::{BuildConfiguration, Project, ProjectError};

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
    /// `--target-dir` passed to cargo (defaults to the project's
    /// `target/`).
    pub target_dir: PathBuf,
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
    /// `export_dir`. `target_dir` defaults to `<project>/target`.
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
            target_dir: project.root().join("target"),
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
            target_dir: project.root().join("target"),
            features: configuration.features.clone(),
            assets_src: project.assets_dir(),
            scene_src,
        }
    }

    /// The exact `cargo` argument list this plan runs.
    pub fn cargo_args(&self) -> Vec<String> {
        let mut args = vec!["build".to_string()];
        if let Some(flag) = self.profile.cargo_flag() {
            args.push(flag.to_string());
        }
        args.push("-p".to_string());
        args.push(self.crate_name.clone());
        args.push("--target-dir".to_string());
        args.push(self.target_dir.display().to_string());
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

    /// Where cargo writes the compiled binary.
    pub fn compiled_binary(&self) -> PathBuf {
        self.target_dir
            .join(self.profile.target_subdir())
            .join(self.binary_name())
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
/// copy the binary → [`engine_asset::pack_dir`] the assets into
/// `assets.pak` → copy the main scene (if present).
///
/// # Errors
///
/// [`ProjectError::Build`] if cargo can't be launched or the compile
/// fails; [`ProjectError::Io`] if staging a file fails; the packing
/// error is wrapped in [`ProjectError::Build`].
pub fn export(project: &Project, plan: &ExportPlan) -> Result<ExportReport, ProjectError> {
    tracing::info!(
        crate_name = %plan.crate_name,
        profile = plan.profile.label(),
        out = %plan.export_dir.display(),
        "starting export build"
    );

    let status = Command::new("cargo")
        .args(plan.cargo_args())
        .current_dir(project.root())
        .status()
        .map_err(|err| ProjectError::Build(format!("could not launch cargo: {err}")))?;
    if !status.success() {
        return Err(ProjectError::Build(format!(
            "cargo build failed ({status})"
        )));
    }

    let compiled = plan.compiled_binary();
    if !compiled.exists() {
        return Err(ProjectError::Build(format!(
            "cargo reported success but {} is missing",
            compiled.display()
        )));
    }

    std::fs::create_dir_all(&plan.export_dir).map_err(|err| io(&plan.export_dir, err))?;

    let binary_dest = plan.binary_dest();
    std::fs::copy(&compiled, &binary_dest).map_err(|err| io(&binary_dest, err))?;

    let bundle_dest = plan.bundle_dest();
    let stats = engine_asset::pack_dir(&plan.assets_src, &bundle_dest)
        .map_err(|err| ProjectError::Build(format!("packing assets: {err}")))?;

    let scene = if plan.scene_src.exists() {
        let dest = plan.scene_dest();
        std::fs::copy(&plan.scene_src, &dest).map_err(|err| io(&dest, err))?;
        Some(dest)
    } else {
        None
    };

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
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_project() -> Project {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "vge-engine_project-build-{}-{n}",
            std::process::id()
        ));
        Project::create(&root, "Demo").expect("scaffold project")
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
        assert!(
            plan.compiled_binary().to_string_lossy().contains("release"),
            "the profile also decides where cargo puts the binary"
        );

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
        assert!(plan_rel.cargo_args().contains(&"--target-dir".to_string()));

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
        assert!(
            plan.compiled_binary()
                .starts_with(project.root().join("target/release"))
        );

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

        let out = project.root().join("export");
        let plan = ExportPlan::new(&project, "demo", BuildProfile::Debug, &out);
        let report = export(&project, &plan).expect("export game");

        assert!(report.binary.is_file(), "staged binary must exist");
        assert!(report.bundle.is_file(), "asset bundle must exist");
        assert_eq!(report.scene.as_deref(), Some(project.root().join("export/main.ron").as_path()));
        assert!(report.scene.as_ref().is_some_and(|path| path.is_file()));

        // Execute from the staged folder, with no editor or source tree on
        // the process' working path. A real exported game must stand alone.
        let run = std::process::Command::new(&report.binary)
            .current_dir(&out)
            .output()
            .expect("run staged game");
        assert!(run.status.success(), "staged game failed: {run:?}");
        assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "standalone game");
        std::fs::remove_dir_all(project.root()).ok();
    }
}
