//! The Inspector's Road section: the road's shape, how it presses into its
//! terrain, the paint laid along it and its drawn surface.
//!
//! Everything is the object's [`SceneRoad`], so edits save with the scene
//! and ⌘Z undoes them like any other field. **Apply to terrain** is asked of
//! the shell ([`take_apply_request`]), which owns the terrain files.

use engine_ecs::prelude::{Entity, World};
use engine_scene::{MAX_ROAD_POINTS, MAX_ROAD_WIDTH, SceneObjectId, SceneRoad, SceneTerrain};

use crate::assets::{AssetIndex, AssetKind};

use super::{choice, note, number, property_row, slider, subsection};

/// Where the section leaves an Apply to terrain click for the shell.
const APPLY_REQUEST: &str = "inspector.road.apply";

/// The road whose Apply to terrain button was clicked this frame, if any.
pub(crate) fn take_apply_request(ctx: &egui::Context) -> Option<Entity> {
    ctx.data_mut(|data| {
        let id = egui::Id::new(APPLY_REQUEST);
        let request = data.get_temp::<Entity>(id);
        data.remove::<Entity>(id);
        request
    })
}

/// Every terrain in the scene as `(object id, name)`.
fn terrains(world: &mut World) -> Vec<(String, String)> {
    let found: Vec<Entity> = world
        .query_filtered::<Entity, bevy_ecs::query::With<SceneTerrain>>()
        .iter(world)
        .collect();
    found
        .into_iter()
        .map(|entity| {
            let id = crate::foliage::ensure_object_id(world, entity);
            (id, crate::hierarchy::display_name(world, entity))
        })
        .collect()
}

