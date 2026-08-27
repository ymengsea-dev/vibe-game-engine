//! winit event-loop glue.
//!
//! This is the only module in the crate that touches `winit::` types
//! directly (besides [`crate::event`]'s pure mapping function). It owns
//! the event loop and the single OS window, and drives a
//! [`PlatformHandler`] supplied by the caller.

use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::WindowId;

use crate::config::WindowConfig;
use crate::error::PlatformError;
use crate::event::map_window_event;

/// The OS window type, re-exported so callers (e.g. `engine_renderer`,
/// which needs it to create a wgpu surface) reference it as
/// `engine_platform::Window` instead of depending on `winit` directly.
pub use winit::window::Window;

/// Receives platform events for the engine's single window.
///
/// Implemented by whatever owns the running application (today: the
/// example `game` binary directly; later: `engine_core::App`).
pub trait PlatformHandler {
    /// Called once the OS window has been created and is ready to render
    /// into. `window` is cheaply cloneable ([`Arc`]) so the handler can
    /// hand it to a renderer without borrowing issues.
    fn on_window_ready(&mut self, window: Arc<Window>);

    /// Called for each platform event the engine surfaces.
    fn on_event(&mut self, event: crate::event::PlatformEvent);

    /// Called with every raw winit window event, before it's translated
    /// into a [`crate::event::PlatformEvent`] and passed to
    /// [`PlatformHandler::on_event`]. Default no-op.
    ///
    /// [`crate::event::PlatformEvent`] deliberately drops most of winit's
    /// detail (that's the point of the translation) — this exists for
    /// consumers that need the full, untranslated event themselves, e.g.
    /// `engine_editor`'s egui integration, which `egui-winit`'s own event
    /// handling needs directly rather than through any intermediate
    /// representation.
    fn on_raw_window_event(&mut self, _window: &Window, _event: &WindowEvent) {}
}

/// Internal [`ApplicationHandler`] implementation that owns the window and
/// forwards translated events to a [`PlatformHandler`].
struct Runner<H: PlatformHandler> {
    config: WindowConfig,
    window: Option<Arc<Window>>,
    handler: H,
    /// Set when `resumed` fails to create the window. `resumed` has no way
    /// to return a `Result` to the caller, so it exits the loop and stashes
    /// the error here for `run_windowed` to surface afterward.
    pending_error: Option<PlatformError>,
}

impl<H: PlatformHandler> ApplicationHandler for Runner<H> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            // Already created (e.g. resumed fired again after a suspend on
            // mobile/web); nothing to do.
            return;
        }

        let attributes = Window::default_attributes()
            .with_title(self.config.title.clone())
            .with_inner_size(winit::dpi::LogicalSize::new(
                self.config.width,
                self.config.height,
            ));

        match event_loop.create_window(attributes) {
            Ok(window) => {
                let window = Arc::new(window);
                self.handler.on_window_ready(Arc::clone(&window));
                self.window = Some(window);
            }
            Err(err) => {
                tracing::error!(error = %err, "failed to create window");
                self.pending_error = Some(PlatformError::WindowCreation(err.to_string()));
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if let Some(window) = &self.window {
            self.handler.on_raw_window_event(window, &event);
        }

        let Some(mapped) = map_window_event(&event) else {
            return;
        };
        if mapped == crate::event::PlatformEvent::CloseRequested {
            event_loop.exit();
        }
        self.handler.on_event(mapped);
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // Drives a continuous render loop: ask for another redraw as soon
        // as the event queue is drained. Presentation pacing (vsync) comes
        // from the surface's present mode, not from throttling here.
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}

/// Creates an OS window per `config` and runs the platform event loop,
/// forwarding events to `handler` until the window is closed.
///
/// This call blocks the calling thread until the event loop exits (window
/// closed, or the OS asks the process to quit). On most platforms it must
/// be called from the main thread.
///
/// # Errors
///
/// Returns [`PlatformError::EventLoop`] if the OS event loop cannot be
/// created or exits with an OS-level error, or [`PlatformError::WindowCreation`]
/// if the OS refused to create the window (surfaced after the loop exits,
/// since window creation happens inside an OS callback that has no
/// `Result` to return to).
///
/// # Example
///
/// ```no_run
/// use engine_platform::{run_windowed, PlatformEvent, PlatformHandler, WindowConfig};
/// use std::sync::Arc;
///
/// struct NoopHandler;
/// impl PlatformHandler for NoopHandler {
///     fn on_window_ready(&mut self, _window: Arc<engine_platform::Window>) {}
///     fn on_event(&mut self, _event: PlatformEvent) {}
/// }
///
/// // `no_run`: opens a real OS window and blocks until closed.
/// run_windowed(WindowConfig::default(), NoopHandler)?;
/// # Ok::<(), engine_platform::PlatformError>(())
/// ```
pub fn run_windowed(
    config: WindowConfig,
    handler: impl PlatformHandler + 'static,
) -> Result<(), PlatformError> {
    let event_loop = EventLoop::new().map_err(|err| PlatformError::EventLoop(err.to_string()))?;
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut runner = Runner {
        config,
        window: None,
        handler,
        pending_error: None,
    };

    event_loop
        .run_app(&mut runner)
        .map_err(|err| PlatformError::EventLoop(err.to_string()))?;

    match runner.pending_error {
        Some(err) => Err(err),
        None => Ok(()),
    }
}
