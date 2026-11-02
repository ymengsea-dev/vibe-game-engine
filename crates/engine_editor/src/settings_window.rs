//! The Settings window (⌘,), shaped like a macOS settings window: native
//! window controls at the top left, a sidebar of sections, and the chosen
//! section on the right. Its first section is Storage. A segmented bar
//! (like macOS's own Storage settings) shows how the space splits by kind,
//! then each project's items are listed in a rounded group with a Clean
//! button per rebuildable item, then the optional automatic limits.

use std::path::Path;

use engine_project::storage::{ProjectStorage, StorageItem, StorageKind};

use crate::EditorState;
use crate::storage::{LedgerEntry, format_bytes};
use crate::theme::Palette;

/// Width of the Settings window.
const WINDOW_WIDTH: f32 = 720.0;
/// Width of the sidebar.
const SIDEBAR_WIDTH: f32 = 196.0;
/// Space around the content column.
const CONTENT_MARGIN: f32 = 32.0;
/// Width of the content column: everything right of the sidebar, minus
/// its margins, so nothing can run past the window's edge.
const CONTENT_WIDTH: f32 = WINDOW_WIDTH - SIDEBAR_WIDTH - 2.0 * CONTENT_MARGIN;
/// Horizontal padding inside a group.
const GROUP_PADDING: f32 = 14.0;
/// Corner radius of the window, as on macOS.
const WINDOW_RADIUS: u8 = 12;
/// Corner radius of a group.
const GROUP_RADIUS: u8 = 9;

/// macOS window-control colours: close, and the grey of a control a
/// window does not offer (this one cannot be minimised or zoomed).
const CLOSE_RED: egui::Color32 = egui::Color32::from_rgb(0xFF, 0x5F, 0x57);
const CLOSE_RED_EDGE: egui::Color32 = egui::Color32::from_rgb(0xE2, 0x46, 0x3F);
const CLOSE_GLYPH: egui::Color32 = egui::Color32::from_rgb(0x4D, 0x00, 0x00);
const CONTROL_OFF: egui::Color32 = egui::Color32::from_rgb(0x34, 0x37, 0x4B);

/// Choices offered for the automatic limits.
const DAY_CHOICES: [u32; 5] = [7, 14, 30, 60, 90];
const GIGABYTE_CHOICES: [u32; 5] = [5, 10, 20, 50, 100];

/// The colour that stands for a kind of storage, in the bar, its key and
/// each row's swatch (Tokyo Night blue, cyan, purple; grey for backups,
/// which Studio keeps).
fn kind_color(kind: StorageKind, palette: &Palette) -> egui::Color32 {
    match kind {
        StorageKind::BuildCache => palette.accent,
        StorageKind::StudioTemp => palette.info,
        StorageKind::OldEngineCopy => palette.purple,
        StorageKind::Backup => palette.subtle,
    }
}

/// Draws the Settings window while [`EditorState::settings_open`].
pub(crate) fn show(ctx: &egui::Context, state: &mut EditorState) {
    if !state.settings_open {
        return;
    }
    if state.storage.measured.is_none() && !state.storage.busy {
        state.storage.refresh();
    }
    let palette = crate::theme::NIGHT;
    let frame = egui::Frame::NONE
        .fill(palette.panel)
        .stroke(egui::Stroke::new(1.0, palette.selected))
        .corner_radius(WINDOW_RADIUS)
        .shadow(egui::epaint::Shadow {
            offset: [0, 18],
            blur: 44,
            spread: 0,
            color: egui::Color32::from_black_alpha(130),
        });
    egui::Window::new("Settings")
        .title_bar(false)
        .frame(frame)
        .resizable(false)
        .collapsible(false)
        // Fixed width, height from the content.
        .min_width(WINDOW_WIDTH)
        .max_width(WINDOW_WIDTH)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, -20.0))
        .show(ctx, |ui| {
            palette.apply(ui.visuals_mut());
            // The sidebar is painted under everything, once the window's
            // full height is known.
            let sidebar_background = ui.painter().add(egui::Shape::Noop);
            let mut close = false;
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
                close = sidebar(ui, &palette);
                ui.add_space(CONTENT_MARGIN);
                ui.vertical(|ui| {
                    ui.set_width(CONTENT_WIDTH);
                    ui.spacing_mut().item_spacing = egui::vec2(8.0, 0.0);
                    ui.add_space(26.0);
                    storage_section(ui, state, &palette);
                    ui.add_space(30.0);
                });
                ui.add_space(CONTENT_MARGIN);
            });
            if close {
                state.settings_open = false;
            }
            let window = ui.min_rect();
            let column =
                egui::Rect::from_min_size(window.min, egui::vec2(SIDEBAR_WIDTH, window.height()));
            let painter = ui.painter();
            painter.set(
                sidebar_background,
                egui::Shape::rect_filled(
                    column,
                    egui::CornerRadius {
                        nw: WINDOW_RADIUS,
                        sw: WINDOW_RADIUS,
                        ne: 0,
                        se: 0,
                    },
                    palette.shell,
                ),
            );
        });
    if ctx.input(|input| input.key_pressed(egui::Key::Escape)) && state.storage.confirm.is_none() {
        state.settings_open = false;
    }
    confirm_engine_copies(ctx, state);
    if state.storage.busy {
        ctx.request_repaint_after(std::time::Duration::from_millis(200));
    }
}

