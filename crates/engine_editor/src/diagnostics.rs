//! The Problems tab's data model and table renderer: a shared list of
//! structured diagnostics (compiler / language-server findings).
//!
//! Nothing produces real diagnostics yet — the rust-analyzer bridge and
//! the build pipeline are later iterations. This module is the sink they
//! will push into, plus the table the tab draws now (empty until then).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// A diagnostic's severity. Ordered worst-first so a `sort` puts errors
/// at the top.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// A hard error — the code will not build / run.
    Error,
    /// A warning — suspicious but not fatal.
    Warning,
    /// An informational note or hint.
    Info,
}

impl Severity {
    /// Label for the Problems table's "Type" column.
    pub fn label(self) -> &'static str {
        match self {
            Severity::Error => "Error",
            Severity::Warning => "Warning",
            Severity::Info => "Info",
        }
    }

    fn color(self) -> egui::Color32 {
        match self {
            Severity::Error => egui::Color32::from_rgb(220, 80, 80),
            Severity::Warning => egui::Color32::from_rgb(220, 180, 80),
            Severity::Info => egui::Color32::from_rgb(150, 180, 220),
        }
    }
}

/// One row in the Problems tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// How serious the finding is.
    pub severity: Severity,
    /// The diagnostic code, if the producer gave one (`E0599`,
    /// `unused_variables`, …).
    pub code: Option<String>,
    /// Human-readable description.
    pub message: String,
    /// Project-relative source file the diagnostic points at, if any.
    pub file: Option<PathBuf>,
    /// 1-based line within `file`, if known.
    pub line: Option<u32>,
}

/// Shared handle to the current set of diagnostics.
///
/// Cheap to clone (an `Arc` underneath). Diagnostics are recomputed
/// wholesale by their producer, so the usual update is [`Diagnostics::set`]
/// rather than incremental [`Diagnostics::push`]. Poison-safe: a
/// poisoned lock reads as empty rather than panicking.
#[derive(Debug, Clone, Default)]
pub struct Diagnostics {
    items: Arc<Mutex<Vec<Diagnostic>>>,
}

impl Diagnostics {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the whole set (e.g. after a fresh compile).
    pub fn set(&self, items: Vec<Diagnostic>) {
        if let Ok(mut guard) = self.items.lock() {
            *guard = items;
        }
    }

    /// Appends one diagnostic.
    pub fn push(&self, item: Diagnostic) {
        if let Ok(mut guard) = self.items.lock() {
            guard.push(item);
        }
    }

    /// Discards every diagnostic.
    pub fn clear(&self) {
        if let Ok(mut guard) = self.items.lock() {
            guard.clear();
        }
    }

    /// A copy of the current diagnostics.
    pub fn snapshot(&self) -> Vec<Diagnostic> {
        self.items
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// How many diagnostics there are (0 if the lock is poisoned).
    pub fn len(&self) -> usize {
        self.items.lock().map(|guard| guard.len()).unwrap_or(0)
    }

    /// Whether there are no diagnostics.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Draws the Problems tab: a Type / Code / Description / File / Line
/// table, or an empty-state hint.
///
/// Clicking a row that has a source file sets `*selected` to its index
/// (into [`Diagnostics::snapshot`]); the caller turns that into a
/// jump-to-source and clears it. Rows with no file are inert.
pub fn show(ui: &mut egui::Ui, diagnostics: &Diagnostics, selected: &mut Option<usize>) {
    let items = diagnostics.snapshot();
    ui.horizontal(|ui| {
        ui.label(format!("{} problems", items.len()));
        if ui.button("Clear").clicked() {
            diagnostics.clear();
            *selected = None;
        }
    });

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if items.is_empty() {
                ui.weak(
                    "No problems. Compiler and language-server diagnostics appear \
                     here once those bridges land.",
                );
                return;
            }
            egui::Grid::new("studio_problems_grid")
                .striped(true)
                .num_columns(5)
                .show(ui, |ui| {
                    ui.strong("Type");
                    ui.strong("Code");
                    ui.strong("Description");
                    ui.strong("File");
                    ui.strong("Line");
                    ui.end_row();

                    for (index, diagnostic) in items.iter().enumerate() {
                        let type_text = egui::RichText::new(diagnostic.severity.label())
                            .color(diagnostic.severity.color());
                        let code = diagnostic.code.clone().unwrap_or_default();
                        let file = diagnostic
                            .file
                            .as_ref()
                            .map(|file| file.display().to_string())
                            .unwrap_or_default();
                        let line = diagnostic
                            .line
                            .map(|line| line.to_string())
                            .unwrap_or_default();

                        if diagnostic.file.is_some() {
                            let is_selected = *selected == Some(index);
                            let mut clicked = false;
                            for cell in [
                                ui.selectable_label(is_selected, type_text),
                                ui.selectable_label(is_selected, code),
                                ui.selectable_label(is_selected, &diagnostic.message),
                                ui.selectable_label(is_selected, file),
                                ui.selectable_label(is_selected, line),
                            ] {
                                clicked |= cell.clicked();
                            }
                            if clicked {
                                *selected = Some(index);
                            }
                        } else {
                            ui.label(type_text);
                            ui.label(code);
                            ui.label(&diagnostic.message);
                            ui.label(file);
                            ui.label(line);
                        }
                        ui.end_row();
                    }
                });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diag(severity: Severity, message: &str) -> Diagnostic {
        Diagnostic {
            severity,
            code: None,
            message: message.to_string(),
            file: None,
            line: None,
        }
    }

    #[test]
    fn new_set_is_empty() {
        let diagnostics = Diagnostics::new();
        assert!(diagnostics.is_empty());
        assert_eq!(diagnostics.len(), 0);
        assert!(diagnostics.snapshot().is_empty());
    }

    #[test]
    fn push_then_set_then_clear() {
        let diagnostics = Diagnostics::new();
        diagnostics.push(diag(Severity::Warning, "one"));
        diagnostics.push(diag(Severity::Error, "two"));
        assert_eq!(diagnostics.len(), 2);

        diagnostics.set(vec![diag(Severity::Info, "only")]);
        assert_eq!(diagnostics.snapshot(), vec![diag(Severity::Info, "only")]);

        diagnostics.clear();
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn severity_orders_worst_first() {
        let mut severities = [Severity::Info, Severity::Error, Severity::Warning];
        severities.sort();
        assert_eq!(
            severities,
            [Severity::Error, Severity::Warning, Severity::Info]
        );
    }

    #[test]
    fn diagnostic_carries_optional_location() {
        let d = Diagnostic {
            severity: Severity::Error,
            code: Some("E0599".to_string()),
            message: "no method named `foo`".to_string(),
            file: Some(PathBuf::from("src/player.rs")),
            line: Some(42),
        };
        assert_eq!(d.code.as_deref(), Some("E0599"));
        assert_eq!(d.line, Some(42));
        assert_eq!(d.file.unwrap(), PathBuf::from("src/player.rs"));
    }
}
