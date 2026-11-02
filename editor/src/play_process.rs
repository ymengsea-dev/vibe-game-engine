//! Managed build-and-run process for the studio's Play button.

use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::thread::JoinHandle;

use engine::editor::OutputLog;
use engine::platform::WakeHandle;
use engine::project::build::{BuiltExecutables, ExportPlan};
use engine::project::{BuildConfiguration, GameplayLanguage, Project};
use engine::scripting::{LUA_DIAGNOSTIC_PREFIX, LuaScriptDiagnostic};

/// Maximum queued child-output lines and maximum lines copied into the UI
/// during one frame. Bounding both prevents a noisy game from growing memory
/// indefinitely or monopolising the editor thread.
const OUTPUT_CAPACITY: usize = 2048;
const OUTPUT_DRAIN_PER_FRAME: usize = 256;
const MAX_SCRIPT_DIAGNOSTICS: usize = 256;

#[derive(Debug)]
enum ProcessMessage {
    Output(String),
}

#[derive(Debug)]
enum Phase {
    Idle,
    Building {
        child: Child,
        /// Reads Cargo's JSON stdout until it closes, then hands back
        /// what was built; taken once the build exits.
        artifacts: Option<JoinHandle<BuiltExecutables>>,
        crate_name: String,
        project_root: PathBuf,
        scene: PathBuf,
        /// Extra environment for the game once it launches.
        env: Vec<(String, String)>,
    },
    Running(Child),
}

/// A state change reported while polling [`PlayProcess`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PlayEvent {
    /// Compilation finished and the game process opened successfully.
    Started,
    /// The game exited successfully on its own.
    Finished,
    /// Compilation, launch, or the running game failed.
    Failed(String),
}

/// Owns the Cargo build and game child processes used by Play mode.
///
/// Dropping the controller terminates an active child, so closing or
/// unexpectedly unwinding the editor cannot deliberately leave a game it
/// owns running in the background.
pub(crate) struct PlayProcess {
    phase: Phase,
    started_pending: bool,
    /// Whether this build has reported compiling anything. Cargo checks
    /// first and compiles only what changed, so a world-only edit never
    /// sets this.
    compiling: bool,
    sender: Option<SyncSender<ProcessMessage>>,
    receiver: Option<Receiver<ProcessMessage>>,
    script_diagnostics: Vec<LuaScriptDiagnostic>,
    wake: Option<WakeHandle>,
}

impl Default for PlayProcess {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            started_pending: false,
            compiling: false,
            sender: None,
            receiver: None,
            script_diagnostics: Vec::new(),
            wake: None,
        }
    }
}

impl PlayProcess {
    pub(crate) fn set_wake_handle(&mut self, wake: WakeHandle) {
        self.wake = Some(wake);
    }
    /// Whether no build or game child is owned.
    pub(crate) fn is_idle(&self) -> bool {
        matches!(self.phase, Phase::Idle)
    }

    /// Starts compiling the selected project configuration without blocking
    /// the editor's render thread. `env` is added to the game's environment
    /// (Play in the Scene View passes `VGE_EMBED` this way).
    pub(crate) fn start(
        &mut self,
        project: &Project,
        configuration: &BuildConfiguration,
        output: &OutputLog,
        env: &[(String, String)],
    ) -> Result<(), String> {
        if !matches!(self.phase, Phase::Idle) {
            return Err("a project game is already building or running".to_owned());
        }
        self.script_diagnostics.clear();
        self.compiling = false;

        if project.language() == GameplayLanguage::Lua {
            let player = bundled_player_binary()?;
            let mut command = lua_play_command(project, configuration, player.clone());
            command.envs(env.iter().map(|(key, value)| (key, value)));
            let (sender, receiver) = mpsc::sync_channel(OUTPUT_CAPACITY);
            let child = spawn_captured(&mut command, &sender, "game", self.wake.clone())?;
            output.write_line(format!("[play] launched {}", player.display()));
            self.phase = Phase::Running(child);
            self.started_pending = true;
            self.sender = Some(sender);
            self.receiver = Some(receiver);
            return Ok(());
        }

        let plan = ExportPlan::for_configuration(
            project,
            configuration,
            project.studio_dir().join("play-output"),
        );
        let mut command = Command::new("cargo");
        command.args(plan.cargo_args()).current_dir(project.root());

        output.write_line(format!(
            "[play] building '{}' ({})",
            plan.crate_name,
            configuration.profile.label()
        ));
        self.start_build(
            &mut command,
            plan.crate_name,
            project.root().to_path_buf(),
            plan.scene_src,
            env.to_vec(),
        )
    }