/// The paint layer names of the terrain with object id `id`.
fn layer_names(world: &mut World, id: Option<&str>) -> Vec<String> {
    let Some(id) = id else {
        return Vec::new();
    };
    world
        .query::<(&SceneObjectId, &SceneTerrain)>()
        .iter(world)
        .find(|(object, _)| object.0 == id)
        .map(|(_, terrain)| {
            terrain
                .0
                .layers
                .iter()
                .map(|layer| layer.name.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// The section's body. Returns whether anything changed.
pub(super) fn inspect(
    world: &mut World,
    entity: Entity,
    ui: &mut egui::Ui,
    assets: &AssetIndex,
) -> bool {
    let Some(before) = world.get::<SceneRoad>(entity).map(|road| road.0.clone()) else {
        return false;
    };
    let mut value = before.clone();
    let terrains = terrains(world);
    let layers = layer_names(world, value.terrain.as_deref());
    let textures: Vec<(String, String)> = assets
        .entries()
        .filter(|(path, _)| AssetKind::from_path(path) == AssetKind::Texture)
        .map(|(path, id)| (id.to_string(), path.display().to_string()))
        .collect();

    property_row(ui, "Width", |ui| {
        number(
            ui,
            egui::DragValue::new(&mut value.width)
                .speed(0.05)
                .range(0.1..=MAX_ROAD_WIDTH)
                .suffix(" m"),
        )
    });
    property_row(ui, "Soft edge", |ui| {
        number(
            ui,
            egui::DragValue::new(&mut value.edge)
                .speed(0.05)
                .range(0.0..=MAX_ROAD_WIDTH)
                .suffix(" m"),
        )
        .on_hover_text("How far beside the road the ground blends back to its own height")
    });
    property_row(ui, "Loop", |ui| {
        ui.checkbox(&mut value.closed, "Join the end to the start")
    });

    subsection(ui, "Terrain", |ui| {
        property_row(ui, "Press into", |ui| {
            let current = value
                .terrain
                .as_ref()
                .and_then(|id| terrains.iter().find(|(tid, _)| tid == id))
                .map_or("None", |(_, name)| name.as_str())
                .to_owned();
            choice((entity, "road-terrain"), ui)
                .selected_text(current)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut value.terrain, None, "None");
                    for (id, name) in &terrains {
                        ui.selectable_value(&mut value.terrain, Some(id.clone()), name);
                    }
                });
        });
        property_row(ui, "Paint", |ui| {
            let current = value
                .paint_layer
                .and_then(|layer| layers.get(layer as usize))
                .map_or("None", String::as_str)
                .to_owned();
            choice((entity, "road-layer"), ui)
                .selected_text(current)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut value.paint_layer, None, "None");
                    for (index, name) in layers.iter().enumerate() {
                        let index = u32::try_from(index).unwrap_or(u32::MAX);
                        ui.selectable_value(&mut value.paint_layer, Some(index), name);
                    }
                });
        });
        if value.paint_layer.is_some() {
            property_row(ui, "Strength", |ui| {
                slider(ui, &mut value.paint_strength, 0.0..=1.0)
            });
        }
        let ready = value.terrain.is_some() && value.points.len() >= 2;
        if ui
            .add_enabled(ready, egui::Button::new("Apply to terrain"))
            .on_hover_text("Level the ground along the road and paint it; ⌘Z undoes it")
            .on_disabled_hover_text("Pick a terrain and give the road at least two points")
            .clicked()
        {
            ui.ctx()
                .data_mut(|data| data.insert_temp(egui::Id::new(APPLY_REQUEST), entity));
        }
    });

    subsection(ui, "Surface", |ui| {
        property_row(ui, "Show", |ui| {
            ui.checkbox(&mut value.surface, "Draw the road")
        });
        if value.surface {
            property_row(ui, "Color", |ui| {
                ui.color_edit_button_rgba_unmultiplied(&mut value.color)
            });
            property_row(ui, "Image", |ui| {
                let current = value
                    .texture
                    .as_ref()
                    .and_then(|texture| textures.iter().find(|(id, _)| *id == texture.id))
                    .map_or("None", |(_, path)| path.as_str())
                    .to_owned();
                choice((entity, "road-image"), ui)
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut value.texture, None, "None");
                        for (id, path) in &textures {
                            ui.selectable_value(
                                &mut value.texture,
                                Some(engine_scene::AssetRef { id: id.clone() }),
                                path,
                            );
                        }
                    });
            });
            property_row(ui, "Repeat", |ui| {
                number(
                    ui,
                    egui::DragValue::new(&mut value.tiling)
                        .speed(0.05)
                        .range(0.1..=MAX_ROAD_WIDTH)
                        .suffix(" m"),
                )
                .on_hover_text("Length of road one copy of the image covers")
            });
        }
    });

    subsection(ui, "Points", |ui| {
        for (index, point) in value.points.iter_mut().enumerate() {
            property_row(ui, &format!("Point {}", index + 1), |ui| {
                for (axis, coordinate) in ["X ", "Y ", "Z "].into_iter().zip(point.iter_mut()) {
                    number(
                        ui,
                        egui::DragValue::new(coordinate).prefix(axis).speed(0.05),
                    );
                }
            });
        }
        if !value.points.is_empty() {
            property_row(ui, "Remove", |ui| {
                let mut remove = None;
                choice((entity, "road-remove"), ui)
                    .selected_text("Pick a point")
                    .show_ui(ui, |ui| {
                        for index in 0..value.points.len() {
                            if ui.button(format!("Point {}", index + 1)).clicked() {
                                remove = Some(index);
                            }
                        }
                    });
                if let Some(index) = remove {
                    value.points.remove(index);
                }
            });
        }
        let room = value.points.len() < MAX_ROAD_POINTS;
        if ui
            .add_enabled(room, egui::Button::new("Add point"))
            .on_hover_text("Add a point past the end; or ⌥-click the ground in the Scene View")
            .clicked()
        {
            value.points.push(next_point(&value.points));
        }
    });
    note(
        ui,
        "Drag the dots in the Scene View to move points; ⌥-click the ground to add one. \
         Apply to terrain again after moving points.",
    );

    let changed = value != before;
    if changed && let Ok(mut entity_mut) = world.get_entity_mut(entity) {
        entity_mut.insert(SceneRoad(value));
    }
    changed
}

/// A new point 5 m past the last one, carrying on in the road's direction.
fn next_point(points: &[[f32; 3]]) -> [f32; 3] {
    match points {
        [] => [0.0, 0.0, 0.0],
        [only] => [only[0], only[1], only[2] + 5.0],
        [.., before, last] => {
            let along = glam::Vec3::from(*last) - glam::Vec3::from(*before);
            let step = along.try_normalize().unwrap_or(glam::Vec3::Z) * 5.0;
            (glam::Vec3::from(*last) + step).to_array()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_point_carries_on_past_the_end() {
        assert_eq!(next_point(&[]), [0.0, 0.0, 0.0]);
        assert_eq!(next_point(&[[1.0, 0.0, 1.0]]), [1.0, 0.0, 6.0]);
        assert_eq!(
            next_point(&[[0.0, 0.0, 0.0], [10.0, 0.0, 0.0]]),
            [15.0, 0.0, 0.0]
        );
    }
}
