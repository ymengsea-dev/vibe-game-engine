//! Level-of-detail meshes for one model file: tiers the artist authored
//! (`Rock_LOD0`, `Rock_LOD1`, …) or simpler copies generated on load.
//!
//! Generation is deterministic and never writes to disk, so Studio and a
//! shipped game build the same tiers from the same file.

use crate::{ImportedGltf, ImportedMesh};

/// How much of the full mesh each generated tier keeps, most detailed
/// first. The full mesh itself is tier 0 and is not listed.
pub const GENERATED_LOD_RATIOS: [f32; 2] = [0.5, 0.25];

/// Meshes with fewer triangles than this are already cheap; generating
/// tiers for them saves nothing worth the extra GPU memory.
pub const MIN_TRIANGLES_TO_SIMPLIFY: usize = 300;

/// A generated tier must drop at least this share of the triangles of
/// the tier before it, or it is skipped as not worth drawing.
const MIN_REDUCTION: f32 = 0.1;

/// Error budget for the simplifier, relative to the mesh's size.
const TARGET_ERROR: f32 = 0.05;

/// The tier number in a mesh name ending `_LOD<n>` (any case), if any.
fn lod_suffix(name: &str) -> Option<(u32, &str)> {
    let at = name.rfind('_')?;
    let (stem, suffix) = name.split_at(at);
    let digits = suffix
        .get(1..4)
        .filter(|tag| tag.eq_ignore_ascii_case("lod"))
        .and_then(|_| suffix.get(4..))?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some((digits.parse().ok()?, stem))
}

/// The authored tiers of `gltf`, most detailed first, when the file holds
/// two or more unskinned meshes all named `<Name>_LOD<n>` with the same
/// `<Name>` and distinct `n`. `None` for any other file.
pub fn authored_lod_tiers(gltf: &ImportedGltf) -> Option<Vec<&ImportedMesh>> {
    if gltf.meshes.len() < 2 {
        return None;
    }
    let mut tiers = Vec::with_capacity(gltf.meshes.len());
    let mut stem = None;
    for mesh in &gltf.meshes {
        if mesh.skin_weights.is_some() {
            return None;
        }
        let (number, name) = lod_suffix(mesh.name.as_deref()?)?;
        match stem {
            None => stem = Some(name),
            Some(first) if first == name => {}
            Some(_) => return None,
        }
        tiers.push((number, mesh));
    }
    tiers.sort_by_key(|(number, _)| *number);
    if tiers.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return None;
    }
    Some(tiers.into_iter().map(|(_, mesh)| mesh).collect())
}

/// Simpler copies of `mesh` for far away, most detailed first (see
/// [`GENERATED_LOD_RATIOS`]). Empty when `mesh` is too small to be worth
/// it or the simplifier cannot reduce it.
pub fn generated_lod_tiers(mesh: &ImportedMesh) -> Vec<ImportedMesh> {
    let full = mesh.indices.len() / 3;
    if full < MIN_TRIANGLES_TO_SIMPLIFY || mesh.skin_weights.is_some() {
        return Vec::new();
    }
    let mut tiers = Vec::new();
    let mut previous = full;
    for ratio in GENERATED_LOD_RATIOS {
        let Some(tier) = simplify(mesh, ratio) else {
            break;
        };
        let triangles = tier.indices.len() / 3;
        if triangles as f32 > previous as f32 * (1.0 - MIN_REDUCTION) {
            break;
        }
        previous = triangles;
        tiers.push(tier);
    }
    tiers
}

/// `mesh` reduced to about `ratio` of its triangles, with unused vertices
/// dropped. `None` if nothing useful is left.
fn simplify(mesh: &ImportedMesh, ratio: f32) -> Option<ImportedMesh> {
    let positions: Vec<u8> = mesh
        .vertices
        .iter()
        .flat_map(|vertex| vertex.position)
        .flat_map(f32::to_ne_bytes)
        .collect();
    let adapter = meshopt::VertexDataAdapter::new(&positions, 12, 0).ok()?;
    let target = ((mesh.indices.len() as f32 * ratio) as usize / 3) * 3;
    // `Permissive` lets edges collapse across UV and normal seams; without
    // it most textured models barely simplify at all.
    let indices = meshopt::simplify(
        &mesh.indices,
        &adapter,
        target,
        TARGET_ERROR,
        meshopt::SimplifyOptions::Permissive,
        None,
    );
    if indices.len() < 3 {
        return None;
    }
    let (vertices, indices) = compact(&mesh.vertices, &indices)?;
    Some(ImportedMesh {
        name: mesh.name.clone(),
        vertices,
        indices,
        material: mesh.material,
        skin_weights: None,
        skeleton: None,
    })
}