    /// Starts `command` (a `cargo build` with JSON messages), to launch the
    /// program it reports for `crate_name` once it succeeds.
    fn start_build(
        &mut self,
        command: &mut Command,
        crate_name: String,
        project_root: PathBuf,
        scene: PathBuf,
        env: Vec<(String, String)>,
    ) -> Result<(), String> {
        let (sender, receiver) = mpsc::sync_channel(OUTPUT_CAPACITY);
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|err| format!("could not start cargo: {err}"))?;
        // Progress and compiler errors are text on stderr; stdout carries
        // Cargo's JSON report of what it built, which is not for the log.
        if let Some(stderr) = child.stderr.take() {
            spawn_reader(stderr, sender.clone(), "build", "stderr", self.wake.clone());
        }
        let artifacts = match child.stdout.take() {
            Some(stdout) => Some(spawn_artifact_reader(stdout)?),
            None => None,
        };
        self.phase = Phase::Building {
            child,
            artifacts,
            crate_name,
            project_root,
            scene,
            env,
        };
        self.sender = Some(sender);
        self.receiver = Some(receiver);
        Ok(())
    }

    /// Drains bounded output and advances build/game lifecycle state.
    pub(crate) fn poll(&mut self, output: &OutputLog) -> Option<PlayEvent> {
        self.drain_output(output);
        if std::mem::take(&mut self.started_pending) {
            return Some(PlayEvent::Started);
        }

        enum Transition {
            None,
            Launch {
                binary: PathBuf,
                root: PathBuf,
                scene: PathBuf,
                env: Vec<(String, String)>,
            },
            BuildFailed(ExitStatus),
            GameExited(ExitStatus),
            PollFailed(String),
        }

        let transition = match &mut self.phase {
            Phase::Idle => Transition::None,
            Phase::Building {
                child,
                artifacts,
                crate_name,
                project_root,
                scene,
                env,
            } => match child.try_wait() {
                Ok(Some(status)) if status.success() => {
                    // Cargo has exited, so its stdout is closed and the
                    // reader finishes at once.
                    let built = artifacts
                        .take()
                        .ok_or_else(|| "cargo output was not captured".to_owned())
                        .and_then(|reader| {
                            reader
                                .join()
                                .map_err(|_| "reading cargo output failed".to_owned())
                        })
                        .and_then(|built| built.executable(crate_name));
                    match built {
                        Ok(binary) => Transition::Launch {
                            binary,
                            root: project_root.clone(),
                            scene: scene.clone(),
                            env: std::mem::take(env),
                        },
                        Err(message) => Transition::PollFailed(message),
                    }
                }
                Ok(Some(status)) => Transition::BuildFailed(status),
                Ok(None) => Transition::None,
                Err(err) => Transition::PollFailed(format!("could not poll Cargo: {err}")),
            },
            Phase::Running(child) => match child.try_wait() {
                Ok(Some(status)) => Transition::GameExited(status),
                Ok(None) => Transition::None,
                Err(err) => Transition::PollFailed(format!("could not poll the game: {err}")),
            },
        };

        match transition {
            Transition::None => None,
            Transition::Launch {
                binary,
                root,
                scene,
                env,
            } => {
                let Some(sender) = self.sender.as_ref() else {
                    return self.fail("play output channel disappeared".to_owned());
                };
                let mut command = rust_play_command(&binary, &root, &scene);
                command.envs(env);
                match spawn_captured(&mut command, sender, "game", self.wake.clone()) {
                    Ok(child) => {
                        output.write_line(format!("[play] launched {}", binary.display()));
                        self.phase = Phase::Running(child);
                        Some(PlayEvent::Started)
                    }
                    Err(message) => self.fail(message),
                }
            }
            Transition::BuildFailed(status) => {
                self.fail(format!("project build failed ({})", display_status(status)))
            }
            Transition::GameExited(status) if status.success() => {
                output.write_line(format!("[play] game exited ({})", display_status(status)));
                self.sender = None;
                self.phase = Phase::Idle;
                Some(PlayEvent::Finished)
            }
            Transition::GameExited(status) => {
                self.fail(format!("game failed ({})", display_status(status)))
            }
            Transition::PollFailed(message) => self.fail(message),
        }
    }

    /// Terminates an active build or game and waits for its process handle to
    /// be reaped. Returns whether anything was active.
    pub(crate) fn stop(&mut self, output: &OutputLog) -> Result<bool, String> {
        let active = !matches!(self.phase, Phase::Idle);
        if !active {
            return Ok(false);
        }

        terminate(&mut self.phase)?;
        self.drain_output(output);
        output.write_line("[play] stopped");
        self.phase = Phase::Idle;
        self.started_pending = false;
        self.sender = None;
        Ok(true)
    }

    /// Like [`PlayProcess::stop`], but first gives a running game up to
    /// `grace` to exit by itself (after Studio asked it to), so it can save.
    pub(crate) fn stop_within(
        &mut self,
        output: &OutputLog,
        grace: std::time::Duration,
    ) -> Result<bool, String> {
        if let Phase::Running(child) = &mut self.phase {
            let deadline = std::time::Instant::now() + grace;
            while std::time::Instant::now() < deadline {
                match child.try_wait() {
                    Ok(Some(_)) | Err(_) => break,
                    Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
                }
            }
        }
        self.stop(output)
    }

    fn drain_output(&mut self, output: &OutputLog) {
        let Some(receiver) = self.receiver.as_ref() else {
            return;
        };
        let mut disconnected = false;
        for _ in 0..OUTPUT_DRAIN_PER_FRAME {
            match receiver.try_recv() {
                Ok(ProcessMessage::Output(line)) => {
                    if let Some(diagnostic) = parse_script_diagnostic(&line) {
                        if self.script_diagnostics.len() < MAX_SCRIPT_DIAGNOSTICS {
                            self.script_diagnostics.push(diagnostic);
                        }
                    } else {
                        self.compiling |= is_compile_line(&line);
                        output.write_line(line);
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        if disconnected {
            self.receiver = None;
        }
    }

    /// Whether the current build is actually compiling code (not just
    /// checking that nothing changed).
    pub(crate) fn is_compiling(&self) -> bool {
        self.compiling && matches!(self.phase, Phase::Building { .. })
    }

    pub(crate) fn take_script_diagnostics(&mut self) -> Vec<LuaScriptDiagnostic> {
        std::mem::take(&mut self.script_diagnostics)
    }

    fn fail(&mut self, message: String) -> Option<PlayEvent> {
        let _ = terminate(&mut self.phase);
        self.phase = Phase::Idle;
        self.started_pending = false;
        self.sender = None;
        Some(PlayEvent::Failed(message))
    }

    #[cfg(test)]
    fn start_test_command(&mut self, command: &mut Command) -> Result<(), String> {
        let (sender, receiver) = mpsc::sync_channel(OUTPUT_CAPACITY);
        let child = spawn_captured(command, &sender, "test", self.wake.clone())?;
        self.phase = Phase::Running(child);
        self.sender = Some(sender);
        self.receiver = Some(receiver);
        Ok(())
    }
}

fn rust_play_command(binary: &PathBuf, root: &std::path::Path, scene: &std::path::Path) -> Command {
    let mut command = Command::new(binary);
    command
        .current_dir(root)
        .env("RUST_BACKTRACE", "1")
        .env("VGE_SCENE", scene);
    command
}

/// Whether `line` is Cargo starting to compile a crate.
fn is_compile_line(line: &str) -> bool {
    line.strip_prefix("[build:stderr] ")
        .is_some_and(|text| text.trim_start().starts_with("Compiling "))
}

fn parse_script_diagnostic(line: &str) -> Option<LuaScriptDiagnostic> {
    let payload = line
        .strip_prefix("[game:stderr] ")?
        .strip_prefix(LUA_DIAGNOSTIC_PREFIX)?;
    if payload.len() > 64 * 1024 {
        return None;
    }
    serde_json::from_str(payload).ok()
}

fn bundled_player_binary() -> Result<PathBuf, String> {
    let editor = std::env::current_exe()
        .map_err(|err| format!("could not locate Studio executable: {err}"))?;
    let player = editor.with_file_name(format!("player{}", std::env::consts::EXE_SUFFIX));
    if !player.is_file() {
        return Err(format!(
            "bundled player is missing at {}; install the complete Studio package or build the player binary",
            player.display()
        ));
    }
    Ok(player)
}

fn lua_play_command(
    project: &Project,
    configuration: &BuildConfiguration,
    player: PathBuf,
) -> Command {
    let plan = ExportPlan::for_configuration(
        project,
        configuration,
        project.studio_dir().join("play-output"),
    );
    let scene = plan
        .scene_src
        .strip_prefix(project.root())
        .unwrap_or(&project.manifest().main_scene);
    let mut command = Command::new(player);
    command
        .arg("--project")
        .arg(project.root())
        .arg("--scene")
        .arg(scene)
        .current_dir(project.root());
    command
}

impl Drop for PlayProcess {
    fn drop(&mut self) {
        let _ = terminate(&mut self.phase);
    }
}

fn spawn_captured(
    command: &mut Command,
    sender: &SyncSender<ProcessMessage>,
    label: &'static str,
    wake: Option<WakeHandle>,
) -> Result<Child, String> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|err| format!("could not start {label}: {err}"))?;

    if let Some(stdout) = child.stdout.take() {
        spawn_reader(stdout, sender.clone(), label, "stdout", wake.clone());
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_reader(stderr, sender.clone(), label, "stderr", wake);
    }
    Ok(child)
}

/// Collects the executables Cargo reports on `stdout` until it closes.
fn spawn_artifact_reader(
    stdout: impl Read + Send + 'static,
) -> Result<JoinHandle<BuiltExecutables>, String> {
    std::thread::Builder::new()
        .name("studio-build-artifacts".to_owned())
        .spawn(move || {
            let mut built = BuiltExecutables::default();
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                built.observe(&line);
            }
            built
        })
        .map_err(|err| format!("could not read cargo output: {err}"))
}

