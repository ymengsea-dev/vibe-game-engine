//! GPU-resident asset storage: where a [`Mesh`]/[`MaterialBinding`]
//! actually lives once a component only holds a handle to it.
//!
//! See `engine_utils::AssetStore`'s docs for the ref-counting contract
//! (`insert` starts a fresh asset at count `1`, `retain`/`release` share
//! and give up ownership). This module just instantiates that generic
//! store for the two GPU resource kinds `engine_ecs::MeshRenderer`
//! references.

use engine_utils::AssetStore;

use crate::mesh::Mesh;
use crate::pipeline::MaterialBinding;
use crate::skinning::SkinnedMesh;

/// The live meshes and material bindings a frame's `MeshRenderer`
/// components resolve their handles against.
///
/// One instance is owned by whoever owns the [`crate::GpuContext`] (the
/// app loop / editor), the same arrangement as every other GPU handle
/// collection in this engine — not a `bevy_ecs` resource, since there's
/// exactly one of these per renderer, not per-`World` state.
#[derive(Default)]
pub struct RenderAssets {
    /// Every currently-registered mesh, keyed by the identity backing its
    /// handles.
    pub meshes: AssetStore<Mesh>,
    /// Every currently-registered skinned mesh, keyed by the identity
    /// backing its handles.
    pub skinned_meshes: AssetStore<SkinnedMesh>,
    /// Every currently-registered material binding, keyed by the identity
    /// backing its handles.
    pub materials: AssetStore<MaterialBinding>,
}

impl RenderAssets {
    /// An empty store — no meshes or materials registered yet.
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_store_is_empty() {
        let assets = RenderAssets::new();
        assert!(assets.meshes.is_empty());
        assert!(assets.skinned_meshes.is_empty());
        assert!(assets.materials.is_empty());
    }
}