/// Keeps only the vertices `indices` uses, renumbered in first-use order.
fn compact<V: Copy>(vertices: &[V], indices: &[u32]) -> Option<(Vec<V>, Vec<u32>)> {
    let mut remap = vec![u32::MAX; vertices.len()];
    let mut kept = Vec::new();
    let mut out = Vec::with_capacity(indices.len());
    for &index in indices {
        let slot = remap.get_mut(index as usize)?;
        if *slot == u32::MAX {
            *slot = u32::try_from(kept.len()).ok()?;
            kept.push(*vertices.get(index as usize)?);
        }
        out.push(*slot);
    }
    Some((kept, out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_renderer::Vertex;

    fn mesh(name: &str, vertices: Vec<Vertex>, indices: Vec<u32>) -> ImportedMesh {
        ImportedMesh {
            name: Some(name.to_owned()),
            vertices,
            indices,
            material: None,
            skin_weights: None,
            skeleton: None,
        }
    }

    fn gltf(meshes: Vec<ImportedMesh>) -> ImportedGltf {
        ImportedGltf {
            meshes,
            ..ImportedGltf::default()
        }
    }

    /// A flat `n` x `n` grid of quads: `2 n²` triangles.
    fn grid(n: u32) -> ImportedMesh {
        let mut vertices = Vec::new();
        for z in 0..=n {
            for x in 0..=n {
                vertices.push(Vertex {
                    position: [x as f32, 0.0, z as f32],
                    normal: [0.0, 1.0, 0.0],
                    uv: [x as f32 / n as f32, z as f32 / n as f32],
                });
            }
        }
        let mut indices = Vec::new();
        for z in 0..n {
            for x in 0..n {
                let a = z * (n + 1) + x;
                let b = a + 1;
                let c = a + n + 1;
                let d = c + 1;
                indices.extend([a, c, b, b, c, d]);
            }
        }
        mesh("Grid", vertices, indices)
    }

    #[test]
    fn lod_suffix_reads_the_tier_number() {
        assert_eq!(lod_suffix("Rock_LOD0"), Some((0, "Rock")));
        assert_eq!(lod_suffix("Big_Rock_lod12"), Some((12, "Big_Rock")));
        assert_eq!(lod_suffix("Rock_LOD"), None);
        assert_eq!(lod_suffix("Rock_LODx"), None);
        assert_eq!(lod_suffix("Rock"), None);
        assert_eq!(lod_suffix("Rock_Mesh1"), None);
    }

    #[test]
    fn authored_tiers_are_ordered_by_number() {
        let file = gltf(vec![
            mesh("Rock_LOD2", Vec::new(), Vec::new()),
            mesh("Rock_LOD0", Vec::new(), Vec::new()),
            mesh("Rock_LOD1", Vec::new(), Vec::new()),
        ]);
        let names: Vec<_> = authored_lod_tiers(&file)
            .unwrap()
            .into_iter()
            .map(|tier| tier.name.clone().unwrap())
            .collect();
        assert_eq!(names, ["Rock_LOD0", "Rock_LOD1", "Rock_LOD2"]);
    }

    #[test]
    fn authored_tiers_need_matching_names() {
        let single = gltf(vec![mesh("Rock_LOD0", Vec::new(), Vec::new())]);
        assert!(authored_lod_tiers(&single).is_none());
        let mixed = gltf(vec![
            mesh("Rock_LOD0", Vec::new(), Vec::new()),
            mesh("Tree_LOD1", Vec::new(), Vec::new()),
        ]);
        assert!(authored_lod_tiers(&mixed).is_none());
        let plain = gltf(vec![
            mesh("Rock_LOD0", Vec::new(), Vec::new()),
            mesh("Rock", Vec::new(), Vec::new()),
        ]);
        assert!(authored_lod_tiers(&plain).is_none());
        let repeated = gltf(vec![
            mesh("Rock_LOD1", Vec::new(), Vec::new()),
            mesh("Rock_lod1", Vec::new(), Vec::new()),
        ]);
        assert!(authored_lod_tiers(&repeated).is_none());
    }

    #[test]
    fn generated_tiers_get_simpler_and_stay_valid() {
        let full = grid(24);
        let tiers = generated_lod_tiers(&full);
        assert!(!tiers.is_empty());
        let mut previous = full.indices.len();
        for tier in &tiers {
            assert!(tier.indices.len() < previous);
            assert_eq!(tier.indices.len() % 3, 0);
            assert!(
                tier.indices
                    .iter()
                    .all(|&index| (index as usize) < tier.vertices.len())
            );
            assert!(tier.vertices.len() <= full.vertices.len());
            previous = tier.indices.len();
        }
    }

    #[test]
    fn small_meshes_get_no_generated_tiers() {
        assert!(generated_lod_tiers(&grid(4)).is_empty());
    }

    #[test]
    fn compact_drops_unused_vertices() {
        let (vertices, indices) = compact(&[10, 11, 12, 13], &[3, 1, 3]).unwrap();
        assert_eq!(vertices, [13, 11]);
        assert_eq!(indices, [0, 1, 0]);
        assert!(compact(&[10], &[4]).is_none());
    }
}
