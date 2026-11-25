//! Material files and images: the object rows of the Material section, and
//! the editor for a material file selected in Project Assets.

use engine_scene::{AssetRef, MaterialAlpha, MaterialData, MaterialImageRefs};

use crate::assets::{AssetIndex, AssetKind};

use super::{asset_drop_zone, choice, note, property_row, slider, subsection};

/// The file name `reference` points at, if the project has it.
fn asset_name(assets: &AssetIndex, reference: &AssetRef) -> Option<String> {
    crate::assets::parse_asset_id(&reference.id)
        .and_then(|id| assets.path_for(id))
        .and_then(|path| path.file_name())
        .and_then(|name| name.to_str())
        .map(str::to_owned)
}

/// A drop slot for one asset of `kind` with a small Clear button: `slot` is
/// set to a dropped asset, or cleared. `empty` is shown when it has none.
fn asset_slot(
    ui: &mut egui::Ui,
    label: &str,
    slot: &mut Option<AssetRef>,
    kind: AssetKind,
    empty: &str,
    assets: &AssetIndex,
) -> bool {
    let palette = crate::theme::NIGHT;
    let mut changed = false;
    property_row(ui, label, |ui| {
        let name = match slot.as_ref() {
            Some(reference) => asset_name(assets, reference).unwrap_or_else(|| "Missing".into()),
            None => empty.to_owned(),
        };
        ui.horizontal(|ui| {
            let room = ui.available_width() - if slot.is_some() { 58.0 } else { 0.0 };
            let dropped = ui
                .allocate_ui(egui::vec2(room.max(60.0), 30.0), |ui| {
                    asset_drop_zone(ui, &name, None, kind)
                })
                .inner;
            if let Some(path) = dropped
                && let Some(id) = assets.id_for(&path)
            {
                *slot = Some(AssetRef { id: id.to_string() });
                changed = true;
            }
            if slot.is_some()
                && crate::widgets::pill_button(ui, &palette, "Clear", palette.muted, true).clicked()
            {
                *slot = None;
                changed = true;
            }
        });
    });
    changed
}

/// The five image slots, each `empty` when it has no image.
fn image_slots(
    ui: &mut egui::Ui,
    images: &mut MaterialImageRefs,
    empty: &str,
    assets: &AssetIndex,
) -> bool {
    let mut changed = false;
    for (label, slot) in [
        ("Colour", &mut images.base_color),
        ("Bumps", &mut images.normal),
        ("Metal / rough", &mut images.metallic_roughness),
        ("Shadowing", &mut images.occlusion),
        ("Glow", &mut images.emissive),
    ] {
        changed |= asset_slot(ui, label, slot, AssetKind::Texture, empty, assets);
    }
    changed
}

/// The material file and image rows of an object's Material section.
/// Returns whether anything changed.
pub(super) fn object_rows(
    ui: &mut egui::Ui,
    value: &mut engine_scene::MaterialOverrideData,
    assets: &AssetIndex,
) -> bool {
    let mut changed = asset_slot(
        ui,
        "Material",
        &mut value.material,
        AssetKind::Material,
        "Model's own — drop a material",
        assets,
    );
    subsection(ui, "Images", |ui| {
        note(ui, "Drop an image to use it instead of the material's own.");
        changed |= image_slots(ui, &mut value.images, "The material's", assets);
    });
    changed
}

/// Edits a material file's settings, shown in the Inspector while the file
/// is selected in Project Assets. Returns whether anything changed; the
/// caller saves the file.
pub fn material_file_editor(
    ui: &mut egui::Ui,
    name: &str,
    data: &mut MaterialData,
    assets: &AssetIndex,
) -> bool {
    let palette = crate::theme::NIGHT;
    super::inspector_style(ui.style_mut());
    let mut changed = false;
    super::section(ui, name, None::<fn(&mut egui::Ui)>, |ui| {
        note(ui, "Drop it on a model in the Scene View to use it.");
        property_row(ui, "Colour", |ui| {
            changed |= ui
                .color_edit_button_rgba_unmultiplied(&mut data.base_color)
                .changed();
        });
        property_row(ui, "Metallic", |ui| {
            changed |= slider(ui, &mut data.metallic, 0.0..=1.0);
        });
        property_row(ui, "Roughness", |ui| {
            changed |= slider(ui, &mut data.roughness, 0.0..=1.0);
        });
        property_row(ui, "Glow", |ui| {
            changed |= ui.color_edit_button_rgb(&mut data.emissive).changed();
        });
        property_row(ui, "Bumpiness", |ui| {
            changed |= slider(ui, &mut data.normal_strength, 0.0..=2.0);
        });
        property_row(ui, "Repeat", |ui| {
            for value in &mut data.uv_scale {
                changed |= super::number(
                    ui,
                    egui::DragValue::new(value).speed(0.05).range(0.01..=1000.0),
                )
                .changed();
            }
        });
        property_row(ui, "Shift", |ui| {
            for value in &mut data.uv_offset {
                changed |= super::number(
                    ui,
                    egui::DragValue::new(value)
                        .speed(0.01)
                        .range(-1000.0..=1000.0),
                )
                .changed();
            }
        });
        property_row(ui, "See-through", |ui| {
            let label = |alpha: MaterialAlpha| match alpha {
                MaterialAlpha::Opaque => "Solid",
                MaterialAlpha::Cutout { .. } => "Cut out",
                MaterialAlpha::Blend => "Blended",
            };
            choice("material-file-alpha", ui)
                .selected_text(label(data.alpha))
                .show_ui(ui, |ui| {
                    for option in [
                        MaterialAlpha::Opaque,
                        MaterialAlpha::Cutout { threshold: 0.5 },
                        MaterialAlpha::Blend,
                    ] {
                        let picked =
                            std::mem::discriminant(&option) == std::mem::discriminant(&data.alpha);
                        if ui.selectable_label(picked, label(option)).clicked() && !picked {
                            data.alpha = option;
                            changed = true;
                        }
                    }
                });
        });
        if let MaterialAlpha::Cutout { threshold } = &mut data.alpha {
            property_row(ui, "Cut below", |ui| {
                changed |= slider(ui, threshold, 0.0..=1.0);
            });
        }
        property_row(ui, "Two-sided", |ui| {
            changed |= crate::widgets::checkbox(ui, &palette, &mut data.two_sided, "")
                .on_hover_text("Draw the back of each face too, for leaves and cloth")
                .changed();
        });
        subsection(ui, "Images", |ui| {
            changed |= image_slots(ui, &mut data.images, "None — drop an image", assets);
        });
    });
    changed
}
