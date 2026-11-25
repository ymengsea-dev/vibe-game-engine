//! The Tilemap section: the sheet, the brush (one tile or a rule tile) and
//! the layer list.

use engine_ecs::components::{Hidden, Lock};
use engine_ecs::prelude::{Entity, World};
use engine_scene::{AutoTileLayout, SceneTilemap};

use crate::assets::AssetIndex;

use super::{choice, note, number, property_row, slider, subsection};

/// Where the section leaves a layer's Hide or Show click for the shell: the
/// layer object's ID. Hiding is the Hierarchy's eye, kept with the editor
/// session.
const HIDE_REQUEST: &str = "inspector.tilemap.hide";

/// The layer object whose Hide or Show was clicked this frame, if any.
pub(crate) fn take_hide_request(ctx: &egui::Context) -> Option<String> {
    ctx.data_mut(|data| {
        let id = egui::Id::new(HIDE_REQUEST);
        let request = data.get_temp::<String>(id);
        data.remove::<String>(id);
        request
    })
}

/// The Tilemap section's body for the map at `entity`.
pub(super) fn show(ui: &mut egui::Ui, world: &mut World, entity: Entity, assets: &AssetIndex) {
    let palette = crate::theme::NIGHT;
    let Some(mut data) = world
        .get::<SceneTilemap>(entity)
        .map(|source| source.0.clone())
    else {
        return;
    };
    let image = crate::assets::parse_asset_id(&data.atlas.id)
        .and_then(|id| assets.path_for(id))
        .and_then(|path| path.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("Missing image");
    property_row(ui, "Image", |ui| {
        ui.add(egui::Label::new(image).truncate());
    });
    note(ui, "Paint or erase from Viewport → Tilemap in 2D mode.");
    property_row(ui, "Columns", |ui| {
        number(ui, egui::DragValue::new(&mut data.columns).range(1..=4096));
    });
    property_row(ui, "Rows", |ui| {
        number(ui, egui::DragValue::new(&mut data.rows).range(1..=4096));
    });
    data.brush_column = data.brush_column.min(data.columns - 1);
    data.brush_row = data.brush_row.min(data.rows - 1);
    property_row(ui, "Tile size", |ui| {
        for value in &mut data.cell_size {
            number(ui, egui::DragValue::new(value).range(0.01..=10_000.0));
        }
    });

    subsection(ui, "Brush", |ui| {
        property_row(ui, "Paints", |ui| {
            crate::widgets::segmented(&palette).show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                if crate::widgets::segment_label(
                    ui,
                    &palette,
                    data.brush_auto.is_none(),
                    "One tile",
                )
                .on_hover_text("Every painted cell gets the brush cell")
                .clicked()
                {
                    data.brush_auto = None;
                }
                if crate::widgets::segment_label(
                    ui,
                    &palette,
                    data.brush_auto.is_some(),
                    "Rule tile",
                )
                .on_hover_text("Each painted cell picks its tile from the cells round it")
                .clicked()
                    && data.brush_auto.is_none()
                {
                    data.brush_auto = Some(AutoTileLayout::Edges16);
                }
            });
        });
        let cell_label = if data.brush_auto.is_some() {
            "Block corner"
        } else {
            "Brush cell"
        };
        property_row(ui, cell_label, |ui| {
            number(
                ui,
                egui::DragValue::new(&mut data.brush_column)
                    .range(0..=data.columns - 1)
                    .prefix("X "),
            );
            number(
                ui,
                egui::DragValue::new(&mut data.brush_row)
                    .range(0..=data.rows - 1)
                    .prefix("Y "),
            );
        });
        if let Some(layout) = &mut data.brush_auto {
            property_row(ui, "Layout", |ui| {
                let label = |layout: AutoTileLayout| match layout {
                    AutoTileLayout::Edges16 => "16 tiles (4 × 4)",
                    AutoTileLayout::Blob47 => "47 tiles (8 × 6)",
                };
                choice("tilemap-auto-layout", ui)
                    .selected_text(label(*layout))
                    .show_ui(ui, |ui| {
                        for option in [AutoTileLayout::Edges16, AutoTileLayout::Blob47] {
                            ui.selectable_value(layout, option, label(option));
                        }
                    });
            });
            note(
                ui,
                "The block starts at the corner cell. See the Scene View guide for its order.",
            );
        }
        property_row(ui, "Solid", |ui| {
            crate::widgets::checkbox(ui, &palette, &mut data.brush_collision, "Paint solid tiles")
                .on_hover_text(
                    "Add fixed collision to painted cells; repaint with this off to clear it",
                );
        });
    });

    let mut add = false;
    subsection(ui, "Layers", |ui| {
        if data.layers.is_empty() {
            note(
                ui,
                "Background, Ground and Front appear when you first paint.",
            );
            add = crate::widgets::pill_button(ui, &palette, "Add layers", palette.text, true)
                .clicked();
            return;
        }
        // Top of the drawing first, as the layers stack on screen.
        for index in (0..data.layers.len()).rev() {
            layer_row(ui, world, entity, &mut data, index);
        }
        ui.horizontal(|ui| {
            add = crate::widgets::pill_button(
                ui,
                &palette,
                "Add layer",
                palette.text,
                data.layers.len() < engine_scene::MAX_TILE_LAYERS,
            )
            .clicked();
            let active = data.active_layer as usize;
            if crate::widgets::pill_button(
                ui,
                &palette,
                "Move up",
                palette.text,
                active + 1 < data.layers.len(),
            )
            .clicked()
            {
                crate::tilemap::move_layer(&mut data, active, true);
            }
            if crate::widgets::pill_button(ui, &palette, "Move down", palette.text, active > 0)
                .clicked()
            {
                crate::tilemap::move_layer(&mut data, active, false);
            }
        });
        if let Some(layer) = data.layers.get_mut(data.active_layer as usize) {
            property_row(ui, "Name", |ui| {
                crate::widgets::text_field(ui, &mut layer.name, "Layer name");
            });
            if layer.name.trim().is_empty() {
                layer.name = "Layer".to_owned();
            }
            property_row(ui, "Opacity", |ui| {
                slider(ui, &mut layer.opacity, 0.0..=1.0);
            });
            property_row(ui, "Solid", |ui| {
                crate::widgets::checkbox(ui, &palette, &mut layer.solid, "Every cell")
                    .on_hover_text("Give every cell of this layer fixed collision");
            });
        }
    });

    if let Err(error) = crate::tilemap::update_settings(world, entity, data) {
        ui.colored_label(palette.danger, error);
    }
    if add {
        let result = if world
            .get::<SceneTilemap>(entity)
            .is_some_and(|map| map.0.layers.is_empty())
        {
            crate::tilemap::ensure_layers(world, entity).map(|_| ())
        } else {
            crate::tilemap::add_layer(world, entity)
        };
        if let Err(error) = result {
            ui.colored_label(palette.danger, error);
        }
    }
}