/// The sidebar: macOS window controls, then the sections (Storage for
/// now). Returns whether the close control was clicked.
fn sidebar(ui: &mut egui::Ui, palette: &Palette) -> bool {
    let mut close = false;
    ui.vertical(|ui| {
        ui.set_width(SIDEBAR_WIDTH);
        close = window_controls(ui);
        ui.add_space(18.0);
        let (row, _) =
            ui.allocate_exact_size(egui::vec2(SIDEBAR_WIDTH, 30.0), egui::Sense::hover());
        let item = row.shrink2(egui::vec2(10.0, 0.0));
        let painter = ui.painter();
        painter.rect_filled(item, 6.0, palette.accent_fill);
        // A drive glyph in an accent tile, like a System Settings icon.
        let tile = egui::Rect::from_center_size(
            item.left_center() + egui::vec2(18.0, 0.0),
            egui::vec2(20.0, 20.0),
        );
        painter.rect_filled(tile, 5.0, palette.accent);
        let drive = egui::Rect::from_center_size(tile.center(), egui::vec2(12.0, 7.0));
        painter.rect_stroke(
            drive,
            2.0,
            egui::Stroke::new(1.2, palette.shell),
            egui::StrokeKind::Inside,
        );
        painter.circle_filled(
            drive.right_center() - egui::vec2(3.0, 0.0),
            1.1,
            palette.shell,
        );
        painter.text(
            tile.right_center() + egui::vec2(9.0, 0.0),
            egui::Align2::LEFT_CENTER,
            "Storage",
            egui::FontId::proportional(13.5),
            palette.text,
        );
    });
    close
}

/// The macOS close, minimise and zoom controls. Only close is offered;
/// the other two are drawn in the grey macOS uses for unavailable ones.
/// Hovering the group reveals the × on close, as on macOS. Returns whether
/// close was clicked.
fn window_controls(ui: &mut egui::Ui) -> bool {
    let (strip, _) = ui.allocate_exact_size(egui::vec2(SIDEBAR_WIDTH, 44.0), egui::Sense::hover());
    let centres = [0.0f32, 20.0, 40.0].map(|x| strip.left_top() + egui::vec2(21.0 + x, 22.0));
    let group = egui::Rect::from_min_max(
        centres[0] - egui::vec2(8.0, 8.0),
        centres[2] + egui::vec2(8.0, 8.0),
    );
    let hovering = ui.rect_contains_pointer(group);
    let close = ui.interact(
        egui::Rect::from_center_size(centres[0], egui::vec2(14.0, 14.0)),
        ui.id().with("settings_close"),
        egui::Sense::click(),
    );
    let painter = ui.painter();
    painter.circle(
        centres[0],
        6.0,
        CLOSE_RED,
        egui::Stroke::new(0.5, CLOSE_RED_EDGE),
    );
    if hovering {
        let ink = egui::Stroke::new(1.2, CLOSE_GLYPH);
        let arm = 2.6;
        painter.line_segment(
            [
                centres[0] + egui::vec2(-arm, -arm),
                centres[0] + egui::vec2(arm, arm),
            ],
            ink,
        );
        painter.line_segment(
            [
                centres[0] + egui::vec2(arm, -arm),
                centres[0] + egui::vec2(-arm, arm),
            ],
            ink,
        );
    }
    for centre in &centres[1..] {
        painter.circle_filled(*centre, 6.0, CONTROL_OFF);
    }
    close.on_hover_text("Close (Esc)").clicked()
}

