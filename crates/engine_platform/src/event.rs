//! Platform events, decoupled from winit's event types.
//!
//! Only [`crate::app`] (the winit glue) knows about `winit::event::*`
//! directly; everything else in the engine sees this minimal, engine-owned
//! enum. [`KeyCode`] and [`MouseButton`] are re-exported from `winit`
//! as-is (hundreds of variants, no engine-specific meaning to add) rather
//! than reinvented.

/// A mouse button. Re-exported from `winit`.
pub use winit::event::MouseButton;
/// Physical key location, independent of keyboard layout. Re-exported from
/// `winit` — see [`winit::keyboard::KeyCode`] for the full variant list.
pub use winit::keyboard::KeyCode;

/// A platform-level event delivered to a [`crate::PlatformHandler`].
///
/// Not `Eq` (holds `f32`/`f64` payloads for cursor/scroll events).
/// Not `Copy`: [`PlatformEvent::FileDropped`] carries an owned path.
/// Every other variant is trivially copyable, so `Clone` costs nothing
/// for them.
#[derive(Debug, Clone, PartialEq)]
pub enum PlatformEvent {
    /// The window was resized to the given physical pixel size.
    Resized {
        /// New width in physical pixels.
        width: u32,
        /// New height in physical pixels.
        height: u32,
    },
    /// The window system asked the window to redraw itself.
    RedrawRequested,
    /// The user requested the window be closed (e.g. clicked the close
    /// button). The event loop exits right after this is delivered.
    CloseRequested,
    /// A keyboard key changed state.
    KeyboardInput {
        /// Which physical key.
        key: KeyCode,
        /// `true` if pressed, `false` if released.
        pressed: bool,
        /// `true` if this is an OS auto-repeat event from holding the key
        /// down, not a fresh press.
        repeat: bool,
    },
    /// A mouse button changed state.
    MouseButtonInput {
        /// Which button.
        button: MouseButton,
        /// `true` if pressed, `false` if released.
        pressed: bool,
    },
    /// The cursor moved, in physical pixels relative to the window's
    /// top-left corner.
    CursorMoved {
        /// New cursor X position.
        x: f64,
        /// New cursor Y position.
        y: f64,
    },
    /// The mouse wheel (or touchpad scroll) moved.
    MouseWheel {
        /// Horizontal scroll delta (lines, or approximated from pixels).
        delta_x: f32,
        /// Vertical scroll delta (lines, or approximated from pixels).
        delta_y: f32,
    },
    /// A file was dragged from the desktop and dropped on the window.
    ///
    /// One event per file: dropping a selection of three delivers three.
    FileDropped {
        /// Absolute path to the dropped file.
        path: std::path::PathBuf,
    },
}

/// Translates a raw winit window event into a [`PlatformEvent`], or `None`
/// for events this engine doesn't surface (yet).
///
/// Pulled out as a pure function (no event loop involved) so the mapping
/// is unit-testable without spinning up a real OS window.
pub(crate) fn map_window_event(event: &winit::event::WindowEvent) -> Option<PlatformEvent> {
    use winit::event::WindowEvent as WE;

    match event {
        WE::CloseRequested => Some(PlatformEvent::CloseRequested),
        WE::Resized(size) => Some(PlatformEvent::Resized {
            width: size.width,
            height: size.height,
        }),
        WE::RedrawRequested => Some(PlatformEvent::RedrawRequested),
        WE::DroppedFile(path) => Some(PlatformEvent::FileDropped { path: path.clone() }),
        WE::KeyboardInput { event, .. } => {
            map_physical_key(event.physical_key, event.state.is_pressed(), event.repeat)
        }
        WE::MouseInput { state, button, .. } => Some(PlatformEvent::MouseButtonInput {
            button: *button,
            pressed: state.is_pressed(),
        }),
        WE::CursorMoved { position, .. } => Some(PlatformEvent::CursorMoved {
            x: position.x,
            y: position.y,
        }),
        WE::MouseWheel { delta, .. } => {
            let (delta_x, delta_y) = match *delta {
                winit::event::MouseScrollDelta::LineDelta(x, y) => (x, y),
                winit::event::MouseScrollDelta::PixelDelta(pos) => (pos.x as f32, pos.y as f32),
            };
            Some(PlatformEvent::MouseWheel { delta_x, delta_y })
        }
        _ => None,
    }
}