/// One row of the layer list: its name (click to paint on it), and Hide and
/// Lock.
fn layer_row(
    ui: &mut egui::Ui,
    world: &mut World,
    parent: Entity,
    data: &mut engine_scene::TilemapData,
    index: usize,
) {
    let palette = crate::theme::NIGHT;
    let layer = data.layers[index].clone();
    let group = crate::tilemap::layer_group(world, parent, &layer);
    let hidden = group.is_some_and(|group| world.get::<Hidden>(group).is_some());
    let locked = group.is_some_and(|group| world.get::<Lock>(group).is_some());
    ui.horizontal(|ui| {
        let active = data.active_layer as usize == index;
        let mut name = layer.name.clone();
        if hidden {
            name.push_str(" · hidden");
        }
        if locked {
            name.push_str(" · locked");
        }
        let width = (ui.available_width() - 120.0).max(60.0);
        let picked = ui
            .add_sized(
                [width, 24.0],
                egui::Button::new(egui::RichText::new(name).size(12.5).color(if active {
                    palette.text
                } else {
                    palette.muted
                }))
                .fill(if active {
                    palette.selected
                } else {
                    egui::Color32::TRANSPARENT
                })
                .stroke(egui::Stroke::NONE)
                .corner_radius(7),
            )
            .on_hover_text("Paint on this layer")
            .clicked();
        if picked {
            data.active_layer = index as u32;
        }
        let show_label = if hidden { "Show" } else { "Hide" };
        if crate::widgets::pill_button(ui, &palette, show_label, palette.muted, group.is_some())
            .on_hover_text("Hide this layer in the Scene View; the game still shows it")
            .clicked()
        {
            ui.ctx().data_mut(|store| {
                store.insert_temp(egui::Id::new(HIDE_REQUEST), layer.object.clone());
            });
        }
        let lock_label = if locked { "Unlock" } else { "Lock" };
        if crate::widgets::pill_button(ui, &palette, lock_label, palette.muted, group.is_some())
            .on_hover_text("A locked layer can't be painted or picked")
            .clicked()
            && let Some(group) = group
        {
            if locked {
                world.entity_mut(group).remove::<Lock>();
            } else {
                world.entity_mut(group).insert(Lock);
            }
        }
    });
}
