//! Managed build-and-run process for the studio's Play button.

use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};

use engine::editor::OutputLog;
use engine::project::build::ExportPlan;
use engine::project::{BuildConfiguration, Project};

/// Maximum queued child-output lines and maximum lines copied into the UI
/// during one frame. Bounding both prevents a noisy game from growing memory
/// indefinitely or monopolising the editor thread.
const OUTPUT_CAPACITY: usize = 2048;
const OUTPUT_DRAIN_PER_FRAME: usize = 256;

#[derive(Debug)]
enum ProcessMessage {
    Output(String),
}

#[derive(Debug)]
enum Phase {
    Idle,
    Building {
        child: Child,
        binary: PathBuf,
        project_root: PathBuf,
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
    sender: Option<SyncSender<ProcessMessage>>,
    receiver: Option<Receiver<ProcessMessage>>,
}

impl Default for PlayProcess {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            sender: None,
            receiver: None,
        }
    }
}

impl PlayProcess {
    /// Whether no build or game child is owned.
    pub(crate) fn is_idle(&self) -> bool {
        matches!(self.phase, Phase::Idle)
    }

    /// Starts compiling the selected project configuration without blocking
    /// the editor's render thread.
    pub(crate) fn start(
        &mut self,
        project: &Project,
        configuration: &BuildConfiguration,
        output: &OutputLog,
    ) -> Result<(), String> {
        if !matches!(self.phase, Phase::Idle) {
            return Err("a project game is already building or running".to_owned());
        }

        let plan = ExportPlan::for_configuration(
            project,
            configuration,
            project.studio_dir().join("play-output"),
        );
        let mut command = Command::new("cargo");
        command.args(plan.cargo_args()).current_dir(project.root());

        let (sender, receiver) = mpsc::sync_channel(OUTPUT_CAPACITY);
        output.write_line(format!(
            "[play] building '{}' ({})",
            plan.crate_name,
            configuration.profile.label()
        ));
        let child = spawn_captured(&mut command, &sender, "build")?;
        self.phase = Phase::Building {
            child,
            binary: plan.compiled_binary(),
            project_root: project.root().to_path_buf(),
        };
        self.sender = Some(sender);
        self.receiver = Some(receiver);
        Ok(())
    }

    /// Drains bounded output and advances build/game lifecycle state.
    pub(crate) fn poll(&mut self, output: &OutputLog) -> Option<PlayEvent> {
        self.drain_output(output);

        enum Transition {
            None,
            Launch { binary: PathBuf, root: PathBuf },
            BuildFailed(ExitStatus),
            GameExited(ExitStatus),
            PollFailed(String),
        }

        let transition = match &mut self.phase {
            Phase::Idle => Transition::None,
            Phase::Building {
                child,
                binary,
                project_root,
            } => match child.try_wait() {
                Ok(Some(status)) if status.success() => Transition::Launch {
                    binary: binary.clone(),
                    root: project_root.clone(),
                },
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
            Transition::Launch { binary, root } => {
                let Some(sender) = self.sender.as_ref() else {
                    return self.fail("play output channel disappeared".to_owned());
                };
                let mut command = Command::new(&binary);
                command.current_dir(root).env("RUST_BACKTRACE", "1");
                match spawn_captured(&mut command, sender, "game") {
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
                self.reset_channels();
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
        self.reset_channels();
        Ok(true)
    }

    fn drain_output(&mut self, output: &OutputLog) {
        let Some(receiver) = self.receiver.as_ref() else {
            return;
        };
        for _ in 0..OUTPUT_DRAIN_PER_FRAME {
            match receiver.try_recv() {
                Ok(ProcessMessage::Output(line)) => output.write_line(line),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
    }

    fn fail(&mut self, message: String) -> Option<PlayEvent> {
        let _ = terminate(&mut self.phase);
        self.phase = Phase::Idle;
        self.reset_channels();
        Some(PlayEvent::Failed(message))
    }

    fn reset_channels(&mut self) {
        self.receiver = None;
        self.sender = None;
    }

    #[cfg(test)]
    fn start_test_command(&mut self, command: &mut Command) -> Result<(), String> {
        let (sender, receiver) = mpsc::sync_channel(OUTPUT_CAPACITY);
        let child = spawn_captured(command, &sender, "test")?;
        self.phase = Phase::Running(child);
        self.sender = Some(sender);
        self.receiver = Some(receiver);
        Ok(())
    }
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
) -> Result<Child, String> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|err| format!("could not start {label}: {err}"))?;

    if let Some(stdout) = child.stdout.take() {
        spawn_reader(stdout, sender.clone(), label, "stdout");
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_reader(stderr, sender.clone(), label, "stderr");
    }
    Ok(child)
}

fn spawn_reader(
    stream: impl Read + Send + 'static,
    sender: SyncSender<ProcessMessage>,
    label: &'static str,
    stream_name: &'static str,
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
    use std::time::{Duration, Instant};

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