/// Maps a raw `PhysicalKey` + press state to a [`PlatformEvent`], or `None`
/// for keys winit couldn't resolve to a known [`KeyCode`].
///
/// Split out from [`map_window_event`] because `winit::event::KeyEvent` has
/// a private field and can't be constructed in tests; `PhysicalKey` has no
/// such restriction, so this is the smallest testable unit of the mapping.
fn map_physical_key(
    physical_key: winit::keyboard::PhysicalKey,
    pressed: bool,
    repeat: bool,
) -> Option<PlatformEvent> {
    use winit::keyboard::PhysicalKey;
    match physical_key {
        PhysicalKey::Code(key) => Some(PlatformEvent::KeyboardInput {
            key,
            pressed,
            repeat,
        }),
        // Hardware that can't be mapped to a known KeyCode: no stable
        // identifier to hand callers, so drop it rather than guess.
        PhysicalKey::Unidentified(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::dpi::PhysicalSize;
    use winit::event::WindowEvent as WE;

    #[test]
    fn maps_close_requested() {
        assert_eq!(
            map_window_event(&WE::CloseRequested),
            Some(PlatformEvent::CloseRequested)
        );
    }

    #[test]
    fn maps_resized() {
        let event = WE::Resized(PhysicalSize::new(800, 600));
        assert_eq!(
            map_window_event(&event),
            Some(PlatformEvent::Resized {
                width: 800,
                height: 600
            })
        );
    }

    #[test]
    fn maps_redraw_requested() {
        assert_eq!(
            map_window_event(&WE::RedrawRequested),
            Some(PlatformEvent::RedrawRequested)
        );
    }

    #[test]
    fn unmapped_events_return_none() {
        assert_eq!(map_window_event(&WE::Focused(true)), None);
    }

    #[test]
    fn maps_known_physical_key_press() {
        use winit::keyboard::PhysicalKey;
        let got = map_physical_key(PhysicalKey::Code(KeyCode::KeyW), true, false);
        assert_eq!(
            got,
            Some(PlatformEvent::KeyboardInput {
                key: KeyCode::KeyW,
                pressed: true,
                repeat: false
            })
        );
    }

    #[test]
    fn maps_known_physical_key_repeat_release() {
        use winit::keyboard::PhysicalKey;
        let got = map_physical_key(PhysicalKey::Code(KeyCode::Space), false, true);
        assert_eq!(
            got,
            Some(PlatformEvent::KeyboardInput {
                key: KeyCode::Space,
                pressed: false,
                repeat: true
            })
        );
    }

    #[test]
    fn unidentified_physical_key_maps_to_none() {
        use winit::keyboard::{NativeKeyCode, PhysicalKey};
        let got = map_physical_key(
            PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
            true,
            false,
        );
        assert_eq!(got, None);
    }

    #[test]
    fn maps_mouse_button_input() {
        use winit::event::{DeviceId, ElementState};
        let event = WE::MouseInput {
            device_id: DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
        assert_eq!(
            map_window_event(&event),
            Some(PlatformEvent::MouseButtonInput {
                button: MouseButton::Left,
                pressed: true
            })
        );
    }

    #[test]
    fn maps_cursor_moved() {
        use winit::dpi::PhysicalPosition;
        use winit::event::DeviceId;
        let event = WE::CursorMoved {
            device_id: DeviceId::dummy(),
            position: PhysicalPosition::new(12.5, 34.0),
        };
        assert_eq!(
            map_window_event(&event),
            Some(PlatformEvent::CursorMoved { x: 12.5, y: 34.0 })
        );
    }

    #[test]
    fn maps_mouse_wheel_line_delta() {
        use winit::event::{DeviceId, MouseScrollDelta, TouchPhase};
        let event = WE::MouseWheel {
            device_id: DeviceId::dummy(),
            delta: MouseScrollDelta::LineDelta(1.0, -2.0),
            phase: TouchPhase::Moved,
        };
        assert_eq!(
            map_window_event(&event),
            Some(PlatformEvent::MouseWheel {
                delta_x: 1.0,
                delta_y: -2.0
            })
        );
    }
}