fn storage_section(ui: &mut egui::Ui, state: &mut EditorState, palette: &Palette) {
    ui.label(
        crate::theme::semibold("Storage")
            .size(22.0)
            .color(palette.text),
    );
    ui.add_space(4.0);
    let Some(measured) = state.storage.measured.clone() else {
        ui.label(
            egui::RichText::new("Measuring…")
                .size(13.0)
                .color(palette.muted),
        );
        return;
    };
    let items: Vec<&StorageItem> = measured
        .iter()
        .flat_map(|(_, storage)| &storage.items)
        .collect();
    let total: u64 = items.iter().map(|item| item.bytes).sum();
    let projects = measured.len();
    let summary = match projects {
        1 => format!("{} in 1 project", format_bytes(total)),
        many => format!("{} across {many} projects", format_bytes(total)),
    };
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(summary).size(13.0).color(palette.muted));
        if state.storage.busy {
            ui.spinner();
        }
    });
    ui.add_space(14.0);
    let by_kind: Vec<(StorageKind, u64)> = StorageKind::ALL
        .into_iter()
        .map(|kind| {
            let bytes = items
                .iter()
                .filter(|item| item.kind == kind)
                .map(|item| item.bytes)
                .sum();
            (kind, bytes)
        })
        .filter(|(_, bytes)| *bytes > 0)
        .collect();
    usage_bar(ui, palette, &by_kind, total);
    ui.add_space(10.0);
    usage_key(ui, palette, &by_kind);

    let current = std::fs::canonicalize(state.project.root())
        .unwrap_or_else(|_| state.project.root().to_path_buf());
    // A build of the open project holds Cargo's lock on its build cache.
    let building = state.play.is_playing() || state.export_running;
    let ordered = measured
        .iter()
        .filter(|(entry, _)| entry.root == current)
        .chain(measured.iter().filter(|(entry, _)| entry.root != current));
    for (entry, storage) in ordered {
        let is_current = entry.root == current;
        let when = if is_current {
            "Open now".to_owned()
        } else {
            days_ago(entry)
        };
        group_heading(ui, palette, &project_name(&entry.root), Some(&when));
        project_group(ui, state, storage, is_current && building, palette);
    }
    group_heading(ui, palette, "Automatic cleanup", None);
    automatic_settings(ui, state, palette);
}

/// The storage bar: one rounded track, split into a segment per kind in
/// proportion to its size, with hairline gaps between segments.
fn usage_bar(ui: &mut egui::Ui, palette: &Palette, by_kind: &[(StorageKind, u64)], total: u64) {
    let (bar, _) = ui.allocate_exact_size(egui::vec2(CONTENT_WIDTH, 10.0), egui::Sense::hover());
    let painter = ui.painter();
    let radius = 5u8;
    painter.rect_filled(bar, radius, palette.hover);
    if total == 0 {
        return;
    }
    let gap = 2.0;
    let usable = bar.width() - gap * by_kind.len().saturating_sub(1) as f32;
    // Every present kind stays visible, however small.
    let widths: Vec<f32> = by_kind
        .iter()
        .map(|(_, bytes)| (usable * *bytes as f32 / total as f32).max(4.0))
        .collect();
    let scale = usable / widths.iter().sum::<f32>().max(1.0);
    let mut x = bar.left();
    let last = by_kind.len().saturating_sub(1);
    for (index, ((kind, _), width)) in by_kind.iter().zip(&widths).enumerate() {
        let width = width * scale;
        let segment =
            egui::Rect::from_min_size(egui::pos2(x, bar.top()), egui::vec2(width, bar.height()));
        let corners = egui::CornerRadius {
            nw: if index == 0 { radius } else { 1 },
            sw: if index == 0 { radius } else { 1 },
            ne: if index == last { radius } else { 1 },
            se: if index == last { radius } else { 1 },
        };
        painter.rect_filled(segment, corners, kind_color(*kind, palette));
        x += width + gap;
    }
}

