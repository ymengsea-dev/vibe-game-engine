//! winit event-loop glue.
//!
//! This is the only module in the crate that touches `winit::` types
//! directly (besides [`crate::event`]'s pure mapping function). It owns
//! the event loop and the single OS window, and drives a
//! [`PlatformHandler`] supplied by the caller.

use std::sync::Arc;
use std::time::Instant;

use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::WindowId;

use crate::config::WindowConfig;
use crate::error::PlatformError;
use crate::event::map_window_event;

/// The OS window type, re-exported so callers (e.g. `engine_renderer`,
/// which needs it to create a wgpu surface) reference it as
/// `engine_platform::Window` instead of depending on `winit` directly.
pub use winit::window::Window;

/// Controls when the platform event loop requests another redraw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedrawPolicy {
    /// Sleep until the OS or a background producer wakes the event loop.
    Wait,
    /// Sleep until a UI timer needs a frame.
    WaitUntil(Instant),
    /// Request another frame as soon as this event-loop turn ends.
    Continuous,
}

#[derive(Debug, Clone, Copy)]
enum UserEvent {
    Wake,
}

/// Signals that background work has queued visible state for the window.
#[derive(Clone)]
pub struct WakeHandle(Arc<dyn Fn() -> bool + Send + Sync>);

impl WakeHandle {
    fn new(proxy: EventLoopProxy<UserEvent>) -> Self {
        Self(Arc::new(move || proxy.send_event(UserEvent::Wake).is_ok()))
    }

    /// Wakes the event loop so it renders the queued state once.
    ///
    /// Returns `false` only after the event loop has stopped.
    pub fn wake(&self) -> bool {
        (self.0)()
    }
}

/// Receives platform events for the engine's single window.
///
/// Implemented by whatever owns the running application (today: the
/// example `game` binary directly; later: `engine_core::App`).
pub trait PlatformHandler {
    /// Receives a handle background work can use to wake the event loop.
    fn set_wake_handle(&mut self, _wake: WakeHandle) {}

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

    /// Polled once per event-loop iteration (in `about_to_wait`, right
    /// before the next redraw is requested). Returning `true` exits the
    /// loop and closes the window — the only programmatic way out, since
    /// [`PlatformHandler`]'s other methods never see the event loop
    /// itself. Default: never exit (wait for the OS close request).
    fn should_exit(&self) -> bool {
        false
    }

    /// Selects whether the event loop waits, schedules a timer, or draws
    /// another frame after the current turn.
    fn redraw_policy(&self) -> RedrawPolicy {
        RedrawPolicy::Wait
    }

    /// Called when the OS asks the window to close (title-bar close
    /// button, `Cmd`/`Alt`+`F4`, ...). Return `true` to let the event
    /// loop exit now, `false` to keep running — e.g. to show an "unsaved
    /// changes" prompt first and exit later via
    /// [`PlatformHandler::should_exit`]. [`PlatformHandler::on_event`]
    /// still receives the [`crate::event::PlatformEvent::CloseRequested`]
    /// beforehand either way. Default: `true` (close immediately).
    /// Called once, immediately before the event loop exits, by either
    /// route: an OS close request the handler accepted, or
    /// [`PlatformHandler::should_exit`] returning `true`.
    ///
    /// The last point at which a handler can act — flushing a save, for
    /// instance. `should_exit` takes `&self` and so cannot, which is why
    /// this exists separately. Default: no-op.
    fn on_exit(&mut self) {}

    /// Called when the OS asks the window to close.
    fn on_close_requested(&mut self) -> bool {
        true
    }
}

/// Internal [`ApplicationHandler`] implementation that owns the window and
/// forwards translated events to a [`PlatformHandler`].
struct Runner<H: PlatformHandler> {
    config: WindowConfig,
    window: Option<Arc<Window>>,
    wake: WakeHandle,
    handler: H,
    /// Set when `resumed` fails to create the window. `resumed` has no way
    /// to return a `Result` to the caller, so it exits the loop and stashes
    /// the error here for `run_windowed` to surface afterward.
    pending_error: Option<PlatformError>,
}

impl<H: PlatformHandler> ApplicationHandler<UserEvent> for Runner<H> {
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
            ))
            .with_min_inner_size(winit::dpi::LogicalSize::new(
                self.config.min_width,
                self.config.min_height,
            ))
            .with_maximized(self.config.maximized);

        match event_loop.create_window(attributes) {
            Ok(window) => {
                let window = Arc::new(window);
                self.handler.set_wake_handle(self.wake.clone());
                self.handler.on_window_ready(Arc::clone(&window));
                window.request_redraw();
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
            if !matches!(event, WindowEvent::RedrawRequested) {
                window.request_redraw();
            }
        }

        let Some(mapped) = map_window_event(&event) else {
            return;
        };
        if mapped == crate::event::PlatformEvent::CloseRequested {
            self.handler.on_event(mapped);
            // The handler may veto the close (e.g. to prompt about unsaved
            // work); it can then exit later via `should_exit`.
            if self.handler.on_close_requested() {
                self.handler.on_exit();
                event_loop.exit();
            }
            return;
        }
        self.handler.on_event(mapped);
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Wake => {
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // A handler that has decided it's done (e.g. a game calling
        // `request_exit`) — the only programmatic way out of the loop.
        if self.handler.should_exit() {
            self.handler.on_exit();
            event_loop.exit();
            return;
        }
        match self.handler.redraw_policy() {
            RedrawPolicy::Wait => event_loop.set_control_flow(ControlFlow::Wait),
            RedrawPolicy::WaitUntil(deadline) => {
                event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
            }
            RedrawPolicy::Continuous => {
                event_loop.set_control_flow(ControlFlow::Poll);
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
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
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .map_err(|err| PlatformError::EventLoop(err.to_string()))?;
    let wake = WakeHandle::new(event_loop.create_proxy());
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut runner = Runner {
        config,
        window: None,
        wake,
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

#[cfg(test)]
mod tests {
    use super::*;

    struct StubHandler;
    impl PlatformHandler for StubHandler {
        fn on_window_ready(&mut self, _window: Arc<Window>) {}
        fn on_event(&mut self, _event: crate::event::PlatformEvent) {}
    }

    #[test]
    fn default_handler_never_exits_and_allows_close() {
        let mut handler = StubHandler;
        assert!(!handler.should_exit());
        assert!(
            handler.on_close_requested(),
            "the default must let the OS close the window"
        );
    }

    #[test]
    fn default_redraw_policy_waits_for_events() {
        let handler = StubHandler;
        assert_eq!(handler.redraw_policy(), RedrawPolicy::Wait);
    }

    #[test]
    fn continuous_redraw_policy_requests_the_next_frame() {
        struct ContinuousHandler;
        impl PlatformHandler for ContinuousHandler {
            fn on_window_ready(&mut self, _window: Arc<Window>) {}

            fn on_event(&mut self, _event: crate::event::PlatformEvent) {}

            fn redraw_policy(&self) -> RedrawPolicy {
                RedrawPolicy::Continuous
            }
        }

        assert_eq!(ContinuousHandler.redraw_policy(), RedrawPolicy::Continuous);
    }

    #[test]
    fn wake_handle_redraw_policy_queues_a_redraw_event() {
        let woke = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let expected = Arc::clone(&woke);
        let wake = WakeHandle(Arc::new(move || {
            expected.store(true, std::sync::atomic::Ordering::Relaxed);
            true
        }));
        assert!(wake.wake());
        assert!(woke.load(std::sync::atomic::Ordering::Relaxed));
    }
}
