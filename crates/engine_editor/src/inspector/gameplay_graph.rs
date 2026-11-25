//! The Inspector's Gameplay graph section: which `.graph` file the object
//! runs, and a button that opens it in the Gameplay graph window.

use engine_ecs::prelude::{Entity, World};
use engine_scene::SceneGameplayGraph;

use crate::assets::{AssetIndex, AssetKind};

use super::{note, property_row};

/// Where the section asks the shell to open a graph file (its path
/// relative to the assets root); the shell takes it once per frame.
pub(crate) fn open_request_id() -> egui::Id {
    egui::Id::new("open-gameplay-graph")
}

/// The section's body. Returns whether anything changed.
pub(super) fn inspect(
    world: &mut World,
    entity: Entity,
    ui: &mut egui::Ui,
    assets: &AssetIndex,
) -> bool {
    let palette = crate::theme::NIGHT;
    let Some(before) = world
        .get::<SceneGameplayGraph>(entity)
        .map(|graph| graph.0.clone())
    else {
        return false;
    };
    let mut value = before.clone();
    note(
        ui,
        "Nodes this object runs while the game plays, as well as its script.",
    );
    super::material::asset_slot(
        ui,
        "Graph",
        &mut value,
        AssetKind::Graph,
        "None — drop a .graph file",
        assets,
    );
    let path = value
        .as_ref()
        .and_then(|reference| crate::assets::parse_asset_id(&reference.id))
        .and_then(|id| assets.path_for(id));
    if let Some(path) = path {
        let path = path.to_path_buf();
        property_row(ui, "", |ui| {
            if crate::widgets::pill_button(ui, &palette, "Edit graph", palette.accent, true)
                .clicked()
            {
                ui.ctx()
                    .data_mut(|data| data.insert_temp(open_request_id(), path));
            }
        });
    } else if value.is_none() {
        note(
            ui,
            "Make one in Project Assets: right-click a folder → New Gameplay Graph.",
        );
    }
    if value != before {
        world.entity_mut(entity).insert(SceneGameplayGraph(value));
        return true;
    }
    false
}
