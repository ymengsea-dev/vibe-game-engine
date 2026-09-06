//! Reading a live entity's renderable state back into serializable
//! scene data — the capture half of [`crate::resolve`].
//!
//! ## Why this exists
//!
//! [`engine_ecs::components::MeshRenderer`] holds GPU handles, not asset
//! ids, so it can't be serialized on its own. The ids travel on sibling
//! carrier components ([`MeshSource`], [`SpriteSource`]) that
//! [`crate::resolve::insert_components`] stamps on every spawn. This
//! module reads them back.
//!
//! ## Why it's a shared function
//!
//! Two callers need exactly this logic — [`crate::Scene::from_world`] and
//! the editor's single-entity prefab capture. They are in different
//! crates, and before this they were the third and fourth divergent copies
//! of the scene-entity conversion (the first two were the spawn paths that
//! [`crate::resolve`] unified). One function, so they cannot drift again.

use bevy_ecs::prelude::{Entity, World};
use engine_ecs::components::{MeshSource, Sprite, SpriteSource};

use crate::format::{AssetRef, MeshRendererData, SpriteData};

/// Reads `entity`'s renderable components back into their serializable
/// form.
///
/// Returns `(mesh_renderer, sprite)`, either of which is `None` when the
/// entity carries no such reference — assign them straight onto a
/// [`crate::SceneEntity`].
///
/// A reference is captured from its carrier component
/// ([`MeshSource`]/[`SpriteSource`]), *not* from the live renderable, so
/// an entity whose asset failed to resolve still round-trips its ids
/// instead of silently losing them.
///
/// For sprites, a live [`Sprite`]'s size and tint win over the stored
/// values, so an Inspector edit survives the save; the stored values are
/// the fallback for the unresolved case, where no live `Sprite` exists.
pub fn capture_renderables(
    world: &World,
    entity: Entity,
) -> (Option<MeshRendererData>, Option<SpriteData>) {
    let mesh_renderer = world
        .get::<MeshSource>(entity)
        .map(|source| MeshRendererData {
            mesh: AssetRef {
                id: source.mesh.clone(),
            },
            material: AssetRef {
                id: source.material.clone(),
            },
        });

    let sprite = world.get::<SpriteSource>(entity).map(|source| {
        let live = world.get::<Sprite>(entity);
        SpriteData {
            atlas: AssetRef {
                id: source.atlas.clone(),
            },
            region: source.region.clone(),
            size: live.map_or(source.size, |sprite| sprite.size.to_array()),
            color: live.map_or(source.color, |sprite| sprite.color),
            z_order: live.map_or(source.z_order, |sprite| sprite.z_order),
        }
    });

    (mesh_renderer, sprite)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_renderer::UvRect;

    fn mesh_source() -> MeshSource {
        MeshSource::new(
            "3fa85f64-5717-4562-b3fc-2c963f66afa6",
            "9c858901-8a57-4791-81fe-4c455b099bc9",
        )
    }

    fn sprite_source() -> SpriteSource {
        SpriteSource {
            atlas: "16fd2706-8baf-433b-82eb-8c7fada847da".into(),
            region: "coin_0".into(),
            size: [2.0, 3.0],
            color: [1.0, 0.5, 0.25, 1.0],
            z_order: 0.0,
        }
    }

    #[test]
    fn captures_mesh_reference_from_the_carrier_component() {
        let mut world = World::new();
        let entity = world.spawn(mesh_source()).id();

        let (mesh_renderer, sprite) = capture_renderables(&world, entity);

        let mesh_renderer = mesh_renderer.expect("mesh reference should be captured");
        assert_eq!(
            mesh_renderer.mesh.id,
            "3fa85f64-5717-4562-b3fc-2c963f66afa6"
        );
        assert_eq!(
            mesh_renderer.material.id,
            "9c858901-8a57-4791-81fe-4c455b099bc9"
        );
        assert!(sprite.is_none());
    }

    #[test]
    fn captures_mesh_reference_even_without_a_live_renderer() {
        // The unresolved case: the asset file is missing, so no
        // `MeshRenderer` was ever built — the ids must survive anyway.
        let mut world = World::new();
        let entity = world.spawn(mesh_source()).id();

        let (mesh_renderer, _) = capture_renderables(&world, entity);

        assert!(
            mesh_renderer.is_some(),
            "a reference must not be dropped just because it didn't resolve"
        );
    }

    #[test]
    fn live_sprite_size_and_tint_win_over_stored_values() {
        let mut world = World::new();
        let mut live = Sprite::new(
            glam::Vec2::new(9.0, 9.0),
            UvRect {
                min: [0.0, 0.0],
                max: [1.0, 1.0],
            },
        );
        live.color = [0.0, 1.0, 0.0, 0.5];
        live.z_order = 4.0;
        let entity = world.spawn((sprite_source(), live)).id();

        let (_, sprite) = capture_renderables(&world, entity);

        let sprite = sprite.expect("sprite should be captured");
        assert_eq!(
            sprite.size,
            [9.0, 9.0],
            "inspector edits must survive a save"
        );
        assert_eq!(sprite.color, [0.0, 1.0, 0.0, 0.5]);
        assert_eq!(
            sprite.z_order, 4.0,
            "a layer changed at runtime must survive a save"
        );
        assert_eq!(
            sprite.region, "coin_0",
            "identity still comes from the carrier"
        );
    }

    #[test]
    fn stored_values_are_the_fallback_when_the_sprite_did_not_resolve() {
        let mut world = World::new();
        let entity = world.spawn(sprite_source()).id();

        let (_, sprite) = capture_renderables(&world, entity);

        let sprite = sprite.expect("sprite reference should be captured");
        assert_eq!(sprite.size, [2.0, 3.0]);
        assert_eq!(sprite.color, [1.0, 0.5, 0.25, 1.0]);
    }

    #[test]
    fn entity_without_carriers_captures_nothing() {
        let mut world = World::new();
        let entity = world.spawn_empty().id();

        let (mesh_renderer, sprite) = capture_renderables(&world, entity);

        assert!(mesh_renderer.is_none());
        assert!(sprite.is_none());
    }
}