/// The key under the bar: a dot and name for each kind present.
fn usage_key(ui: &mut egui::Ui, palette: &Palette, by_kind: &[(StorageKind, u64)]) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        if by_kind.is_empty() {
            ui.label(
                egui::RichText::new("Nothing to clean yet")
                    .size(12.0)
                    .color(palette.subtle),
            );
        }
        for (kind, _) in by_kind {
            let (dot, _) = ui.allocate_exact_size(egui::vec2(8.0, 14.0), egui::Sense::hover());
            ui.painter()
                .circle_filled(dot.center(), 4.0, kind_color(*kind, palette));
            ui.label(
                egui::RichText::new(kind.label())
                    .size(12.0)
                    .color(palette.muted),
            );
            ui.add_space(10.0);
        }
    });
}

/// A heading above a group: the name, and an optional quieter note at the
/// right (when the project was opened).
fn group_heading(ui: &mut egui::Ui, palette: &Palette, name: &str, note: Option<&str>) {
    ui.add_space(26.0);
    let (row, _) = ui.allocate_exact_size(egui::vec2(CONTENT_WIDTH, 20.0), egui::Sense::hover());
    let painter = ui.painter();
    // The note first, so a long project name stops short of it.
    let note_width = note.map_or(0.0, |note| {
        let galley = painter.layout_no_wrap(
            note.to_owned(),
            egui::FontId::proportional(12.0),
            palette.subtle,
        );
        let width = galley.size().x;
        painter.galley(
            row.right_center() - egui::vec2(width, galley.size().y / 2.0),
            galley,
            palette.subtle,
        );
        width + 16.0
    });
    let heading = crate::theme::weighted_galley(
        painter,
        name,
        egui::FontId::proportional(14.0),
        palette.text,
        crate::theme::SEMIBOLD,
        Some(CONTENT_WIDTH - note_width),
    );
    let top = row.center().y - heading.size().y / 2.0;
    painter.galley(egui::pos2(row.left(), top), heading, palette.text);
    ui.add_space(8.0);
}

/// A rounded group of rows; `rows` draws them, using [`group_row`].
fn group(ui: &mut egui::Ui, palette: &Palette, rows: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::NONE
        .fill(palette.raised)
        .corner_radius(GROUP_RADIUS)
        .inner_margin(egui::Margin::symmetric(GROUP_PADDING as i8, 0))
        .show(ui, |ui| {
            ui.set_width(CONTENT_WIDTH - 2.0 * GROUP_PADDING);
            ui.spacing_mut().item_spacing.y = 0.0;
            rows(ui);
        });
}

/// What one [`group_row`] shows on its left.
struct Row<'a> {
    /// Whether this is the group's first row (no divider above it).
    first: bool,
    /// A colour dot linking the row to its segment of the storage bar.
    swatch: Option<egui::Color32>,
    /// The row's name.
    title: &'a str,
    /// A smaller line under the title.
    detail: Option<&'a str>,
    /// Dimmed and non-interactive when `false`.
    enabled: bool,
}

