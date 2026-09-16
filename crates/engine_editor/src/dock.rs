//! Bounded docking host for the production editor shell.
//!
//! The feature remains explicit (`dock-shell`) so runtime/game builds do not
//! pull in docking, while the standalone editor enables it. Layout data and
//! the render budget are bounded to keep malformed sessions and long-running
//! windows from causing unbounded memory or CPU growth.

use egui::Ui;
use egui_dock::{DockArea, DockState, TabViewer};
use serde::{Deserialize, Serialize};

/// Maximum serialized layout accepted from an editor session.
pub const MAX_LAYOUT_BYTES: usize = 64 * 1024;
/// Maximum frames a host may render without an explicit reset in a soak.
pub const MAX_SOAK_FRAMES: u64 = 10_000;

/// Stable tab identifiers used by the docking prototype.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tab {
    /// Hierarchy panel.
    Hierarchy,
    /// Scene panel.
    Scene,
    /// Asset Browser panel.
    Assets,
    /// Code panel.
    Code,
    /// Console panel.
    Console,
    /// Inspector panel.
    Inspector,
    /// AI Assistant panel.
    Ai,
}

/// Bounded, reusable owner of one docking tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Host {
    state: DockState<Tab>,
    frames: u64,
}

impl Host {
    /// Creates a small default tree without allocating per frame.
    pub fn new() -> Self {
        Self {
            state: DockState::new(vec![
                Tab::Scene,
                Tab::Hierarchy,
                Tab::Assets,
                Tab::Code,
                Tab::Console,
                Tab::Inspector,
                Tab::Ai,
            ]),
            frames: 0,
        }
    }

    /// Draws one dock frame and increments the bounded soak counter. The
    /// counter wraps at the safety limit so a long-running editor never
    /// silently stops rendering its panels.
    pub fn show(&mut self, ui: &mut Ui, viewer: &mut impl TabViewer<Tab = Tab>) {
        if self.frames == MAX_SOAK_FRAMES {
            self.frames = 0;
        }
        DockArea::new(&mut self.state).show_inside(ui, viewer);
        self.frames += 1;
    }

    /// Restarts the soak counter without rebuilding the docking tree.
    pub fn reset_frame_budget(&mut self) {
        self.frames = 0;
    }

    /// Encodes the tree for session persistence, rejecting oversized data.
    pub fn encode(&self) -> Option<String> {
        let encoded = ron::ser::to_string(self).ok()?;
        (encoded.len() <= MAX_LAYOUT_BYTES).then_some(encoded)
    }

    /// Decodes a persisted tree only when it is bounded and valid.
    pub fn decode(encoded: &str) -> Option<Self> {
        if encoded.len() > MAX_LAYOUT_BYTES {
            return None;
        }
        ron::from_str(encoded).ok()
    }
}

impl Default for Host {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_round_trips_without_growth() {
        let host = Host::new();
        let encoded = host.encode().expect("default dock should serialize");
        let restored = Host::decode(&encoded).expect("default dock should decode");
        assert_eq!(restored.encode(), Some(encoded));
    }

    #[test]
    fn oversized_layout_is_rejected() {
        assert!(Host::decode(&"x".repeat(MAX_LAYOUT_BYTES + 1)).is_none());
    }

    #[test]
    fn soak_budget_is_bounded() {
        let mut host = Host::new();
        host.frames = MAX_SOAK_FRAMES;
        host.reset_frame_budget();
        assert_eq!(host.frames, 0);
    }
}
