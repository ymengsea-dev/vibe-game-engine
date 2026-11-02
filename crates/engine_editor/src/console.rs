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
    /// The event's `message` field: its text, if it has one.
    pub message: String,
    /// The event's other fields as space-separated `name=value`, shown
    /// after the message as data.
    pub fields: String,
}

impl ConsoleLine {
    /// The message and its fields as one line of text.
    pub fn text(&self) -> String {
        match (self.message.is_empty(), self.fields.is_empty()) {
            (_, true) => self.message.clone(),
            (true, false) => self.fields.clone(),
            (false, false) => format!("{} {}", self.message, self.fields),
        }
    }
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

    /// Label for the level picker: what the choice shows.
    pub fn label(self) -> &'static str {
        match self {
            LevelFilter::Error => "Errors",
            LevelFilter::Warn => "Warnings",
            LevelFilter::Info => "Info",
            LevelFilter::Debug => "Debug",
            LevelFilter::Trace => "Everything",
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
        line.text().to_lowercase().contains(&needle) || line.target.to_lowercase().contains(&needle)
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
        // Records bridged from the `log` crate arrive with the target "log"
        // and their real target, module, file and line as fields; show the
        // real target and drop the rest.
        let target = visitor
            .log_target
            .unwrap_or_else(|| event.metadata().target().to_string());
        self.log.push(ConsoleLine {
            level: *event.metadata().level(),
            target,
            message: visitor.message,
            fields: visitor.fields,
        });
    }
}

/// Collects an event's `message` field (`tracing`'s conventional name
/// for its main text, e.g. from `tracing::info!("...")`) and, apart, any
/// other fields, formatted as space-separated `name=value`.
#[derive(Default)]
struct MessageVisitor {
    message: String,
    fields: String,
    /// The `log.target` field of a record bridged from the `log` crate.
    log_target: Option<String>,
}

impl Visit for MessageVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "log.target" {
            self.log_target = Some(value.to_owned());
        } else {
            self.record_debug(field, &value);
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name().starts_with("log.") {
            return;
        }
        if field.name() == "message" {
            self.message = format!("{value:?}");
            return;
        }
        if !self.fields.is_empty() {
            self.fields.push(' ');
        }
        self.fields.push_str(&format!("{}={value:?}", field.name()));
    }
}

/// Draws the Console tab: a toolbar (search, level picker, line count,
/// Copy, Clear) above `log`'s captured lines, filtered by `filter`, most
/// recent at the bottom. Each line is its level tag, its source and the
/// message in fixed columns, so wrapped messages stay aligned.
pub fn show(ui: &mut egui::Ui, log: &ConsoleLog, filter: &mut ConsoleFilter) {
    let palette = crate::theme::NIGHT;
    let lines = log.snapshot();
    let shown: Vec<&ConsoleLine> = lines.iter().filter(|line| filter.matches(line)).collect();

    let copy_all = |ui: &egui::Ui| {
        let text = shown
            .iter()
            .map(|line| format!("[{}] {} {}", line.level, line.target, line.text()))
            .collect::<Vec<_>>()
            .join("\n");
        ui.ctx().copy_text(text);
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.spacing_mut().interact_size.y = 24.0;
        // The buttons take their place at the right first; the search
        // field gets what is left, so a narrow panel never overlaps them.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if crate::widgets::pill_button(ui, &palette, "Clear", palette.text, !lines.is_empty())
                .clicked()
            {
                log.clear();
            }
            if crate::widgets::pill_button(ui, &palette, "Copy", palette.text, !shown.is_empty())
                .on_hover_text("Copy the messages shown")
                .clicked()
            {
                copy_all(ui);
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                const LEVEL_WIDTH: f32 = 112.0;
                let show_count = ui.available_width() >= 420.0;
                let count_width = if show_count { 96.0 } else { 0.0 };
                let room = ui.available_width() - LEVEL_WIDTH - count_width - 16.0;
                let width = room.clamp(80.0, 280.0);
                crate::widgets::search_field(ui, &palette, &mut filter.search, "Filter", width);
                egui::ComboBox::from_id_salt("console_level_filter")
                    .width(LEVEL_WIDTH)
                    .icon(crate::widgets::combo_chevron)
                    .selected_text(filter.min_level.label())
                    .show_ui(ui, |ui| {
                        for level in LevelFilter::ALL {
                            ui.selectable_value(&mut filter.min_level, level, level.label());
                        }
                    });
                if show_count {
                    let count = if shown.len() == lines.len() {
                        format!("{} messages", lines.len())
                    } else {
                        format!("{} of {}", shown.len(), lines.len())
                    };
                    ui.label(egui::RichText::new(count).color(palette.subtle));
                }
            });
        });
    });
    ui.add_space(8.0);

    egui::ScrollArea::vertical()
        .stick_to_bottom(true)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            if shown.is_empty() {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(if lines.is_empty() {
                        "No messages yet."
                    } else {
                        "No messages match the filter."
                    })
                    .color(palette.subtle),
                );
            }
            for line in shown {
                console_row(ui, line);
            }
        });
}