/// One row of a [`group`], laid out exactly: a fixed height, the swatch,
/// title and detail centred vertically on the left, and `trailing`
/// controls in a right-aligned slot on the same line. A hairline, inset to
/// the text, separates it from the row above.
fn group_row(
    ui: &mut egui::Ui,
    palette: &Palette,
    row: Row<'_>,
    trailing: impl FnOnce(&mut egui::Ui),
) {
    let height = if row.detail.is_some() { 52.0 } else { 42.0 };
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::hover(),
    );
    let painter = ui.painter();
    let text_left = rect.left() + if row.swatch.is_some() { 22.0 } else { 0.0 };
    if !row.first {
        painter.line_segment(
            [
                egui::pos2(text_left, rect.top()),
                rect.right_top() + egui::vec2(GROUP_PADDING, 0.0),
            ],
            egui::Stroke::new(1.0, palette.hover),
        );
    }
    if let Some(swatch) = row.swatch {
        // The same dot as the bar's key, so the row reads as that segment.
        painter.circle_filled(egui::pos2(rect.left() + 5.0, rect.center().y), 4.0, swatch);
    }
    // Controls first, so the text knows how much room they leave.
    let slot = egui::Rect::from_min_max(egui::pos2(rect.center().x, rect.top()), rect.max);
    let mut controls = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(slot)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    controls.spacing_mut().item_spacing.x = 12.0;
    if !row.enabled {
        controls.disable();
    }
    trailing(&mut controls);
    let controls_left = if controls.min_rect().width() > 0.0 {
        controls.min_rect().left()
    } else {
        rect.right()
    };
    let text_width = (controls_left - text_left - 16.0).max(40.0);

    let (title_color, detail_color) = if row.enabled {
        (palette.text, palette.muted)
    } else {
        (palette.subtle, palette.subtle)
    };
    let painter = ui.painter();
    let title = crate::widgets::one_line(painter, row.title, 13.5, title_color, text_width);
    let detail = row
        .detail
        .map(|detail| crate::widgets::one_line(painter, detail, 12.0, detail_color, text_width));
    let gap = 3.0;
    let block = title.size().y + detail.as_ref().map_or(0.0, |detail| gap + detail.size().y);
    let top = rect.center().y - block / 2.0;
    let title_height = title.size().y;
    painter.galley(egui::pos2(text_left, top), title, title_color);
    if let Some(detail) = detail {
        painter.galley(
            egui::pos2(text_left, top + title_height + gap),
            detail,
            detail_color,
        );
    }
}