fn spawn_reader(
    stream: impl Read + Send + 'static,
    sender: SyncSender<ProcessMessage>,
    label: &'static str,
    stream_name: &'static str,
    wake: Option<WakeHandle>,
) {
    let thread_name = format!("studio-{label}-{stream_name}");
    let spawn = std::thread::Builder::new()
        .name(thread_name)
        .spawn(move || {
            for line in BufReader::new(stream).lines() {
                let text = match line {
                    Ok(text) => format!("[{label}:{stream_name}] {text}"),
                    Err(err) => format!("[{label}:{stream_name}] output read failed: {err}"),
                };
                if sender.send(ProcessMessage::Output(text)).is_err() {
                    break;
                }
                if let Some(wake) = &wake {
                    let _ = wake.wake();
                }
            }
        });
    if let Err(err) = spawn {
        engine::prelude::tracing::warn!(
            error = %err,
            stream = stream_name,
            "could not start output reader"
        );
    }
}

fn terminate(phase: &mut Phase) -> Result<(), String> {
    let child = match phase {
        Phase::Idle => return Ok(()),
        Phase::Building { child, .. } | Phase::Running(child) => child,
    };
    match child.try_wait() {
        Ok(Some(_)) => Ok(()),
        Ok(None) => {
            child
                .kill()
                .map_err(|err| format!("could not stop child process: {err}"))?;
            child
                .wait()
                .map_err(|err| format!("could not reap child process: {err}"))?;
            Ok(())
        }
        Err(err) => Err(format!("could not inspect child process: {err}")),
    }
}

