//! The editor's [`SceneResolver`]: turns a scene's asset references into
//! live renderable components, using the CPU-side data
//! [`crate::AssetImporter`] already holds.
//!
//! ## Where the pieces come from
//!
//! [`AssetImporter`] owns decoded assets keyed by
//! [`engine_asset::AssetId`] — an [`ImportedGltf`] per model, an
//! [`ImportedTexture`] per image. A `MeshRenderer`, though, holds *GPU*
//! handles. This module is the bridge: look the id up, upload once, hand
//! back the component.
//!
//! ## Why uploads are cached
//!
//! A scene commonly places the same model many times. Uploading per
//! entity would cost one vertex buffer, one texture, and one bind group
//! per instance. [`EditorResolver`] keeps a per-id cache of the handles
//! it has already registered in [`RenderAssets`], so the fiftieth crate
//! in a scene reuses the first one's GPU memory.
//!
//! The cache lives on the resolver, which is built fresh per scene load,
//! so it can't outlive the [`RenderAssets`] its handles index into.

use std::collections::HashMap;

use bevy_ecs::prelude::Without;
use engine_asset::AssetId;
use engine_ecs::components::{MeshRenderer, MeshSource};
use engine_ecs::prelude::{Entity, World};
use engine_renderer::{
    GpuContext, Material, MaterialBinding, Mesh, Pipeline, RenderAssets, UvRect,
};
use engine_scene::{AssetRef, SceneError, SceneResolver};
use engine_utils::AssetHandle;

use crate::import::{AssetImporter, ImportedAsset};

/// Resolves scene asset references against the editor's import cache.
///
/// Borrows everything it needs rather than owning it: the importer holds
/// the decoded data, [`RenderAssets`] owns the uploaded handles, and both
/// outlive any single scene load.
pub struct EditorResolver<'a> {
    importer: &'a AssetImporter,
    gpu: &'a GpuContext,
    pipeline: &'a Pipeline,
    assets: &'a mut RenderAssets,
    /// Handles for meshes already uploaded this load, by asset id — see
    /// the module docs on why. Stores the handle pair rather than a whole
    /// `MeshRenderer`: the GPU resources are shared, but each entity needs
    /// its own model-matrix binding, and each share has to go through
    /// [`MeshRenderer::from_handles`] so the ref counts stay right.
    uploaded: HashMap<AssetId, (AssetHandle<Mesh>, AssetHandle<MaterialBinding>)>,
}

impl<'a> EditorResolver<'a> {
    /// Builds a resolver over the editor's importer and GPU state.
    pub fn new(
        importer: &'a AssetImporter,
        gpu: &'a GpuContext,
        pipeline: &'a Pipeline,
        assets: &'a mut RenderAssets,
    ) -> Self {
        Self {
            importer,
            gpu,
            pipeline,
            assets,
            uploaded: HashMap::new(),
        }
    }

    /// How many distinct meshes this resolver has uploaded.
    pub fn uploaded_count(&self) -> usize {
        self.uploaded.len()
    }
}

/// Parses an [`AssetRef`]'s UUID text into an [`AssetId`].
///
/// Scene files are untrusted, so malformed text is an error rather than a
/// panic. `engine_scene` can only hand back a `uuid::Uuid` (it has no
/// `engine_asset` dependency), so the conversion happens here.
fn asset_id(reference: &AssetRef) -> Result<AssetId, SceneError> {
    let uuid = reference.parse_id().map_err(SceneError::Resolve)?;
    Ok(AssetId::from_uuid(uuid))
}

