//! Engine application lifecycle.

use crate::config::EngineConfig;
use crate::error::EngineError;

/// Lifecycle state of an [`App`].
///
/// ```text
/// Created --run()--> Running --shutdown requested--> ShuttingDown --> Stopped
/// Created ----------------------shutdown()------------------------> Stopped
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppState {
    /// `App` has been constructed but [`App::run`] has not been called.
    Created,
    /// [`App::run`] is actively driving ticks.
    Running,
    /// A shutdown has been requested; the current tick is finishing and no
    /// further ticks will run.
    ShuttingDown,
    /// The app has finished running (or was shut down before running) and
    /// will not run again.
    Stopped,
}

/// Owns engine configuration and drives the top-level application
/// lifecycle.
///
/// `App` does not itself define what a "tick" does — later milestones
/// (window creation, rendering) supply that. This keeps the lifecycle
/// skeleton decoupled from any particular event-loop shape, since window
/// integration will drive ticks from winit's own event loop rather than a
/// plain closure loop.
#[derive(Debug)]
pub struct App {
    config: EngineConfig,
    state: AppState,
}

impl App {
    /// Creates a new `App` from `config`.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::InvalidConfig`] if `config.app_name` is
    /// empty.
    ///
    /// # Example
    ///
    /// ```
    /// use engine_core::{App, EngineConfig};
    ///
    /// let app = App::new(EngineConfig::default())?;
    /// assert_eq!(app.state(), engine_core::AppState::Created);
    /// # Ok::<(), engine_core::EngineError>(())
    /// ```
    pub fn new(config: EngineConfig) -> Result<Self, EngineError> {
        if config.app_name.trim().is_empty() {
            return Err(EngineError::InvalidConfig(
                "app_name must not be empty".to_string(),
            ));
        }
        Ok(Self {
            config,
            state: AppState::Created,
        })
    }

    /// Current lifecycle state.
    pub fn state(&self) -> AppState {
        self.state
    }

    /// The config this app was created with.
    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    /// Whether a shutdown has been requested (via [`App::request_shutdown`]
    /// or reached through [`App::shutdown`]).
    pub fn is_shutdown_requested(&self) -> bool {
        matches!(self.state, AppState::ShuttingDown | AppState::Stopped)
    }

    /// Requests that the running loop stop after the current tick.
    ///
    /// No-op if the app is not currently [`AppState::Running`].
    pub fn request_shutdown(&mut self) {
        if self.state == AppState::Running {
            self.state = AppState::ShuttingDown;
        }
    }

    /// Immediately transitions the app to [`AppState::Stopped`], skipping
    /// [`AppState::Running`] entirely if `run` was never called.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::AlreadyStopped`] if the app is already
    /// stopped.
    pub fn shutdown(&mut self) -> Result<(), EngineError> {
        if self.state == AppState::Stopped {
            return Err(EngineError::AlreadyStopped);
        }
        self.state = AppState::Stopped;
        tracing::info!(app_name = %self.config.app_name, "app stopped");
        Ok(())
    }

    /// Drives the app's main loop, calling `tick` once per iteration until
    /// a shutdown is requested (from inside `tick`, via
    /// [`App::request_shutdown`]).
    ///
    /// On return the app is [`AppState::Stopped`].
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::AlreadyRunning`] if the app is not currently
    /// [`AppState::Created`] (i.e. `run` was already called, or the app was
    /// already shut down).
    ///
    /// # Example
    ///
    /// ```
    /// use engine_core::{App, EngineConfig};
    ///
    /// let mut app = App::new(EngineConfig::default())?;
    /// let mut ticks = 0;
    /// app.run(|app| {
    ///     ticks += 1;
    ///     if ticks == 3 {
    ///         app.request_shutdown();
    ///     }
    /// })?;
    /// assert_eq!(ticks, 3);
    /// # Ok::<(), engine_core::EngineError>(())
    /// ```
    pub fn run<F>(&mut self, mut tick: F) -> Result<(), EngineError>
    where
        F: FnMut(&mut App),
    {
        if self.state != AppState::Created {
            return Err(EngineError::AlreadyRunning);
        }
        tracing::info!(app_name = %self.config.app_name, "app starting");
        self.state = AppState::Running;
        while self.state == AppState::Running {
            tick(self);
        }
        self.state = AppState::Stopped;
        tracing::info!(app_name = %self.config.app_name, "app stopped");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_rejects_empty_app_name() {
        let config = EngineConfig::new("   ", "0.1.0");
        let err = App::new(config).unwrap_err();
        assert!(matches!(err, EngineError::InvalidConfig(_)));
    }

    #[test]
    fn new_accepts_valid_config() {
        let app = App::new(EngineConfig::default()).unwrap();
        assert_eq!(app.state(), AppState::Created);
        assert_eq!(app.config().app_name, "VGE Game");
    }

    #[test]
    fn run_ticks_until_shutdown_requested() {
        let mut app = App::new(EngineConfig::default()).unwrap();
        let mut ticks = 0;
        app.run(|app| {
            ticks += 1;
            if ticks == 5 {
                app.request_shutdown();
            }
        })
        .unwrap();
        assert_eq!(ticks, 5);
        assert_eq!(app.state(), AppState::Stopped);
    }

    #[test]
    fn run_twice_errors_already_running() {
        let mut app = App::new(EngineConfig::default()).unwrap();
        app.run(|app| app.request_shutdown()).unwrap();
        let err = app.run(|_| {}).unwrap_err();
        assert!(matches!(err, EngineError::AlreadyRunning));
    }

    #[test]
    fn shutdown_before_run_then_run_errors_already_running() {
        let mut app = App::new(EngineConfig::default()).unwrap();
        app.shutdown().unwrap();
        assert_eq!(app.state(), AppState::Stopped);
        let err = app.run(|_| {}).unwrap_err();
        assert!(matches!(err, EngineError::AlreadyRunning));
    }

    #[test]
    fn shutdown_twice_errors_already_stopped() {
        let mut app = App::new(EngineConfig::default()).unwrap();
        app.shutdown().unwrap();
        let err = app.shutdown().unwrap_err();
        assert!(matches!(err, EngineError::AlreadyStopped));
    }

    #[test]
    fn request_shutdown_is_noop_before_running() {
        let mut app = App::new(EngineConfig::default()).unwrap();
        app.request_shutdown();
        assert_eq!(app.state(), AppState::Created);
        assert!(!app.is_shutdown_requested());
    }

    #[test]
    fn is_shutdown_requested_true_while_shutting_down_and_stopped() {
        let mut app = App::new(EngineConfig::default()).unwrap();
        let mut ticks = 0;
        app.run(|app| {
            ticks += 1;
            if ticks == 2 {
                app.request_shutdown();
                assert!(app.is_shutdown_requested());
            }
        })
        .unwrap();
        assert!(app.is_shutdown_requested());
    }
}
