//! The Inspector's World area section: which scene loads while the camera
//! is near this object, how big the area is, and how near counts.
//!
//! Everything is the object's [`SceneStreamArea`], so edits save with the
//! scene and ⌘Z undoes them like any other field.

use engine_ecs::prelude::{Entity, World};
use engine_scene::{MAX_AREA_DISTANCE, SceneStreamArea};

use super::{ProjectScenes, note, number, property_row, scene_label, scene_picker};

/// The section's body. Returns whether anything changed.
pub(super) fn inspect(world: &mut World, entity: Entity, ui: &mut egui::Ui) -> bool {
    let Some(before) = world
        .get::<SceneStreamArea>(entity)
        .map(|area| area.0.clone())
    else {
        return false;
    };
    // The loading screen is never a world area.
    let scenes: Vec<String> = world
        .get_resource::<ProjectScenes>()
        .map_or_else(Vec::new, |list| list.0.clone())
        .into_iter()
        .filter(|scene| scene != engine_project::LOADING_SCENE)
        .collect();
    let mut value = before.clone();

    property_row(ui, "Scene", |ui| {
        scene_picker(ui, (entity, usize::MAX), &mut value.scene, &scenes);
    });
    property_row(ui, "Size", |ui| {
        for (axis, half) in ["X ", "Y ", "Z "]
            .into_iter()
            .zip(value.half_extents.iter_mut())
        {
            // Shown and edited as the full size; saved as half of it.
            let mut size = *half * 2.0;
            if number(
                ui,
                egui::DragValue::new(&mut size)
                    .prefix(axis)
                    .speed(0.25)
                    .range(0.1..=MAX_AREA_DISTANCE),
            )
            .changed()
            {
                *half = size / 2.0;
            }
        }
    });
    property_row(ui, "Load within", |ui| {
        number(
            ui,
            egui::DragValue::new(&mut value.load_distance)
                .speed(0.25)
                .range(0.0..=MAX_AREA_DISTANCE)
                .suffix(" m"),
        )
        .on_hover_text("How close to the box the game camera must come for the scene to load");
    });
    if value.scene.is_empty() {
        note(ui, "Pick the scene that holds this part of the world.");
    } else {
        note(
            ui,
            &format!(
                "Loads {} when the game camera comes within {} m of the box, and unloads it \
                 once the camera has gone. Edit it with Open beside in the Scenes window.",
                scene_label(&value.scene),
                value.load_distance.round()
            ),
        );
    }

    let changed = value != before;
    if changed && let Ok(mut entity_mut) = world.get_entity_mut(entity) {
        entity_mut.insert(SceneStreamArea(value));
    }
    changed
}
