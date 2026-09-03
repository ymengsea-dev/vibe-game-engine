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
    /// The event's `tracing` target (module path or explicit `target:`),
    /// e.g. `engine_editor::shell`.
    pub target: String,
    /// The event's `message` field (if any) followed by its other
    /// fields as `name=value` — see [`ConsoleLayer::on_event`].
    pub message: String,
}

/// The lowest severity the Console tab shows. `Trace` shows everything;
/// `Error` shows only errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LevelFilter {
    /// Errors only.
    Error,
    /// Warnings and errors.
    Warn,
    /// Info and above — the default.
    #[default]
    Info,
    /// Debug and above.
    Debug,
    /// Everything.
    Trace,
}

impl LevelFilter {
    /// Every variant, most severe first — the order the picker shows.
    pub const ALL: [LevelFilter; 5] = [
        LevelFilter::Error,
        LevelFilter::Warn,
        LevelFilter::Info,
        LevelFilter::Debug,
        LevelFilter::Trace,
    ];

    /// Label for the level picker.
    pub fn label(self) -> &'static str {
        match self {
            LevelFilter::Error => "Error",
            LevelFilter::Warn => "Warn",
            LevelFilter::Info => "Info",
            LevelFilter::Debug => "Debug",
            LevelFilter::Trace => "Trace",
        }
    }

    fn rank(self) -> u8 {
        match self {
            LevelFilter::Error => 0,
            LevelFilter::Warn => 1,
            LevelFilter::Info => 2,
            LevelFilter::Debug => 3,
            LevelFilter::Trace => 4,
        }
    }

    fn level_rank(level: Level) -> u8 {
        match level {
            Level::ERROR => 0,
            Level::WARN => 1,
            Level::INFO => 2,
            Level::DEBUG => 3,
            Level::TRACE => 4,
        }
    }

    /// Whether a line at `level` passes this filter.
    pub fn allows(self, level: Level) -> bool {
        Self::level_rank(level) <= self.rank()
    }
}

/// The Console tab's display filter. Lives in `EditorState` so it
/// persists across frames; it only affects what the Console tab draws,
/// never what [`ConsoleLayer`] captures.
#[derive(Debug, Clone, Default)]
pub struct ConsoleFilter {
    /// Lines more verbose than this are hidden.
    pub min_level: LevelFilter,
    /// Case-insensitive substring the message or target must contain.
    /// Empty matches every line.
    pub search: String,
}

impl ConsoleFilter {
    /// Whether `line` should be shown under this filter.
    pub fn matches(&self, line: &ConsoleLine) -> bool {
        if !self.min_level.allows(line.level) {
            return false;
        }
        if self.search.is_empty() {
            return true;
        }
        let needle = self.search.to_lowercase();
        line.message.to_lowercase().contains(&needle)
            || line.target.to_lowercase().contains(&needle)
    }
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
            target: event.metadata().target().to_string(),
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

/// Draws the Console tab: a filter row (level picker, text search, line
/// count, Clear, Copy) above `log`'s captured lines — filtered by
/// `filter`, most recent at the bottom, color-coded by level.
pub fn show(ui: &mut egui::Ui, log: &ConsoleLog, filter: &mut ConsoleFilter) {
    let lines = log.snapshot();
    let shown: Vec<&ConsoleLine> = lines.iter().filter(|line| filter.matches(line)).collect();

    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("console_level_filter")
            .selected_text(filter.min_level.label())
            .show_ui(ui, |ui| {
                for level in LevelFilter::ALL {
                    ui.selectable_value(&mut filter.min_level, level, level.label());
                }
            });
        ui.add(
            egui::TextEdit::singleline(&mut filter.search)
                .hint_text("Filter")
                .desired_width(160.0),
        );
        ui.label(format!("{} / {}", shown.len(), lines.len()));
        if ui.button("Clear").clicked() {
            log.clear();
        }
        if ui.button("Copy").clicked() {
            let text = shown
                .iter()
                .map(|line| format!("[{}] {} {}", line.level, line.target, line.message))
                .collect::<Vec<_>>()
                .join("\n");
            ui.ctx().copy_text(text);
        }
    });

    egui::ScrollArea::vertical()
        .stick_to_bottom(true)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if shown.is_empty() {
                ui.weak(if lines.is_empty() {
                    "No log output yet."
                } else {
                    "No lines match the filter."
                });
            }
            for line in shown {
                ui.horizontal(|ui| {
                    ui.colored_label(level_color(line.level), format!("[{}]", line.level));
                    ui.weak(short_target(&line.target));
                    ui.label(&line.message);
                });
            }
        });
}

/// The last one or two `::`-separated segments of a target, so the
/// console shows `editor::shell` rather than the full crate path.
fn short_target(target: &str) -> String {
    let parts: Vec<&str> = target.split("::").collect();
    if parts.len() <= 2 {
        target.to_string()
    } else {
        parts[parts.len() - 2..].join("::")
    }
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
            target: String::new(),
            message: message.to_string(),
        }
    }

    fn line_at(level: Level, target: &str, message: &str) -> ConsoleLine {
        ConsoleLine {
            level,
            target: target.to_string(),
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

    #[test]
    fn console_layer_captures_the_event_target() {
        let log = ConsoleLog::new();
        let subscriber = tracing_subscriber::registry().with(ConsoleLayer::new(log.clone()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("hi");
        });

        let snapshot = log.snapshot();
        assert!(
            snapshot[0].target.contains("console"),
            "target was {:?}",
            snapshot[0].target
        );
    }

    #[test]
    fn level_filter_allows_by_rank() {
        assert!(LevelFilter::Error.allows(Level::ERROR));
        assert!(!LevelFilter::Error.allows(Level::WARN));
        assert!(LevelFilter::Info.allows(Level::ERROR));
        assert!(LevelFilter::Info.allows(Level::INFO));
        assert!(!LevelFilter::Info.allows(Level::DEBUG));
        assert!(LevelFilter::Trace.allows(Level::TRACE));
    }

    #[test]
    fn console_filter_combines_level_and_case_insensitive_search() {
        let mut filter = ConsoleFilter::default(); // min_level = Info
        let err = line_at(Level::ERROR, "engine_editor::shell", "boom happened");
        let debug = line_at(Level::DEBUG, "engine_editor::shell", "boom happened");
        let info_other = line_at(Level::INFO, "engine_editor::assets", "loaded fine");

        assert!(filter.matches(&err));
        assert!(!filter.matches(&debug), "debug is below the Info filter");

        filter.search = "BOOM".to_string();
        assert!(filter.matches(&err), "search is case-insensitive");
        assert!(
            !filter.matches(&info_other),
            "non-matching message is filtered"
        );

        filter.search = "assets".to_string();
        assert!(
            filter.matches(&info_other),
            "search also matches the target"
        );
    }
}
