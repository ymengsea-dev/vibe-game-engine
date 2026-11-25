//! The Inspector's light sections beyond the basics: the Spot Light section
//! and the "Casts shadows" row shared with point lights.
//!
//! Everything is the object's light component, so edits save with the scene
//! and ⌘Z undoes them like any other field.

use engine_ecs::prelude::{Entity, World};
use engine_renderer::MAX_SHADOWED_LIGHTS;
use engine_scene::{MAX_SPOT_ANGLE_DEGREES, ScenePointLight, SceneSpotLight};

use super::{note, property_row, slider};

/// The Spot Light section's body. Returns whether anything changed.
pub(super) fn spot(world: &mut World, entity: Entity, ui: &mut egui::Ui) -> bool {
    let Some(before) = world.get::<SceneSpotLight>(entity).map(|light| light.0) else {
        return false;
    };
    let mut data = before;
    property_row(ui, "Color", |ui| {
        ui.color_edit_button_rgb(&mut data.color);
    });
    property_row(ui, "Intensity", |ui| {
        slider(ui, &mut data.intensity, 0.0..=1000.0);
    });
    property_row(ui, "Range", |ui| {
        slider(ui, &mut data.range, 0.01..=10_000.0);
    });
    // Both in degrees: the cone is half the beam's width, the soft edge how
    // much of that the beam takes to fade out.
    property_row(ui, "Cone angle", |ui| {
        slider(
            ui,
            &mut data.outer_angle_degrees,
            1.0..=MAX_SPOT_ANGLE_DEGREES,
        )
    });
    property_row(ui, "Soft edge", |ui| {
        let mut soft = data.outer_angle_degrees - data.inner_angle_degrees;
        if slider(ui, &mut soft, 0.0..=data.outer_angle_degrees) {
            data.inner_angle_degrees = (data.outer_angle_degrees - soft).max(0.0);
        }
    });
    data.inner_angle_degrees = data.inner_angle_degrees.min(data.outer_angle_degrees);
    shadow_row(ui, world, &mut data.casts_shadows);
    note(
        ui,
        "Shines along the object's forward direction. Turn the object to aim it.",
    );

    let changed = data != before;
    if changed && let Ok(mut entity_mut) = world.get_entity_mut(entity) {
        entity_mut.insert(SceneSpotLight(data));
    }
    changed
}

/// The "Shadows" row (whether the light casts them), with a note once more
/// lights cast shadows than can at a time.
pub(super) fn shadow_row(ui: &mut egui::Ui, world: &mut World, casts_shadows: &mut bool) {
    let palette = crate::theme::NIGHT;
    property_row(ui, "Shadows", |ui| {
        crate::widgets::checkbox(ui, &palette, casts_shadows, "").on_hover_text(
            "Objects block this light and cast shadows. Shadows cost time each frame, so tick \
             only the lights that need them",
        );
    });
    if *casts_shadows && shadow_casting_lights(world) > MAX_SHADOWED_LIGHTS {
        note(
            ui,
            &format!(
                "Only the {MAX_SHADOWED_LIGHTS} shadow-casting lights nearest the camera cast \
                 shadows at a time."
            ),
        );
    }
}

/// How many point and spot lights in the world cast shadows.
fn shadow_casting_lights(world: &mut World) -> usize {
    let points = world
        .query::<&ScenePointLight>()
        .iter(world)
        .filter(|light| light.0.casts_shadows)
        .count();
    let spots = world
        .query::<&SceneSpotLight>()
        .iter(world)
        .filter(|light| light.0.casts_shadows)
        .count();
    points + spots
}
