//! Console panel: captures `tracing` events into a bounded in-memory log
//! and shows them as scrollable text.
//!
//! [`ConsoleLayer`] is a `tracing_subscriber::Layer` installed alongside
//! the normal stderr output via
//! `engine_core::logging::init_default_with_layer` — so the editor's own
//! log lines (and anything else running in-process that logs through
//! `tracing`) show up in its UI too, not just whatever terminal it was
//! launched from.

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

/// How many of the most recent log lines [`ConsoleLog`] keeps — older
/// ones are evicted, bounding memory use for a long-running editor
/// session.
const MAX_LINES: usize = 500;

/// One captured log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleLine {
    /// The event's severity.
    pub level: Level,
    /// The event's `message` field (if any) followed by its other
    /// fields as `name=value` — see [`ConsoleLayer::on_event`].
    pub message: String,
}

/// Shared handle to the captured log lines.
///
/// Cheap to clone (an `Arc` underneath) — [`ConsoleLayer`] holds one to
/// write into, the editor's UI holds another to read from.
#[derive(Debug, Clone, Default)]
pub struct ConsoleLog {
    lines: Arc<Mutex<VecDeque<ConsoleLine>>>,
}

impl ConsoleLog {
    /// An empty log.
    pub fn new() -> Self {
        Self::default()
    }

    /// A snapshot of the currently captured lines, oldest first.
    ///
    /// Returns an empty `Vec` (rather than panicking) if the lock is
    /// poisoned — a prior panic elsewhere already broke something worse
    /// than the console losing its history.
    pub fn snapshot(&self) -> Vec<ConsoleLine> {
        self.lines
            .lock()
            .map(|lines| lines.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Discards every captured line.
    pub fn clear(&self) {
        if let Ok(mut lines) = self.lines.lock() {
            lines.clear();
        }
    }

    fn push(&self, line: ConsoleLine) {
        if let Ok(mut lines) = self.lines.lock() {
            if lines.len() >= MAX_LINES {
                lines.pop_front();
            }
            lines.push_back(line);
        }
    }
}

/// A `tracing_subscriber` layer that appends every event it observes to
/// a [`ConsoleLog`]. Install via
/// `engine_core::logging::init_default_with_layer`.
pub struct ConsoleLayer {
    log: ConsoleLog,
}

impl ConsoleLayer {
    /// Appends every event this layer observes to `log`.
    pub fn new(log: ConsoleLog) -> Self {
        Self { log }
    }
}

impl<S: Subscriber> Layer<S> for ConsoleLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        self.log.push(ConsoleLine {
            level: *event.metadata().level(),
            message: visitor.message,
        });
    }
}

/// Collects an event's `message` field (`tracing`'s conventional name
/// for its main text, e.g. from `tracing::info!("...")`) plus any other
/// fields, formatted as `name=value` and space-separated after it.
#[derive(Default)]
struct MessageVisitor {
    message: String,
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
            return;
        }
        if self.message.is_empty() {
            self.message = format!("{}={value:?}", field.name());
        } else {
            self.message
                .push_str(&format!(" {}={value:?}", field.name()));
        }
    }
}

/// Draws the console panel: `log`'s captured lines, most recent at the
/// bottom, color-coded by level.
pub fn show(ui: &mut egui::Ui, log: &ConsoleLog) {
    let lines = log.snapshot();
    ui.horizontal(|ui| {
        ui.label(format!("{} lines", lines.len()));
        if ui.button("Clear").clicked() {
            log.clear();
        }
    });
    egui::ScrollArea::vertical()
        .max_height(160.0)
        .stick_to_bottom(true)
        .show(ui, |ui| {
            if lines.is_empty() {
                ui.label("No log output yet.");
            }
            for line in lines {
                ui.colored_label(
                    level_color(line.level),
                    format!("[{}] {}", line.level, line.message),
                );
            }
        });
}

fn level_color(level: Level) -> egui::Color32 {
    match level {
        Level::ERROR => egui::Color32::from_rgb(220, 80, 80),
        Level::WARN => egui::Color32::from_rgb(220, 180, 80),
        Level::INFO => egui::Color32::from_rgb(200, 200, 200),
        Level::DEBUG => egui::Color32::from_rgb(120, 160, 220),
        Level::TRACE => egui::Color32::from_rgb(140, 140, 140),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;

    fn line(message: &str) -> ConsoleLine {
        ConsoleLine {
            level: Level::INFO,
            message: message.to_string(),
        }
    }

    #[test]
    fn new_log_starts_empty() {
        assert!(ConsoleLog::new().snapshot().is_empty());
    }

    #[test]
    fn push_appends_in_order() {
        let log = ConsoleLog::new();
        log.push(line("first"));
        log.push(line("second"));
        let snapshot = log.snapshot();
        assert_eq!(snapshot, vec![line("first"), line("second")]);
    }

    #[test]
    fn push_evicts_the_oldest_line_once_over_capacity() {
        let log = ConsoleLog::new();
        for i in 0..MAX_LINES + 10 {
            log.push(line(&i.to_string()));
        }
        let snapshot = log.snapshot();
        assert_eq!(snapshot.len(), MAX_LINES);
        assert_eq!(snapshot.first(), Some(&line("10")));
        assert_eq!(snapshot.last(), Some(&line(&(MAX_LINES + 9).to_string())));
    }

    #[test]
    fn clear_empties_the_log() {
        let log = ConsoleLog::new();
        log.push(line("hello"));
        log.clear();
        assert!(log.snapshot().is_empty());
    }

    #[test]
    fn console_layer_captures_an_events_message() {
        let log = ConsoleLog::new();
        let subscriber = tracing_subscriber::registry().with(ConsoleLayer::new(log.clone()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("hello console");
        });

        let snapshot = log.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].level, Level::INFO);
        assert_eq!(snapshot[0].message, "hello console");
    }

    #[test]
    fn console_layer_captures_structured_fields_alongside_the_message() {
        let log = ConsoleLog::new();
        let subscriber = tracing_subscriber::registry().with(ConsoleLayer::new(log.clone()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(code = 42, "trouble");
        });

        let snapshot = log.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].level, Level::WARN);
        assert_eq!(snapshot[0].message, "trouble code=42");
    }
}