impl SceneResolver for EditorResolver<'_> {
    fn resolve_mesh(
        &mut self,
        mesh: &AssetRef,
        _material: &AssetRef,
    ) -> Result<Option<MeshRenderer>, SceneError> {
        let id = asset_id(mesh)?;

        // Already uploaded for an earlier entity in this same load. The
        // ref-count check guards against a stale entry: if every entity
        // using this mesh was despawned, the store freed it and the
        // cached handle now points at nothing, so fall through and
        // re-upload rather than hand out a dangling handle.
        if let Some(&(mesh_handle, material_handle)) = self.uploaded.get(&id)
            && self.assets.meshes.ref_count(mesh_handle).is_some()
            && self.assets.materials.ref_count(material_handle).is_some()
        {
            return Ok(Some(MeshRenderer::from_handles(
                self.gpu,
                self.pipeline,
                self.assets,
                mesh_handle,
                material_handle,
            )));
        }

        // Not imported (yet) is not a failure: the asset pass may not
        // have reached it, or the file may arrive later via the watcher.
        let Some(asset) = self.importer.get(id) else {
            return Ok(None);
        };

        let ImportedAsset::Mesh(gltf) = asset else {
            return Err(SceneError::Resolve(format!(
                "asset {id} is not a mesh (a scene entity referenced it as one)"
            )));
        };

        let Some(primitive) = gltf.meshes.first() else {
            return Err(SceneError::Resolve(format!(
                "glTF asset {id} contains no mesh primitives"
            )));
        };

        // First embedded image as the base color; `None` lets
        // `from_geometry` fall back to white, so an untextured model still
        // renders lit. Shared with the standalone player's resolver so the
        // editor and a shipped build agree on what an asset looks like.
        let base_color = gltf
            .images
            .first()
            .map(|image| (image.width, image.height, image.rgba8.as_slice()));

        let renderer = MeshRenderer::from_geometry(
            self.gpu,
            self.pipeline,
            self.assets,
            &primitive.vertices,
            &primitive.indices,
            base_color,
            Material::default(),
        )
        .map_err(|err| SceneError::Resolve(err.to_string()))?;

        // Remember the handles so the next entity using this asset
        // shares the upload. `renderer` already holds ref count 1 for
        // each; every later share goes through `from_handles`, which
        // retains, so the counts track the number of live entities.
        self.uploaded.insert(id, (renderer.mesh, renderer.material));
        Ok(Some(renderer))
    }

    fn resolve_sprite_uv(
        &mut self,
        atlas: &AssetRef,
        _region: &str,
    ) -> Result<Option<UvRect>, SceneError> {
        let id = asset_id(atlas)?;
        let Some(asset) = self.importer.get(id) else {
            return Ok(None);
        };
        if !matches!(asset, ImportedAsset::Texture(_)) {
            return Err(SceneError::Resolve(format!(
                "asset {id} is not a texture (a sprite referenced it as an atlas)"
            )));
        }

        // Named atlas regions need an atlas layout the importer doesn't
        // build yet, so every sprite samples the whole texture for now.
        // Wrong UVs for a real sheet, but visible and non-fatal, and the
        // region name is preserved on `SpriteSource` for when layouts land.
        Ok(Some(UvRect {
            min: [0.0, 0.0],
            max: [1.0, 1.0],
        }))
    }
}

/// Turns every not-yet-resolved renderable reference in `world` into a
/// live component, and reports how many failed.
///
/// Run once per frame. It is the single mechanism behind every way an
/// entity can acquire geometry — a scene load, an asset dragged into the
/// viewport, an import that finished after the entity was created, a
/// source file changed on disk and re-imported. All of them leave a
/// carrier component ([`MeshSource`] or its sprite equivalent) with no live
/// renderable beside it, and all of them are picked up here.
///
/// Cheap when there is nothing to do: the query matches only entities
/// missing their renderable, so a fully-resolved scene walks an empty
/// result.
///
/// A reference that fails is logged once per pass and counted, never
/// retried into an error loop — the entity keeps its carrier, so a later
/// successful import still fixes it.
pub fn resolve_pending(
    world: &mut World,
    importer: &AssetImporter,
    gpu: &GpuContext,
    pipeline: &Pipeline,
    assets: &mut RenderAssets,
) -> ResolveReport {
    let pending: Vec<(Entity, MeshSource)> = world
        .query_filtered::<(Entity, &MeshSource), Without<MeshRenderer>>()
        .iter(world)
        .map(|(entity, source)| (entity, source.clone()))
        .collect();

    if pending.is_empty() {
        return ResolveReport::default();
    }

    let mut resolver = EditorResolver::new(importer, gpu, pipeline, assets);
    let mut report = ResolveReport::default();

    for (entity, source) in pending {
        let mesh = AssetRef { id: source.mesh };
        let material = AssetRef {
            id: source.material,
        };
        match resolver.resolve_mesh(&mesh, &material) {
            Ok(Some(renderer)) => {
                world.entity_mut(entity).insert(renderer);
                report.resolved += 1;
            }
            Ok(None) => report.pending += 1,
            Err(err) => {
                report.failed += 1;
                tracing::warn!(error = %err, "could not resolve a scene entity's mesh");
            }
        }
    }

    if report.resolved > 0 {
        tracing::info!(
            resolved = report.resolved,
            pending = report.pending,
            failed = report.failed,
            "resolved scene asset references into renderable components"
        );
    }

    report
}

/// What one [`resolve_pending`] pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResolveReport {
    /// References turned into live renderable components this pass.
    pub resolved: usize,
    /// References whose asset is not imported yet — will be retried.
    pub pending: usize,
    /// References that failed outright (unknown id, wrong kind, bad
    /// geometry). Surfaced to the Console.
    pub failed: usize,
}

impl ResolveReport {
    /// Whether this pass changed anything worth redrawing for.
    pub fn changed(&self) -> bool {
        self.resolved > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(id: &str) -> AssetRef {
        AssetRef { id: id.into() }
    }

    #[test]
    fn malformed_asset_id_is_an_error_not_a_panic() {
        let result = asset_id(&reference("not-a-uuid"));
        assert!(matches!(result, Err(SceneError::Resolve(_))));
    }

    #[test]
    fn well_formed_asset_id_parses() {
        let parsed = asset_id(&reference("3fa85f64-5717-4562-b3fc-2c963f66afa6"));
        assert!(parsed.is_ok());
    }

    #[test]
    fn empty_asset_id_is_rejected() {
        assert!(asset_id(&reference("")).is_err());
    }
}