/// Below this width the Console drops its source column, so messages keep
/// the room.
const SOURCE_COLUMN_BELOW: f32 = 460.0;

/// The width of the Console's source column.
const SOURCE_WIDTH: f32 = 132.0;

/// The width of the Console's level column, room for "[ERROR]".
const LEVEL_WIDTH: f32 = 60.0;

/// One Console line: its level tag, source and message, each in its own
/// column, with Copy in its context menu. Plain text, no row fill: it is
/// log output.
fn console_row(ui: &mut egui::Ui, line: &ConsoleLine) {
    const PAD_X: f32 = 4.0;
    const PAD_Y: f32 = 4.0;
    let palette = crate::theme::NIGHT;
    let width = ui.available_width();
    let source_width = if width >= SOURCE_COLUMN_BELOW {
        SOURCE_WIDTH
    } else {
        0.0
    };
    let message_x = PAD_X + LEVEL_WIDTH + source_width;
    let ink = crate::theme::NIGHT.text;
    // The message reads as text; its fields follow as quieter data.
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = (width - message_x - PAD_X).max(40.0);
    if !line.message.is_empty() {
        job.append(
            &line.message,
            0.0,
            egui::TextFormat::simple(egui::FontId::proportional(12.5), ink),
        );
    }
    if !line.fields.is_empty() {
        job.append(
            &line.fields,
            if line.message.is_empty() { 0.0 } else { 8.0 },
            egui::TextFormat {
                font_id: egui::FontId::monospace(11.5),
                color: palette.muted,
                valign: egui::Align::Center,
                ..Default::default()
            },
        );
    }
    let message = ui.painter().layout_job(job);
    let first_row = message
        .rows
        .first()
        .map_or(message.size().y, |row| row.height());
    let height = message.size().y.max(first_row) + 2.0 * PAD_Y;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    let painter = ui.painter();
    let middle = rect.top() + PAD_Y + first_row / 2.0;
    let color = level_color(line.level);
    let tag = crate::widgets::one_line(
        painter,
        &format!("[{}]", line.level),
        12.0,
        color,
        LEVEL_WIDTH - 6.0,
    );
    painter.galley(
        egui::pos2(rect.left() + PAD_X, middle - tag.size().y / 2.0),
        tag,
        color,
    );
    if source_width > 0.0 {
        let source = crate::widgets::one_line(
            painter,
            &short_target(&line.target),
            11.5,
            palette.subtle,
            source_width - 12.0,
        );
        let at = egui::pos2(
            rect.left() + PAD_X + LEVEL_WIDTH,
            middle - source.size().y / 2.0,
        );
        painter.galley(at, source, palette.subtle);
    }
    painter.galley(
        egui::pos2(rect.left() + message_x, rect.top() + PAD_Y),
        message,
        ink,
    );
    let response = if source_width == 0.0 {
        response.on_hover_text_at_pointer(&line.target)
    } else {
        response
    };
    response.context_menu(|ui| {
        crate::theme::style_context_menu(ui);
        if ui.button("Copy message").clicked() {
            ui.ctx().copy_text(line.text());
            ui.close();
        }
        if ui.button("Copy with source").clicked() {
            ui.ctx()
                .copy_text(format!("[{}] {} {}", line.level, line.target, line.text()));
            ui.close();
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

/// The colour of a line's level tag.
fn level_color(level: Level) -> egui::Color32 {
    match level {
        Level::ERROR => crate::theme::NIGHT.danger,
        Level::WARN => crate::theme::NIGHT.highlight,
        Level::INFO => crate::theme::NIGHT.text,
        Level::DEBUG => crate::theme::NIGHT.info,
        Level::TRACE => crate::theme::NIGHT.subtle,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;

    #[test]
    fn long_console_message_wraps_within_the_panel() {
        let log = ConsoleLog::new();
        log.push(line(&"long console message ".repeat(20)));
        log.push(line("short status"));
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(480.0, 240.0),
                )),
                ..Default::default()
            },
            |ui| show(ui, &log, &mut ConsoleFilter::default()),
        );
        let message = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text)
                if text.galley.text().contains("long console message") =>
            {
                Some(text)
            }
            _ => None,
        });
        let message = message.expect("console message");
        assert!(message.galley.rows.len() > 1);
        assert!(message.pos.x + message.galley.size().x <= 480.0);
        let short = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.text() == "short status" => Some(text),
            _ => None,
        });
        assert_eq!(short.expect("short message").galley.rows.len(), 1);
        output.textures_delta.clear();
    }

    fn line(message: &str) -> ConsoleLine {
        ConsoleLine {
            level: Level::INFO,
            target: String::new(),
            message: message.to_string(),
            fields: String::new(),
        }
    }

    fn line_at(level: Level, target: &str, message: &str) -> ConsoleLine {
        ConsoleLine {
            level,
            target: target.to_string(),
            message: message.to_string(),
            fields: String::new(),
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
        assert_eq!(snapshot[0].message, "trouble");
        assert_eq!(snapshot[0].fields, "code=42");
        assert_eq!(snapshot[0].text(), "trouble code=42");
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
