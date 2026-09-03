//! The Output tab: a bounded plain-text sink for build and task output.
//!
//! Nothing writes to it yet — the build pipeline and the integrated
//! terminal are later iterations. This is the buffer they will call
//! [`OutputLog::write_line`] on; the tab renders it now (empty until
//! then).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// How many of the most recent output lines [`OutputLog`] keeps.
const MAX_LINES: usize = 2000;

/// Shared handle to the Output tab's text.
///
/// Cheap to clone (an `Arc` underneath). Poison-safe: a poisoned lock
/// reads as empty and drops writes rather than panicking.
#[derive(Debug, Clone, Default)]
pub struct OutputLog {
    lines: Arc<Mutex<VecDeque<String>>>,
}

impl OutputLog {
    /// An empty log.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends one line. Any trailing `\r`/`\n` is trimmed so callers can
    /// pass raw lines straight from a child process's stdout.
    pub fn write_line(&self, line: impl Into<String>) {
        let mut line = line.into();
        while line.ends_with('\n') || line.ends_with('\r') {
            line.pop();
        }
        if let Ok(mut lines) = self.lines.lock() {
            if lines.len() >= MAX_LINES {
                lines.pop_front();
            }
            lines.push_back(line);
        }
    }

    /// Discards every line.
    pub fn clear(&self) {
        if let Ok(mut lines) = self.lines.lock() {
            lines.clear();
        }
    }

    /// A copy of the current lines, oldest first.
    pub fn snapshot(&self) -> Vec<String> {
        self.lines
            .lock()
            .map(|lines| lines.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Whether there is no output (also `true` if the lock is poisoned).
    pub fn is_empty(&self) -> bool {
        self.lines
            .lock()
            .map(|lines| lines.is_empty())
            .unwrap_or(true)
    }
}

/// Draws the Output tab: monospace lines, newest at the bottom, or an
/// empty-state hint.
pub fn show(ui: &mut egui::Ui, output: &OutputLog) {
    let lines = output.snapshot();
    ui.horizontal(|ui| {
        ui.label(format!("{} lines", lines.len()));
        if ui.button("Clear").clicked() {
            output.clear();
        }
    });

    egui::ScrollArea::vertical()
        .stick_to_bottom(true)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if lines.is_empty() {
                ui.weak(
                    "No output. Build and task output appears here once the build pipeline lands.",
                );
            }
            for line in &lines {
                ui.label(egui::RichText::new(line).monospace());
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_log_is_empty() {
        let log = OutputLog::new();
        assert!(log.is_empty());
        assert!(log.snapshot().is_empty());
    }

    #[test]
    fn write_line_trims_trailing_newlines() {
        let log = OutputLog::new();
        log.write_line("compiling\n");
        log.write_line("done\r\n");
        assert_eq!(
            log.snapshot(),
            vec!["compiling".to_string(), "done".to_string()]
        );
    }

    #[test]
    fn write_line_evicts_the_oldest_once_over_capacity() {
        let log = OutputLog::new();
        for i in 0..MAX_LINES + 5 {
            log.write_line(i.to_string());
        }
        let snapshot = log.snapshot();
        assert_eq!(snapshot.len(), MAX_LINES);
        assert_eq!(snapshot.first(), Some(&"5".to_string()));
        assert_eq!(snapshot.last(), Some(&(MAX_LINES + 4).to_string()));
    }

    #[test]
    fn clear_empties_the_log() {
        let log = OutputLog::new();
        log.write_line("something");
        log.clear();
        assert!(log.is_empty());
    }
}