fn project_name(root: &Path) -> String {
    root.file_name().map_or_else(
        || root.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

fn days_ago(entry: &LedgerEntry) -> String {
    let now = crate::storage::seconds(std::time::SystemTime::now());
    match now.saturating_sub(entry.last_opened) / (24 * 60 * 60) {
        0 => "Opened today".to_owned(),
        1 => "Opened yesterday".to_owned(),
        days => format!("Opened {days} days ago"),
    }
}

/// One group per project: a row per kind of storage it has.
fn project_group(
    ui: &mut egui::Ui,
    state: &mut EditorState,
    storage: &ProjectStorage,
    building: bool,
    palette: &Palette,
) {
    group(ui, palette, |ui| {
        let mut first = true;
        for kind in StorageKind::ALL {
            let items: Vec<_> = storage.items_of(kind).cloned().collect();
            if items.is_empty() {
                continue;
            }
            let bytes: u64 = items.iter().map(|item| item.bytes).sum();
            let row = Row {
                first,
                swatch: Some(kind_color(kind, palette)),
                title: kind.label(),
                detail: Some(kind.explanation()),
                enabled: true,
            };
            group_row(ui, palette, row, |ui| {
                if kind.cleanable() {
                    let button = crate::widgets::pill_button(
                        ui,
                        palette,
                        "Clean",
                        palette.accent,
                        !building && !state.storage.busy,
                    )
                    .on_disabled_hover_text(if building {
                        "Stop Play, Run or Export first: the build is using this cache"
                    } else {
                        "Busy…"
                    });
                    if button.clicked() {
                        if kind == StorageKind::OldEngineCopy {
                            // Engine copies sit beside the user's code, so ask once.
                            state.storage.confirm = Some((storage.root.clone(), items.clone()));
                        } else {
                            for item in items.iter().cloned() {
                                state.storage.clean(storage.root.clone(), item);
                            }
                        }
                    }
                } else if let Some(first) = items.first()
                    && crate::widgets::pill_button(
                        ui,
                        palette,
                        "Show in Finder",
                        palette.text,
                        true,
                    )
                    .clicked()
                {
                    state.storage.reveal = Some(first.path.clone());
                }
                ui.label(
                    egui::RichText::new(format_bytes(bytes))
                        .size(13.0)
                        .color(palette.muted),
                );
            });
            first = false;
        }
        if first {
            let row = Row {
                first: true,
                swatch: None,
                title: "Nothing to clean",
                detail: None,
                enabled: true,
            };
            group_row(ui, palette, row, |_| {});
        }
    });
}

fn automatic_settings(ui: &mut egui::Ui, state: &mut EditorState, palette: &Palette) {
    let settings = &mut state.settings.storage;
    let before = *settings;
    group(ui, palette, |ui| {
        let automatic = Row {
            first: true,
            swatch: None,
            title: "Clean up automatically",
            detail: Some("Skips the project you have open. Each cleanup is listed in Output."),
            enabled: true,
        };
        group_row(ui, palette, automatic, |ui| {
            crate::widgets::toggle(ui, palette, &mut settings.automatic);
        });
        let enabled = settings.automatic;
        let days = Row {
            first: false,
            swatch: None,
            title: "Remove a project's build cache after",
            detail: None,
            enabled,
        };
        group_row(ui, palette, days, |ui| {
            if let Some(days) = crate::widgets::popup_choice(
                ui,
                palette,
                settings.unused_days,
                &DAY_CHOICES,
                "days",
            ) {
                settings.unused_days = days;
            }
        });
        let cap = Row {
            first: false,
            swatch: None,
            title: "Keep all build caches under",
            detail: None,
            enabled,
        };
        group_row(ui, palette, cap, |ui| {
            if let Some(gigabytes) = crate::widgets::popup_choice(
                ui,
                palette,
                settings.max_build_cache_gb,
                &GIGABYTE_CHOICES,
                "GB",
            ) {
                settings.max_build_cache_gb = gigabytes;
            }
        });
    });
    if *settings != before {
        state.storage.mark_changed();
    }
}

/// Asks once before deleting old engine copies.
fn confirm_engine_copies(ctx: &egui::Context, state: &mut EditorState) {
    let Some((root, items)) = state.storage.confirm.clone() else {
        return;
    };
    let bytes: u64 = items.iter().map(|item| item.bytes).sum();
    let palette = crate::theme::NIGHT;
    egui::Modal::new(egui::Id::new("confirm_engine_copies"))
        .frame(
            egui::Frame::NONE
                .fill(palette.raised)
                .stroke(egui::Stroke::new(1.0, palette.selected))
                .corner_radius(WINDOW_RADIUS)
                .inner_margin(egui::Margin::symmetric(22, 20)),
        )
        .show(ctx, |ui| {
            palette.apply(ui.visuals_mut());
            ui.set_max_width(340.0);
            ui.label(
                egui::RichText::new("Delete old engine copies?")
                    .size(15.0)
                    .color(palette.text),
            );
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(format!(
                    "This frees {}. Upgrade Rust SDK left them behind, and this project no \
                     longer uses them.",
                    format_bytes(bytes),
                ))
                .size(12.5)
                .color(palette.muted),
            );
            ui.add_space(14.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if crate::widgets::pill_button(ui, &palette, "Delete", palette.danger, true)
                    .clicked()
                {
                    for item in items.iter().cloned() {
                        state.storage.clean(root.clone(), item);
                    }
                    state.storage.confirm = None;
                }
                if crate::widgets::pill_button(ui, &palette, "Cancel", palette.text, true).clicked()
                {
                    state.storage.confirm = None;
                }
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_project::storage::StorageItem;
    use std::path::PathBuf;

    /// Studio state with the Settings window open over realistic storage:
    /// the open project with a build cache and a backup, and an older one.
    fn settings_state() -> (PathBuf, EditorState) {
        let root =
            std::env::temp_dir().join(format!("vge-settings-ui-{}", uuid::Uuid::new_v4().simple()));
        let project = engine_project::Project::create(&root, "Island").expect("project");
        let mut state = EditorState::new(
            project,
            engine_ecs::prelude::World::new(),
            Vec::new(),
            crate::ConsoleLog::new(),
        );
        let current = std::fs::canonicalize(&root).expect("root");
        let item = |kind, path: PathBuf, bytes| StorageItem { kind, path, bytes };
        let now = crate::storage::seconds(std::time::SystemTime::now());
        state.storage.measured = Some(vec![
            (
                LedgerEntry {
                    root: current.clone(),
                    last_opened: now,
                },
                ProjectStorage {
                    root: current.clone(),
                    items: vec![
                        item(
                            StorageKind::BuildCache,
                            current.join("target"),
                            4_100_000_000,
                        ),
                        item(StorageKind::Backup, current.join("project.ron.v1.bak"), 342),
                    ],
                },
            ),
            (
                LedgerEntry {
                    root: PathBuf::from("/Games/Space Game"),
                    last_opened: now - 41 * 24 * 60 * 60,
                },
                ProjectStorage {
                    root: PathBuf::from("/Games/Space Game"),
                    items: vec![item(
                        StorageKind::BuildCache,
                        PathBuf::from("/Games/Space Game/target"),
                        3_900_000_000,
                    )],
                },
            ),
        ]);
        state.settings_open = true;
        (root, state)
    }

    fn draw(ui: &mut egui::Ui, state: &mut EditorState) {
        ui.painter()
            .rect_filled(ui.max_rect(), 0.0, crate::theme::NIGHT.shell);
        show(ui.ctx(), state);
    }

    #[test]
    fn everything_in_settings_stays_inside_the_window() {
        let (root, mut state) = settings_state();
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let mut output = egui::FullOutput::default();
        for frame in 0..4 {
            output.textures_delta.clear();
            output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1100.0, 760.0),
                    )),
                    time: Some(f64::from(frame) * 0.5),
                    ..Default::default()
                },
                |ui| draw(ui, &mut state),
            );
        }
        output.textures_delta.clear();
        // The sidebar background spans the window's full height from its
        // top-left corner, so it gives the window's rectangle.
        let sidebar = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Rect(rect)
                    if rect.fill == crate::theme::NIGHT.shell
                        && (rect.rect.width() - SIDEBAR_WIDTH).abs() < 1.0 =>
                {
                    Some(rect.rect)
                }
                _ => None,
            })
            .expect("settings sidebar");
        let window =
            egui::Rect::from_min_size(sidebar.min, egui::vec2(WINDOW_WIDTH, sidebar.height()));
        let inside = window.expand(0.5);
        for shape in &output.shapes {
            match &shape.shape {
                egui::epaint::Shape::Text(text) => {
                    let rect = text.visual_bounding_rect();
                    assert!(
                        inside.contains_rect(rect),
                        "{:?} at {rect:?} leaves the window {window:?}",
                        text.galley.text()
                    );
                }
                egui::epaint::Shape::Rect(rect) if rect.fill == crate::theme::NIGHT.raised => {
                    assert!(
                        inside.contains_rect(rect.rect),
                        "a card at {:?} leaves the window",
                        rect.rect
                    );
                }
                _ => {}
            }
        }
        let _ = std::fs::remove_dir_all(root);
    }

    /// Writes `settings-<weight>.png` at regular, medium and semibold,
    /// to compare font weights; needs a GPU adapter.
    #[test]
    #[ignore = "needs a GPU adapter; writes PNGs for a visual check"]
    fn snapshot_settings_font_weights() {
        let (root, mut state) = settings_state();
        for weight in [400.0f32, 500.0, 600.0] {
            let path = crate::ui_snapshot::snapshot_dir().join(format!("settings-{weight}.png"));
            crate::ui_snapshot::render_png_with_weight(
                &path,
                egui::vec2(900.0, 700.0),
                2.0,
                weight,
                |ui| draw(ui, &mut state),
            )
            .expect("snapshot");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    /// Writes `settings.png` for a visual check; needs a GPU adapter.
    #[test]
    #[ignore = "needs a GPU adapter; writes a PNG for a visual check"]
    fn snapshot_settings_window() {
        let (root, mut state) = settings_state();
        for (file, automatic) in [("settings.png", false), ("settings-automatic.png", true)] {
            state.settings.storage.automatic = automatic;
            let path = crate::ui_snapshot::snapshot_dir().join(file);
            crate::ui_snapshot::render_png(&path, egui::vec2(900.0, 700.0), 2.0, |ui| {
                draw(ui, &mut state);
            })
            .expect("snapshot");
        }
        let _ = std::fs::remove_dir_all(root);
    }
}