fn display_status(status: ExitStatus) -> String {
    status.code().map_or_else(
        || "terminated by signal".to_owned(),
        |code| format!("exit code {code}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::project::GameplayLanguage;
    use std::time::{Duration, Instant};

    #[test]
    fn only_cargo_compile_lines_count_as_compiling() {
        assert!(is_compile_line(
            "[build:stderr]    Compiling island v0.1.0 (/games/island)"
        ));
        assert!(!is_compile_line(
            "[build:stderr]     Finished `dev` profile in 0.25s"
        ));
        assert!(
            !is_compile_line("[game:stderr] Compiling shaders"),
            "a game's own message"
        );
    }

    #[test]
    fn player_diagnostic_reaches_problem_queue_without_json_in_output() {
        let output = OutputLog::new();
        let mut controller = PlayProcess::default();
        let (sender, receiver) = mpsc::sync_channel(4);
        controller.receiver = Some(receiver);
        sender
            .send(ProcessMessage::Output(format!(
                "[game:stderr] {}{{\"path\":\"assets/scripts/main.lua\",\"line\":3,\"message\":\"bad rule\"}}",
                engine::scripting::LUA_DIAGNOSTIC_PREFIX
            )))
            .expect("send diagnostic");
        controller.drain_output(&output);
        let diagnostics = controller.take_script_diagnostics();
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].path, "assets/scripts/main.lua");
        assert_eq!(diagnostics[0].line, 3);
        assert!(output.snapshot().is_empty());
        sender
            .send(ProcessMessage::Output(format!(
                "[game:stderr] {}not-json",
                LUA_DIAGNOSTIC_PREFIX
            )))
            .expect("send malformed record");
        controller.drain_output(&output);
        assert!(controller.take_script_diagnostics().is_empty());
        assert!(
            output
                .snapshot()
                .iter()
                .any(|line| line.contains("not-json"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn player_diagnostic_survives_reader_delivery_after_fast_exit() {
        let output = OutputLog::new();
        let mut controller = PlayProcess::default();
        let mut child = Command::new("sh")
            .args(["-c", "exit 0"])
            .spawn()
            .expect("child");
        child.wait().expect("child exits");
        controller.phase = Phase::Running(child);
        let (sender, receiver) = mpsc::sync_channel(4);
        controller.sender = Some(sender.clone());
        controller.receiver = Some(receiver);
        assert_eq!(controller.poll(&output), Some(PlayEvent::Finished));
        sender
            .send(ProcessMessage::Output(format!(
                "[game:stderr] {}{{\"path\":\"assets/scripts/main.lua\",\"line\":2,\"message\":\"bad\"}}",
                LUA_DIAGNOSTIC_PREFIX
            )))
            .expect("reader can still deliver after process exit");
        controller.poll(&output);
        assert_eq!(controller.take_script_diagnostics()[0].line, 2);
    }

    fn poll_until_event(controller: &mut PlayProcess, output: &OutputLog) -> PlayEvent {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(event) = controller.poll(output) {
                return event;
            }
            assert!(Instant::now() < deadline, "child process did not finish");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn lua_play_uses_the_bundled_player_without_cargo() {
        let root = std::env::temp_dir().join(format!("vge-lua-play-{}", std::process::id()));
        let project = Project::create_with_language(&root, "Lua Play", GameplayLanguage::Lua)
            .expect("Lua project");
        let command = lua_play_command(
            &project,
            &BuildConfiguration::new("Debug", engine::project::build::BuildProfile::Debug),
            PathBuf::from("/studio/player"),
        );
        assert_eq!(command.get_program(), "/studio/player");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            vec![
                std::ffi::OsStr::new("--project"),
                project.root().as_os_str(),
                std::ffi::OsStr::new("--scene"),
                std::ffi::OsStr::new("scenes/main.ron"),
            ]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn lua_play_passes_the_selected_configuration_scene() {
        let root = std::env::temp_dir().join(format!("vge-lua-play-scene-{}", std::process::id()));
        let project = Project::create_with_language(&root, "Lua Play", GameplayLanguage::Lua)
            .expect("Lua project");
        std::fs::write(root.join("scenes/second.ron"), "(version: 5, entities: [])")
            .expect("scene");
        let mut configuration =
            BuildConfiguration::new("Debug", engine::project::build::BuildProfile::Debug);
        configuration.scene = Some(PathBuf::from("scenes/second.ron"));
        let command = lua_play_command(&project, &configuration, PathBuf::from("/studio/player"));
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            vec![
                std::ffi::OsStr::new("--project"),
                project.root().as_os_str(),
                std::ffi::OsStr::new("--scene"),
                std::ffi::OsStr::new("scenes/second.ron"),
            ]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn a_build_launches_the_program_cargo_reports() {
        let dir = std::env::temp_dir().join(format!("vge-play-artifact-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let game = dir.join("my-game");
        std::fs::write(
            &game,
            "#!/bin/sh\necho launched from $0 with $VGE_TEST_ENV\n",
        )
        .expect("game");
        std::process::Command::new("chmod")
            .arg("+x")
            .arg(&game)
            .status()
            .expect("chmod");
        let artifact = serde_json::json!({
            "reason": "compiler-artifact",
            "target": { "name": "my-game", "kind": ["bin"] },
            "executable": game,
        });
        let mut build = Command::new("sh");
        build.arg("-c").arg(format!(
            "echo '   Compiling my-game' >&2; echo '{artifact}'"
        ));
        let mut controller = PlayProcess::default();
        let output = OutputLog::new();
        controller
            .start_build(
                &mut build,
                "my-game".to_owned(),
                dir.clone(),
                dir.join("main.ron"),
                vec![("VGE_TEST_ENV".to_owned(), "passed".to_owned())],
            )
            .expect("build starts");
        assert_eq!(
            poll_until_event(&mut controller, &output),
            PlayEvent::Started
        );
        assert_eq!(
            poll_until_event(&mut controller, &output),
            PlayEvent::Finished
        );
        // The game's last line may still be on its way from the reader thread.
        let deadline = Instant::now() + Duration::from_secs(3);
        while !output
            .snapshot()
            .iter()
            .any(|line| line.contains("launched from"))
            && Instant::now() < deadline
        {
            controller.poll(&output);
            std::thread::sleep(Duration::from_millis(10));
        }
        let lines = output.snapshot();
        assert!(
            lines.iter().any(|line| line.contains("Compiling my-game")),
            "build progress reaches the Output tab: {lines:?}"
        );
        assert!(
            !lines.iter().any(|line| line.contains("compiler-artifact")),
            "cargo's JSON stays out of the log: {lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("launched from") && line.contains("with passed")),
            "the game runs with the extra environment: {lines:?}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rust_play_passes_the_selected_scene_without_arguments() {
        let root = std::env::temp_dir().join(format!("vge-rust-play-scene-{}", std::process::id()));
        let project = Project::create_with_language(&root, "Rust Play", GameplayLanguage::Rust)
            .expect("Rust project");
        std::fs::write(root.join("scenes/second.ron"), "(version: 5, entities: [])")
            .expect("scene");
        let mut configuration =
            BuildConfiguration::new("Debug", engine::project::build::BuildProfile::Debug);
        configuration.scene = Some(PathBuf::from("scenes/second.ron"));
        let plan = ExportPlan::for_configuration(&project, &configuration, root.join("out"));
        let command = rust_play_command(
            &PathBuf::from("/work/target/debug/rust_play"),
            project.root(),
            &plan.scene_src,
        );
        assert_eq!(
            command.get_args().count(),
            0,
            "legacy games keep their argument contract"
        );
        assert_eq!(
            command
                .get_envs()
                .find(|(key, _)| *key == "VGE_SCENE")
                .and_then(|(_, value)| value),
            Some(plan.scene_src.as_os_str())
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn play_runs_the_selected_project_process() {
        let output = OutputLog::new();
        let mut controller = PlayProcess::default();
        let mut command = Command::new("sh");
        command.args(["-c", "printf 'gameplay ran\\n'; sleep 0.1"]);
        controller
            .start_test_command(&mut command)
            .expect("launch test child");

        assert_eq!(
            poll_until_event(&mut controller, &output),
            PlayEvent::Finished
        );
        assert!(
            output
                .snapshot()
                .iter()
                .any(|line| line.contains("gameplay ran"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn system_failure_is_reported_with_its_output() {
        let output = OutputLog::new();
        let mut controller = PlayProcess::default();
        let mut command = Command::new("sh");
        command.args(["-c", "printf 'before failure\\n'; sleep 0.1; exit 7"]);
        controller
            .start_test_command(&mut command)
            .expect("launch test child");

        let event = poll_until_event(&mut controller, &output);
        assert!(matches!(event, PlayEvent::Failed(message) if message.contains("exit code 7")));
        assert!(
            output
                .snapshot()
                .iter()
                .any(|line| line.contains("before failure"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn stop_terminates_and_reaps_a_running_game() {
        let output = OutputLog::new();
        let mut controller = PlayProcess::default();
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30"]);
        controller
            .start_test_command(&mut command)
            .expect("launch test child");

        assert!(controller.stop(&output).expect("stop child"));
        assert!(!controller.stop(&output).expect("already stopped"));
    }

    #[cfg(windows)]
    #[test]
    fn system_failure_is_reported() {
        let output = OutputLog::new();
        let mut controller = PlayProcess::default();
        let mut command = Command::new("cmd");
        command.args(["/C", "exit 7"]);
        controller
            .start_test_command(&mut command)
            .expect("launch test child");
        assert!(matches!(
            poll_until_event(&mut controller, &output),
            PlayEvent::Failed(message) if message.contains("exit code 7")
        ));
    }
}
